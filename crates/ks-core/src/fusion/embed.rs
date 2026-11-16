//! Query (and card) embedding clients of the switch: a local llama-server (`POST /v1/embeddings`, the OpenAI-style
//! endpoint the search lab used; bge-m3) and a read-only vector cache directory (`<sha256(text)>.f32`, the search lab's
//! query cache; for benchmarks without a server). Card and query text never leave this machine: only a loopback URL is
//! accepted.
use super::payload::unit_in_place;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Embeds texts into unit vectors. `limit`: the whole call must answer within it (the switch's timeout).
pub trait Embedder: Send + Sync {
    fn embed(&self, texts: &[String], limit: Duration) -> Result<Vec<Vec<f32>>, String>;
}

/// No embedding source configured: every call fails at once (switch auto ranks lexically, switch on is an error).
pub struct NoEmbedder;
impl Embedder for NoEmbedder {
    fn embed(&self, _: &[String], _: Duration) -> Result<Vec<Vec<f32>>, String> {
        Err("no embedding server configured".into())
    }
}

pub struct HttpEmbedder {
    addr: SocketAddr,
    host: String,
}

impl HttpEmbedder {
    /// `http://127.0.0.1:<port>` or `http://localhost:<port>` only.
    pub fn new(url: &str) -> Result<HttpEmbedder, String> {
        let rest = url.strip_prefix("http://").ok_or_else(|| format!("the embedding server must be on this machine (http://127.0.0.1:<port>), not {url}"))?;
        let rest = rest.trim_end_matches('/');
        let (host, port) = rest.split_once(':').ok_or_else(|| format!("the embedding server URL needs a port: {url}"))?;
        if host != "127.0.0.1" && host != "localhost" {
            return Err(format!("the embedding server must be on this machine (http://127.0.0.1:<port>), not {url}"));
        }
        let port: u16 = port.parse().map_err(|_| format!("bad port in {url}"))?;
        let addr = ("127.0.0.1", port).to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("no address")?;
        Ok(HttpEmbedder { addr, host: format!("{host}:{port}") })
    }

    fn request(&self, body: &str, limit: Duration) -> Result<String, String> {
        let t0 = Instant::now();
        let left = || limit.checked_sub(t0.elapsed()).filter(|d| !d.is_zero()).ok_or_else(|| format!("no answer in {} ms", limit.as_millis()));
        let mut s = TcpStream::connect_timeout(&self.addr, left()?).map_err(|e| format!("embedding server: {e}"))?;
        s.set_nodelay(true).ok();
        s.set_write_timeout(Some(left()?)).map_err(|e| e.to_string())?;
        let req = format!("POST /v1/embeddings HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", self.host, body.len());
        s.write_all(req.as_bytes()).map_err(|e| format!("embedding server: {e}"))?;
        let mut buf = vec![];
        let mut chunk = [0u8; 65536];
        loop {
            s.set_read_timeout(Some(left()?)).map_err(|e| e.to_string())?;
            match s.read(&mut chunk) {
                Ok(0) => break,
                Ok(k) => buf.extend_from_slice(&chunk[..k]),
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => return Err(format!("no answer in {} ms", limit.as_millis())),
                Err(e) => return Err(format!("embedding server: {e}")),
            }
        }
        let text = String::from_utf8(buf).map_err(|_| "embedding server: reply is not UTF-8".to_string())?;
        let (head, body) = text.split_once("\r\n\r\n").ok_or("embedding server: malformed reply")?;
        let status = head.split_whitespace().nth(1).unwrap_or("");
        if status != "200" {
            return Err(format!("embedding server answered {status}"));
        }
        if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
            return dechunk(body);
        }
        Ok(body.to_string())
    }
}

fn dechunk(mut s: &str) -> Result<String, String> {
    let mut out = String::new();
    loop {
        let (size, rest) = s.split_once("\r\n").ok_or("embedding server: malformed chunk")?;
        let n = usize::from_str_radix(size.split(';').next().unwrap_or("").trim(), 16).map_err(|_| "embedding server: malformed chunk size")?;
        if n == 0 {
            return Ok(out);
        }
        out.push_str(rest.get(..n).ok_or("embedding server: truncated chunk")?);
        s = rest.get(n + 2..).ok_or("embedding server: truncated chunk")?;
    }
}

