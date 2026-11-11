//! Redis index tests. Pure tests always run; Redis tests run only when KS_TEST_REDIS_URL is set and use a
//! unique key prefix that is purged afterwards.
use ks_core::card::{Card, Element};
use ks_core::collect::{collect, load_cards, query_terms, Query, QueryTerms};
use ks_core::index::{candidate_query, card_from_json, card_to_json, cards_fingerprint, escape_tag, Application, Fingerprints, Index};
use ks_core::vocab::Vocab;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

fn base(id: &str) -> Card {
    let mut c = Card::new(&format!("k-{id}"));
    c.title = Some(format!("T {id}"));
    c.source_path = Some(format!("p/{id}.md"));
    c.tech_keywords = vec!["kafka".into(), "flink".into(), "spark".into()];
    c.problems = vec!["가나다".into(), "라마바".into()];
    c.summary = vec!["첫째 줄 요약입니다 열 자 이상.".into(), "둘째 줄 요약입니다 열 자 이상.".into(), "셋째 줄 요약입니다 열 자 이상.".into()];
    c.limits = "한계 설명 한 줄입니다 열 자 이상.".into();
    c
}
fn kw(mut c: Card, t: &[&str]) -> Card { c.tech_keywords = t.iter().map(|s| s.to_string()).collect(); c }
fn title(mut c: Card, t: &str) -> Card { c.title = Some(t.into()); c }
const VOCAB_JSON: &str = r#"{"version":1,"terms":[
      {"term":"redis","facet":"entity","aliases":["redis-stack"]},
      {"term":"ttl","facet":"technique","aliases":[]},
      {"term":"agent-memory","facet":"domain","aliases":[]},
      {"term":"llm-as-judge","facet":"technique","aliases":["llm-judge"]},
      {"term":"a-b-testing","facet":"technique","aliases":[]},
      {"term":"langchain","facet":"entity","aliases":["langchainjs"]},
      {"term":"kafka","facet":"entity","aliases":[]},{"term":"flink","facet":"entity","aliases":[]},{"term":"spark","facet":"entity","aliases":[]}]}"#;
fn vocab() -> Vocab {
    Vocab::from_json(VOCAB_JSON, "t").unwrap()
}
/// Cards as `ks card import` would store them: facet terms classified with the vocabulary.
fn cards() -> Vec<Card> {
    let v = vocab();
    let mut d = title(base("d"), "Expiry notes");
    d.reusable_elements = vec![Element { term: "TTL".into(), kind: "method".into(), desc: "만료 시간을 갱신한다 열 자 이상".into(), anchor: "anchor text".into(), requires: None }];
    let mut e = title(base("e"), "Streaming notes");
    e.limits = "Needs a short TTL to be useful. The token rate-limit policy: rate limit applies.".into();
    let mut skipped = kw(title(base("s"), "Skipped redis ttl card"), &["redis", "ttl"]);
    skipped.skip_reason = Some("paywalled".into());
    let mut stop = title(base("h"), "On the way");
    stop.summary[0] = "The quick brown fox jumps over the lazy dog today.".into();
    stop.limits = "Supports C++ and Go; needs a-b-testing infra.".into();
    stop.tech_keywords = vec!["a-b-testing".into(), "ttl".into(), "kafka".into()];
    let all = vec![
        kw(title(base("a"), "Session store notes"), &["redis", "ttl", "langchain"]),
        kw(title(base("b"), "Agent notes"), &["redis-stack", "agent-memory", "kafka"]),
        kw(title(base("c"), "Grading notes"), &["llm-judge", "ttl", "kafka"]),
        d,
        e,
        title(base("f"), "Redis Stack in practice"),
        kw(title(base("g"), "TTL tips"), &["llm-judge", "ttl", "langchain"]),
        stop,
        skipped,
    ];
    all.into_iter()
        .map(|mut c| {
            let mut t = c.tech_keywords.clone();
            t.extend(c.reusable_elements.iter().map(|e| e.term.clone()));
            let got = v.classify_terms(&t);
            (c.entities, c.techniques, c.domains, c.new_terms) = (got.entities, got.techniques, got.domains, got.new_terms);
            c
        })
        .collect()
}
fn q(qid: &str, query: &str, terms: Option<&[&str]>) -> Query {
    Query { qid: qid.into(), query: query.into(), terms: terms.map(|t| t.iter().map(|s| s.to_string()).collect()) }
}
fn queries() -> Vec<Query> {
    vec![
        q("Q1", "x", Some(&["Redis Stack", "ttl"])),
        q("Q2", "a LangChainJS tips x", None),
        q("Q3", "x", Some(&["rate-limit"])),
        q("Q4", "x", Some(&["imit"])),
        q("Q5", "x", Some(&["the"])),
        q("Q6", "x", Some(&["가나다"])),
        q("Q7", "x", Some(&["zzzz"])),
        q("Q8", "x", Some(&["x"])),
        q("Q9", "x", Some(&["rate limit"])),
        q("Q10", "x", Some(&["ttl", "kafka", "c++"])),
        q("Q11", "x", Some(&["Redis", "ttl"])),
        q("Q12", "x", Some(&["a-b-testing", "agent-memory"])),
        q("Q13", "x", Some(&["만료 시간"])),
        q("Q14", "x", Some(&["QUICK Brown"])),
    ]
}

