//! `ks mcp serve --http <addr:port>`: the MCP server over stateless Streamable HTTP (endpoint `/mcp`) for remote callers.
//!
//! - Tools: `ks_search` and `ks_get` only (`mcp::build` with `http`). The caller's role decides what `ks_get` returns
//!   and which cards a search shows.
//! - Each request: path `/mcp` → API key (`Authorization: Bearer <key>`, looked up by its SHA-256 on a connection of its
//!   own; missing, unknown or revoked 401, role below `--min-role` 403, store failure 503 without running anything) →
//!   the key's role goes into the request extensions → rmcp checks Host (loopback names plus `--allowed-host`) and
//!   Origin (refused unless listed in `--allowed-origin`) and runs the tool. `--no-auth` skips the key and makes every
//!   request a reader.
//! - Before listening (exit code 2, no listening line): `--no-auth` only on a loopback address; on any other address an
//!   active key of at least `--min-role` must exist; when readers can connect, every card must be `scope: public`.
//! - stderr: `ks mcp allowed hosts: …`, `ks mcp listening on <addr>`, then one line per request (key id, role, method,
//!   tool, latency, status). Never the Authorization header or a key.
//!
//! Out of scope (backlog): OAuth, rate limits, an audit log, TLS (put a tunnel or a proxy in front).
use crate::mcp::{self, Ks};
use crate::Backend;
use anyhow::{anyhow, bail, Context, Result};
use bytes::Bytes;
use http::{HeaderMap, Request, Response, StatusCode};
use http_body_util::{combinators::BoxBody, BodyExt, Full, Limited};
use ks_core::auth::{key_hash, AuthStore, KeyMeta, RedisAuthStore, Role, KEY_PREFIX};
use ks_core::card::Scope;
use ks_core::collect::load_cards;
use rmcp::transport::streamable_http_server::{session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tower_service::Service;

/// Host header values that are always accepted (any port): an `ssh -L` tunnel arrives as `127.0.0.1:<port>`.
pub(crate) const LOOPBACK_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];
/// Request body cap. A search request is well under 4 KB.
const MAX_BODY: usize = 1 << 20;
/// Longest Authorization token looked up; anything longer is not a key.
const MAX_KEY_LEN: usize = 256;

pub struct HttpOpts {
    pub addr: String,
    pub allowed_hosts: Vec<String>,
    pub allowed_origins: Vec<String>,
    pub min_role: Role,
    pub no_auth: bool,
}

type Body = BoxBody<Bytes, Infallible>;
type Mcp = StreamableHttpService<Ks, LocalSessionManager>;

/// The loopback names, then each non-empty `--allowed-host` value (an empty `--allowed-host=` is ignored), without
/// duplicates.
pub(crate) fn allowed_hosts(extra: &[String]) -> Vec<String> {
    let mut out: Vec<String> = LOOPBACK_HOSTS.iter().map(|h| h.to_string()).collect();
    for h in extra.iter().map(|h| h.trim()).filter(|h| !h.is_empty()) {
        if !out.iter().any(|o| o == h) {
            out.push(h.to_string());
        }
    }
    out
}

/// rmcp's Streamable HTTP service: stateless (no sessions), JSON responses, Host allowlist, Origin always checked.
pub(crate) fn mcp_service(ks: Ks, hosts: Vec<String>, origins: Vec<String>) -> Mcp {
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_allowed_hosts(hosts)
        .with_allowed_origins(origins)
        .enforce_origin_validation()
        .with_max_request_body_bytes(MAX_BODY);
    StreamableHttpService::new(move || Ok(ks.clone()), Default::default(), config)
}

/// The request layer in front of rmcp.
pub(crate) struct Gate {
    pub(crate) mcp: Mcp,
    /// None: `--no-auth`, every request is a reader.
    pub(crate) auth: Option<Arc<dyn AuthStore>>,
    pub(crate) min_role: Role,
}

