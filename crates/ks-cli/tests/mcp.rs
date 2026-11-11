//! MCP stdio round trip: spawn the real `ks mcp serve` and talk to it with rmcp's client.
//! The Redis variant runs only when KS_TEST_REDIS_URL is set.
use ks_core::card::Card;
use ks_core::index::Index;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::ServiceExt;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const VOCAB: &str = r#"{"version":1,"terms":[
 {"term":"redis","facet":"entity","aliases":["redis-stack"]},
 {"term":"ttl","facet":"technique","aliases":[]},
 {"term":"kafka","facet":"entity","aliases":[]},{"term":"flink","facet":"entity","aliases":[]},{"term":"spark","facet":"entity","aliases":[]}]}"#;

fn card(id: &str, title: &str, tech: &[&str]) -> Card {
    let mut c = Card::new(&format!("https://m.com/{id}"));
    c.title = Some(title.into());
    c.topic = Some("topic-a".into());
    c.evidence_type = Some("tutorial".into());
    c.tech_keywords = tech.iter().map(|s| s.to_string()).collect();
    c.problems = vec!["가나다".into(), "라마바".into()];
    c.summary = vec!["첫째 줄 요약입니다 열 자 이상.".into(), "둘째 줄 요약입니다 열 자 이상.".into(), "셋째 줄 요약입니다 열 자 이상.".into()];
    c.limits = "한계 설명 한 줄입니다 열 자 이상.".into();
    let v = ks_core::vocab::Vocab::from_json(VOCAB, "t").unwrap();
    let got = v.classify_terms(&c.tech_keywords);
    (c.entities, c.techniques, c.domains, c.new_terms) = (got.entities, got.techniques, got.domains, got.new_terms);
    c
}

struct Fixture { root: PathBuf, cards: Vec<Card> }
impl Fixture {
    fn new(tag: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!("ks-mcp-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("cards/topic-a")).unwrap();
        fs::create_dir_all(root.join("vocab")).unwrap();
        fs::write(root.join("vocab/terms.json"), VOCAB).unwrap();
        let cards = vec![
            card("a", "Session store notes", &["redis", "ttl", "kafka"]),
            card("b", "Agent notes", &["redis-stack", "flink", "spark"]),
            card("c", "Grading notes", &["ttl", "kafka", "spark"]),
        ];
        for c in &cards {
            fs::write(root.join(format!("cards/topic-a/{}.md", c.id)), c.render()).unwrap();
        }
        Fixture { root, cards }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.root); }
}

fn vocab_file(f: &Fixture) -> PathBuf { f.root.join("vocab/terms.json") }

/// What `ks index rebuild --data <root>` does: fingerprint the files, read them, build the index and store the fingerprints.
fn rebuild_like_the_cli(idx: &mut Index, f: &Fixture) {
    let fp = ks_core::index::Fingerprints::of_files(&vocab_file(f), &f.root.join("cards")).unwrap();
    let (cards, vocab) = (ks_core::collect::load_cards(&f.root.join("cards")).unwrap(), ks_core::vocab::Vocab::load(&vocab_file(f)).unwrap());
    idx.rebuild_with(&cards, &vocab, None, &fp).unwrap();
}

/// Deletes every key under the test prefix when dropped, so a test that fails half way leaves no `kstest-*` keys behind.
struct Purge(String, String);
impl Drop for Purge {
    fn drop(&mut self) {
        if let Ok(mut i) = Index::connect(&self.0, &self.1) { let _ = i.purge(); }
    }
}

type Client = rmcp::service::RunningService<rmcp::RoleClient, ()>;

async fn connect(data: &Path, extra: &[&str], env: &[(&str, &str)]) -> Client {
    let transport = TokioChildProcess::new(tokio::process::Command::new(env!("CARGO_BIN_EXE_ks")).configure(|c| {
        c.arg("mcp").arg("serve").arg("--data").arg(data).args(extra);
        for k in ["KS_REDIS_URL", "KS_REDIS_PASSWORD", "KS_REDIS_HOST", "KS_REDIS_PORT", "KS_KEY_PREFIX"] { c.env_remove(k); }
        // Default to a dead port so a test that forgets its env can never reach whatever else listens on 6379.
        c.env("KS_REDIS_URL", "redis://127.0.0.1:1");
        for (k, v) in env { c.env(k, v); }
    }))
    .expect("spawn ks mcp serve");
    tokio::time::timeout(Duration::from_secs(30), ().serve(transport)).await.expect("initialize timed out").expect("initialize")
}

