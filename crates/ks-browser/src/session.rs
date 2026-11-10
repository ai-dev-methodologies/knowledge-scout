//! One Chrome on one persistent profile directory, driven over CDP (one page target).
use crate::cdp::chrome::{launch_chrome, ChromeProcess, LaunchOptions};
use crate::cdp::client::CdpClient;
use crate::cdp::types::generated::{cdp_page::NavigateResult, cdp_runtime::EvaluateResult, cdp_target::AttachToTargetResult, cdp_target::CreateTargetResult};
use crate::detect::PageSnapshot;
use crate::Mode;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const LOAD_TIMEOUT: Duration = Duration::from_secs(30);
const SNAPSHOT_JS: &str = r#"(() => {
  const n = performance.getEntriesByType('navigation')[0];
  return JSON.stringify({
    url: location.href,
    title: document.title,
    status: (n && n.responseStatus) || 0,
    html: document.documentElement ? document.documentElement.outerHTML : ''
  });
})()"#;

pub struct Session {
    chrome: ChromeProcess,
    cdp: CdpClient,
    page: String,
    pub mode: Mode,
    profile_dir: PathBuf,
}

/// Pid of the Chrome holding `<profile>/SingletonLock` (a symlink to "host-pid"), if it is alive.
pub fn singleton_holder(profile_dir: &Path) -> Option<i32> {
    let target = std::fs::read_link(profile_dir.join("SingletonLock")).ok()?;
    let pid: i32 = target.to_str()?.rsplit('-').next()?.parse().ok()?;
    let alive = unsafe { libc::kill(pid, 0) } == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    alive.then_some(pid)
}