fn plain(status: StatusCode, msg: &'static str) -> Response<Body> {
    let mut r = Response::new(Full::new(Bytes::from_static(msg.as_bytes())).boxed());
    *r.status_mut() = status;
    r.headers_mut().insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("text/plain; charset=utf-8"));
    if status == StatusCode::UNAUTHORIZED {
        r.headers_mut().insert(http::header::WWW_AUTHENTICATE, http::HeaderValue::from_static("Bearer"));
    }
    r
}

/// The token of the one `Authorization: Bearer <token>` header, if it can be a key at all.
fn bearer(h: &HeaderMap) -> Option<&str> {
    let mut all = h.get_all(http::header::AUTHORIZATION).iter();
    let v = all.next()?.to_str().ok()?;
    if all.next().is_some() {
        return None;
    }
    let (scheme, token) = v.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && token.starts_with(KEY_PREFIX) && token.len() <= MAX_KEY_LEN).then_some(token)
}

async fn authenticate(store: Arc<dyn AuthStore>, h: &HeaderMap, min: Role) -> Result<KeyMeta, Response<Body>> {
    let unauthorized = || plain(StatusCode::UNAUTHORIZED, "Unauthorized: send Authorization: Bearer <key> with a valid key\n");
    let hash = key_hash(bearer(h).ok_or_else(unauthorized)?);
    let unavailable = || plain(StatusCode::SERVICE_UNAVAILABLE, "Service Unavailable: the key store cannot be read\n");
    let meta = match tokio::task::spawn_blocking(move || store.lookup(&hash)).await {
        Ok(Ok(Some(m))) if m.active() => m,
        Ok(Ok(_)) => return Err(unauthorized()),
        Ok(Err(e)) => {
            eprintln!("ks mcp error: key store: {e}");
            return Err(unavailable());
        }
        Err(e) => {
            eprintln!("ks mcp error: key store: {e}");
            return Err(unavailable());
        }
    };
    if meta.role < min {
        return Err(plain(StatusCode::FORBIDDEN, "Forbidden: this key's role is below the server's minimum role\n"));
    }
    Ok(meta)
}

/// Client-supplied text as it may appear in a log line: `[A-Za-z0-9_./-]`, at most 64 characters, else `-`.
fn log_safe(s: Option<&str>) -> String {
    let s: String = s.unwrap_or("").chars().filter(|c| c.is_ascii_alphanumeric() || "_./-".contains(*c)).take(64).collect();
    if s.is_empty() {
        "-".into()
    } else {
        s
    }
}

/// JSON-RPC method and tool name of a request body, for the log line.
fn describe(body: &[u8]) -> (String, String) {
    let v: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    (log_safe(v["method"].as_str()), log_safe(v["params"]["name"].as_str()))
}

pub(crate) async fn handle<B>(gate: Arc<Gate>, req: Request<B>) -> Response<Body>
where
    B: hyper::body::Body<Data = Bytes> + Send + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let start = Instant::now();
    let (mut parts, body) = req.into_parts();
    let log = |key: &str, role: &str, method: &str, tool: &str, status: StatusCode| {
        eprintln!("ks mcp request key={key} role={role} method={method} tool={tool} ms={} status={}", start.elapsed().as_millis(), status.as_u16());
    };
    if parts.uri.path() != "/mcp" {
        let r = plain(StatusCode::NOT_FOUND, "Not Found: the MCP endpoint is /mcp\n");
        log("-", "-", "-", "-", r.status());
        return r;
    }
    // The key comes first: without one, nothing else about the request is looked at.
    let (key_id, role) = match &gate.auth {
        None => ("-".to_string(), Role::Reader),
        Some(store) => match authenticate(store.clone(), &parts.headers, gate.min_role).await {
            Ok(m) => (m.id, m.role),
            Err(r) => {
                log("-", "-", "-", "-", r.status());
                return r;
            }
        },
    };
    // Set by this layer only; the tools refuse a request without it (`Ks::role`).
    parts.extensions.insert(role);
    let bytes = match Limited::new(body, MAX_BODY).collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => {
            let r = plain(StatusCode::PAYLOAD_TOO_LARGE, "Payload Too Large, or the body could not be read\n");
            log(&key_id, role.as_str(), "-", "-", r.status());
            return r;
        }
    };
    let (method, tool) = describe(&bytes);
    let mut mcp = gate.mcp.clone();
    let r = match mcp.call(Request::from_parts(parts, Full::new(bytes))).await {
        Ok(r) => r,
        Err(e) => match e {},
    };
    log(&key_id, role.as_str(), &method, &tool, r.status());
    r
}

