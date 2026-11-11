use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("ks-cli-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn join(&self, s: &str) -> PathBuf { self.0.join(s) }
}
impl Drop for Tmp {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

fn ks(args: &[&dyn AsRef<std::ffi::OsStr>]) -> Output {
    ks_env(args, &[])
}
/// Run `ks` with a Redis environment of the test's choosing: nothing is inherited from the developer's shell.
fn ks_env(args: &[&dyn AsRef<std::ffi::OsStr>], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ks"));
    for a in args { c.arg(a.as_ref()); }
    for k in ["KS_REDIS_URL", "KS_REDIS_PASSWORD", "KS_REDIS_HOST", "KS_REDIS_PORT", "KS_KEY_PREFIX"] { c.env_remove(k); }
    // Default to a dead port so a test that forgets its env can never reach whatever else listens on 6379.
    c.env("KS_REDIS_URL", "redis://127.0.0.1:1");
    for (k, v) in env { c.env(k, v); }
    c.output().unwrap()
}
fn out(o: &Output) -> String { String::from_utf8_lossy(&o.stdout).into_owned() }
fn err(o: &Output) -> String { String::from_utf8_lossy(&o.stderr).into_owned() }

fn kit_card(id: &str, tier: &str, title: &str, tech: &[&str]) -> Value {
    json!({
        "schema": "card-v1", "card_key": format!("https://m.com/{id}"), "tier": tier, "topic": "topic-a", "status": "synthesized",
        "path": format!("sources/{id}.md"), "sha256": "abc", "title": title, "author": "Someone", "source_url": format!("https://m.com/{id}"),
        "published_at": "2019-07-08", "collected_at": "2019-08-09", "access_tier": "unknown", "learning_record": null, "learning_record_state": null,
        "skip_reason": null, "tech_keywords": tech, "problem_keywords": ["가나다", "라마바"],
        "summary_3l": ["첫째 줄 요약입니다 열 자 이상.", "둘째 줄 요약입니다 열 자 이상.", "셋째 줄 요약입니다 열 자 이상."],
        "evidence_type": "tutorial", "limits": "한계 설명 한 줄입니다 열 자 이상.",
    })
}
fn write_jsonl(p: &Path, rows: &[Value]) {
    fs::write(p, rows.iter().map(|r| r.to_string()).collect::<Vec<_>>().join("\n") + "\n").unwrap();
}

const VOCAB: &str = r#"{"version":1,"terms":[
 {"term":"redis","facet":"entity","aliases":["redis-stack"]},
 {"term":"ttl","facet":"technique","aliases":[]},
 {"term":"kafka","facet":"entity","aliases":[]},{"term":"flink","facet":"entity","aliases":[]},{"term":"spark","facet":"entity","aliases":[]}]}"#;

struct Fixture { t: Tmp, kit: PathBuf, vocab: PathBuf, data: PathBuf }
fn fixture() -> Fixture {
    let t = Tmp::new();
    let kit = t.join("kit");
    fs::create_dir_all(&kit).unwrap();
    write_jsonl(&kit.join("topic-a.jsonl"), &[
        kit_card("a", "L1", "Session store notes", &["redis", "ttl", "kafka"]),
        kit_card("b", "L1", "Agent notes", &["redis-stack", "flink", "spark"]),
        kit_card("l0", "L0", "Redis TTL L0 only", &["redis", "ttl", "kafka"]),
    ]);
    let mut other = kit_card("c", "L1", "Grading notes", &["ttl", "kafka", "spark"]);
    other["topic"] = json!("topic-b");
    write_jsonl(&kit.join("topic-b.jsonl"), &[other]);
    write_jsonl(&kit.join("rejects.jsonl"), &[kit_card("rej", "L1", "Rejected redis ttl", &["redis", "ttl", "kafka"])]);
    fs::write(kit.join("merge-report.json"), "{}\n").unwrap();
    let vocab = t.join("terms.json");
    fs::write(&vocab, VOCAB).unwrap();
    let data = t.join("data");
    Fixture { t, kit, vocab, data }
}
fn import(f: &Fixture, extra: &[&str]) -> Output {
    let cards = f.data.join("cards");
    let mut args: Vec<&dyn AsRef<std::ffi::OsStr>> = vec![&"card", &"import", &"--jsonl-dir", &f.kit, &"--vocab", &f.vocab, &"--out", &cards];
    for e in extra { args.push(e); }
    ks(&args)
}
fn md_files(dir: &Path) -> Vec<String> {
    let mut v = vec![];
    for t in fs::read_dir(dir).unwrap() {
        let t = t.unwrap().path();
        for f in fs::read_dir(&t).unwrap() {
            v.push(format!("{}/{}", t.file_name().unwrap().to_string_lossy(), f.unwrap().file_name().to_string_lossy()));
        }
    }
    v.sort();
    v
}

