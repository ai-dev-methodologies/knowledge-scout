// Vendored from vercel-labs/agent-browser (Apache-2.0), cli/src/native/cdp/client.rs at commit
// e7519957b9067c541825afa21a792fbe1f483047. See NOTICE and third_party/agent-browser/LICENSE.
// Modifications: trimmed to what a local, single-tab client needs, and the transport changed from
// a WebSocket to Chrome's --remote-debugging-pipe (NUL-terminated JSON messages on two pipes), so
// the browser listens on no TCP port. Removed TLS connectors, custom headers, keepalive, the
// inspect proxy (raw message broadcast), private session routes and lossless method taps. Kept
// the request/response correlation, the event broadcast, cancellation cleanup and the
// lone-surrogate repair (page text returned by Runtime.evaluate can contain them). Added is_closed
// so the daemon can tell a dead browser from a live one.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::de::IgnoredAny;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::pipe;
use tokio::sync::{broadcast, oneshot, Mutex};
use tokio::task::JoinHandle;

use super::types::{CdpCommand, CdpError, CdpEvent, CdpMessage};

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<CdpMessage>>>>;
type WsTx = Arc<Mutex<pipe::Sender>>;

const CONNECTION_CLOSED_ERROR: &str = "CDP connection closed";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
struct CdpIdEnvelope {
    id: Option<u64>,
    #[serde(rename = "result")]
    _result: Option<IgnoredAny>,
    #[serde(rename = "error")]
    _error: Option<IgnoredAny>,
    #[serde(rename = "method")]
    _method: Option<IgnoredAny>,
    #[serde(rename = "params")]
    _params: Option<IgnoredAny>,
    #[serde(rename = "sessionId")]
    _session_id: Option<IgnoredAny>,
}

fn sanitize_lone_surrogates(msg: &str) -> Option<String> {
    const ESCAPE_LEN: usize = 6;
    const HIGH_START: u32 = 0xD800;
    const LOW_START: u32 = 0xDC00;
    const SURROGATE_END: u32 = 0xE000;

    let read_escape = |bytes: &[u8], at: usize| -> Option<u32> {
        let end = at.checked_add(ESCAPE_LEN)?;
        if end > bytes.len() || bytes[at] != b'\\' || bytes[at + 1] != b'u' {
            return None;
        }
        let hex = &bytes[at + 2..end];
        if !hex.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        u32::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()
    };

    let bytes = msg.as_bytes();
    let mut out: Option<String> = None;
    let mut copied = 0;
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] != b'u' {
            i += 2;
            continue;
        }

        let Some(unit) = read_escape(bytes, i) else {
            i += 1;
            continue;
        };

        let is_high = (HIGH_START..LOW_START).contains(&unit);
        let is_low = (LOW_START..SURROGATE_END).contains(&unit);
        let paired = is_high
            && read_escape(bytes, i + ESCAPE_LEN)
                .is_some_and(|next| (LOW_START..SURROGATE_END).contains(&next));

        if paired {
            i += ESCAPE_LEN * 2;
            continue;
        }

        if is_high || is_low {
            let buf = out.get_or_insert_with(String::new);
            buf.push_str(&msg[copied..i]);
            buf.push(char::REPLACEMENT_CHARACTER);
            copied = i + ESCAPE_LEN;
        }
        i += ESCAPE_LEN;
    }

    if let Some(buf) = out.as_mut() {
        buf.push_str(&msg[copied..]);
    }

    out
}

fn extract_command_id(msg: &str) -> Option<u64> {
    let envelope: CdpIdEnvelope = serde_json::from_str(msg).ok()?;
    envelope.id.filter(|id| *id > 0)
}

pub struct CdpClient {
    ws_tx: WsTx,
    closed: Arc<AtomicBool>,
    next_id: AtomicU64,
    pending: PendingMap,
    event_tx: broadcast::Sender<CdpEvent>,
    reader_handle: Mutex<Option<JoinHandle<()>>>,
}