// ---------- pure tests ----------

#[test]
fn card_json_round_trips_summary_limits_and_elements() {
    for c in cards() {
        let back = card_from_json(&card_to_json(&c)).unwrap();
        assert_eq!(back, c);
    }
}

#[test]
fn escape_tag_escapes_punctuation_and_space_but_not_letters() {
    assert_eq!(escape_tag("a-b.c d"), r"a\-b\.c\ d");
    assert_eq!(escape_tag("c++"), r"c\+\+");
    assert_eq!(escape_tag("한국어_x1"), "한국어_x1");
}

fn terms(text: &[&str], canon: &[&str]) -> QueryTerms {
    QueryTerms { text_terms: text.iter().map(|s| s.to_string()).collect(), canon: canon.iter().map(|s| s.to_string()).collect() }
}

#[test]
fn candidate_query_is_a_superset_filter_over_tags_and_text_pieces() {
    assert_eq!(
        candidate_query(&terms(&["rate-limit"], &["redis", "a-b"])).unwrap(),
        r"@eligible:{1} (@terms:{redis|a\-b} | @title|summary|problems|limits|elements:(*rate* *limit*))"
    );
    assert_eq!(candidate_query(&terms(&[], &["redis"])).unwrap(), "@eligible:{1} (@terms:{redis})");
    assert_eq!(candidate_query(&terms(&["한국어"], &[])).unwrap(), "@eligible:{1} (@title|summary|problems|limits|elements:(*한국어*))");
    // a text term with no usable piece (one character) can match anywhere: fetch every eligible card
    assert_eq!(candidate_query(&terms(&["x", "redis"], &["redis"])).unwrap(), "@eligible:{1}");
    assert_eq!(candidate_query(&terms(&["++"], &[])).unwrap(), "@eligible:{1}");
    assert!(candidate_query(&terms(&[], &[])).is_none());
}

#[test]
fn query_terms_match_what_collect_uses() {
    let v = vocab();
    let t = query_terms(&q("Q", "How to Use Redis Stack", None), &v);
    assert_eq!(t.text_terms, ["how", "to", "use", "redis", "stack"]);
    assert_eq!(t.canon, ["redis"]);
}

// ---------- redis tests ----------

static N: AtomicUsize = AtomicUsize::new(0);