#[test]
fn import_writes_only_l1_cards_into_topic_dirs_and_is_idempotent() {
    let f = fixture();
    let o = import(&f, &[]);
    assert!(o.status.success(), "{}", err(&o));
    let summary: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!((summary["imported"].clone(), summary["skipped_non_l1"].clone(), summary["errors"].clone()), (json!(3), json!(1), json!([])));
    let cards = f.data.join("cards");
    let id = |k: &str| ks_core::card::card_id(k);
    assert_eq!(md_files(&cards), {
        let mut v = vec![format!("topic-a/{}.md", id("https://m.com/a")), format!("topic-a/{}.md", id("https://m.com/b")), format!("topic-b/{}.md", id("https://m.com/c"))];
        v.sort();
        v
    });
    let md = fs::read_to_string(cards.join("topic-a").join(format!("{}.md", id("https://m.com/a")))).unwrap();
    assert!(md.contains("scope: private") && md.contains("card_key: https://m.com/a") && !md.contains("Someone"), "{md}");
    assert!(md.contains("entities:\n- redis") && md.contains("techniques:\n- ttl"), "{md}");
    let before = fs::read_to_string(cards.join("topic-b").join(format!("{}.md", id("https://m.com/c")))).unwrap();
    let again = import(&f, &[]);
    assert!(again.status.success());
    assert_eq!(md_files(&cards).len(), 3);
    assert_eq!(fs::read_to_string(cards.join("topic-b").join(format!("{}.md", id("https://m.com/c")))).unwrap(), before);
    drop(f.t);
}

#[test]
fn import_scope_flag_and_yaml_vocab() {
    let f = fixture();
    let yaml = f.t.join("terms.yaml");
    fs::write(&yaml, "version: 1\nterms:\n  - {term: redis, facet: entity, aliases: [redis-stack]}\n  - {term: ttl, facet: technique}\n").unwrap();
    let cards = f.data.join("cards");
    let o = ks(&[&"card", &"import", &"--jsonl-dir", &f.kit, &"--vocab", &yaml, &"--out", &cards, &"--scope", &"team"]);
    assert!(o.status.success(), "{}", err(&o));
    let md = fs::read_to_string(cards.join("topic-a").join(format!("{}.md", ks_core::card::card_id("https://m.com/b")))).unwrap();
    assert!(md.contains("scope: team") && md.contains("entities:\n- redis"), "{md}");
    let bad = ks(&[&"card", &"import", &"--jsonl-dir", &f.kit, &"--vocab", &yaml, &"--out", &cards, &"--scope", &"public"]);
    assert_eq!(bad.status.code(), Some(2));
}

#[test]
fn import_reports_unrepresentable_cards_and_exits_1_but_keeps_the_good_ones() {
    let f = fixture();
    let mut bad = kit_card("nl", "L1", "Bad", &["redis", "ttl", "kafka"]);
    bad["summary_3l"] = json!(["line one\nline two", "b", "c"]);
    write_jsonl(&f.kit.join("topic-c.jsonl"), &[bad]);
    let o = import(&f, &[]);
    assert_eq!(o.status.code(), Some(1));
    let s: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(s["imported"], json!(3));
    assert_eq!(s["errors"].as_array().unwrap().len(), 1);
    assert!(s["errors"][0].to_string().contains("not representable"));
}

#[test]
fn validate_ok_then_fails_on_a_broken_card() {
    let f = fixture();
    assert!(import(&f, &[]).status.success());
    let o = ks(&[&"card", &"validate", &"--data", &f.data]);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    let s: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!((s["cards"].clone(), s["invalid"].clone(), s["errors"].clone()), (json!(3), json!(0), json!([])));
    let p = f.data.join("cards").join("topic-a").join(format!("{}.md", ks_core::card::card_id("https://m.com/a")));
    let md = fs::read_to_string(&p).unwrap();
    fs::write(&p, md.replace("한계 설명 한 줄입니다 열 자 이상.", "짧음")).unwrap();
    let o = ks(&[&"card", &"validate", &"--data", &f.data]);
    assert_eq!(o.status.code(), Some(1));
    let s: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!((s["cards"].clone(), s["invalid"].clone()), (json!(3), json!(1)));
    assert!(s["errors"][0]["path"].as_str().unwrap().ends_with(&format!("{}.md", ks_core::card::card_id("https://m.com/a"))));
    assert!(s["errors"][0]["errors"][0].as_str().unwrap().starts_with("limits:"));
}