/// Removes a pending entry if `send_command` is cancelled mid-await, so a
/// command whose response never comes can't leak until the connection closes.
/// Normal exits disarm it via `done`.
struct PendingGuard {
    pending: PendingMap,
    id: u64,
    done: bool,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let pending = self.pending.clone();
        let id = self.id;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                pending.lock().await.remove(&id);
            });
        }
    }
}

impl CdpClient {
    /// Talk CDP over Chrome's --remote-debugging-pipe: `write` is Chrome's fd 3 (commands),
    /// `read` is Chrome's fd 4 (responses and events). Must be called inside a tokio runtime.
    pub fn from_pipe(write: std::fs::File, read: std::fs::File) -> Result<Self, String> {
        let ws_tx = pipe::Sender::from_file(write).map_err(|e| format!("CDP pipe: {}", e))?;
        let mut ws_rx = pipe::Receiver::from_file(read).map_err(|e| format!("CDP pipe: {}", e))?;
        let ws_tx = Arc::new(Mutex::new(ws_tx));

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, _) = broadcast::channel(4096);
        let closed = Arc::new(AtomicBool::new(false));

        let pending_clone = pending.clone();
        let event_tx_clone = event_tx.clone();
        let closed_reader = closed.clone();

        let reader_handle = tokio::spawn(async move {
            let mut buf: Vec<u8> = Vec::new();
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                let n = match ws_rx.read(&mut chunk).await {
                    Ok(0) | Err(_) => break, // Chrome closed its end (exited) or the pipe failed
                    Ok(n) => n,
                };
                buf.extend_from_slice(&chunk[..n]);
                // every message is terminated by a NUL byte
                while let Some(end) = buf.iter().position(|b| *b == 0) {
                    let frame: Vec<u8> = buf.drain(..=end).collect();
                    if let Ok(msg) = std::str::from_utf8(&frame[..end]) {
                        dispatch_message(msg, &pending_clone, &event_tx_clone).await;
                    }
                }
            }

            // Reader loop exited (connection closed or error). Drop all pending
            // command senders so callers get an immediate channel-closed error
            // instead of waiting for the timeout.
            closed_reader.store(true, Ordering::SeqCst);
            pending_clone.lock().await.clear();
        });

        Ok(Self {
            ws_tx,
            closed,
            next_id: AtomicU64::new(1),
            pending,
            event_tx,
            reader_handle: Mutex::new(Some(reader_handle)),
        })
    }

    fn ensure_open(&self) -> Result<(), String> {
        if self.closed.load(Ordering::SeqCst) {
            Err(CONNECTION_CLOSED_ERROR.to_string())
        } else {
            Ok(())
        }
    }

    /// True once the pipe reached EOF (Chrome exited) or failed, or `close` was called.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub async fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let _ = tokio::time::timeout(Duration::from_millis(500), async {
            let mut ws_tx = self.ws_tx.lock().await;
            let _ = ws_tx.shutdown().await;
        })
        .await;
        if let Some(task) = self.reader_handle.lock().await.take() {
            task.abort();
        }
        self.pending.lock().await.clear();
    }

    pub async fn send_command(
        &self,
        method: &str,
        params: Option<Value>,
        session_id: Option<&str>,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let cmd = CdpCommand {
            id,
            method: method.to_string(),
            params,
            session_id: session_id.filter(|s| !s.is_empty()).map(|s| s.to_string()),
        };

        let json = serde_json::to_string(&cmd)
            .map_err(|e| format!("Failed to serialize CDP command: {}", e))?;

        let (tx, rx) = oneshot::channel();

        // Cleans up the pending entry if this future is cancelled mid-await.
        let mut guard = PendingGuard {
            pending: self.pending.clone(),
            id,
            done: false,
        };

        {
            let mut ws_tx = self.ws_tx.lock().await;
            self.ensure_open()?;
            let mut pending = self.pending.lock().await;
            pending.insert(id, tx);
            drop(pending);
            let mut frame = json.into_bytes();
            frame.push(0); // pipe framing: NUL-terminated
            if let Err(error) = ws_tx.write_all(&frame).await {
                self.pending.lock().await.remove(&id);
                guard.done = true;
                return Err(format!("Failed to send CDP command: {}", error));
            }
        }

        let response = match tokio::time::timeout(COMMAND_TIMEOUT, rx).await {
            Ok(Ok(resp)) => {
                guard.done = true;
                resp
            }
            Ok(Err(_)) => {
                guard.done = true;
                return Err("CDP response channel closed".to_string());
            }
            Err(_) => {
                guard.done = true;
                self.pending.lock().await.remove(&id);
                return Err(format!("CDP command timed out: {}", method));
            }
        };

        if let Some(error) = response.error {
            return Err(format!("CDP error ({}): {}", method, error));
        }

        Ok(response.result.unwrap_or(Value::Null))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.event_tx.subscribe()
    }

    pub async fn send_command_typed<R: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<R, String> {
        let result = self.send_command(method, Some(params), session_id).await?;
        serde_json::from_value(result)
            .map_err(|e| format!("Failed to deserialize CDP response for {}: {}", method, e))
    }
}