async fn call(c: &Client, tool: &'static str, args: Value) -> CallToolResult {
    let params = CallToolRequestParams::new(tool).with_arguments(args.as_object().unwrap().clone());
    tokio::time::timeout(Duration::from_secs(30), c.call_tool(params)).await.expect("call timed out").expect("call")
}
fn text(r: &CallToolResult) -> String {
    r.content.iter().filter_map(|b| b.as_text().map(|t| t.text.clone())).collect::<Vec<_>>().join("\n")
}
fn json_of(r: &CallToolResult) -> Value {
    assert_ne!(r.is_error, Some(true), "{}", text(r));
    serde_json::from_str(&text(r)).unwrap_or_else(|e| panic!("not JSON ({e}): {}", text(r)))
}

async fn check_list_tools(c: &Client) {
    let tools = c.list_all_tools().await.unwrap();
    let mut names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    assert_eq!(names, ["ks_get", "ks_record", "ks_search", "ks_search_web"]);
    for t in &tools {
        let d = t.description.as_deref().unwrap_or("").to_lowercase();
        assert!(d.contains("untrusted") && d.contains("data"), "{}: {d}", t.name);
    }
    let search = tools.iter().find(|t| t.name == "ks_search").unwrap();
    assert!(search.description.as_deref().unwrap_or("").contains("unknown_terms"), "the tool description tells the model about unknown_terms");
    let schema = serde_json::to_value(&search.input_schema).unwrap().to_string();
    assert!(schema.contains("terms") && schema.contains("query") && schema.contains("limit"), "{schema}");
    let web = tools.iter().find(|t| t.name == "ks_search_web").unwrap();
    let schema = serde_json::to_value(&web.input_schema).unwrap().to_string();
    assert!(schema.contains("query") && schema.contains("limit") && schema.contains("mode"), "{schema}");
}

async fn check_search_and_get(c: &Client, f: &Fixture) {
    let r = call(c, "ks_search", json!({"terms": ["Redis Stack", "ttl"], "query": "session expiry", "limit": 2})).await;
    let v = json_of(&r);
    assert_eq!(v["scope"], json!({"l1_cards": 3}));
    let cands = v["candidates"].as_array().unwrap();
    assert_eq!(cands.len(), 2, "limit is honoured: {v}");
    let first = &cands[0];
    assert_eq!(first["id"], json!(f.cards[0].id));
    assert_eq!(first["rank"], 1);
    assert_eq!(first["title"], "Session store notes");
    assert_eq!(first["matched_terms"], json!(["redis", "ttl"]));
    assert_eq!(first["summary"].as_array().unwrap().len(), 3);
    assert_eq!(first["problems"], json!(["가나다", "라마바"]));
    assert_eq!(first["limits"], "한계 설명 한 줄입니다 열 자 이상.");
    assert_eq!(first["evidence_type"], "tutorial");
    assert!(first["text_hits"].is_array());
    assert!(v["notice"].as_str().unwrap().to_lowercase().contains("untrusted"));
    assert_eq!(v["unknown_terms"], json!([]), "every term resolves: the field is still there");

    // a term the vocabulary does not know is reported, not silently dropped; known terms still find their cards
    let r = call(c, "ks_search", json!({"terms": ["ttl", "Zzzz Term", "zzzz term"], "query": "x"})).await;
    let v = json_of(&r);
    assert_eq!(v["unknown_terms"], json!(["zzzz term"]), "{v}");
    assert!(!v["candidates"].as_array().unwrap().is_empty(), "{v}");

    // no terms: the query text is tokenized like `ks search` does
    let r = call(c, "ks_search", json!({"terms": [], "query": "ttl kafka"})).await;
    let v = json_of(&r);
    assert!(!v["candidates"].as_array().unwrap().is_empty(), "{v}");

    let r = call(c, "ks_get", json!({"id": f.cards[1].id})).await;
    let md = text(&r);
    assert_ne!(r.is_error, Some(true), "{md}");
    assert_eq!(md, f.cards[1].render());

    for bad in ["0000000000000000", "../../etc/passwd", ""] {
        let r = call(c, "ks_get", json!({"id": bad})).await;
        assert_eq!(r.is_error, Some(true), "id {bad:?}: {}", text(&r));
    }
    let r = call(c, "ks_search", json!({"terms": [], "query": "ttl", "limit": 0})).await;
    assert_eq!(r.is_error, Some(true), "limit must be positive: {}", text(&r));
}