#[test]
fn validate_flags_unparseable_files_and_id_mismatch() {
    let f = fixture();
    assert!(import(&f, &[]).status.success());
    let dir = f.data.join("cards").join("topic-a");
    fs::write(dir.join("junk.md"), "not a card").unwrap();
    let o = ks(&[&"card", &"validate", &"--data", &f.data]);
    assert_eq!(o.status.code(), Some(1));
    assert!(out(&o).contains("junk.md"));
    fs::remove_file(dir.join("junk.md")).unwrap();
    let p = dir.join(format!("{}.md", ks_core::card::card_id("https://m.com/a")));
    let md = fs::read_to_string(&p).unwrap();
    fs::write(&p, md.replace(&format!("id: {}", ks_core::card::card_id("https://m.com/a")), "id: 0000000000000000")).unwrap();
    let o = ks(&[&"card", &"validate", &"--data", &f.data]);
    assert_eq!(o.status.code(), Some(1));
    assert!(out(&o).contains("id:"), "{}", out(&o));
}

fn search(f: &Fixture, qs: Value, extra: &[&str]) -> (Output, Option<Value>) {
    search_with(f, qs, "file", &[], extra)
}

fn search_with(f: &Fixture, qs: Value, backend: &str, env: &[(&str, &str)], extra: &[&str]) -> (Output, Option<Value>) {
    let q = f.t.join("q.json");
    fs::write(&q, qs.to_string()).unwrap();
    let res = f.t.join("res").join(format!("cand-{backend}.json"));
    let mut args: Vec<&dyn AsRef<std::ffi::OsStr>> = vec![&"search", &"--data", &f.data, &"--vocab", &f.vocab, &"--queries", &q, &"--out", &res, &"--backend", &backend];
    for e in extra { args.push(e); }
    let o = ks_env(&args, env);
    let v = fs::read_to_string(&res).ok().map(|s| {
        assert!(s.ends_with("]\n"), "trailing newline like the node collector");
        serde_json::from_str(&s).unwrap()
    });
    (o, v)
}