impl Embedder for HttpEmbedder {
    fn embed(&self, texts: &[String], limit: Duration) -> Result<Vec<Vec<f32>>, String> {
        let body = serde_json::json!({"input": texts, "encoding_format": "float"}).to_string();
        let reply = self.request(&body, limit)?;
        let v: serde_json::Value = serde_json::from_str(&reply).map_err(|_| "embedding server: reply is not JSON".to_string())?;
        let mut data: Vec<&serde_json::Value> = v.get("data").and_then(|d| d.as_array()).ok_or("embedding server: no data")?.iter().collect();
        data.sort_by_key(|d| d.get("index").and_then(|i| i.as_u64()).unwrap_or(0));
        if data.len() != texts.len() {
            return Err("embedding server: wrong number of vectors".into());
        }
        data.into_iter()
            .map(|d| {
                let mut e: Vec<f32> = d.get("embedding").and_then(|x| x.as_array()).ok_or("embedding server: no embedding")?.iter().map(|x| x.as_f64().unwrap_or(f64::NAN) as f32).collect();
                unit_in_place(&mut e, true);
                if !e.iter().all(|x| x.is_finite()) {
                    return Err("embedding server: the vector has values that are not finite (overflow or NaN)".to_string());
                }
                Ok(e)
            })
            .collect()
    }
}

/// Read-only cache of unit query vectors: `<dir>/<sha256(text) hex>.f32` (little-endian float32), the search lab's layout.
pub struct CacheEmbedder {
    pub dir: PathBuf,
}

impl Embedder for CacheEmbedder {
    fn embed(&self, texts: &[String], _: Duration) -> Result<Vec<Vec<f32>>, String> {
        texts
            .iter()
            .map(|t| {
                let f = self.dir.join(format!("{}.f32", hex::encode(Sha256::digest(t.as_bytes()))));
                let b = std::fs::read(&f).map_err(|_| format!("query vector not in the cache ({})", f.display()))?;
                let v: Vec<f32> = b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
                if b.len() % 4 != 0 || !v.iter().all(|x| x.is_finite()) {
                    return Err(format!("query vector cache: {} is not a finite float32 vector", f.display()));
                }
                Ok(v)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_servers_are_accepted() {
        assert!(HttpEmbedder::new("http://127.0.0.1:18092").is_ok());
        assert!(HttpEmbedder::new("http://localhost:18092/").is_ok());
        for u in ["http://10.0.0.1:18092", "https://127.0.0.1:1", "http://127.0.0.1", "http://example.com:80"] {
            assert!(HttpEmbedder::new(u).is_err(), "{u}");
        }
    }

    #[test]
    fn a_closed_port_fails_fast() {
        // bind and drop a listener to get a port nobody listens on
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let e = HttpEmbedder::new(&format!("http://127.0.0.1:{port}")).unwrap();
        let t0 = Instant::now();
        assert!(e.embed(&["x".into()], Duration::from_millis(700)).is_err());
        assert!(t0.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn server_reply_is_parsed_and_normalized() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut b = [0u8; 4096];
            let _ = s.read(&mut b);
            let body = r#"{"data":[{"index":0,"embedding":[3.0,4.0]}]}"#;
            write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        });
        let e = HttpEmbedder::new(&format!("http://127.0.0.1:{port}")).unwrap();
        let v = e.embed(&["q".into()], Duration::from_millis(2000)).unwrap();
        h.join().unwrap();
        assert_eq!(v, vec![vec![0.6f32, 0.8]]);
    }

    #[test]
    fn a_silent_server_times_out() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (_s, _) = l.accept().unwrap();
            std::thread::sleep(Duration::from_millis(400));
        });
        let e = HttpEmbedder::new(&format!("http://127.0.0.1:{port}")).unwrap();
        let t0 = Instant::now();
        let err = e.embed(&["q".into()], Duration::from_millis(100)).unwrap_err();
        assert!(err.contains("no answer"), "{err}");
        assert!(t0.elapsed() < Duration::from_millis(300));
        h.join().unwrap();
    }
    #[test]
    fn values_that_overflow_float32_are_refused() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut b = [0u8; 4096];
            let _ = s.read(&mut b);
            let body = r#"{"data":[{"index":0,"embedding":[1e39,1.0]}]}"#;
            write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        });
        let e = HttpEmbedder::new(&format!("http://127.0.0.1:{port}")).unwrap();
        let err = e.embed(&["q".into()], Duration::from_millis(2000)).unwrap_err();
        h.join().unwrap();
        assert!(err.contains("finite"), "{err}");
    }
}