/// Ids of the cards under `<data>/cards` that are not `scope: public`.
fn nonpublic_cards(data: &std::path::Path) -> Result<Vec<String>> {
    Ok(load_cards(&data.join("cards"))?.into_iter().filter(|c| c.scope != Scope::Public).map(|c| c.id).collect())
}

pub fn serve_http(data: PathBuf, vocab: Option<PathBuf>, backend: Backend, o: HttpOpts) -> Result<()> {
    let addr: SocketAddr = o.addr.parse().with_context(|| format!("--http {}: expected <ip>:<port>, e.g. 127.0.0.1:8790", o.addr))?;
    let loopback = addr.ip().is_loopback();
    if o.no_auth && o.min_role != Role::Reader {
        bail!("--no-auth serves every request as a reader, so --min-role {} would refuse them all", o.min_role.as_str());
    }
    if o.no_auth && !loopback {
        bail!("refusing to listen on {addr} with --no-auth: without keys the server listens only on a loopback address (127.0.0.1 or ::1). Issue keys with `ks key issue` and drop --no-auth");
    }
    if o.no_auth || o.min_role == Role::Reader {
        let bad = nonpublic_cards(&data)?;
        if !bad.is_empty() {
            bail!(
                "refusing to serve readers: {} card(s) are not `scope: public` ({}{}). Serve this pool with --min-role admin, or keep only public cards in it",
                bad.len(),
                bad.iter().take(5).cloned().collect::<Vec<_>>().join(", "),
                if bad.len() > 5 { ", ..." } else { "" }
            );
        }
    }
    let auth: Option<Arc<dyn AuthStore>> = if o.no_auth {
        None
    } else {
        let store = RedisAuthStore::from_env()?;
        let n = store.count_active(o.min_role).map_err(|e| anyhow!("refusing to start: the key store (Redis) cannot be read: {e}"))?;
        if !loopback && n == 0 {
            bail!(
                "refusing to listen on {addr}: there is no active key with role {} or higher. Issue one first: ks key issue --role {} --purpose <what for> --user <who> --out <file>",
                o.min_role.as_str(),
                o.min_role.as_str()
            );
        }
        Some(Arc::new(store))
    };
    let hosts = allowed_hosts(&o.allowed_hosts);
    let ks = mcp::build(data, vocab, backend, true)?;
    let gate = Arc::new(Gate { mcp: mcp_service(ks, hosts.clone(), o.allowed_origins.clone()), auth, min_role: o.min_role });
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("cannot listen on {addr}"))?;
        eprintln!("ks mcp allowed hosts: {}", hosts.join(", "));
        eprintln!("ks mcp listening on {}", listener.local_addr()?);
        loop {
            let stream = match listener.accept().await {
                Ok((s, _)) => s,
                Err(e) => {
                    eprintln!("ks mcp error: accept: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
            };
            let gate = gate.clone();
            tokio::spawn(async move {
                let svc = hyper::service::service_fn(move |req| {
                    let gate = gate.clone();
                    async move { Ok::<_, Infallible>(handle(gate, req).await) }
                });
                let _ = hyper::server::conn::http1::Builder::new().keep_alive(true).serve_connection(hyper_util::rt::TokioIo::new(stream), svc).await;
            });
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new(tag: &str) -> Fixture {
            let root = std::env::temp_dir().join(format!("ks-http-unit-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("cards")).unwrap();
            std::fs::create_dir_all(root.join("vocab")).unwrap();
            std::fs::write(root.join("vocab/terms.json"), r#"{"version":1,"terms":[{"term":"redis","facet":"entity","aliases":[]}]}"#).unwrap();
            Fixture(root)
        }
        fn ks(&self) -> Ks {
            mcp::build(self.0.clone(), None, Backend::File, true).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn post(body: Value, key: Option<&str>) -> Request<Full<Bytes>> {
        let mut b = Request::post("/mcp")
            .header("host", "127.0.0.1:8790")
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", "2025-06-18");
        if let Some(k) = key {
            b = b.header("authorization", format!("Bearer {k}"));
        }
        b.body(Full::new(Bytes::from(body.to_string()))).unwrap()
    }
    fn tool_call(name: &str, args: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": name, "arguments": args}})
    }
    async fn read(r: Response<Body>) -> (StatusCode, String) {
        let status = r.status();
        let b = r.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(b.to_vec()).unwrap())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_role_context_is_denied() {
        let f = Fixture::new("norole");
        // rmcp's service alone, without the request layer that sets the role
        let mut mcp = mcp_service(f.ks(), allowed_hosts(&[]), vec![]);
        for (tool, args) in [("ks_search", json!({"terms": ["redis"], "query": ""})), ("ks_get", json!({"id": "0123456789abcdef"}))] {
            let (status, body) = read(mcp.call(post(tool_call(tool, args.clone()), None)).await.unwrap()).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let v: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(v["result"]["isError"], json!(true), "{tool}: {body}");
            assert!(body.contains("no role"), "{tool}: {body}");
            // with a role the same request runs
            let mut req = post(tool_call(tool, args), None);
            req.extensions_mut().insert(Role::Owner);
            let (_, body) = read(mcp.call(req).await.unwrap()).await;
            assert!(!body.contains("no role"), "{tool}: {body}");
        }
    }

    struct DownStore;
    impl AuthStore for DownStore {
        fn lookup(&self, _hash: &str) -> ks_core::Result<Option<KeyMeta>> {
            Err(ks_core::Error::msg("redis: connection refused"))
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn auth_store_down_denies() {
        let f = Fixture::new("down");
        let gate = Arc::new(Gate { mcp: mcp_service(f.ks(), allowed_hosts(&[]), vec![]), auth: Some(Arc::new(DownStore)), min_role: Role::Reader });
        let key = format!("ks_{}", "a".repeat(64));
        for body in [tool_call("ks_search", json!({"terms": ["redis"], "query": ""})), json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}})] {
            let (status, text) = read(handle(gate.clone(), post(body, Some(&key))).await).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{text}");
            assert!(!text.contains("jsonrpc") && !text.contains("redis"), "the request reached rmcp or the cause leaked: {text}");
        }
        // no key at all is still 401, not 503: the store is not even asked
        let (status, _) = read(handle(gate.clone(), post(json!({}), None)).await).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn bearer_accepts_only_one_well_formed_key() {
        let mk = |vals: &[&str]| {
            let mut h = HeaderMap::new();
            for v in vals {
                h.append(http::header::AUTHORIZATION, v.parse().unwrap());
            }
            h
        };
        assert_eq!(bearer(&mk(&["Bearer ks_abc"])), Some("ks_abc"));
        assert_eq!(bearer(&mk(&["BEARER  ks_abc "])), Some("ks_abc"));
        for bad in [&["Basic ks_abc"][..], &["Bearer abc"], &["ks_abc"], &["Bearer ks_a", "Bearer ks_b"], &[]] {
            assert_eq!(bearer(&mk(bad)), None, "{bad:?}");
        }
        assert_eq!(bearer(&mk(&[&format!("Bearer ks_{}", "a".repeat(300))])), None);
    }

    #[test]
    fn allowed_hosts_appends_non_empty_values() {
        assert_eq!(allowed_hosts(&["".into(), " ".into()]), LOOPBACK_HOSTS);
        assert_eq!(allowed_hosts(&["a.example".into(), "127.0.0.1".into(), "a.example".into()]), ["localhost", "127.0.0.1", "::1", "a.example"]);
    }
}
