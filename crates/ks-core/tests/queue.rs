//! The collection queue: a JSONL file the engine appends discovered URLs to and another program reads.
use ks_core::card::Card;
use ks_core::queue::*;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Tmp {
        let p = PathBuf::from(format!("/tmp/ks-queue-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn q(&self) -> PathBuf {
        self.0.join("queue.jsonl")
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn entry(key: &str) -> QueueEntry {
    QueueEntry::new(key, &format!("https://example.com/{key}"), &format!("Title {key}"), "google-browser", "some query", "2026-11-11T01:02:03Z")
}
fn none() -> HashSet<String> {
    HashSet::new()
}
fn lines(p: &Path) -> Vec<String> {
    std::fs::read_to_string(p).unwrap().lines().map(String::from).collect()
}
fn keys(p: &Path) -> Vec<String> {
    lines(p).iter().filter(|l| !l.is_empty()).map(|l| serde_json::from_str::<Value>(l).unwrap()["card_key"].as_str().unwrap().to_string()).collect()
}

#[test]
fn a_line_has_fixed_key_order_and_version_1() {
    let e = QueueEntry::new("https://example.com/a", "https://example.com/a", "A \"quoted\" 한글 title", "google-browser", "q one", "2026-11-11T01:02:03Z");
    assert_eq!(
        e.to_line(),
        r#"{"v":1,"card_key":"https://example.com/a","url":"https://example.com/a","title":"A \"quoted\" 한글 title","engine":"google-browser","query":"q one","found_at":"2026-11-11T01:02:03Z"}"#
    );
}

#[test]
fn a_title_with_newlines_stays_on_one_line() {
    let mut e = entry("a");
    e.title = "two\nlines\r\nand \u{2028} separators".into();
    let line = e.to_line();
    assert!(!line.contains('\n') && !line.contains('\r'), "{line:?}");
    assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["title"], "two\nlines\r\nand \u{2028} separators");
}

#[test]
fn enqueue_creates_the_file_with_mode_0600_and_one_line_per_entry() {
    let t = Tmp::new();
    let r = enqueue(&t.q(), &[entry("a"), entry("b")], &none()).unwrap();
    assert_eq!((r.enqueued, r.skipped_known), (2, 0));
    assert_eq!(std::fs::metadata(t.q()).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(keys(&t.q()), ["a", "b"]);
    assert!(std::fs::read_to_string(t.q()).unwrap().ends_with('\n'));
}

#[test]
fn enqueue_appends_and_leaves_existing_lines_and_mode_alone() {
    let t = Tmp::new();
    std::fs::write(t.q(), format!("{}\n", entry("a").to_line())).unwrap();
    std::fs::set_permissions(t.q(), std::fs::Permissions::from_mode(0o644)).unwrap();
    let before = lines(&t.q());
    enqueue(&t.q(), &[entry("b")], &none()).unwrap();
    let after = lines(&t.q());
    assert_eq!(after[..1], before[..]);
    assert_eq!(keys(&t.q()), ["a", "b"]);
    assert_eq!(std::fs::metadata(t.q()).unwrap().permissions().mode() & 0o777, 0o644);
}

#[test]
fn a_card_key_already_in_the_file_is_not_written_again() {
    let t = Tmp::new();
    enqueue(&t.q(), &[entry("a")], &none()).unwrap();
    let r = enqueue(&t.q(), &[entry("a"), entry("b")], &none()).unwrap();
    assert_eq!((r.enqueued, r.skipped_known), (1, 1));
    assert_eq!(keys(&t.q()), ["a", "b"]);
}

#[test]
fn a_card_key_repeated_within_one_call_is_written_once() {
    let t = Tmp::new();
    let r = enqueue(&t.q(), &[entry("a"), entry("a"), entry("a")], &none()).unwrap();
    assert_eq!((r.enqueued, r.skipped_known), (1, 2));
    assert_eq!(keys(&t.q()), ["a"]);
}

#[test]
fn a_card_key_that_already_has_a_card_is_not_written() {
    let t = Tmp::new();
    let known: HashSet<String> = ["a".to_string()].into();
    let r = enqueue(&t.q(), &[entry("a"), entry("b")], &known).unwrap();
    assert_eq!((r.enqueued, r.skipped_known), (1, 1));
    assert_eq!(keys(&t.q()), ["b"]);
}

#[test]
fn nothing_to_write_still_works_and_does_not_need_a_new_line() {
    let t = Tmp::new();
    let known: HashSet<String> = ["a".to_string()].into();
    let r = enqueue(&t.q(), &[entry("a")], &known).unwrap();
    assert_eq!((r.enqueued, r.skipped_known), (0, 1));
    assert_eq!(std::fs::read_to_string(t.q()).unwrap(), "");
}

#[test]
fn a_missing_parent_directory_is_an_error_and_is_not_created() {
    let t = Tmp::new();
    let q = t.0.join("nope/deeper/queue.jsonl");
    let e = enqueue(&q, &[entry("a")], &none()).unwrap_err().to_string();
    assert!(e.contains("nope/deeper") && e.contains("does not exist"), "{e}");
    assert!(!t.0.join("nope").exists());
    assert!(check_parent(&q).is_err());
    assert!(check_parent(&t.q()).is_ok());
    assert!(check_parent(Path::new("queue.jsonl")).is_ok(), "a bare file name lives in the current directory");
}

#[test]
fn a_broken_existing_line_stops_with_an_error_naming_the_line_and_writes_nothing() {
    let t = Tmp::new();
    let good = entry("a").to_line();
    let original = format!("{good}\n{{\"v\":1,\"card_key\":\n{good}\n");
    std::fs::write(t.q(), &original).unwrap();
    let e = enqueue(&t.q(), &[entry("b")], &none()).unwrap_err().to_string();
    assert!(e.contains("line 2"), "{e}");
    assert_eq!(std::fs::read_to_string(t.q()).unwrap(), original);
}

#[test]
fn an_existing_line_that_is_json_but_has_no_card_key_is_an_error() {
    for bad in [r#"{"v":1}"#, r#"[1,2]"#, r#""x""#, r#"{"card_key":5}"#] {
        let t = Tmp::new();
        std::fs::write(t.q(), format!("{bad}\n")).unwrap();
        let e = enqueue(&t.q(), &[entry("b")], &none()).unwrap_err().to_string();
        assert!(e.contains("line 1"), "{bad}: {e}");
        assert_eq!(std::fs::read_to_string(t.q()).unwrap(), format!("{bad}\n"));
    }
}

#[test]
fn a_torn_last_line_is_an_error_not_silently_repaired() {
    let t = Tmp::new();
    let torn = format!("{}\n{{\"v\":1,\"card_key\":\"x", entry("a").to_line());
    std::fs::write(t.q(), &torn).unwrap();
    assert!(enqueue(&t.q(), &[entry("b")], &none()).unwrap_err().to_string().contains("line 2"));
    assert_eq!(std::fs::read_to_string(t.q()).unwrap(), torn);
}

#[test]
fn blank_lines_are_tolerated_and_a_missing_final_newline_is_supplied() {
    let t = Tmp::new();
    std::fs::write(t.q(), format!("\n{}\n\n{}", entry("a").to_line(), entry("b").to_line())).unwrap();
    let r = enqueue(&t.q(), &[entry("b"), entry("c")], &none()).unwrap();
    assert_eq!((r.enqueued, r.skipped_known), (1, 1));
    assert_eq!(keys(&t.q()), ["a", "b", "c"]);
    for l in lines(&t.q()).iter().filter(|l| !l.is_empty()) {
        serde_json::from_str::<Value>(l).unwrap_or_else(|e| panic!("{e}: {l:?}"));
    }
}

#[test]
fn concurrent_writers_never_duplicate_or_interleave() {
    let t = Tmp::new();
    let q = t.q();
    // 6 writers each offer the same 20 keys plus 10 of their own
    let handles: Vec<_> = (0..6)
        .map(|w| {
            let q = q.clone();
            std::thread::spawn(move || {
                let mut es: Vec<QueueEntry> = (0..20).map(|i| entry(&format!("shared-{i}"))).collect();
                es.extend((0..10).map(|i| entry(&format!("w{w}-{i}"))));
                enqueue(&q, &es, &HashSet::new()).unwrap().enqueued
            })
        })
        .collect();
    let total: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
    let ks = keys(&q);
    let uniq: HashSet<&String> = ks.iter().collect();
    assert_eq!(ks.len(), 20 + 60);
    assert_eq!(uniq.len(), ks.len(), "duplicate card_key lines");
    assert_eq!(total, ks.len());
}

#[test]
fn entries_come_from_search_results() {
    let results = json!([
        {"rank": 1, "title": "Redis TTL", "url": "https://Example.com/a/?utm=1#frag", "snippet": "s", "engine": "google-browser", "fetched_at": "2026-11-11T01:02:03Z"},
        {"rank": 2, "title": "Other", "url": "https://example.com/b/", "snippet": "s", "engine": "google-browser", "fetched_at": "2026-11-11T01:02:03Z"},
    ]);
    let es = entries_from_results("redis ttl", &results).unwrap();
    assert_eq!(es.len(), 2);
    assert_eq!(es[0].card_key, "https://example.com/a");
    assert_eq!(es[0].url, "https://example.com/a");
    assert_eq!((es[0].title.as_str(), es[0].engine.as_str(), es[0].query.as_str(), es[0].found_at.as_str()), ("Redis TTL", "google-browser", "redis ttl", "2026-11-11T01:02:03Z"));
    assert_eq!(es[1].card_key, "https://example.com/b");
}

#[test]
fn a_result_that_is_not_an_http_url_is_an_error() {
    let r = |url: &str| json!([{"title": "t", "url": url, "engine": "google-browser", "fetched_at": "2026-11-11T01:02:03Z"}]);
    for bad in ["ftp://example.com/x", "not a url", ""] {
        assert!(entries_from_results("q", &r(bad)).is_err(), "{bad}");
    }
    assert!(entries_from_results("q", &json!([{"title": "t", "engine": "e", "fetched_at": "x"}])).is_err(), "missing url");
    assert!(entries_from_results("q", &json!("x")).is_err());
}

#[test]
fn known_card_keys_come_from_the_cards_under_data() {
    let t = Tmp::new();
    let topic = t.0.join("cards/topic");
    std::fs::create_dir_all(&topic).unwrap();
    for k in ["https://example.com/a", "path:notes/x.md"] {
        let c = Card::new(k);
        std::fs::write(topic.join(format!("{}.md", c.id)), c.render()).unwrap();
    }
    let known = known_card_keys(&t.0).unwrap();
    assert_eq!(known, HashSet::from(["https://example.com/a".to_string(), "path:notes/x.md".to_string()]));
    assert!(known_card_keys(&t.0.join("missing")).is_err(), "a data directory without cards is an error, not an empty set");
}

fn results(urls: &[&str]) -> Value {
    Value::Array(urls.iter().map(|u| json!({"rank": 1, "title": "T", "url": u, "snippet": "s", "engine": "google-browser", "fetched_at": "2026-11-11T01:02:03Z"})).collect())
}

#[test]
fn the_writer_adds_each_search_and_reports_totals() {
    let t = Tmp::new();
    let known: HashSet<String> = ["https://example.com/known".to_string()].into();
    let mut w = QueueWriter::new(&t.q(), known).unwrap();
    w.add("first query", &results(&["https://example.com/a", "https://example.com/known"])).unwrap();
    w.add("second query", &results(&["https://example.com/a/", "https://example.com/b"])).unwrap();
    assert_eq!(w.report(), json!({"path": t.q().display().to_string(), "enqueued": 2, "skipped_known": 2}));
    assert_eq!(keys(&t.q()), ["https://example.com/a", "https://example.com/b"]);
    let first: Value = serde_json::from_str(&lines(&t.q())[0]).unwrap();
    assert_eq!(first["query"], "first query");
}

#[test]
fn the_writer_refuses_a_missing_parent_before_any_search() {
    let t = Tmp::new();
    assert!(QueueWriter::new(&t.0.join("missing/q.jsonl"), HashSet::new()).is_err());
    assert!(!t.q().exists(), "creating the writer must not create the file");
}
