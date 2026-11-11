//! The public sample pool (examples/sample-data): every card is public, every source is listed with its license, and
//! the golden queries in queries.json find the cards they are written for, with the file backend and with Redis.
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn pool() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/sample-data")
}

fn ks(args: &[&dyn AsRef<std::ffi::OsStr>], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ks"));
    for a in args {
        c.arg(a.as_ref());
    }
    for k in ["KS_REDIS_URL", "KS_REDIS_PASSWORD", "KS_REDIS_HOST", "KS_REDIS_PORT", "KS_KEY_PREFIX"] {
        c.env_remove(k);
    }
    // A dead port unless the test names a Redis: nothing else on this machine can be reached by accident.
    c.env("KS_REDIS_URL", "redis://127.0.0.1:1");
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// The frontmatter of one card: `key: value` scalars and `key:` block lists (`- item`), as `ks` writes them.
struct Card {
    path: PathBuf,
    text: String,
    scalars: HashMap<String, String>,
    lists: HashMap<String, Vec<String>>,
}
impl Card {
    fn scalar(&self, k: &str) -> &str {
        self.scalars.get(k).map(String::as_str).unwrap_or_else(|| panic!("{}: no `{k}` in the frontmatter", self.path.display()))
    }
    fn list(&self, k: &str) -> &[String] {
        self.lists.get(k).map(Vec::as_slice).unwrap_or(&[])
    }
    /// The `- ` bullets under `## 요약`.
    fn summary_lines(&self) -> usize {
        let body = self.text.splitn(3, "---\n").nth(2).unwrap_or("");
        let sum = body.split("## 요약").nth(1).unwrap_or("").split("## 한계").next().unwrap_or("");
        sum.lines().filter(|l| l.starts_with("- ")).count()
    }
}
fn unquote(v: &str) -> String {
    v.trim().trim_matches('\'').trim_matches('"').to_string()
}
fn parse_card(path: &Path) -> Card {
    let text = fs::read_to_string(path).unwrap();
    let head = text.strip_prefix("---\n").unwrap_or_else(|| panic!("{}: no frontmatter", path.display()));
    let head = head.split("\n---\n").next().unwrap();
    let (mut scalars, mut lists): (HashMap<String, String>, HashMap<String, Vec<String>>) = Default::default();
    let mut open: Option<String> = None;
    for line in head.lines() {
        if let Some(item) = line.strip_prefix("- ") {
            if let Some(k) = &open {
                lists.entry(k.clone()).or_default().push(unquote(item));
            }
        } else if let Some((k, v)) = line.split_once(':') {
            let v = v.trim();
            open = if v.is_empty() { Some(k.to_string()) } else { None };
            if !v.is_empty() {
                scalars.insert(k.to_string(), unquote(v));
            }
        }
    }
    Card { path: path.to_path_buf(), text, scalars, lists }
}
fn cards() -> Vec<Card> {
    let mut v = vec![];
    for topic in fs::read_dir(pool().join("cards")).expect("examples/sample-data/cards exists") {
        for f in fs::read_dir(topic.unwrap().path()).unwrap() {
            let p = f.unwrap().path();
            if p.extension().is_some_and(|e| e == "md") {
                v.push(parse_card(&p));
            }
        }
    }
    v.sort_by(|a, b| a.path.cmp(&b.path));
    v
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            files_under(&p, out);
        } else {
            out.push(p);
        }
    }
}

#[test]
fn sample_pool_all_public() {
    let cs = cards();
    assert!((15..=25).contains(&cs.len()), "15..=25 cards expected, found {}", cs.len());
    for c in &cs {
        let at = c.path.display();
        assert_eq!(c.scalar("scope"), "public", "{at}");
        assert_eq!(c.scalar("access"), "A", "{at}");
        assert!(c.summary_lines() <= 3, "{at}: the summary is at most 3 lines");
        assert!(c.scalar("source_url").starts_with("https://"), "{at}");
        assert!(!c.scalars.contains_key("source_path") || c.scalar("source_path") == "null", "{at}: a public card names no local path");
    }
    // No Medium text, link or name in anything the pool ships (cards, vocabulary, sources, queries).
    let mut all = vec![];
    files_under(&pool(), &mut all);
    for f in &all {
        let t = fs::read_to_string(f).unwrap().to_lowercase();
        assert!(!t.contains("medium.com"), "{}: contains a medium.com fragment", f.display());
    }
    let o = ks(&[&"card", &"validate", &"--data", &pool()], &[]);
    assert!(o.status.success(), "ks card validate: {}{}", String::from_utf8_lossy(&o.stdout), err(&o));
}

#[test]
fn sample_pool_covers_activities_and_domains() {
    let cs = cards();
    let union = |k: &str| cs.iter().flat_map(|c| c.list(k).iter().cloned()).collect::<BTreeSet<_>>();
    assert!(union("techniques").len() >= 10, "development activities covered: {:?}", union("techniques"));
    assert!(union("domains").len() >= 6, "technical areas covered: {:?}", union("domains"));
    assert!(union("entities").len() >= 10, "named tools covered: {:?}", union("entities"));
    for c in &cs {
        assert!(c.list("new_terms").is_empty(), "{}: every keyword is in the sample vocabulary, found {:?}", c.path.display(), c.list("new_terms"));
    }
}