/// Route one CDP message to the command waiting for it, or to the event broadcast.
async fn dispatch_message(msg: &str, pending: &PendingMap, event_tx: &broadcast::Sender<CdpEvent>) {
    let parsed: CdpMessage = match serde_json::from_str(msg) {
        Ok(parsed) => parsed,
        Err(_) => {
            let repaired = sanitize_lone_surrogates(msg);
            let candidate = repaired.as_deref().unwrap_or(msg);
            match serde_json::from_str(candidate) {
                Ok(parsed) => parsed,
                Err(error) => {
                    if let Some(id) = extract_command_id(candidate) {
                        let waiter = pending.lock().await.remove(&id);
                        if let Some(tx) = waiter {
                            let _ = tx.send(CdpMessage {
                                id: Some(id),
                                result: None,
                                error: Some(CdpError {
                                    code: None,
                                    message: format!("Malformed CDP response: {}", error),
                                    data: None,
                                }),
                                method: None,
                                params: None,
                                session_id: None,
                            });
                        }
                    }
                    return;
                }
            }
        }
    };

    if let Some(id) = parsed.id {
        // Response to a command
        if let Some(tx) = pending.lock().await.remove(&id) {
            let _ = tx.send(parsed);
        }
    } else if let Some(ref method) = parsed.method {
        // Event
        let _ = event_tx.send(CdpEvent {
            method: method.clone(),
            params: parsed.params.clone().unwrap_or(Value::Null),
            session_id: parsed.session_id.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repairs_only_unpaired_surrogate_escapes() {
        let msg = r#"{"id":1,"result":{"high":"\ud800","low":"\udfff","pair":"😀","escaped":"\\ud800","text":"café 中文"}}"#;
        let repaired = sanitize_lone_surrogates(msg).expect("lone surrogates should be repaired");
        let parsed: CdpMessage = serde_json::from_str(&repaired).expect("repaired message");
        let result = parsed.result.expect("result");

        assert_eq!(result["high"], "\u{fffd}");
        assert_eq!(result["low"], "\u{fffd}");
        assert_eq!(result["pair"], "😀");
        assert_eq!(result["escaped"], r"\ud800");
        assert_eq!(result["text"], "café 中文");
    }

    #[test]
    fn extracts_only_positive_top_level_command_ids() {
        assert_eq!(
            extract_command_id(r#"{"id":7,"result":{"value":"\ud800"}}"#),
            Some(7)
        );
        assert_eq!(
            extract_command_id(r#"{"result":{"id":42,"value":"\ud800"}}"#),
            None
        );
        assert_eq!(extract_command_id(r#"{"id":0,"result":{}}"#), None);
    }
}
