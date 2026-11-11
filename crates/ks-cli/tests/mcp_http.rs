//! Remote MCP over stateless HTTP (`ks mcp serve --http`) and the operator key commands (`ks key …`).
//! Spawns the real binary and talks HTTP/1.1 over a plain socket, so the tests control Host, Origin, Authorization and
//! keep-alive exactly. Tests that need keys or the Redis backend run only when KS_TEST_REDIS_URL is set (testkit guard),
//! each under its own key prefix that is purged afterwards.
use ks_core::card::{Card, Scope};
use ks_core::index::Index;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::ServiceExt;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

static N: AtomicUsize = AtomicUsize::new(0);

const VOCAB: &str = r#"{"version":1,"terms":[
 {"term":"redis","facet":"entity","aliases":["redis-stack"]},
 {"term":"ttl","facet":"technique","aliases":[]},
 {"term":"kafka","facet":"entity","aliases":[]},{"term":"flink","facet":"entity","aliases":[]},{"term":"spark","facet":"entity","aliases":[]}]}"#;
const READER_FIELDS: [&str; 11] = ["id", "title", "summary", "problems", "limits", "evidence_type", "entities", "techniques", "domains", "topic", "published_at"];
const FORBIDDEN_WORDS: [&str; 2] = ["record", "private"];

fn card(src: &str, title: &str, tech: &[&str], summary: &str, scope: Scope) -> Card {
    let mut c = Card::new(&format!("https://source.example/{src}"));
    c.title = Some(title.into());
    c.source_url = Some(format!("https://source.example/{src}"));
    c.source_path = Some(format!("raw/secret-path-{src}.md"));
    c.scope = scope;
    c.topic = Some("topic-a".into());
    c.evidence_type = Some("tutorial".into());
    c.published_at = Some("2026-01-02".into());
    c.tech_keywords = tech.iter().map(|s| s.to_string()).collect();
    c.problems = vec!["가나다 문제".into(), "라마바 문제".into()];
    c.summary = vec![summary.into(), "둘째 줄 요약입니다 열 자 이상.".into(), "셋째 줄 요약입니다 열 자 이상.".into()];
    c.limits = "한계 설명 한 줄입니다 열 자 이상.".into();
    let v = ks_core::vocab::Vocab::from_json(VOCAB, "t").unwrap();
    let got = v.classify_terms(&c.tech_keywords);
    (c.entities, c.techniques, c.domains, c.new_terms) = (got.entities, got.techniques, got.domains, got.new_terms);
    c
}