#[test]
fn sample_sources_match() {
    let cs = cards();
    let sources = fs::read_to_string(pool().join("SOURCES.md")).expect("SOURCES.md exists");
    assert!(sources.contains("own words"), "SOURCES.md says the summaries are the pool's own words");
    // Rows: | card id | source URL | license | license evidence URL | checked (YYYY-MM-DD) |
    let mut rows: HashMap<String, Vec<String>> = HashMap::new();
    for line in sources.lines().filter(|l| l.starts_with('|')) {
        let cells: Vec<String> = line.trim().trim_matches('|').split('|').map(|c| c.trim().to_string()).collect();
        if cells.len() == 5 && cells[0].len() == 16 && cells[0].bytes().all(|b| b.is_ascii_hexdigit()) {
            assert!(rows.insert(cells[0].clone(), cells).is_none(), "a card id is listed once");
        }
    }
    assert_eq!(rows.len(), cs.len(), "one SOURCES row per card");
    for c in &cs {
        let id = c.scalar("id");
        let row = rows.get(id).unwrap_or_else(|| panic!("{id}: no SOURCES row"));
        assert_eq!(row[1], c.scalar("source_url"), "{id}: the SOURCES URL is the card's source_url");
        assert!(!row[2].is_empty() && !row[2].to_lowercase().contains("unknown"), "{id}: a license is named");
        assert!(row[3].starts_with("https://"), "{id}: the license evidence is a URL");
        let d = row[4].as_bytes();
        assert!(d.len() == 10 && d[4] == b'-' && d[7] == b'-' && row[4].chars().filter(char::is_ascii_digit).count() == 8, "{id}: checked date is YYYY-MM-DD, got {:?}", row[4]);
        assert!(c.path.ends_with(format!("{id}.md")), "{id}: the file is named after the card id");
    }
}

/// Run every golden query through `backend` and check the expected cards are in each query's top results.
fn check_golden(backend: &str, env: &[(&str, &str)]) -> Vec<u8> {
    let cs = cards();
    let key_of: HashMap<String, String> = cs.iter().map(|c| (c.scalar("id").to_string(), c.scalar("card_key").to_string())).collect();
    let qpath = pool().join("queries.json");
    let queries: Value = serde_json::from_str(&fs::read_to_string(&qpath).expect("queries.json exists")).unwrap();
    let queries = queries.as_array().expect("queries.json is a JSON array of {qid, kind, query, terms?, expect, top?}");
    assert!((8..=10).contains(&queries.len()), "8..=10 golden queries expected, found {}", queries.len());
    let kinds: BTreeSet<&str> = queries.iter().map(|q| q["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, BTreeSet::from(["keyword", "natural"]), "both keyword and natural-language queries");
    for q in queries {
        match q["kind"].as_str().unwrap() {
            "keyword" => assert!(q["terms"].as_array().is_some_and(|t| !t.is_empty()), "{}: a keyword query carries terms", q["qid"]),
            _ => assert!(q.get("terms").is_none(), "{}: a natural-language query is free text only", q["qid"]),
        }
    }
    let out_dir = std::env::temp_dir().join(format!("ks-sample-pool-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    fs::create_dir_all(&out_dir).unwrap();
    let out = out_dir.join(format!("cand-{backend}.json"));
    let vocab = pool().join("vocab/terms.json");
    let o = ks(&[&"search", &"--data", &pool(), &"--vocab", &vocab, &"--queries", &qpath, &"--out", &out, &"--backend", &backend], env);
    assert!(o.status.success(), "ks search --backend {backend}: {}", err(&o));
    let raw = fs::read(&out).unwrap();
    let _ = fs::remove_dir_all(&out_dir);
    let results: Value = serde_json::from_slice(&raw).unwrap();
    for q in queries {
        let (qid, top) = (q["qid"].as_str().unwrap(), q["top"].as_u64().unwrap_or(3) as usize);
        let r = results.as_array().unwrap().iter().find(|r| r["qid"] == qid).unwrap_or_else(|| panic!("{qid}: no result"));
        let got: Vec<&str> = r["candidates"].as_array().unwrap().iter().take(top).map(|c| c["card_key"].as_str().unwrap()).collect();
        let expect = q["expect"].as_array().filter(|e| !e.is_empty()).unwrap_or_else(|| panic!("{qid}: expect lists card ids"));
        for id in expect {
            let id = id.as_str().unwrap();
            let key = key_of.get(id).unwrap_or_else(|| panic!("{qid}: expected card {id} is not in the pool"));
            assert!(got.contains(&key.as_str()), "{qid}: card {id} is not in the top {top} ({backend}); got {got:?}");
        }
        if q["kind"] == "keyword" {
            assert_eq!(r["unknown_terms"], json!([]), "{qid}: every keyword is in the sample vocabulary");
        }
    }
    raw
}

#[test]
fn sample_pool_golden_queries() {
    check_golden("file", &[]);
}

fn test_redis() -> Option<(String, String)> {
    let url = std::env::var("KS_TEST_REDIS_URL").ok().filter(|u| !u.is_empty())?;
    Some((url, format!("kstest-sample-{}-{}:", std::process::id(), N.fetch_add(1, Ordering::SeqCst))))
}
struct Purge(String, String);
impl Drop for Purge {
    fn drop(&mut self) {
        if let Ok(mut i) = ks_core::index::Index::connect(&self.0, &self.1) {
            let _ = i.purge();
        }
    }
}

#[test]
fn sample_pool_golden_queries_redis() {
    let Some((url, prefix)) = test_redis() else { return };
    let _purge = Purge(url.clone(), prefix.clone());
    let env = [("KS_REDIS_URL", url.as_str()), ("KS_KEY_PREFIX", prefix.as_str())];
    let vocab = pool().join("vocab/terms.json");
    let o = ks(&[&"index", &"rebuild", &"--data", &pool(), &"--vocab", &vocab], &env);
    assert!(o.status.success(), "ks index rebuild: {}", err(&o));
    let redis = check_golden("redis", &env);
    assert_eq!(String::from_utf8_lossy(&redis), String::from_utf8_lossy(&check_golden("file", &env)), "Redis and file search agree on the sample pool");
}