#[tokio::test(flavor = "multi_thread")]
async fn file_backend_lists_tools_and_serves_search_and_get() {
    let f = Fixture::new("file");
    let c = connect(&f.root, &["--backend", "file"], &[]).await;
    check_list_tools(&c).await;
    check_search_and_get(&c, &f).await;
    // ks_record needs Redis; with none reachable it is a tool error, not a crash of the server
    let r = call(&c, "ks_record", json!({"task": "t", "card": f.cards[0].id, "verdict": "A", "reason": "r"})).await;
    assert_eq!(r.is_error, Some(true), "{}", text(&r));
    assert_eq!(json_of(&call(&c, "ks_search", json!({"terms": ["ttl"], "query": ""})).await)["scope"], json!({"l1_cards": 3}));
    c.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn redis_backend_serves_search_and_records_applications() {
    let Some(url) = ks_core::testkit::test_redis_url() else { return };
    let prefix = format!("kstest-mcp-{}:", std::process::id());
    let _purge = Purge(url.clone(), prefix.clone());
    let f = Fixture::new("redis");
    let mut idx = Index::connect(&url, &prefix).unwrap();
    rebuild_like_the_cli(&mut idx, &f);
    let env = [("KS_REDIS_URL", url.as_str()), ("KS_KEY_PREFIX", prefix.as_str())];

    let c = connect(&f.root, &[], &env).await; // default backend is redis
    check_list_tools(&c).await;
    check_search_and_get(&c, &f).await;
    let r = call(&c, "ks_record", json!({"task": "T-9", "card": f.cards[0].id, "verdict": "A", "reason": "같은 문제", "outcome": "success"})).await;
    let v = json_of(&r);
    assert!(v["id"].as_str().unwrap().contains('-'), "{v}");
    let r = call(&c, "ks_record", json!({"task": "T-9", "card": f.cards[0].id, "verdict": "maybe", "reason": "x"})).await;
    assert_eq!(r.is_error, Some(true), "{}", text(&r));
    c.cancel().await.unwrap();

    let out = f.root.join("dump");
    let d = idx.dump_applications(&out).unwrap();
    assert_eq!((d.appended, d.total), (1, 1));
    let line: Value = serde_json::from_str(fs::read_to_string(out.join("applications.jsonl")).unwrap().trim_end()).unwrap();
    assert_eq!((line["task"].as_str(), line["verdict"].as_str(), line["outcome"].as_str()), (Some("T-9"), Some("A"), Some("success")));
    idx.purge().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn search_web_rejects_bad_arguments_without_starting_a_browser() {
    let f = Fixture::new("web");
    let home = f.root.join("kshome");
    let home_s = home.to_str().unwrap().to_string();
    let c = connect(&f.root, &["--backend", "file"], &[("KS_HOME", home_s.as_str()), ("KS_NOTIFY", "0")]).await;
    for args in [json!({"query": "redis", "limit": 0}), json!({"query": "redis", "mode": "visible"}), json!({"query": "  "}), json!({"query": "x", "limit": 101})] {
        let r = call(&c, "ks_search_web", args.clone()).await;
        assert_eq!(r.is_error, Some(true), "{args}: {}", text(&r));
    }
    assert!(!home.join("run/default.sock").exists(), "a browser daemon was started");
    c.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn get_does_not_follow_a_symlinked_card() {
    let f = Fixture::new("link");
    let outside = f.root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    // a valid card that lives outside <data>/cards, linked into it
    let stray = card("z", "Stray card", &["redis"]);
    fs::write(outside.join(format!("{}.md", stray.id)), stray.render()).unwrap();
    std::os::unix::fs::symlink(outside.join(format!("{}.md", stray.id)), f.root.join(format!("cards/topic-a/{}.md", stray.id))).unwrap();
    let c = connect(&f.root, &["--backend", "file"], &[]).await;
    let r = call(&c, "ks_get", json!({"id": stray.id})).await;
    assert_eq!(r.is_error, Some(true), "a symlinked card was served: {}", text(&r));
    assert_eq!(json_of(&call(&c, "ks_search", json!({"terms": ["redis"], "query": ""})).await)["scope"], json!({"l1_cards": 3}), "a symlinked card was loaded");
    c.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn search_input_is_capped() {
    let f = Fixture::new("caps");
    let c = connect(&f.root, &["--backend", "file"], &[]).await;
    let many: Vec<String> = (0..33).map(|i| format!("t{i}")).collect();
    for args in [json!({"terms": many, "query": "x"}), json!({"terms": ["x".repeat(65)], "query": "x"}), json!({"terms": [], "query": "a".repeat(513)})] {
        let r = call(&c, "ks_search", args.clone()).await;
        assert_eq!(r.is_error, Some(true), "accepted: {}", text(&r).chars().take(200).collect::<String>());
    }
    let r = call(&c, "ks_search", json!({"terms": ["x".repeat(64)], "query": "a".repeat(512)})).await;
    assert_ne!(r.is_error, Some(true), "{}", text(&r));
    c.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn redis_backend_refuses_every_search_while_the_index_is_stale() {
    let Some(url) = ks_core::testkit::test_redis_url() else { return };
    let prefix = format!("kstest-mcp-stale-{}:", std::process::id());
    let _purge = Purge(url.clone(), prefix.clone());
    let f = Fixture::new("stale");
    let mut idx = Index::connect(&url, &prefix).unwrap();
    rebuild_like_the_cli(&mut idx, &f);
    let env = [("KS_REDIS_URL", url.as_str()), ("KS_KEY_PREFIX", prefix.as_str())];
    let c = connect(&f.root, &[], &env).await;
    let search = || call(&c, "ks_search", json!({"terms": ["ttl"], "query": "x"}));
    let hint = format!("ks index rebuild --data {} --vocab {}", f.root.display(), vocab_file(&f).display());
    let refused = |r: CallToolResult, what: &str| {
        assert_eq!(r.is_error, Some(true), "{what}: {}", text(&r));
        assert!(text(&r).contains(&hint), "{what}: {}", text(&r));
        text(&r)
    };
    assert_eq!(json_of(&search().await)["scope"], json!({"l1_cards": 3}));

    // the server is long-running: the check happens on every search, not once at startup
    let original = fs::read_to_string(vocab_file(&f)).unwrap();
    fs::write(vocab_file(&f), original.replace("redis-stack", "redis-db")).unwrap();
    for _ in 0..2 {
        let e = refused(search().await, "vocab changed");
        assert!(e.contains("vocab file") && !e.contains("card files"), "{e}");
    }
    // rebuilt for the new vocabulary, but this server still holds the old one in memory: it must be restarted
    rebuild_like_the_cli(&mut idx, &f);
    let e = text(&search().await);
    assert!(e.contains("restart") && e.contains("ks mcp serve"), "{e}");
    // back to the vocabulary the server loaded, index rebuilt for it: current again
    fs::write(vocab_file(&f), &original).unwrap();
    rebuild_like_the_cli(&mut idx, &f);
    assert_eq!(json_of(&search().await)["scope"], json!({"l1_cards": 3}));

    // cards added, then removed again
    let extra = f.root.join("cards/topic-a/added.md");
    fs::write(&extra, Card::new("https://m.com/d").render()).unwrap();
    let e = refused(search().await, "card added");
    assert!(e.contains("card files") && !e.contains("vocab file"), "{e}");
    rebuild_like_the_cli(&mut idx, &f);
    assert_eq!(json_of(&search().await)["scope"], json!({"l1_cards": 4}));
    fs::remove_file(&extra).unwrap();
    refused(search().await, "card removed");
    rebuild_like_the_cli(&mut idx, &f);
    assert_eq!(json_of(&search().await)["scope"], json!({"l1_cards": 3}));

    // a card edited in place
    let p = f.root.join(format!("cards/topic-a/{}.md", f.cards[0].id));
    fs::write(&p, format!("{}\n", fs::read_to_string(&p).unwrap())).unwrap();
    refused(search().await, "card modified");
    rebuild_like_the_cli(&mut idx, &f);
    assert_eq!(json_of(&search().await)["scope"], json!({"l1_cards": 3}));

    // an index without fingerprints (built by an older ks)
    let (cards, vocab) = (ks_core::collect::load_cards(&f.root.join("cards")).unwrap(), ks_core::vocab::Vocab::load(&vocab_file(&f)).unwrap());
    idx.rebuild(&cards, &vocab, None).unwrap();
    let e = refused(search().await, "no fingerprints");
    assert!(e.contains("without fingerprints"), "{e}");
    rebuild_like_the_cli(&mut idx, &f);
    assert_eq!(json_of(&search().await)["scope"], json!({"l1_cards": 3}));

    // tools that do not search keep working while the index is stale
    fs::write(&extra, Card::new("https://m.com/d").render()).unwrap();
    assert_ne!(call(&c, "ks_get", json!({"id": f.cards[1].id})).await.is_error, Some(true));
    c.cancel().await.unwrap();
    idx.purge().unwrap();
}