// Redis tests in this binary run one at a time: one of them misconfigures the server for a moment.
static REDIS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Env { url: String, prefix: String, tmp: PathBuf, _serial: std::sync::MutexGuard<'static, ()> }
impl Env {
    fn new() -> Option<Env> {
        let url = ks_core::testkit::test_redis_url()?;
        let n = N.fetch_add(1, Ordering::SeqCst);
        let tmp = std::env::temp_dir().join(format!("ks-index-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let _serial = REDIS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        Some(Env { url, prefix: format!("kstest-{}-{n}:", std::process::id()), tmp, _serial })
    }
    fn index(&self) -> Index { Index::connect(&self.url, &self.prefix).expect("connect to test redis") }
    fn raw(&self) -> redis::Connection { redis::Client::open(self.url.as_str()).unwrap().get_connection().unwrap() }
}
impl Drop for Env {
    fn drop(&mut self) {
        if let Ok(mut i) = Index::connect(&self.url, &self.prefix) {
            let _ = i.purge();
        }
        let _ = fs::remove_dir_all(&self.tmp);
    }
}

fn key_count(con: &mut redis::Connection, pattern: &str) -> usize {
    let keys: Vec<String> = redis::cmd("KEYS").arg(pattern).query(con).unwrap();
    keys.len()
}

#[test]
fn rebuild_loads_cards_and_meta() {
    let Some(env) = Env::new() else { return };
    let mut idx = env.index();
    let all = cards();
    let rep = idx.rebuild(&all, &vocab(), Some(&env.tmp)).unwrap();
    assert_eq!((rep.cards, rep.eligible, rep.git_sha.clone()), (9, 8, None));
    let meta = idx.meta().unwrap();
    assert_eq!(meta.get("schema_version").map(String::as_str), Some("1"));
    assert_eq!(meta.get("card_count").map(String::as_str), Some("9"));
    assert_eq!(meta.get("eligible_count").map(String::as_str), Some("8"));
    assert!(!meta.contains_key("git_sha"));
    let mut con = env.raw();
    assert_eq!(key_count(&mut con, &format!("{}card:*", env.prefix)), 9);
    let id = &all[0].id;
    let doc: String = redis::cmd("JSON.GET").arg(format!("{}card:{id}", env.prefix)).arg("$.terms").arg("$.eligible").query(&mut con).unwrap();
    assert!(doc.contains("redis") && doc.contains("ttl") && doc.contains("langchain"), "{doc}");
}

#[test]
fn rebuild_is_idempotent_and_drops_stale_cards() {
    let Some(env) = Env::new() else { return };
    let mut idx = env.index();
    let all = cards();
    idx.rebuild(&all, &vocab(), None).unwrap();
    let mut first = idx.meta().unwrap();
    idx.rebuild(&all, &vocab(), None).unwrap();
    let mut second = idx.meta().unwrap();
    // every rebuild gets a new build_id (so a search can tell it was rebuilt meanwhile); everything else is the same
    let (a, b) = (first.remove("build_id").unwrap(), second.remove("build_id").unwrap());
    assert_ne!(a, b);
    assert_eq!(second, first);
    let mut con = env.raw();
    assert_eq!(key_count(&mut con, &format!("{}card:*", env.prefix)), 9);
    // fewer cards: the removed ones disappear from keys and from the index
    idx.rebuild(&all[..2], &vocab(), None).unwrap();
    assert_eq!(key_count(&mut con, &format!("{}card:*", env.prefix)), 2);
    assert_eq!(idx.meta().unwrap().get("card_count").map(String::as_str), Some("2"));
    let res = idx.search(&vocab(), &[q("Q", "x", Some(&["ttl"]))], 40).unwrap();
    assert_eq!(res[0].l1_cards, 2);
    assert_eq!(res[0].found.len(), 1);
}

#[test]
fn rebuild_records_git_sha_only_when_the_data_dir_is_a_repo_root() {
    let Some(env) = Env::new() else { return };
    let git = |args: &[&str]| {
        let o = std::process::Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).args(args).current_dir(&env.tmp).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    git(&["init", "-q"]);
    git(&["commit", "-q", "--allow-empty", "-m", "x"]);
    let sha = git(&["rev-parse", "HEAD"]);
    let mut idx = env.index();
    let rep = idx.rebuild(&cards()[..1], &vocab(), Some(&env.tmp)).unwrap();
    assert_eq!(rep.git_sha.as_deref(), Some(sha.as_str()));
    assert_eq!(idx.meta().unwrap().get("git_sha"), Some(&sha));
    // a subdirectory of a repo is not the repo root: no sha is claimed for it
    let sub = env.tmp.join("sub");
    fs::create_dir_all(&sub).unwrap();
    assert_eq!(idx.rebuild(&cards()[..1], &vocab(), Some(&sub)).unwrap().git_sha, None);
}

#[test]
fn redis_search_is_identical_to_file_collect() {
    let Some(env) = Env::new() else { return };
    let mut idx = env.index();
    let all = cards();
    idx.rebuild(&all, &vocab(), None).unwrap();
    let v = vocab();
    for limit in [40usize, 2] {
        let want = collect(&all, &v, &queries(), limit);
        let got: Vec<_> = idx.search(&v, &queries(), limit).unwrap().into_iter().map(|r| r.into_result()).collect();
        for (w, g) in want.iter().zip(&got) {
            assert_eq!(serde_json::to_string(g).unwrap(), serde_json::to_string(w).unwrap(), "query {} limit {limit}", w.qid);
        }
        assert_eq!(want.len(), got.len());
    }
    // the queries above must actually exercise the paths: every kind of hit shows up somewhere
    let want = collect(&all, &v, &queries(), 40);
    let by = |id: &str| want.iter().find(|r| r.qid == id).unwrap();
    assert!(by("Q3").candidates.iter().any(|c| c.card_key == "k-e"), "punctuated substring");
    assert!(by("Q4").candidates.iter().any(|c| c.card_key == "k-e"), "infix inside a token");
    assert!(by("Q5").candidates.iter().any(|c| c.card_key == "k-h"), "stopword text");
    assert!(by("Q9").candidates.iter().any(|c| c.card_key == "k-e"), "substring across tokens");
    assert!(by("Q10").candidates.iter().any(|c| c.card_key == "k-h"), "no usable piece falls back to every eligible card");
    assert!(by("Q7").candidates.is_empty());
    assert!(by("Q1").candidates.iter().all(|c| c.card_key != "k-s"), "skipped cards are never candidates");
    assert!(by("Q13").candidates.iter().any(|c| c.card_key == "k-d"), "multi-word Korean phrase");
}

#[test]
fn record_validates_and_dump_appends_without_touching_existing_lines() {
    let Some(env) = Env::new() else { return };
    let mut idx = env.index();
    let app = |task: &str, verdict: &str, outcome: Option<&str>| Application {
        task: task.into(), card: "0123456789abcdef".into(), verdict: verdict.into(), reason: "same problem, 한글 이유".into(), outcome: outcome.map(String::from),
    };
    for bad in [app("", "A", None), app("t", "Z", None), app("t", "A", Some("maybe"))] {
        assert!(idx.record(&bad).is_err(), "{bad:?}");
    }
    let mut empty_reason = app("t", "A", None);
    empty_reason.reason = " ".into();
    assert!(idx.record(&empty_reason).is_err());

    let out = env.tmp.join("dump");
    idx.record(&app("t1", "A", Some("success"))).unwrap();
    idx.record(&app("t2", "excluded", None)).unwrap();
    let d = idx.dump_applications(&out).unwrap();
    assert_eq!((d.appended, d.total), (2, 2));
    let file = out.join("applications.jsonl");
    let first = fs::read_to_string(&file).unwrap();
    let lines: Vec<serde_json::Value> = first.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!((lines[0]["task"].as_str(), lines[0]["verdict"].as_str(), lines[0]["outcome"].as_str()), (Some("t1"), Some("A"), Some("success")));
    assert_eq!((lines[1]["task"].as_str(), lines[1].get("outcome")), (Some("t2"), None));
    assert!(lines[0]["id"].as_str().unwrap().contains('-'), "stream id");

    // dumping again adds nothing; a new record is appended after the untouched old bytes
    assert_eq!(idx.dump_applications(&out).unwrap().appended, 0);
    assert_eq!(fs::read_to_string(&file).unwrap(), first);
    idx.record(&app("t3", "C", Some("failed"))).unwrap();
    let d = idx.dump_applications(&out).unwrap();
    assert_eq!((d.appended, d.total), (1, 3));
    let second = fs::read_to_string(&file).unwrap();
    assert!(second.starts_with(&first) && second.lines().count() == 3, "{second}");

    // a missing trailing newline must not glue the next record onto the last line
    fs::write(&file, first.trim_end_matches('\n')).unwrap();
    idx.record(&app("t4", "B", None)).unwrap();
    assert_eq!(idx.dump_applications(&out).unwrap().appended, 2);
    let third = fs::read_to_string(&file).unwrap();
    assert!(third.lines().all(|l| serde_json::from_str::<serde_json::Value>(l).is_ok()) && third.lines().count() == 4, "{third}");

    // a corrupt existing line is an error and the file stays as it was
    fs::write(&file, "not json\n").unwrap();
    assert!(idx.dump_applications(&out).is_err());
    assert_eq!(fs::read_to_string(&file).unwrap(), "not json\n");
}

fn config_get(con: &mut redis::Connection, name: &str) -> String {
    let v: Vec<String> = redis::cmd("CONFIG").arg("GET").arg(name).query(con).unwrap();
    v.get(1).cloned().unwrap_or_default()
}

#[test]
fn search_refuses_unsafe_query_engine_settings_and_never_changes_them() {
    let Some(env) = Env::new() else { return };
    let mut con = env.raw();
    // put the server setting back whatever happens
    struct Restore(redis::Connection, String);
    impl Drop for Restore {
        fn drop(&mut self) { let _ = redis::cmd("CONFIG").arg("SET").arg("search-timeout").arg(&self.1).query::<()>(&mut self.0); }
    }
    let _restore = Restore(env.raw(), config_get(&mut con, "search-timeout"));
    let mut idx = env.index();
    idx.rebuild(&cards(), &vocab(), None).unwrap();
    // TIMEOUT 0 means "no timeout": a runaway wildcard query could hang the shared server
    redis::cmd("CONFIG").arg("SET").arg("search-timeout").arg("0").query::<()>(&mut con).unwrap();
    let e = env.index().search(&vocab(), &[q("Q", "x", Some(&["ttl"]))], 40).unwrap_err().to_string();
    assert!(e.contains("search-timeout") && e.contains("restart"), "{e}");
    assert_eq!(config_get(&mut con, "search-timeout"), "0", "the client changed a server setting at runtime");
    // the configured finite timeout is accepted
    redis::cmd("CONFIG").arg("SET").arg("search-timeout").arg("10000").query::<()>(&mut con).unwrap();
    assert!(env.index().search(&vocab(), &[q("Q", "x", Some(&["ttl"]))], 40).is_ok());
}

#[test]
fn record_trims_the_application_stream_to_about_100000_entries() {
    let Some(env) = Env::new() else { return };
    let mut con = env.raw();
    let key = format!("{}apply", env.prefix);
    for chunk in 0..101 {
        let mut pipe = redis::pipe();
        for _ in 0..1000 {
            pipe.cmd("XADD").arg(&key).arg("*").arg("f").arg("x").ignore();
        }
        pipe.query::<()>(&mut con).unwrap();
        let _ = chunk;
    }
    let before: usize = redis::cmd("XLEN").arg(&key).query(&mut con).unwrap();
    assert_eq!(before, 101_000);
    env.index().record(&Application { task: "t".into(), card: "0123456789abcdef".into(), verdict: "A".into(), reason: "r".into(), outcome: None }).unwrap();
    let after: usize = redis::cmd("XLEN").arg(&key).query(&mut con).unwrap();
    assert!((100_000..101_000).contains(&after), "XLEN {after}: the stream was not trimmed (MAXLEN ~ 100000)");
}

// ---------- vocabulary drift: stored terms vs the current vocabulary ----------

/// A card whose stored entities/techniques/domains are what an OLDER vocabulary produced: `stored` is written
/// verbatim, `kw_terms` is the raw tech_keywords the current vocabulary classifies at search time.
fn drifted(id: &str, kw_terms: &[&str], stored: &[&str]) -> Card {
    let mut c = kw(title(base(id), &format!("Notes {id}")), kw_terms);
    c.entities = stored.iter().map(|s| s.to_string()).collect();
    c
}

fn assert_redis_equals_file(idx: &mut Index, all: &[Card], qs: &[Query]) -> Vec<ks_core::collect::QueryResult> {
    let v = vocab();
    let want = collect(all, &v, qs, 40);
    let got: Vec<_> = idx.search(&v, qs, 40).unwrap().into_iter().map(|r| r.into_result()).collect();
    assert_eq!(want.len(), got.len());
    for (w, g) in want.iter().zip(&got) {
        assert_eq!(serde_json::to_string(g).unwrap(), serde_json::to_string(w).unwrap(), "query {}", w.qid);
    }
    want
}

#[test]
fn redis_candidates_follow_the_current_vocabulary_not_the_stored_terms() {
    let Some(env) = Env::new() else { return };
    let mut idx = env.index();
    // imported while the vocabulary had no "kafka"/"flink": nothing stored; the vocabulary now has both
    let mut el = drifted("m3", &["unrelated"], &[]);
    el.reusable_elements = vec![Element { term: "TTL".into(), kind: "method".into(), desc: "만료 시간을 갱신한다 열 자 이상".into(), anchor: "anchor text".into(), requires: None }];
    let all = vec![drifted("m1", &["kafka"], &[]), drifted("m2", &["flink", "spark"], &[]), el];
    let qs = vec![q("Q1", "x", Some(&["kafka"])), q("Q2", "x", Some(&["flink", "kafka"])), q("Q3", "x", Some(&["ttl"]))];
    idx.rebuild(&all, &vocab(), None).unwrap();
    let want = assert_redis_equals_file(&mut idx, &all, &qs);
    let keys = |qid: &str| want.iter().find(|r| r.qid == qid).unwrap().candidates.iter().map(|c| c.card_key.clone()).collect::<Vec<_>>();
    assert_eq!(keys("Q1"), ["k-m1"], "file backend reclassifies tech_keywords with the current vocabulary");
    assert_eq!(keys("Q2"), ["k-m1", "k-m2"]);
    assert_eq!(keys("Q3"), ["k-m3"], "reusable_elements[].term counts too");
}

#[test]
fn redis_document_frequency_follows_the_current_vocabulary_not_the_stored_terms() {
    let Some(env) = Env::new() else { return };
    let mut idx = env.index();
    // k-x1 STORES kafka but its keywords no longer resolve to it: the file backend does not count it as kafka.
    // Counting stored terms would make kafka as common as flink and put the tie-break on card_key instead.
    let all = vec![
        drifted("x1", &["unrelated"], &["kafka"]),
        drifted("y2", &["kafka"], &["kafka"]),
        drifted("x3", &["flink"], &["flink"]),
        drifted("x4", &["flink"], &["flink"]),
    ];
    let qs = vec![q("Q", "x", Some(&["kafka", "flink"]))];
    idx.rebuild(&all, &vocab(), None).unwrap();
    let want = assert_redis_equals_file(&mut idx, &all, &qs);
    let keys: Vec<_> = want[0].candidates.iter().map(|c| c.card_key.as_str()).collect();
    assert_eq!(keys, ["k-y2", "k-x3", "k-x4"], "rarer kafka (df 1) ranks before flink (df 2)");
}

// ---------- freshness: the index against the vocabulary file and the card files it was built from ----------

const HINT: &str = "ks index rebuild --data DATA --vocab VOCAB";

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Scratch {
        let p = std::env::temp_dir().join(format!("ks-fp-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Scratch(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

fn write_card(cards_dir: &std::path::Path, c: &Card) -> PathBuf {
    fs::create_dir_all(cards_dir.join("t")).unwrap();
    let p = cards_dir.join("t").join(format!("{}.md", c.id));
    fs::write(&p, c.render()).unwrap();
    p
}

/// A vocabulary file and a card directory holding the first three test cards, like `<data>/vocab` and `<data>/cards`.
fn on_disk(root: &std::path::Path) -> (PathBuf, PathBuf) {
    let (vocab_file, cards_dir) = (root.join("terms.json"), root.join("cards"));
    fs::write(&vocab_file, VOCAB_JSON).unwrap();
    for c in &cards()[..3] {
        write_card(&cards_dir, c);
    }
    (vocab_file, cards_dir)
}

/// What `ks index rebuild` does: fingerprint first, then read the files, then build.
fn rebuild_from_disk(idx: &mut Index, vocab_file: &std::path::Path, cards_dir: &std::path::Path) {
    let fp = Fingerprints::of_files(vocab_file, cards_dir).unwrap();
    let (cards, v) = (load_cards(cards_dir).unwrap(), Vocab::load(vocab_file).unwrap());
    idx.rebuild_with(&cards, &v, None, &fp).unwrap();
}

fn stale(idx: &mut Index, vocab_file: &std::path::Path, cards_dir: &std::path::Path) -> Result<(), String> {
    let now = Fingerprints::of_files(vocab_file, cards_dir).unwrap();
    idx.check_fresh(&now, HINT).map(|_| ()).map_err(|e| e.to_string())
}

#[test]
fn cards_fingerprint_follows_the_file_list_sizes_and_mtimes() {
    let s = Scratch::new();
    let (_, dir) = on_disk(&s.0);
    let base = cards_fingerprint(&dir).unwrap();
    assert_eq!(base.len(), 64, "sha256 hex: {base}");
    assert_eq!(cards_fingerprint(&dir).unwrap(), base, "stable while nothing changes");
    // only *.md files count
    fs::write(dir.join("t").join("notes.txt"), "ignored").unwrap();
    assert_eq!(cards_fingerprint(&dir).unwrap(), base);
    // a new card changes it, and removing it again restores it
    let extra = write_card(&dir, &cards()[3]);
    let added = cards_fingerprint(&dir).unwrap();
    assert_ne!(added, base);
    fs::remove_file(&extra).unwrap();
    assert_eq!(cards_fingerprint(&dir).unwrap(), base);
    // a changed size changes it
    let p = dir.join("t").join(format!("{}.md", cards()[0].id));
    let orig = fs::read_to_string(&p).unwrap();
    fs::write(&p, format!("{orig}\n")).unwrap();
    assert_ne!(cards_fingerprint(&dir).unwrap(), base);
    // the same bytes with another modification time change it too
    fs::write(&p, &orig).unwrap();
    let f = fs::OpenOptions::new().write(true).open(&p).unwrap();
    f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3600)).unwrap();
    drop(f);
    assert_ne!(cards_fingerprint(&dir).unwrap(), base);
}

#[test]
fn fingerprints_of_files_hash_the_vocabulary_bytes() {
    let s = Scratch::new();
    let (vocab_file, dir) = on_disk(&s.0);
    let a = Fingerprints::of_files(&vocab_file, &dir).unwrap();
    use sha2::{Digest, Sha256};
    assert_eq!(a.vocab_fp, hex::encode(Sha256::digest(VOCAB_JSON.as_bytes())));
    assert_eq!(a.cards_fp, cards_fingerprint(&dir).unwrap());
    assert_eq!(Vocab::load_with_fingerprint(&vocab_file).unwrap().1, a.vocab_fp, "the fingerprint of the bytes that were parsed");
    fs::write(&vocab_file, format!("{VOCAB_JSON}\n")).unwrap();
    let b = Fingerprints::of_files(&vocab_file, &dir).unwrap();
    assert_ne!(b.vocab_fp, a.vocab_fp);
    assert_eq!(b.cards_fp, a.cards_fp);
}

#[test]
fn rebuild_with_records_both_fingerprints_in_meta() {
    let Some(env) = Env::new() else { return };
    let (vocab_file, dir) = on_disk(&env.tmp);
    let mut idx = env.index();
    rebuild_from_disk(&mut idx, &vocab_file, &dir);
    let want = Fingerprints::of_files(&vocab_file, &dir).unwrap();
    let meta = idx.meta().unwrap();
    assert_eq!(meta.get("vocab_fp"), Some(&want.vocab_fp));
    assert_eq!(meta.get("cards_fp"), Some(&want.cards_fp));
    assert_eq!(meta.get("card_count").map(String::as_str), Some("3"), "the existing meta fields are still written");
}

#[test]
fn search_is_allowed_right_after_a_rebuild_and_refused_after_the_vocabulary_changes() {
    let Some(env) = Env::new() else { return };
    let (vocab_file, dir) = on_disk(&env.tmp);
    let mut idx = env.index();
    rebuild_from_disk(&mut idx, &vocab_file, &dir);
    assert_eq!(stale(&mut idx, &vocab_file, &dir), Ok(()));
    fs::write(&vocab_file, VOCAB_JSON.replace(r#""aliases":["redis-stack"]"#, r#""aliases":["redis-stack","redisdb"]"#)).unwrap();
    let e = stale(&mut idx, &vocab_file, &dir).unwrap_err();
    assert!(e.contains("vocab file") && !e.contains("card files") && e.contains(HINT), "{e}");
    rebuild_from_disk(&mut idx, &vocab_file, &dir);
    assert_eq!(stale(&mut idx, &vocab_file, &dir), Ok(()), "a rebuild makes the index current again");
}

#[test]
fn search_is_refused_after_a_card_is_added_modified_or_removed_until_rebuilt() {
    let Some(env) = Env::new() else { return };
    let (vocab_file, dir) = on_disk(&env.tmp);
    let mut idx = env.index();
    rebuild_from_disk(&mut idx, &vocab_file, &dir);
    let only_cards = |e: &str| e.contains("card files") && !e.contains("vocab file") && e.contains(HINT);
    // added
    let extra = write_card(&dir, &cards()[3]);
    let e = stale(&mut idx, &vocab_file, &dir).unwrap_err();
    assert!(only_cards(&e), "{e}");
    rebuild_from_disk(&mut idx, &vocab_file, &dir);
    assert_eq!(stale(&mut idx, &vocab_file, &dir), Ok(()));
    // modified
    let p = dir.join("t").join(format!("{}.md", cards()[0].id));
    fs::write(&p, format!("{}\n", fs::read_to_string(&p).unwrap())).unwrap();
    let e = stale(&mut idx, &vocab_file, &dir).unwrap_err();
    assert!(only_cards(&e), "{e}");
    rebuild_from_disk(&mut idx, &vocab_file, &dir);
    assert_eq!(stale(&mut idx, &vocab_file, &dir), Ok(()));
    // removed
    fs::remove_file(&extra).unwrap();
    let e = stale(&mut idx, &vocab_file, &dir).unwrap_err();
    assert!(only_cards(&e), "{e}");
    // both at once are both named
    fs::write(&vocab_file, format!("{VOCAB_JSON}\n")).unwrap();
    let e = stale(&mut idx, &vocab_file, &dir).unwrap_err();
    assert!(e.contains("vocab file") && e.contains("card files"), "{e}");
}

#[test]
fn search_is_refused_for_an_index_without_fingerprints_and_for_a_missing_index() {
    let Some(env) = Env::new() else { return };
    let (vocab_file, dir) = on_disk(&env.tmp);
    let mut idx = env.index();
    // never built
    let e = stale(&mut idx, &vocab_file, &dir).unwrap_err();
    assert!(e.contains("does not exist") && e.contains(HINT), "{e}");
    // built by a version that wrote no fingerprints
    idx.rebuild(&cards()[..3], &vocab(), None).unwrap();
    let e = stale(&mut idx, &vocab_file, &dir).unwrap_err();
    assert!(e.contains("without fingerprints") && e.contains(HINT), "{e}");
    // one of the two missing is as good as both missing
    rebuild_from_disk(&mut idx, &vocab_file, &dir);
    redis::cmd("HDEL").arg(format!("{}meta", env.prefix)).arg("cards_fp").query::<()>(&mut env.raw()).unwrap();
    let e = stale(&mut idx, &vocab_file, &dir).unwrap_err();
    assert!(e.contains("without fingerprints") && e.contains(HINT), "{e}");
    rebuild_from_disk(&mut idx, &vocab_file, &dir);
    assert_eq!(stale(&mut idx, &vocab_file, &dir), Ok(()));
    // fingerprints but no build id (an older `ks`): `search_checked` could not tell a rebuild from the same files, so
    // it is refused like a missing fingerprint
    redis::cmd("HDEL").arg(format!("{}meta", env.prefix)).arg("build_id").query::<()>(&mut env.raw()).unwrap();
    let e = stale(&mut idx, &vocab_file, &dir).unwrap_err();
    assert!(e.contains("without fingerprints") && e.contains("build id") && e.contains(HINT), "{e}");
    rebuild_from_disk(&mut idx, &vocab_file, &dir);
    assert_eq!(stale(&mut idx, &vocab_file, &dir), Ok(()));
}

// ---------- rebuild safety: failed rebuilds, searches that overlap a rebuild, concurrent rebuilds ----------

/// A Redis user limited to this test's keys that may run the commands a rebuild needs except `refused`, so a rebuild
/// through it fails at that step the way a real server error would. No @dangerous or @admin command, except
/// FT.DROPINDEX (in @dangerous, and the rebuild's second step). Removed again on drop; a test run that is killed
/// (SIGKILL) never drops it, so a `kstest-*user` can be left behind: remove it with `ACL DELUSER`.
struct RefusingUser { url: String, name: String, admin: redis::Connection }
impl RefusingUser {
    fn new(env: &Env, refused: &str) -> RefusingUser {
        let name = format!("{}user", env.prefix.trim_end_matches(':'));
        let mut b = [0u8; 32];
        fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut b)).unwrap();
        let pw = hex::encode(b);
        let mut admin = env.raw();
        redis::cmd("ACL").arg("SETUSER").arg(&name).arg("reset").arg("on").arg(format!(">{pw}")).arg(format!("~{}*", env.prefix))
            .arg("+@all").arg("-@dangerous").arg("-@admin").arg("+ft.dropindex").arg(format!("-{refused}"))
            .query::<()>(&mut admin).unwrap();
        let mut u = url::Url::parse(&env.url).unwrap();
        u.set_username(&name).unwrap();
        u.set_password(Some(&pw)).unwrap();
        RefusingUser { url: u.to_string(), name, admin }
    }
}
impl Drop for RefusingUser {
    fn drop(&mut self) { let _ = redis::cmd("ACL").arg("DELUSER").arg(&self.name).query::<()>(&mut self.admin); }
}

fn rebuild_through(url: &str, prefix: &str, vocab_file: &std::path::Path, cards_dir: &std::path::Path) -> ks_core::Result<ks_core::index::RebuildReport> {
    let fp = Fingerprints::of_files(vocab_file, cards_dir).unwrap();
    let (cards, v) = (load_cards(cards_dir).unwrap(), Vocab::load(vocab_file).unwrap());
    Index::connect(url, prefix).unwrap().rebuild_with(&cards, &v, None, &fp)
}

#[test]
fn a_rebuild_that_fails_half_way_leaves_no_meta_so_every_search_is_refused() {
    let Some(env) = Env::new() else { return };
    let (vocab_file, dir) = on_disk(&env.tmp);
    let mut idx = env.index();
    // Redis refuses one step of the rebuild, in the order the rebuild takes them: dropping the index, finding the old
    // card keys, deleting them, creating the index, writing a card document, writing the final meta. Whichever step
    // fails, the meta of the previous, complete build must not vouch for what is there now.
    for refused in ["ft.dropindex", "scan", "unlink", "ft.create", "json.set", "hset"] {
        rebuild_from_disk(&mut idx, &vocab_file, &dir);
        assert_eq!(stale(&mut idx, &vocab_file, &dir), Ok(()), "{refused}: current before the failed rebuild");
        let user = RefusingUser::new(&env, refused);
        let e = rebuild_through(&user.url, &env.prefix, &vocab_file, &dir).unwrap_err().to_string();
        assert!(e.to_lowercase().contains(refused), "{refused}: the rebuild failed for another reason: {e}");
        assert!(idx.meta().unwrap().is_empty(), "{refused}: the old meta survived a failed rebuild");
        let e = stale(&mut idx, &vocab_file, &dir).unwrap_err();
        assert!(e.contains("does not exist") && e.contains(HINT), "{refused}: {e}");
    }
}

#[test]
fn a_search_that_overlaps_a_rebuild_is_refused_instead_of_returning_its_results() {
    let Some(env) = Env::new() else { return };
    let (vocab_file, dir) = on_disk(&env.tmp);
    let mut idx = env.index();
    rebuild_from_disk(&mut idx, &vocab_file, &dir);
    let now = Fingerprints::of_files(&vocab_file, &dir).unwrap();
    let v = Vocab::load(&vocab_file).unwrap();
    let qs = queries();
    let rebuilt = |e: String| assert!(e.contains("rebuilt while") && e.contains("search again"), "{e}");

    // nothing in between: the result stands, and equals a plain search
    let checked = idx.check_fresh(&now, HINT).unwrap();
    let got = idx.search_checked(&checked, &v, &qs, 40).unwrap();
    assert_eq!(got.into_iter().map(|r| r.into_result()).collect::<Vec<_>>(), idx.search(&v, &qs, 40).unwrap().into_iter().map(|r| r.into_result()).collect::<Vec<_>>());

    // a rebuild from the very same files (same fingerprints) ran between the check and the end of the search
    let checked = idx.check_fresh(&now, HINT).unwrap();
    rebuild_from_disk(&mut env.index(), &vocab_file, &dir);
    rebuilt(idx.search_checked(&checked, &v, &qs, 40).unwrap_err().to_string());

    // a rebuild still running when the search ends: it deleted meta first
    let checked = idx.check_fresh(&now, HINT).unwrap();
    redis::cmd("DEL").arg(format!("{}meta", env.prefix)).query::<()>(&mut env.raw()).unwrap();
    rebuilt(idx.search_checked(&checked, &v, &qs, 40).unwrap_err().to_string());
}

#[test]
fn only_one_rebuild_runs_at_a_time_and_only_its_own_token_releases_the_lock() {
    let Some(env) = Env::new() else { return };
    let (vocab_file, dir) = on_disk(&env.tmp);
    let lock_key = format!("{}rebuild-lock", env.prefix);
    let mut idx = env.index();
    rebuild_from_disk(&mut idx, &vocab_file, &dir);
    assert_eq!(key_count(&mut env.raw(), &lock_key), 0, "released after a successful rebuild");
    let meta = idx.meta().unwrap();

    let token = idx.lock_rebuild().unwrap();
    let pttl: i64 = redis::cmd("PTTL").arg(&lock_key).query(&mut env.raw()).unwrap();
    assert!(pttl > 10 * 60 * 1000, "the lock expires (so a crashed rebuild frees it) but not before a long rebuild ends: {pttl} ms");
    let mut other = env.index();
    let e = rebuild_through(&env.url, &env.prefix, &vocab_file, &dir).unwrap_err().to_string();
    assert!(e.contains("another `ks index rebuild` is running") && e.contains(&lock_key), "{e}");
    // a lock left by a rebuild that crashed: the message says it frees itself, and names no secret
    assert!(e.contains("if no other rebuild is running") && e.contains("at most 15 minutes"), "{e}");
    assert!(!e.contains(&env.url) && !e.contains("DEL "), "{e}");
    assert_eq!(idx.meta().unwrap(), meta, "a refused rebuild changes nothing");
    assert_eq!(stale(&mut idx, &vocab_file, &dir), Ok(()));
    assert!(other.lock_rebuild().is_err(), "a second holder");

    // only the holder's token releases it
    assert!(!other.unlock_rebuild("0123456789abcdef0123456789abcdef").unwrap());
    assert_eq!(key_count(&mut env.raw(), &lock_key), 1, "released with another token");
    assert!(idx.unlock_rebuild(&token).unwrap());
    assert_eq!(key_count(&mut env.raw(), &lock_key), 0);
    assert!(!idx.unlock_rebuild(&token).unwrap(), "released twice");

    // free again: a rebuild runs, and a failed rebuild releases the lock too
    rebuild_from_disk(&mut other, &vocab_file, &dir);
    assert_eq!(key_count(&mut env.raw(), &lock_key), 0);
    let user = RefusingUser::new(&env, "json.set");
    assert!(rebuild_through(&user.url, &env.prefix, &vocab_file, &dir).is_err());
    assert_eq!(key_count(&mut env.raw(), &lock_key), 0, "a failed rebuild left the lock behind");
}