/// Wait until no live Chrome holds the profile. A stale lock (dead pid) is left for Chrome to clear;
/// we never delete files in a profile directory.
async fn wait_for_profile(profile_dir: &Path, timeout: Duration) -> Result<(), String> {
    let start = Instant::now();
    while let Some(pid) = singleton_holder(profile_dir) {
        if start.elapsed() > timeout {
            return Err(format!("profile {} is in use by another Chrome (pid {pid}); close it first", profile_dir.display()));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

impl Session {
    pub async fn launch(profile_dir: &Path, mode: Mode) -> Result<Session, String> {
        crate::paths::ensure_private_dir(profile_dir).map_err(|e| format!("{}: {e}", profile_dir.display()))?;
        wait_for_profile(profile_dir, Duration::from_secs(15)).await?;
        let opts = LaunchOptions { headless: mode == Mode::Headless, user_data_dir: profile_dir.to_path_buf(), executable_path: None };
        let mut chrome = tokio::task::spawn_blocking(move || launch_chrome(&opts)).await.map_err(|e| e.to_string())??;
        let (to_chrome, from_chrome) = chrome.take_pipes().ok_or("Chrome CDP pipe missing")?;
        let cdp = CdpClient::from_pipe(to_chrome, from_chrome)?;

        // Use the window's initial tab so a headful user sees exactly one tab.
        let mut target = None;
        let start = Instant::now();
        while target.is_none() && start.elapsed() < Duration::from_secs(5) {
            let t = cdp.send_command("Target.getTargets", None, None).await?;
            target = t["targetInfos"].as_array().and_then(|a| a.iter().find(|i| i["type"] == "page")).and_then(|i| i["targetId"].as_str()).map(String::from);
            if target.is_none() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        let target_id = match target {
            Some(t) => t,
            None => cdp.send_command_typed::<CreateTargetResult>("Target.createTarget", json!({"url": "about:blank"}), None).await?.target_id,
        };
        let attach: AttachToTargetResult = cdp.send_command_typed("Target.attachToTarget", json!({"targetId": target_id, "flatten": true}), None).await?;
        let page = attach.session_id;
        cdp.send_command("Page.enable", None, Some(&page)).await?;
        cdp.send_command("Network.enable", None, Some(&page)).await?;
        Ok(Session { chrome, cdp, page, mode, profile_dir: profile_dir.to_path_buf() })
    }

    pub fn profile_dir(&self) -> &Path {
        &self.profile_dir
    }

    /// Pid of the main Chrome process.
    pub fn browser_pid(&self) -> u32 {
        self.chrome.id()
    }

    /// Navigate and wait for the load event (at most 30 s). Returns the HTTP status of the main
    /// document as seen on the network (0 when unknown), which also covers Chrome's own error page
    /// for empty 4xx/5xx responses where the page itself cannot report a status.
    pub async fn navigate(&self, url: &str) -> Result<u16, String> {
        let mut events = self.cdp.subscribe();
        let nav: NavigateResult = self.cdp.send_command_typed("Page.navigate", json!({"url": url}), Some(&self.page)).await?;
        if let Some(err) = nav.error_text.filter(|e| !e.is_empty() && !e.contains("ERR_HTTP_RESPONSE_CODE_FAILURE")) {
            return Err(format!("navigation failed: {err}"));
        }
        let mut status = 0u16;
        let deadline = tokio::time::Instant::now() + LOAD_TIMEOUT;
        loop {
            let ev = match tokio::time::timeout_at(deadline, events.recv()).await {
                Err(_) => break, // load never fired: read whatever is there
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(_)) => return Err("browser connection closed".into()),
                Ok(Ok(ev)) => ev,
            };
            if ev.session_id.as_deref() != Some(self.page.as_str()) {
                continue;
            }
            match ev.method.as_str() {
                "Network.responseReceived" if ev.params["type"] == "Document" && ev.params["frameId"].as_str() == Some(nav.frame_id.as_str()) => {
                    status = ev.params["response"]["status"].as_u64().unwrap_or(0) as u16;
                }
                "Page.loadEventFired" => break,
                "Page.frameStoppedLoading" if ev.params["frameId"].as_str() == Some(nav.frame_id.as_str()) => break,
                _ => {}
            }
        }
        Ok(status)
    }

    /// The current page as the user would see it: URL, title, HTTP status and outerHTML, read through
    /// `Runtime.evaluate` in a fresh isolated world, so page scripts (which can replace `JSON`,
    /// `document.title` and friends in their own world) cannot change what is reported.
    pub async fn snapshot(&self) -> Result<PageSnapshot, String> {
        let tree = self.cdp.send_command("Page.getFrameTree", None, Some(&self.page)).await?;
        let frame_id = tree["frameTree"]["frame"]["id"].as_str().ok_or("page snapshot: no main frame")?;
        let world = self.cdp.send_command("Page.createIsolatedWorld", Some(json!({"frameId": frame_id, "worldName": "ks-snapshot"})), Some(&self.page)).await?;
        let context = world["executionContextId"].as_i64().ok_or("page snapshot: no isolated world")?;
        let r: EvaluateResult = self
            .cdp
            .send_command_typed("Runtime.evaluate", json!({"expression": SNAPSHOT_JS, "returnByValue": true, "contextId": context}), Some(&self.page))
            .await?;
        if let Some(e) = r.exception_details {
            return Err(format!("page snapshot failed: {}", e.text));
        }
        let raw = r.result.value.as_ref().and_then(Value::as_str).ok_or("page snapshot returned no value")?;
        let v: Value = serde_json::from_str(raw).map_err(|e| format!("page snapshot: {e}"))?;
        Ok(PageSnapshot {
            url: v["url"].as_str().unwrap_or_default().to_string(),
            title: v["title"].as_str().unwrap_or_default().to_string(),
            status: v["status"].as_u64().unwrap_or(0) as u16,
            html: v["html"].as_str().unwrap_or_default().to_string(),
        })
    }

    /// False once Chrome has exited (crash, Cmd+Q on a visible window) or the CDP pipe is closed:
    /// the session can never work again.
    pub fn is_alive(&mut self) -> bool {
        !self.chrome.has_exited() && !self.cdp.is_closed()
    }

    /// Drop a dead session: kill what is left of Chrome's process group (helpers of a crashed
    /// browser) and reap it, so the profile lock is free for the next launch.
    pub fn discard(self) {
        let Session { mut chrome, .. } = self;
        chrome.kill();
    }

    pub async fn bring_to_front(&self) -> Result<(), String> {
        self.cdp.send_command("Page.bringToFront", None, Some(&self.page)).await.map(|_| ())
    }

    /// Graceful close: Browser.close lets Chrome flush cookies to the profile before it exits; it is
    /// killed only if it has not exited after 10 s. Returns once the profile lock is released.
    pub async fn close(self) {
        let Session { mut chrome, cdp, profile_dir, .. } = self;
        let _ = tokio::time::timeout(Duration::from_secs(5), cdp.send_command("Browser.close", None, None)).await;
        cdp.close().await;
        let _ = tokio::task::spawn_blocking(move || {
            chrome.wait_or_kill(Duration::from_secs(10));
            chrome
        })
        .await;
        let _ = wait_for_profile(&profile_dir, Duration::from_secs(5)).await;
    }
}