#[test]
fn search_contract_ranking_and_limit() {
    let f = fixture();
    assert!(import(&f, &[]).status.success());
    let qs = json!([{"qid": "Q1", "query": "x", "terms": ["Redis Stack", "ttl"]}]);
    let (o, v) = search(&f, qs.clone(), &[]);
    assert!(o.status.success(), "{}", err(&o));
    let v = v.unwrap();
    assert_eq!(v[0]["qid"], "Q1");
    assert_eq!(v[0]["scope"], json!({"l1_cards": 3}));
    // a: redis+ttl (2) | b: redis (1, df 2) | c: ttl (1, df 2) -> tie -> card_key order
    let keys: Vec<&str> = v[0]["candidates"].as_array().unwrap().iter().map(|c| c["card_key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["https://m.com/a", "https://m.com/b", "https://m.com/c"]);
    assert_eq!(v[0]["candidates"][0]["path"], "sources/a.md");
    assert_eq!(v[0]["candidates"][0]["title"], "Session store notes");
    assert_eq!(v[0]["candidates"][0]["matched_terms"], json!(["redis", "ttl"]));
    assert_eq!(v[0]["candidates"][0]["rank"], 1);
    let (_, v) = search(&f, qs, &["--limit", "1"]);
    assert_eq!(v.unwrap()[0]["candidates"].as_array().unwrap().len(), 1);
}

#[test]
fn search_errors_exit_2() {
    let f = fixture();
    assert!(import(&f, &[]).status.success());
    let q = json!([{"qid": "Q", "query": "x", "terms": ["ttl"]}]);
    assert_eq!(search(&f, q.clone(), &["--limit", "0"]).0.status.code(), Some(2));
    assert_eq!(search(&f, q, &["--limit", "abc"]).0.status.code(), Some(2));
    let (o, _) = search(&f, json!([{"query": "no qid"}]), &[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("qid"), "{}", err(&o));
    let o = ks(&[&"search", &"--data", &f.data]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).to_lowercase().contains("usage"), "{}", err(&o));
}

#[test]
fn help_lists_redis_but_hides_the_file_backend() {
    for args in [["search", "--help"], ["mcp", "serve"]] {
        let mut a: Vec<&dyn AsRef<std::ffi::OsStr>> = args.iter().map(|s| s as &dyn AsRef<std::ffi::OsStr>).collect();
        if args[0] == "mcp" { a.push(&"--help"); }
        let o = ks(&a);
        assert!(o.status.success(), "{}", err(&o));
        let help = out(&o);
        let line = help.lines().skip_while(|l| !l.contains("--backend")).take(6).collect::<Vec<_>>().join("\n");
        assert!(line.contains("possible values") && line.contains("redis"), "{args:?}: {line}");
        assert!(!line.contains("file"), "{args:?}: {line}");
    }
}

#[test]
fn search_defaults_to_the_redis_backend_and_rejects_unknown_backends() {
    let f = fixture();
    assert!(import(&f, &[]).status.success());
    let q = json!([{"qid": "Q", "query": "x", "terms": ["ttl"]}]);
    let qp = f.t.join("q.json");
    fs::write(&qp, q.to_string()).unwrap();
    let res = f.t.join("res.json");
    // no --backend: Redis is used, so an unreachable Redis is an error and nothing is written
    let o = ks_env(&[&"search", &"--data", &f.data, &"--vocab", &f.vocab, &"--queries", &qp, &"--out", &res], &[("KS_REDIS_URL", "redis://127.0.0.1:1")]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(err(&o).to_lowercase().contains("redis"), "{}", err(&o));
    assert!(!res.exists());
    let o = ks(&[&"search", &"--data", &f.data, &"--vocab", &f.vocab, &"--queries", &qp, &"--out", &res, &"--backend", &"sqlite"]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn record_rejects_bad_verdicts_and_outcomes_before_touching_redis() {
    for extra in [["--verdict", "Z"], ["--verdict", "a"]] {
        let o = ks(&[&"record", &"--task", &"t", &"--card", &"c", &extra[0], &extra[1], &"--reason", &"r"]);
        assert_eq!(o.status.code(), Some(2), "{extra:?}");
    }
    let o = ks(&[&"record", &"--task", &"t", &"--card", &"c", &"--verdict", &"A", &"--reason", &"r", &"--outcome", &"maybe"]);
    assert_eq!(o.status.code(), Some(2));
    let o = ks(&[&"record", &"--task", &"t"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).to_lowercase().contains("usage"), "{}", err(&o));
}

fn test_redis() -> Option<(String, String)> {
    let url = ks_core::testkit::test_redis_url()?;
    Some((url, format!("kstest-cli-{}-{}:", std::process::id(), N.fetch_add(1, Ordering::SeqCst))))
}
struct Purge(String, String);
impl Drop for Purge {
    fn drop(&mut self) {
        if let Ok(mut i) = ks_core::index::Index::connect(&self.0, &self.1) { let _ = i.purge(); }
    }
}

#[test]
fn index_rebuild_then_redis_search_is_byte_identical_to_file_search() {
    let Some((url, prefix)) = test_redis() else { return };
    let _purge = Purge(url.clone(), prefix.clone());
    let env = [("KS_REDIS_URL", url.as_str()), ("KS_KEY_PREFIX", prefix.as_str())];
    let f = fixture();
    assert!(import(&f, &[]).status.success());
    let o = ks_env(&[&"index", &"rebuild", &"--data", &f.data, &"--vocab", &f.vocab], &env);
    assert!(o.status.success(), "{}", err(&o));
    let s: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!((s["cards"].clone(), s["eligible"].clone()), (json!(3), json!(3)), "{s}");
    let again = ks_env(&[&"index", &"rebuild", &"--data", &f.data, &"--vocab", &f.vocab], &env);
    assert_eq!(out(&again), out(&o), "rebuild is idempotent");
    for qs in [json!([{"qid": "Q1", "query": "x", "terms": ["Redis Stack", "ttl"]}]), json!([{"qid": "Q2", "query": "첫째 줄 요약 and Session store", "terms": null}, {"qid": "Q3", "query": "x", "terms": ["가나다", "zzz"]}])] {
        let qs = qs.as_array().unwrap().iter().map(|q| { let mut q = q.clone(); if q["terms"].is_null() { q.as_object_mut().unwrap().remove("terms"); } q }).collect::<Vec<_>>();
        for limit in ["40", "1"] {
            let (fo, _) = search_with(&f, Value::Array(qs.clone()), "file", &env, &["--limit", limit]);
            assert!(fo.status.success(), "{}", err(&fo));
            let (ro, _) = search_with(&f, Value::Array(qs.clone()), "redis", &env, &["--limit", limit]);
            assert!(ro.status.success(), "{}", err(&ro));
            let file = fs::read(f.t.join("res").join("cand-file.json")).unwrap();
            let redis = fs::read(f.t.join("res").join("cand-redis.json")).unwrap();
            assert_eq!(String::from_utf8_lossy(&redis), String::from_utf8_lossy(&file));
            assert!(file.len() > 50);
        }
    }
}

#[test]
fn record_then_dump_writes_append_only_jsonl() {
    let Some((url, prefix)) = test_redis() else { return };
    let _purge = Purge(url.clone(), prefix.clone());
    let env = [("KS_REDIS_URL", url.as_str()), ("KS_KEY_PREFIX", prefix.as_str())];
    let t = Tmp::new();
    let o = ks_env(&[&"record", &"--task", &"T-1", &"--card", &"0123456789abcdef", &"--verdict", &"B", &"--reason", &"수단 차용", &"--outcome", &"partial"], &env);
    assert!(o.status.success(), "{}", err(&o));
    let rec: Value = serde_json::from_str(&out(&o)).unwrap();
    assert!(rec["id"].as_str().unwrap().contains('-'), "{rec}");
    let dir = t.join("dump");
    let o = ks_env(&[&"record", &"dump", &"--out", &dir], &env);
    assert!(o.status.success(), "{}", err(&o));
    let s: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!((s["appended"].clone(), s["total"].clone()), (json!(1), json!(1)));
    let line = fs::read_to_string(dir.join("applications.jsonl")).unwrap();
    let v: Value = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!((v["task"].as_str(), v["card"].as_str(), v["verdict"].as_str(), v["reason"].as_str(), v["outcome"].as_str()), (Some("T-1"), Some("0123456789abcdef"), Some("B"), Some("수단 차용"), Some("partial")));
    let o = ks_env(&[&"record", &"dump", &"--out", &dir], &env);
    assert_eq!(serde_json::from_str::<Value>(&out(&o)).unwrap()["appended"], json!(0));
    assert_eq!(fs::read_to_string(dir.join("applications.jsonl")).unwrap(), line);
}

#[test]
fn redis_search_without_an_index_says_to_rebuild() {
    let Some((url, prefix)) = test_redis() else { return };
    let _purge = Purge(url.clone(), prefix.clone());
    let env = [("KS_REDIS_URL", url.as_str()), ("KS_KEY_PREFIX", prefix.as_str())];
    let f = fixture();
    assert!(import(&f, &[]).status.success());
    let (o, res) = search_with(&f, json!([{"qid": "Q", "query": "x", "terms": ["ttl"]}]), "redis", &env, &[]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(err(&o).contains("ks index rebuild"), "{}", err(&o));
    assert!(res.is_none());
}

#[test]
fn record_checks_card_id_and_text_sizes_before_touching_redis() {
    let long = "x".repeat(2049);
    let max = "가".repeat(682); // 2046 bytes: within the 2048-byte cap
    for (card, task, reason, want) in [
        ("c", "t", "r", "16 hex"),
        ("0123456789ABCDEFG", "t", "r", "16 hex"),
        ("../../../etc/pass", "t", "r", "16 hex"),
        ("0123456789abcdef", long.as_str(), "r", "2048"),
        ("0123456789abcdef", "t", long.as_str(), "2048"),
    ] {
        let o = ks(&[&"record", &"--task", &task, &"--card", &card, &"--verdict", &"A", &"--reason", &reason]);
        assert_eq!(o.status.code(), Some(2));
        assert!(err(&o).contains(want) && !err(&o).to_lowercase().contains("redis"), "{card}: {}", err(&o).chars().take(200).collect::<String>());
    }
    // a valid record gets as far as Redis (a dead port here)
    let o = ks(&[&"record", &"--task", &max, &"--card", &"0123456789abcdef", &"--verdict", &"A", &"--reason", &max]);
    assert!(err(&o).to_lowercase().contains("redis"), "{}", err(&o).chars().take(200).collect::<String>());
}

#[test]
fn index_rebuild_uses_the_current_vocabulary_not_the_terms_stored_in_the_cards() {
    let Some((url, prefix)) = test_redis() else { return };
    let _purge = Purge(url.clone(), prefix.clone());
    let env = [("KS_REDIS_URL", url.as_str()), ("KS_KEY_PREFIX", prefix.as_str())];
    let f = fixture();
    // import with a vocabulary that does not know "kafka": the cards store entities without it
    let old = f.t.join("terms-v1.json");
    fs::write(&old, VOCAB.replace(r#"{"term":"kafka","facet":"entity","aliases":[]},"#, "")).unwrap();
    let cards = f.data.join("cards");
    let o = ks(&[&"card", &"import", &"--jsonl-dir", &f.kit, &"--vocab", &old, &"--out", &cards]);
    assert!(o.status.success(), "{}", err(&o));
    // the vocabulary then grows (it is the file `ks mcp serve` and `ks index rebuild` find under <data>/vocab)
    fs::create_dir_all(f.data.join("vocab")).unwrap();
    fs::copy(&f.vocab, f.data.join("vocab").join("terms.json")).unwrap();
    let o = ks_env(&[&"index", &"rebuild", &"--data", &f.data], &env);
    assert!(o.status.success(), "{}", err(&o));
    let qs = json!([{"qid": "Q1", "query": "x", "terms": ["kafka"]}, {"qid": "Q2", "query": "x", "terms": ["kafka", "ttl"]}]);
    let (fo, file) = search_with(&f, qs.clone(), "file", &env, &[]);
    assert!(fo.status.success(), "{}", err(&fo));
    let (ro, _) = search_with(&f, qs, "redis", &env, &[]);
    assert!(ro.status.success(), "{}", err(&ro));
    let file_bytes = fs::read(f.t.join("res").join("cand-file.json")).unwrap();
    let redis_bytes = fs::read(f.t.join("res").join("cand-redis.json")).unwrap();
    assert_eq!(String::from_utf8_lossy(&redis_bytes), String::from_utf8_lossy(&file_bytes));
    assert_eq!(file.unwrap()[0]["candidates"].as_array().unwrap().len(), 2, "cards a and c carry kafka in their keywords");
}

#[test]
fn search_reports_the_query_terms_the_vocabulary_does_not_know() {
    let f = fixture();
    assert!(import(&f, &[]).status.success());
    let qs = json!([
        {"qid": "K", "query": "x", "terms": ["Redis Stack", "ttl"]},
        {"qid": "U", "query": "x", "terms": ["ttl", "Zzz Term", "nothing-here", "zzz term"]},
        {"qid": "W", "query": "session TTLs and frobnicate"},
    ]);
    let (o, v) = search(&f, qs, &[]);
    assert!(o.status.success(), "{}", err(&o));
    let v = v.unwrap();
    assert_eq!(v[0]["unknown_terms"], json!([]), "every term resolves: the field is still there");
    assert_eq!(v[1]["unknown_terms"], json!(["zzz term", "nothing-here"]), "lowercased, deduped, in query order; ttl resolves");
    assert_eq!(v[1]["candidates"].as_array().unwrap().len(), 2, "a known term still finds its cards next to unknown ones");
    assert_eq!(v[2]["unknown_terms"], json!(["session", "and", "frobnicate"]), "no terms: the words of the query text; ttls resolves by plural folding");
    // the earlier fields keep their names and order: unknown_terms is appended after candidates
    let raw = fs::read_to_string(f.t.join("res").join("cand-file.json")).unwrap();
    let at = |k: &str| raw.find(&format!("\"{k}\": ")).unwrap();
    assert!(at("qid") < at("scope") && at("scope") < at("candidates") && at("candidates") < at("unknown_terms"), "{raw}");
}

#[test]
fn redis_search_refuses_a_stale_index_until_it_is_rebuilt() {
    let Some((url, prefix)) = test_redis() else { return };
    let _purge = Purge(url.clone(), prefix.clone());
    let env = [("KS_REDIS_URL", url.as_str()), ("KS_KEY_PREFIX", prefix.as_str())];
    let f = fixture();
    assert!(import(&f, &[]).status.success());
    let rebuild = || ks_env(&[&"index", &"rebuild", &"--data", &f.data, &"--vocab", &f.vocab], &env);
    let qs = json!([{"qid": "Q", "query": "x", "terms": ["ttl", "zzz"]}]);
    let refused = |what: &str| {
        let _ = fs::remove_file(f.t.join("res").join("cand-redis.json"));
        let (o, res) = search_with(&f, qs.clone(), "redis", &env, &[]);
        let e = err(&o);
        assert_eq!(o.status.code(), Some(2), "{what}: {e}");
        assert!(res.is_none(), "{what}: nothing is written for a refused search");
        assert!(e.contains(&format!("ks index rebuild --data {} --vocab {}", f.data.display(), f.vocab.display())), "{what}: {e}");
        e
    };
    let works = |what: &str| {
        let (o, res) = search_with(&f, qs.clone(), "redis", &env, &[]);
        assert!(o.status.success(), "{what}: {}", err(&o));
        assert_eq!(res.unwrap()[0]["unknown_terms"], json!(["zzz"]));
    };

    assert!(rebuild().status.success());
    works("right after the rebuild");

    // the vocabulary file changes
    fs::write(&f.vocab, VOCAB.replace("redis-stack", "redis-db")).unwrap();
    let e = refused("vocab changed");
    assert!(e.contains("vocab file") && !e.contains("card files"), "{e}");
    assert!(rebuild().status.success());
    works("after rebuilding for the new vocabulary");

    // a card is added
    let topic = f.data.join("cards").join("topic-a");
    fs::write(topic.join("added.md"), ks_core::card::Card::new("https://m.com/d").render()).unwrap();
    let e = refused("card added");
    assert!(e.contains("card files") && !e.contains("vocab file"), "{e}");
    assert!(rebuild().status.success());
    works("after rebuilding for the added card");

    // a card is modified
    let p = topic.join(format!("{}.md", ks_core::card::card_id("https://m.com/a")));
    fs::write(&p, format!("{}\n", fs::read_to_string(&p).unwrap())).unwrap();
    assert!(refused("card modified").contains("card files"));
    assert!(rebuild().status.success());
    works("after rebuilding for the modified card");

    // an index built without fingerprints (older ks) is refused as well
    let (cards, vocab) = (ks_core::collect::load_cards(&f.data.join("cards")).unwrap(), ks_core::vocab::Vocab::load(&f.vocab).unwrap());
    ks_core::index::Index::connect(&url, &prefix).unwrap().rebuild(&cards, &vocab, None).unwrap();
    assert!(refused("no fingerprints").contains("without fingerprints"));
    assert!(rebuild().status.success());
    works("after a rebuild with fingerprints");

    // the file backend is a cross-check and does not look at the index
    let (o, _) = search_with(&f, qs.clone(), "file", &env, &[]);
    assert!(o.status.success(), "{}", err(&o));
}

#[test]
fn validate_flags_a_card_whose_file_name_is_not_its_id_or_whose_directory_is_not_its_topic() {
    let f = fixture();
    assert!(import(&f, &[]).status.success());
    let cards = f.data.join("cards");
    let id = ks_core::card::card_id("https://m.com/a");
    let placed = cards.join("topic-a").join(format!("{id}.md"));
    let validate = || {
        let o = ks(&[&"card", &"validate", &"--data", &f.data]);
        (o.status.code(), serde_json::from_str::<Value>(&out(&o)).unwrap())
    };
    assert_eq!(validate().0, Some(0));

    let renamed = cards.join("topic-a").join("renamed.md");
    fs::rename(&placed, &renamed).unwrap();
    let (code, s) = validate();
    assert_eq!((code, s["invalid"].clone()), (Some(1), json!(1)), "{s}");
    assert!(s["errors"][0]["path"].as_str().unwrap().ends_with("renamed.md"), "{s}");
    let e = s["errors"][0]["errors"].to_string();
    assert!(e.contains("path:") && e.contains(&format!("{id}.md")), "{e}");

    fs::create_dir_all(cards.join("topic-b")).unwrap();
    let moved = cards.join("topic-b").join(format!("{id}.md"));
    fs::rename(&renamed, &moved).unwrap();
    let (code, s) = validate();
    assert_eq!((code, s["invalid"].clone()), (Some(1), json!(1)), "{s}");
    let e = s["errors"][0]["errors"].to_string();
    assert!(e.contains("path:") && e.contains("topic-b") && e.contains("topic-a"), "{e}");

    let top = cards.join(format!("{id}.md"));
    fs::rename(&moved, &top).unwrap();
    let (code, s) = validate();
    assert_eq!(code, Some(1), "a card directly under cards/ is in no topic directory: {s}");

    fs::rename(&top, &placed).unwrap();
    assert_eq!(validate().0, Some(0));
}

#[test]
fn card_id_prints_the_card_key_and_id_that_import_gives_the_same_source() {
    let f = fixture();
    let mut local = kit_card("local", "L1", "Local notes", &["redis", "ttl", "kafka"]);
    local["card_key"] = json!("path:notes/local-001.md");
    local["source_url"] = Value::Null;
    write_jsonl(&f.kit.join("topic-c.jsonl"), &[local]);
    assert!(import(&f, &[]).status.success());
    let files = md_files(&f.data.join("cards"));
    for (source, key) in [
        ("https://m.com/a", "https://m.com/a"),
        (" HTTPS://M.com/a/?utm_source=x#top ", "https://m.com/a"),
        ("https://m.com/b", "https://m.com/b"),
        ("notes/local-001.md", "path:notes/local-001.md"),
    ] {
        let o = ks(&[&"card", &"id", &"--source", &source]);
        assert!(o.status.success(), "{source}: {}", err(&o));
        let v: Value = serde_json::from_str(&out(&o)).unwrap_or_else(|e| panic!("{source}: not JSON ({e}): {}", out(&o)));
        assert_eq!(v.as_object().unwrap().len(), 2, "{v}");
        assert_eq!(v["card_key"], key, "{source}");
        let id = v["id"].as_str().unwrap();
        assert!(id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit()), "{v}");
        assert!(files.iter().any(|p| p.ends_with(&format!("/{id}.md"))), "{source}: import wrote no card with id {id}: {files:?}");
    }
    for bad in ["", "   "] {
        let o = ks(&[&"card", &"id", &"--source", &bad]);
        assert_eq!(o.status.code(), Some(2), "{bad:?}: {}", out(&o));
        assert!(out(&o).is_empty(), "nothing on stdout for a refused source");
    }
}

#[test]
fn validate_requires_exactly_topic_slash_id_under_cards() {
    let f = fixture();
    assert!(import(&f, &[]).status.success());
    let cards = f.data.join("cards");
    let id = ks_core::card::card_id("https://m.com/a");
    let placed = cards.join("topic-a").join(format!("{id}.md"));
    let validate = || {
        let o = ks(&[&"card", &"validate", &"--data", &f.data]);
        (o.status.code(), serde_json::from_str::<Value>(&out(&o)).unwrap())
    };
    assert_eq!(validate().0, Some(0));

    // deeper than <topic>/<id>.md, although the directory right above is named after the topic
    let deep = cards.join("extra").join("topic-a").join(format!("{id}.md"));
    fs::create_dir_all(deep.parent().unwrap()).unwrap();
    fs::rename(&placed, &deep).unwrap();
    let (code, s) = validate();
    assert_eq!((code, s["invalid"].clone()), (Some(1), json!(1)), "a card nested below its topic directory: {s}");
    let e = s["errors"][0]["errors"].to_string();
    assert!(e.contains("path:") && e.contains(&format!("topic-a/{id}.md")), "{e}");
    fs::rename(&deep, &placed).unwrap();
    fs::remove_dir_all(cards.join("extra")).unwrap();
    assert_eq!(validate().0, Some(0));

    // a card whose topic is "cards", put directly under cards/: the directory above is named "cards", but it is the root
    let md = fs::read_to_string(&placed).unwrap();
    assert!(md.contains("\ntopic: topic-a\n"), "{md}");
    let top = cards.join(format!("{id}.md"));
    fs::write(&top, md.replace("\ntopic: topic-a\n", "\ntopic: cards\n")).unwrap();
    fs::remove_file(&placed).unwrap();
    let (code, s) = validate();
    assert_eq!((code, s["invalid"].clone()), (Some(1), json!(1)), "a card directly under cards/ is in no topic directory: {s}");
    let e = s["errors"][0]["errors"].to_string();
    assert!(e.contains("path:") && e.contains(&format!("cards/{id}.md")), "{e}");
    // the same card in cards/cards/ is where it belongs
    fs::create_dir_all(cards.join("cards")).unwrap();
    fs::rename(&top, cards.join("cards").join(format!("{id}.md"))).unwrap();
    assert_eq!(validate().0, Some(0));
}

#[test]
fn card_id_trims_surrounding_whitespace_of_a_source_that_is_not_a_url() {
    for (source, key) in [("  notes/local-001.md\t", "path:notes/local-001.md"), ("\n my name \u{a0}", "path:my name")] {
        let o = ks(&[&"card", &"id", &"--source", &source]);
        assert!(o.status.success(), "{source:?}: {}", err(&o));
        let v: Value = serde_json::from_str(&out(&o)).unwrap();
        assert_eq!(v["card_key"], key, "{source:?}");
        assert_eq!(v["id"], ks_core::card::card_id(key), "{source:?}");
    }
}

#[test]
fn ks_version_prints_name_and_version() {
    for flag in ["--version", "-V"] {
        let o = ks(&[&flag]);
        assert!(o.status.success(), "{flag}: {}", err(&o));
        assert_eq!(out(&o), format!("ks {}\n", env!("CARGO_PKG_VERSION")));
    }
}