struct Fixture {
    root: PathBuf,
    cards: Vec<Card>,
}
impl Fixture {
    fn new(tag: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!("ks-http-{}-{tag}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("cards/topic-a")).unwrap();
        fs::create_dir_all(root.join("vocab")).unwrap();
        fs::write(root.join("vocab/terms.json"), VOCAB).unwrap();
        let cards = vec![
            card("a", "Session store notes", &["redis", "ttl", "kafka"], "Expire user sessions stored in Redis with a TTL.", Scope::Public),
            card("b", "Agent notes", &["redis-stack", "flink", "spark"], "첫째 줄 요약입니다 열 자 이상.", Scope::Public),
            card("c", "Grading notes", &["ttl", "kafka", "spark"], "첫째 줄 요약입니다 열 자 이상.", Scope::Public),
        ];
        let f = Fixture { root, cards };
        for c in &f.cards {
            f.add(c);
        }
        f
    }
    fn add(&self, c: &Card) {
        fs::write(self.root.join(format!("cards/topic-a/{}.md", c.id)), c.render()).unwrap();
    }
    fn rebuild(&self, idx: &mut Index) {
        let vf = self.root.join("vocab/terms.json");
        let fp = ks_core::index::Fingerprints::of_files(&vf, &self.root.join("cards")).unwrap();
        let (cards, vocab) = (ks_core::collect::load_cards(&self.root.join("cards")).unwrap(), ks_core::vocab::Vocab::load(&vf).unwrap());
        idx.rebuild_with(&cards, &vocab, None, &fp).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// The test Redis and a fresh key prefix, purged on drop.
struct Redis {
    url: String,
    prefix: String,
}
impl Redis {
    fn get() -> Option<Redis> {
        let url = ks_core::testkit::test_redis_url()?;
        let prefix = format!("kstest-http-{}-{}:", std::process::id(), N.fetch_add(1, Ordering::SeqCst));
        Some(Redis { url, prefix })
    }
    fn env(&self) -> Vec<(&str, &str)> {
        vec![("KS_REDIS_URL", self.url.as_str()), ("KS_KEY_PREFIX", self.prefix.as_str())]
    }
    fn index(&self) -> Index {
        Index::connect(&self.url, &self.prefix).unwrap()
    }
    fn con(&self) -> redis::Connection {
        ks_core::testkit::connect_test_redis(&self.url)
    }
    /// `ks key issue … --out <file>`; returns (id, key).
    fn issue(&self, role: &str, dir: &Path) -> (String, String) {
        let out = dir.join(format!("key-{}", N.fetch_add(1, Ordering::SeqCst)));
        let o = ks(&["key", "issue", "--role", role, "--purpose", "test", "--user", "tester", "--out", out.to_str().unwrap()], &self.env());
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let v: Value = serde_json::from_slice(&o.stdout).unwrap();
        (v["id"].as_str().unwrap().to_string(), fs::read_to_string(&out).unwrap().trim().to_string())
    }
}
impl Drop for Redis {
    fn drop(&mut self) {
        if let Ok(mut i) = Index::connect(&self.url, &self.prefix) {
            let _ = i.purge();
        }
    }
}

fn clean_env(c: &mut Command, env: &[(&str, &str)]) {
    for k in ["KS_REDIS_URL", "KS_REDIS_PASSWORD", "KS_REDIS_HOST", "KS_REDIS_PORT", "KS_KEY_PREFIX", "KS_HOME"] {
        c.env_remove(k);
    }
    // a dead port unless the test names a Redis: nothing else on this machine is reached by accident
    c.env("KS_REDIS_URL", "redis://127.0.0.1:1");
    for (k, v) in env {
        c.env(k, v);
    }
}

fn ks(args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ks"));
    c.args(args);
    clean_env(&mut c, env);
    c.output().unwrap()
}

struct Server {
    child: Child,
    port: u16,
    stderr: Arc<Mutex<String>>,
}
impl Server {
    fn host(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }
    fn conn(&self) -> Conn {
        Conn::new(self.port)
    }
    fn stderr(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start `ks mcp serve --data <data> --http 127.0.0.1:0 <args>`; Err((exit code, stderr)) if it exits before listening.
fn try_serve(data: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<Server, (Option<i32>, String)> {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ks"));
    c.arg("mcp").arg("serve").arg("--data").arg(data);
    if !args.iter().any(|a| *a == "--http") {
        c.args(["--http", "127.0.0.1:0"]);
    }
    c.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    clean_env(&mut c, env);
    let mut child = c.spawn().unwrap();
    let stderr = Arc::new(Mutex::new(String::new()));
    let (tx, rx) = mpsc::channel();
    let (buf, pipe) = (stderr.clone(), child.stderr.take().unwrap());
    std::thread::spawn(move || {
        for line in BufReader::new(pipe).lines().map_while(Result::ok) {
            if let Some(addr) = line.strip_prefix("ks mcp listening on ") {
                let _ = tx.send(addr.rsplit(':').next().unwrap().parse::<u16>().unwrap());
            }
            buf.lock().unwrap().push_str(&(line + "\n"));
        }
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(port) = rx.try_recv() {
            return Ok(Server { child, port, stderr });
        }
        if let Some(st) = child.try_wait().unwrap() {
            std::thread::sleep(Duration::from_millis(100)); // let the reader thread drain
            let err = stderr.lock().unwrap().clone();
            return Err((st.code(), err));
        }
        assert!(Instant::now() < deadline, "server neither listened nor exited: {}", stderr.lock().unwrap());
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn serve(data: &Path, args: &[&str], env: &[(&str, &str)]) -> Server {
    try_serve(data, args, env).unwrap_or_else(|(c, e)| panic!("server exited ({c:?}): {e}"))
}
fn refused(data: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    match try_serve(data, args, env) {
        Ok(s) => panic!("server started on {} but should have refused: {}", s.host(), s.stderr()),
        Err((code, err)) => {
            assert!(code.is_some_and(|c| c != 0), "exit code {code:?}: {err}");
            assert!(!err.contains("listening"), "{err}");
            err
        }
    }
}

/// One keep-alive HTTP/1.1 connection.
struct Conn(BufReader<TcpStream>);
struct Resp {
    status: u16,
    body: String,
}
impl Resp {
    fn json(&self) -> Value {
        assert_eq!(self.status, 200, "{}", self.body);
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.body))
    }
}
impl Conn {
    fn new(port: u16) -> Conn {
        let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        Conn(BufReader::new(s))
    }
    fn post(&mut self, host: &str, headers: &[(&str, &str)], body: &str) -> Resp {
        let mut req = format!("POST /mcp HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\n", body.len());
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        req.push_str(body);
        self.0.get_mut().write_all(req.as_bytes()).unwrap();
        let mut line = String::new();
        self.0.read_line(&mut line).unwrap();
        let status: u16 = line.split(' ').nth(1).unwrap_or_else(|| panic!("bad status line {line:?}")).parse().unwrap();
        let (mut len, mut chunked) = (0usize, false);
        loop {
            line.clear();
            self.0.read_line(&mut line).unwrap();
            let l = line.trim_end();
            if l.is_empty() {
                break;
            }
            let (k, v) = l.split_once(':').unwrap();
            match k.to_ascii_lowercase().as_str() {
                "content-length" => len = v.trim().parse().unwrap(),
                "transfer-encoding" => chunked = v.trim().eq_ignore_ascii_case("chunked"),
                _ => {}
            }
        }
        let mut body = vec![];
        if chunked {
            loop {
                line.clear();
                self.0.read_line(&mut line).unwrap();
                let n = usize::from_str_radix(line.trim(), 16).unwrap();
                let mut chunk = vec![0; n + 2];
                self.0.read_exact(&mut chunk).unwrap();
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..n]);
            }
        } else {
            body.resize(len, 0);
            self.0.read_exact(&mut body).unwrap();
        }
        Resp { status, body: String::from_utf8(body).unwrap() }
    }
    fn rpc(&mut self, host: &str, key: Option<&str>, method: &str, params: Value) -> Resp {
        let auth = key.map(|k| format!("Bearer {k}"));
        let mut h: Vec<(&str, &str)> = vec![];
        if method != "initialize" {
            h.push(("MCP-Protocol-Version", "2025-06-18"));
        }
        if let Some(a) = &auth {
            h.push(("Authorization", a.as_str()));
        }
        self.post(host, &h, &json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string())
    }
}

fn init_params() -> Value {
    json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}})
}
/// tools/call: Ok(text of the result) or Err(text) for a tool error or a JSON-RPC error.
fn call(s: &Server, key: Option<&str>, tool: &str, args: Value) -> Result<String, String> {
    call_on(&mut s.conn(), &s.host(), key, tool, args)
}
fn call_on(c: &mut Conn, host: &str, key: Option<&str>, tool: &str, args: Value) -> Result<String, String> {
    let v = c.rpc(host, key, "tools/call", json!({"name": tool, "arguments": args})).json();
    if let Some(e) = v.get("error") {
        return Err(e.to_string());
    }
    let r = &v["result"];
    let text = r["content"].as_array().unwrap().iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n");
    if r["isError"] == json!(true) {
        Err(text)
    } else {
        Ok(text)
    }
}
fn search(s: &Server, key: Option<&str>, terms: &[&str], query: &str) -> Value {
    let t = call(s, key, "ks_search", json!({"terms": terms, "query": query})).unwrap();
    serde_json::from_str(&t).unwrap()
}
fn ids(v: &Value) -> Vec<String> {
    v["candidates"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap().to_string()).collect()
}
fn tool_names(s: &Server, key: Option<&str>) -> (Vec<String>, Vec<Value>) {
    let v = s.conn().rpc(&s.host(), key, "tools/list", json!({})).json();
    let tools = v["result"]["tools"].as_array().unwrap().clone();
    let mut names: Vec<String> = tools.iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    names.sort();
    (names, tools)
}
fn no_forbidden_words(what: &str, text: &str) {
    let t = text.to_lowercase();
    for w in FORBIDDEN_WORDS {
        assert!(!t.contains(w), "{what} mentions {w:?}: {text}");
    }
}
/// A reader `ks_get`: a JSON object with only allowlisted fields, the required ones equal to the card's.
fn assert_reader_get(text: &str, c: &Card) {
    let v: Value = serde_json::from_str(text).unwrap_or_else(|e| panic!("reader ks_get is not JSON ({e}): {text}"));
    let keys: BTreeSet<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    let allowed: BTreeSet<&str> = READER_FIELDS.into_iter().collect();
    assert!(keys.is_subset(&allowed), "extra fields {:?}", keys.difference(&allowed).collect::<Vec<_>>());
    assert_eq!(v["id"], json!(c.id));
    assert_eq!(v["title"], json!(c.title));
    assert_eq!(v["summary"], json!(c.summary));
    for secret in [c.source_url.as_deref().unwrap(), c.source_path.as_deref().unwrap(), c.card_key.as_str()] {
        assert!(!text.contains(secret), "leaks {secret}: {text}");
    }
}

// ---------------------------------------------------------------- T1a: no keys needed

#[test]
fn remote_tools_are_search_and_get_only() {
    let f = Fixture::new("tools");
    let s = serve(&f.root, &["--no-auth", "--backend", "file"], &[]);
    let init = s.conn().rpc(&s.host(), None, "initialize", init_params()).json();
    let instructions = init["result"]["instructions"].as_str().unwrap();
    no_forbidden_words("instructions", instructions);
    assert!(instructions.to_lowercase().contains("untrusted"), "{instructions}");
    let (names, tools) = tool_names(&s, None);
    assert_eq!(names, ["ks_get", "ks_search"]);
    for t in &tools {
        let d = t["description"].as_str().unwrap();
        no_forbidden_words(t["name"].as_str().unwrap(), d);
        assert!(d.to_lowercase().contains("untrusted"), "{d}");
    }
    let d = tools.iter().find(|t| t["name"] == "ks_search").unwrap()["description"].as_str().unwrap().to_lowercase();
    assert!(d.contains("natural") && d.contains("keyword"), "{d}");
}

#[test]
fn remote_forbidden_tool_call_rejected() {
    let f = Fixture::new("forbidden");
    let home = f.root.join("kshome");
    let home_s = home.to_str().unwrap().to_string();
    let redis = Redis::get();
    let mut env = vec![("KS_HOME", home_s.as_str()), ("KS_NOTIFY", "0")];
    let mut args = vec!["--no-auth"];
    match &redis {
        Some(r) => {
            f.rebuild(&mut r.index());
            env.extend(r.env());
        }
        None => args.extend(["--backend", "file"]),
    }
    let s = serve(&f.root, &args, &env);
    let e = call(&s, None, "ks_search_web", json!({"query": "redis"})).unwrap_err();
    assert!(e.to_lowercase().contains("not found") || e.contains("ks_search_web"), "{e}");
    let e = call(&s, None, "ks_record", json!({"task": "t", "card": f.cards[0].id, "verdict": "A", "reason": "r"})).unwrap_err();
    assert!(!e.is_empty());
    assert!(!home.join("run").exists(), "a browser daemon was started");
    if let Some(r) = &redis {
        let n: usize = redis::cmd("XLEN").arg(format!("{}apply", r.prefix)).query(&mut r.con()).unwrap();
        assert_eq!(n, 0, "an application was written");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn stdio_tools_unchanged() {
    let f = Fixture::new("stdio");
    let root = f.root.clone();
    let transport = TokioChildProcess::new(tokio::process::Command::new(env!("CARGO_BIN_EXE_ks")).configure(|c| {
        c.arg("mcp").arg("serve").arg("--data").arg(&root).args(["--backend", "file"]);
        for k in ["KS_REDIS_URL", "KS_REDIS_PASSWORD", "KS_REDIS_HOST", "KS_REDIS_PORT", "KS_KEY_PREFIX"] {
            c.env_remove(k);
        }
        c.env("KS_REDIS_URL", "redis://127.0.0.1:1");
    }))
    .unwrap();
    let c = tokio::time::timeout(Duration::from_secs(30), ().serve(transport)).await.unwrap().unwrap();
    let tools = c.list_all_tools().await.unwrap();
    let mut names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    assert_eq!(names, ["ks_get", "ks_record", "ks_search", "ks_search_web"]);
    let search = tools.iter().find(|t| t.name == "ks_search").unwrap();
    assert!(search.description.as_deref().unwrap().starts_with("Search the private knowledge-card pool"), "stdio wording is unchanged");
    // stdio is the owner: ks_get returns the full card
    let params = rmcp::model::CallToolRequestParams::new("ks_get").with_arguments(json!({"id": f.cards[0].id}).as_object().unwrap().clone());
    let r = c.call_tool(params).await.unwrap();
    let md: String = r.content.iter().filter_map(|b| b.as_text().map(|t| t.text.clone())).collect();
    assert_eq!(md, f.cards[0].render());
    c.cancel().await.unwrap();
}

#[test]
fn noauth_loopback_search_keyword_and_nl() {
    let f = Fixture::new("nl");
    let s = serve(&f.root, &["--no-auth", "--backend", "file"], &[]);
    let init = s.conn().rpc(&s.host(), None, "initialize", init_params()).json();
    assert_eq!(init["result"]["serverInfo"]["name"], "knowledge-scout");
    let v = search(&s, None, &["redis", "ttl"], "");
    assert_eq!(ids(&v)[0], f.cards[0].id, "{v}");
    // natural language only: the sentence's words are matched against vocabulary and card text
    let v = search(&s, None, &[], "How do I expire user sessions kept in Redis?");
    assert_eq!(ids(&v).first(), Some(&f.cards[0].id), "{v}");
    let v = search(&s, None, &[], "세션 만료를 redis TTL로 처리하는 방법");
    assert!(ids(&v).contains(&f.cards[0].id), "{v}");
}

#[test]
fn search_without_terms_field_is_natural_language() {
    let f = Fixture::new("noterms");
    let s = serve(&f.root, &["--no-auth", "--backend", "file"], &[]);
    // a generic MCP client follows the schema: `terms` must not be required
    let (_, tools) = tool_names(&s, None);
    let schema = &tools.iter().find(|t| t["name"] == "ks_search").unwrap()["inputSchema"];
    let required = schema["required"].as_array().cloned().unwrap_or_default();
    assert!(!required.contains(&json!("terms")), "{schema}");
    // and a call that leaves `terms` out searches by the sentence's words
    let t = call(&s, None, "ks_search", json!({"query": "How do I expire user sessions kept in Redis?"})).unwrap();
    let v: Value = serde_json::from_str(&t).unwrap();
    assert_eq!(ids(&v).first(), Some(&f.cards[0].id), "{v}");
}

#[test]
fn no_auth_loopback_is_reader() {
    let f = Fixture::new("reader");
    let s = serve(&f.root, &["--no-auth", "--backend", "file"], &[]);
    let t = call(&s, None, "ks_get", json!({"id": f.cards[0].id})).unwrap();
    assert!(!t.starts_with("---"), "the reader got the card file: {t}");
    assert_reader_get(&t, &f.cards[0]);
}

#[test]
fn reader_get_is_allowlist_object() {
    let f = Fixture::new("allow");
    let s = serve(&f.root, &["--no-auth", "--backend", "file"], &[]);
    for c in &f.cards {
        let t = call(&s, None, "ks_get", json!({"id": c.id})).unwrap();
        assert_reader_get(&t, c);
        let v: Value = serde_json::from_str(&t).unwrap();
        assert!(v["title"].as_str().is_some_and(|t| !t.is_empty()) && v["summary"].as_array().is_some_and(|a| a.len() == 3), "{t}");
    }
    // search candidates carry no source fields either
    let v = search(&s, None, &["redis"], "");
    for c in &f.cards {
        for secret in [c.source_url.as_deref().unwrap(), c.source_path.as_deref().unwrap()] {
            assert!(!v.to_string().contains(secret), "{v}");
        }
    }
}

#[test]
fn reader_filters_nonpublic_per_request() {
    let f = Fixture::new("filter");
    let hidden = [
        card("p", "Session store private notes", &["redis", "ttl", "kafka"], "Expire user sessions stored in Redis with a TTL.", Scope::Private),
        card("t", "Session store team notes", &["redis", "ttl", "kafka"], "Expire user sessions stored in Redis with a TTL.", Scope::Team),
    ];
    // file backend: ks_get reads the files on every call
    {
        let s = serve(&f.root, &["--no-auth", "--backend", "file"], &[]);
        for h in &hidden {
            f.add(h);
        }
        for h in &hidden {
            let e = call(&s, None, "ks_get", json!({"id": h.id})).unwrap_err();
            assert!(e.contains("no card"), "{e}");
        }
        for h in &hidden {
            fs::remove_file(f.root.join(format!("cards/topic-a/{}.md", h.id))).unwrap();
        }
    }
    // Redis backend: the cards are indexed after the server started
    let Some(r) = Redis::get() else { return };
    f.rebuild(&mut r.index());
    let s = serve(&f.root, &["--no-auth"], &r.env());
    assert_eq!(ids(&search(&s, None, &["redis", "ttl"], ""))[0], f.cards[0].id);
    for h in &hidden {
        f.add(h);
    }
    f.rebuild(&mut r.index());
    for v in [search(&s, None, &["redis", "ttl", "kafka"], ""), search(&s, None, &[], "expire user sessions stored in redis")] {
        let got = ids(&v);
        assert!(got.contains(&f.cards[0].id), "{v}");
        for h in &hidden {
            assert!(!got.contains(&h.id), "a {:?} card reached the reader: {v}", h.scope);
        }
        let ranks: Vec<u64> = v["candidates"].as_array().unwrap().iter().map(|c| c["rank"].as_u64().unwrap()).collect();
        assert_eq!(ranks, (1..=ranks.len() as u64).collect::<Vec<_>>(), "ranks are renumbered after filtering");
    }
    for h in &hidden {
        assert!(call(&s, None, "ks_get", json!({"id": h.id})).unwrap_err().contains("no card"));
    }
}

#[test]
fn nonloopback_no_auth_refuses() {
    let f = Fixture::new("nonloop");
    for addr in ["0.0.0.0:0", "[::]:0"] {
        let e = refused(&f.root, &["--http", addr, "--no-auth", "--backend", "file"], &[]);
        assert!(e.contains("--no-auth") && e.contains("loopback"), "{e}");
    }
}

#[test]
fn host_rejected_noauth() {
    let f = Fixture::new("host");
    let s = serve(&f.root, &["--no-auth", "--backend", "file"], &[]);
    for host in ["evil.example", "evil.example:80", "10.0.0.1"] {
        assert_eq!(s.conn().rpc(host, None, "tools/list", json!({})).status, 403, "{host}");
    }
    for host in [s.host(), format!("localhost:{}", s.port), format!("[::1]:{}", s.port)] {
        assert_eq!(s.conn().rpc(&host, None, "tools/list", json!({})).status, 200, "{host}");
    }
}

#[test]
fn origin_rejected() {
    let f = Fixture::new("origin");
    let s = serve(&f.root, &["--no-auth", "--backend", "file"], &[]);
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}).to_string();
    let local = format!("http://127.0.0.1:{}", s.port);
    for origin in ["https://evil.example", "null", local.as_str()] {
        let r = s.conn().post(&s.host(), &[("Origin", origin), ("MCP-Protocol-Version", "2025-06-18")], &body);
        assert_eq!(r.status, 403, "{origin}: {}", r.body);
    }
    assert_eq!(s.conn().post(&s.host(), &[("MCP-Protocol-Version", "2025-06-18")], &body).status, 200, "a CLI client sends no Origin");
}

#[test]
fn allowed_host_empty_is_ignored() {
    let f = Fixture::new("emptyhost");
    let s = serve(&f.root, &["--no-auth", "--backend", "file", "--allowed-host="], &[]);
    assert!(s.stderr().contains("ks mcp allowed hosts: localhost, 127.0.0.1, ::1\n"), "{}", s.stderr());
    assert_eq!(s.conn().rpc(&s.host(), None, "tools/list", json!({})).status, 200);
    assert_eq!(s.conn().rpc("evil.example", None, "tools/list", json!({})).status, 403);
    // an empty Host header is not an allowed host either
    assert_ne!(s.conn().rpc("", None, "tools/list", json!({})).status, 200);
}

// ---------------------------------------------------------------- T1b: keys (test Redis)

#[test]
fn valid_key_initialize_and_search() {
    let Some(r) = Redis::get() else { return };
    let f = Fixture::new("valid");
    f.rebuild(&mut r.index());
    let (_, key) = r.issue("reader", &f.root);
    let s = serve(&f.root, &[], &r.env());
    let init = s.conn().rpc(&s.host(), Some(&key), "initialize", init_params()).json();
    assert_eq!(init["result"]["serverInfo"]["name"], "knowledge-scout");
    assert_eq!(tool_names(&s, Some(&key)).0, ["ks_get", "ks_search"]);
    assert_eq!(ids(&search(&s, Some(&key), &["redis", "ttl"], ""))[0], f.cards[0].id);
    assert_eq!(ids(&search(&s, Some(&key), &[], "how do I expire user sessions kept in redis")).first(), Some(&f.cards[0].id));
}

#[test]
fn auth_missing_wrong_revoked_is_401() {
    let Some(r) = Redis::get() else { return };
    let f = Fixture::new("401");
    let (id, key) = r.issue("owner", &f.root);
    let (_, other) = r.issue("owner", &f.root);
    assert!(ks(&["key", "revoke", &id], &r.env()).status.success());
    let s = serve(&f.root, &["--backend", "file"], &r.env());
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": init_params()}).to_string();
    let wrong = format!("Bearer ks_{}", "0".repeat(64));
    let revoked = format!("Bearer {key}");
    let basic = format!("Basic {other}");
    let bare = other.clone();
    let cases: [&[(&str, &str)]; 6] = [&[], &[("Authorization", wrong.as_str())], &[("Authorization", revoked.as_str())], &[("Authorization", basic.as_str())], &[("Authorization", bare.as_str())], &[("Authorization", "Bearer ")]];
    for h in cases {
        let resp = s.conn().post(&s.host(), h, &init);
        assert_eq!(resp.status, 401, "{h:?}: {}", resp.body);
        assert!(!resp.body.contains("jsonrpc"), "{}", resp.body);
    }
    let good = format!("Bearer {other}");
    assert_eq!(s.conn().post(&s.host(), &[("Authorization", good.as_str())], &init).status, 200);
    // the scheme is case-insensitive
    let lower = format!("bearer {other}");
    assert_eq!(s.conn().post(&s.host(), &[("Authorization", lower.as_str())], &init).status, 200);
}

#[test]
fn role_below_min_is_403() {
    let Some(r) = Redis::get() else { return };
    let f = Fixture::new("403");
    let (_, reader) = r.issue("reader", &f.root);
    let (_, admin) = r.issue("admin", &f.root);
    let (_, owner) = r.issue("owner", &f.root);
    let s = serve(&f.root, &["--backend", "file", "--min-role", "admin"], &r.env());
    assert_eq!(s.conn().rpc(&s.host(), Some(&reader), "tools/list", json!({})).status, 403);
    for k in [&admin, &owner] {
        assert_eq!(s.conn().rpc(&s.host(), Some(k), "tools/list", json!({})).status, 200);
    }
    // admin sees the full card
    assert_eq!(call(&s, Some(&admin), "ks_get", json!({"id": f.cards[1].id})).unwrap(), f.cards[1].render());
}

#[test]
fn same_connection_owner_then_reader() {
    let Some(r) = Redis::get() else { return };
    let f = Fixture::new("sameconn");
    let (_, owner) = r.issue("owner", &f.root);
    let (_, reader) = r.issue("reader", &f.root);
    let s = serve(&f.root, &["--backend", "file"], &r.env());
    let mut c = s.conn();
    let id = json!({"id": f.cards[0].id});
    assert_eq!(call_on(&mut c, &s.host(), Some(&owner), "ks_get", id.clone()).unwrap(), f.cards[0].render());
    assert_reader_get(&call_on(&mut c, &s.host(), Some(&reader), "ks_get", id.clone()).unwrap(), &f.cards[0]);
    assert_eq!(call_on(&mut c, &s.host(), Some(&owner), "ks_get", id.clone()).unwrap(), f.cards[0].render());
    // and with no key on the same connection: 401
    assert_eq!(c.rpc(&s.host(), None, "tools/list", json!({})).status, 401);
}

#[test]
fn revoked_key_rejected_immediately() {
    let Some(r) = Redis::get() else { return };
    let f = Fixture::new("revoke");
    let (id, key) = r.issue("reader", &f.root);
    let s = serve(&f.root, &["--backend", "file"], &r.env());
    let mut c = s.conn();
    assert_eq!(c.rpc(&s.host(), Some(&key), "tools/list", json!({})).status, 200);
    let o = ks(&["key", "revoke", &id], &r.env());
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(c.rpc(&s.host(), Some(&key), "tools/list", json!({})).status, 401, "same connection");
    assert_eq!(s.conn().rpc(&s.host(), Some(&key), "tools/list", json!({})).status, 401, "new connection");
    assert!(!ks(&["key", "revoke", "ffffffffffffffff"], &r.env()).status.success(), "unknown id");
}

#[test]
fn host_rejected() {
    let Some(r) = Redis::get() else { return };
    let f = Fixture::new("hostkey");
    let (_, key) = r.issue("reader", &f.root);
    let s = serve(&f.root, &["--backend", "file"], &r.env());
    assert_eq!(s.conn().rpc("evil.example", Some(&key), "tools/list", json!({})).status, 403);
    assert_eq!(s.conn().rpc(&s.host(), Some(&key), "tools/list", json!({})).status, 200);
}

#[test]
fn nonloopback_without_keys_refuses() {
    let f = Fixture::new("nokeys");
    // no key store at all
    let e = refused(&f.root, &["--http", "0.0.0.0:0", "--backend", "file"], &[]);
    assert!(e.to_lowercase().contains("key store"), "{e}");
    let Some(r) = Redis::get() else { return };
    let e = refused(&f.root, &["--http", "0.0.0.0:0", "--backend", "file"], &r.env());
    assert!(e.contains("ks key issue"), "{e}");
    r.issue("reader", &f.root);
    let e = refused(&f.root, &["--http", "0.0.0.0:0", "--backend", "file", "--min-role", "admin"], &r.env());
    assert!(e.contains("ks key issue") && e.contains("admin"), "{e}");
    let (id, _) = r.issue("owner", &f.root);
    assert!(ks(&["key", "revoke", &id], &r.env()).status.success());
    let e = refused(&f.root, &["--http", "0.0.0.0:0", "--backend", "file", "--min-role", "owner"], &r.env());
    assert!(e.contains("ks key issue"), "a revoked key does not count: {e}");
}

#[test]
fn reader_pool_with_private_card_refuses() {
    let f = Fixture::new("privpool");
    let p = card("p", "Private notes", &["redis"], "첫째 줄 요약입니다 열 자 이상.", Scope::Private);
    f.add(&p);
    for args in [&["--backend", "file"][..], &["--backend", "file", "--min-role", "reader"], &["--backend", "file", "--no-auth"]] {
        let e = refused(&f.root, args, &[]);
        assert!(e.contains("public") && e.contains(&p.id), "{args:?}: {e}");
    }
    // a pool for admins and owners may hold non-public cards
    let Some(r) = Redis::get() else { return };
    let (_, admin) = r.issue("admin", &f.root);
    let s = serve(&f.root, &["--backend", "file", "--min-role", "admin"], &r.env());
    assert_eq!(call(&s, Some(&admin), "ks_get", json!({"id": p.id})).unwrap(), p.render());
}

#[test]
fn key_raw_not_stored() {
    let Some(r) = Redis::get() else { return };
    let f = Fixture::new("raw");
    let (id, key) = r.issue("owner", &f.root);
    let mut con = r.con();
    let keys: Vec<String> = redis::cmd("KEYS").arg(format!("{}*", r.prefix)).query(&mut con).unwrap();
    assert!(!keys.is_empty());
    let hash = ks_core::auth::key_hash(&key);
    assert!(keys.iter().any(|k| k == &format!("{}auth:key:{hash}", r.prefix)), "{keys:?}");
    for k in &keys {
        assert!(!k.contains(&key[3..]), "{k}");
        let t: String = redis::cmd("TYPE").arg(k).query(&mut con).unwrap();
        let vals: Vec<String> = match t.as_str() {
            "hash" => redis::cmd("HGETALL").arg(k).query(&mut con).unwrap(),
            "string" => vec![redis::cmd("GET").arg(k).query(&mut con).unwrap()],
            other => panic!("unexpected {other} at {k}"),
        };
        for v in vals {
            assert!(!v.contains(&key[3..]), "{k} holds the key");
        }
    }
    let meta: std::collections::HashMap<String, String> = redis::cmd("HGETALL").arg(format!("{}auth:key:{hash}", r.prefix)).query(&mut con).unwrap();
    let fields: BTreeSet<&str> = meta.keys().map(String::as_str).collect();
    assert_eq!(fields, ["created_at", "id", "purpose", "role", "user"].into_iter().collect());
    assert_eq!((meta["id"].as_str(), meta["role"].as_str(), meta["purpose"].as_str(), meta["user"].as_str()), (id.as_str(), "owner", "test", "tester"));
}

#[test]
fn key_create_stdout_has_no_secret() {
    let Some(r) = Redis::get() else { return };
    let f = Fixture::new("stdout");
    let out = f.root.join("k");
    // `create` / `--label` / `--subject` are aliases of `issue` / `--purpose` / `--user`
    let o = ks(&["key", "create", "--role", "reader", "--label", "demo", "--subject", "bob", "--out", out.to_str().unwrap()], &r.env());
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let key = fs::read_to_string(&out).unwrap().trim().to_string();
    let (so, se) = (String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(!so.contains(&key[3..]) && !se.contains(&key[3..]), "{so}{se}");
    let v: Value = serde_json::from_str(&so).unwrap();
    assert_eq!((v["role"].as_str(), v["purpose"].as_str(), v["user"].as_str()), (Some("reader"), Some("demo"), Some("bob")));
    assert!(v.get("key").is_none());
    // without --out the key is printed once, on stdout, and nowhere else
    let o = ks(&["key", "issue", "--role", "reader", "--purpose", "demo", "--user", "bob"], &r.env());
    assert!(o.status.success());
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    let shown = v["key"].as_str().unwrap();
    assert!(shown.starts_with("ks_") && shown.len() == 67);
    assert!(String::from_utf8_lossy(&o.stderr).contains("only once"));
    assert!(!String::from_utf8_lossy(&o.stderr).contains(&shown[3..]));
}

#[test]
fn key_list_hides_secret() {
    let Some(r) = Redis::get() else { return };
    let f = Fixture::new("list");
    let (id, key) = r.issue("admin", &f.root);
    let (id2, _) = r.issue("reader", &f.root);
    assert!(ks(&["key", "revoke", &id2], &r.env()).status.success());
    let o = ks(&["key", "list"], &r.env());
    assert!(o.status.success());
    let text = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(!text.contains(&key[3..]) && !text.contains(&ks_core::auth::key_hash(&key)), "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    let rows = v.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let row = rows.iter().find(|x| x["id"] == json!(id)).unwrap();
    let fields: BTreeSet<&str> = row.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(fields, ["created_at", "id", "purpose", "revoked_at", "role", "user"].into_iter().collect());
    assert_eq!((row["role"].as_str(), row["revoked_at"].clone()), (Some("admin"), Value::Null));
    assert!(rows.iter().find(|x| x["id"] == json!(id2)).unwrap()["revoked_at"].is_string());
}

#[test]
fn key_out_file_is_0600_and_refuses_existing() {
    use std::os::unix::fs::PermissionsExt;
    let Some(r) = Redis::get() else { return };
    let f = Fixture::new("out");
    let out = f.root.join("k");
    let args = ["key", "issue", "--role", "reader", "--purpose", "p", "--user", "u", "--out", out.to_str().unwrap()];
    assert!(ks(&args, &r.env()).status.success());
    assert_eq!(fs::metadata(&out).unwrap().permissions().mode() & 0o777, 0o600);
    let before = fs::read(&out).unwrap();
    let o = ks(&args, &r.env());
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("exists"), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(fs::read(&out).unwrap(), before, "the existing file is untouched");
    let list: Value = serde_json::from_slice(&ks(&["key", "list"], &r.env()).stdout).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1, "no key is stored when the file cannot be written");
}

#[test]
fn stderr_has_no_authorization() {
    let Some(r) = Redis::get() else { return };
    let f = Fixture::new("stderr");
    let (id, key) = r.issue("reader", &f.root);
    let s = serve(&f.root, &["--backend", "file"], &r.env());
    search(&s, Some(&key), &["redis"], "");
    call(&s, Some(&key), "ks_get", json!({"id": f.cards[0].id})).unwrap();
    s.conn().rpc(&s.host(), Some(&format!("ks_{}", "1".repeat(64))), "tools/list", json!({}));
    std::thread::sleep(Duration::from_millis(200));
    let err = s.stderr();
    assert!(!err.contains(&key[3..]) && !err.contains(&ks_core::auth::key_hash(&key)), "{err}");
    assert!(!err.contains(&"1".repeat(64)) && !err.to_lowercase().contains("bearer"), "{err}");
    assert!(err.lines().any(|l| l.contains(&format!("key={id}")) && l.contains("role=reader") && l.contains("tool=ks_search") && l.contains("status=200")), "{err}");
    assert!(err.lines().any(|l| l.contains("status=401")), "{err}");
}

// ---------------------------------------------------------------- T2: features

#[test]
fn owner_get_is_full_card() {
    let Some(r) = Redis::get() else { return };
    let f = Fixture::new("owner");
    let (_, owner) = r.issue("owner", &f.root);
    let s = serve(&f.root, &["--backend", "file"], &r.env());
    for c in &f.cards {
        assert_eq!(call(&s, Some(&owner), "ks_get", json!({"id": c.id})).unwrap(), c.render());
    }
}

#[test]
fn allowed_hosts_list_accepts_each() {
    let f = Fixture::new("hosts");
    let s = serve(&f.root, &["--no-auth", "--backend", "file", "--allowed-host", "a.example,b.example", "--allowed-host", "c.example", "--allowed-host=d.example"], &[]);
    for h in ["a.example", "b.example", "c.example", "d.example:8790"] {
        assert_eq!(s.conn().rpc(h, None, "tools/list", json!({})).status, 200, "{h}");
    }
    assert_eq!(s.conn().rpc("e.example", None, "tools/list", json!({})).status, 403);
}

#[test]
fn allowed_host_appends_to_loopback_defaults() {
    let f = Fixture::new("append");
    let s = serve(&f.root, &["--no-auth", "--backend", "file", "--allowed-host", "tail.example"], &[]);
    assert!(s.stderr().contains("ks mcp allowed hosts: localhost, 127.0.0.1, ::1, tail.example\n"), "{}", s.stderr());
    for h in [s.host(), format!("localhost:{}", s.port), "tail.example".into()] {
        assert_eq!(s.conn().rpc(&h, None, "tools/list", json!({})).status, 200, "{h}");
    }
}

#[test]
fn concurrent_search_and_list_complete() {
    let f = Fixture::new("concurrent");
    let s = Arc::new(serve(&f.root, &["--no-auth", "--backend", "file"], &[]));
    let start = Instant::now();
    let handles: Vec<_> = (0..20)
        .map(|i| {
            let s = s.clone();
            std::thread::spawn(move || {
                if i % 2 == 0 {
                    !ids(&search(&s, None, &["redis"], "")).is_empty()
                } else {
                    tool_names(&s, None).0.len() == 2
                }
            })
        })
        .collect();
    for h in handles {
        assert!(h.join().unwrap());
    }
    assert!(start.elapsed() < Duration::from_secs(10), "{:?}", start.elapsed());
}

#[test]
fn http_flags_need_http() {
    let f = Fixture::new("flags");
    for args in [&["--no-auth"][..], &["--allowed-host", "a.example"], &["--min-role", "admin"]] {
        let o = ks(&[&["mcp", "serve", "--data", f.root.to_str().unwrap(), "--backend", "file"][..], args].concat(), &[]);
        assert!(!o.status.success(), "{args:?}");
    }
    let e = refused(&f.root, &["--no-auth", "--min-role", "admin", "--backend", "file"], &[]);
    assert!(e.contains("--no-auth"), "{e}");
}
