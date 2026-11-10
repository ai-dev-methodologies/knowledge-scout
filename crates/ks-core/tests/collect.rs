use ks_core::card::Card;
use ks_core::collect::*;
use ks_core::vocab::Vocab;
use serde_json::{json, Value};

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

fn cards() -> Vec<Card> {
    let mut d = title(base("d"), "Expiry notes");
    d.reusable_elements = vec![ks_core::card::Element { term: "TTL".into(), kind: "method".into(), desc: "만료 시간을 갱신한다 열 자 이상".into(), anchor: "anchor text".into(), requires: None }];
    let mut e = title(base("e"), "Streaming notes");
    e.limits = "Needs a short TTL to be useful.".into();
    vec![
        kw(title(base("a"), "Session store notes"), &["redis", "ttl", "langchain"]),
        kw(title(base("b"), "Agent notes"), &["redis-stack", "agent-memory", "kafka"]),
        kw(title(base("c"), "Grading notes"), &["llm-judge", "ttl", "kafka"]),
        d,
        e,
        title(base("f"), "Redis Stack in practice"),
        kw(title(base("g"), "TTL tips"), &["llm-judge", "ttl", "langchain"]),
    ]
}
fn vocab() -> Vocab {
    Vocab::from_json(r#"{"version":1,"terms":[
      {"term":"redis","facet":"entity","aliases":["redis-stack"]},
      {"term":"ttl","facet":"technique","aliases":[]},
      {"term":"agent-memory","facet":"domain","aliases":[]},
      {"term":"llm-as-judge","facet":"technique","aliases":["llm-judge"]},
      {"term":"langchain","facet":"entity","aliases":["langchainjs"]},
      {"term":"kafka","facet":"entity","aliases":[]},{"term":"flink","facet":"entity","aliases":[]},{"term":"spark","facet":"entity","aliases":[]}]}"#, "t").unwrap()
}
fn queries() -> Vec<Query> {
    vec![
        Query { qid: "Q1".into(), query: "how to expire redis sessions".into(), terms: Some(vec!["Redis Stack".into(), "ttl".into()]) },
        Query { qid: "Q2".into(), query: "a LangChainJS tips x".into(), terms: None },
    ]
}
fn run(limit: usize) -> Vec<QueryResult> { collect(&cards(), &vocab(), &queries(), limit) }
fn keys(q: &QueryResult) -> Vec<&str> { q.candidates.iter().map(|c| c.card_key.as_str()).collect() }

#[test]
fn output_shape_and_scope_count() {
    let res = run(40);
    assert_eq!(res.iter().map(|r| r.qid.as_str()).collect::<Vec<_>>(), ["Q1", "Q2"]);
    assert_eq!(res[0].scope.l1_cards, 7);
    // Field order is part of the output contract (same as the node collector).
    let s = serde_json::to_string(&res[0]).unwrap();
    assert!(s.starts_with(r#"{"qid":"Q1","scope":{"l1_cards":7},"candidates":[{"card_key":"k-a","path":"p/a.md","title":"Session store notes","matched_terms":["redis","ttl"],"text_hits":[],"rank":1},"#), "{s}");
}

#[test]
fn scope_counts_only_cards_without_skip_reason() {
    let mut cs = cards();
    cs[0].skip_reason = Some("not_technical".into());
    let res = collect(&cs, &vocab(), &queries(), 40);
    assert_eq!(res[0].scope.l1_cards, 6);
    assert!(!keys(&res[0]).contains(&"k-a"));
}

#[test]
fn q1_ranking_matched_then_hits_then_rarest_then_key() {
    // a: 2 matched | g: 1 matched + 1 hit | b: redis (df 2) | c,d: ttl (df 4, tie -> key) | e,f: no match, 1 hit each (tie -> key)
    let q = &run(40)[0];
    assert_eq!(keys(q), ["k-a", "k-g", "k-b", "k-c", "k-d", "k-e", "k-f"]);
    assert_eq!(q.candidates.iter().map(|c| c.rank).collect::<Vec<_>>(), [1, 2, 3, 4, 5, 6, 7]);
}

#[test]
fn alias_resolution_on_query_and_card_side() {
    let res = run(40);
    let by = |k: &str| res[0].candidates.iter().find(|c| c.card_key == k).unwrap().clone();
    assert_eq!(by("k-a").matched_terms, ["redis", "ttl"]);
    assert_eq!(by("k-b").matched_terms, ["redis"]);
    assert_eq!(by("k-c").matched_terms, ["ttl"]);
    assert!(by("k-c").text_hits.is_empty());
}

#[test]
fn reusable_element_terms_join_term_set() {
    let res = run(40);
    let d = res[0].candidates.iter().find(|c| c.card_key == "k-d").unwrap();
    assert_eq!(d.matched_terms, ["ttl"]);
}

#[test]
fn text_hits_are_case_insensitive_distinct_and_in_query_order() {
    let res = run(40);
    let by = |k: &str| res[0].candidates.iter().find(|c| c.card_key == k).unwrap().clone();
    assert_eq!(by("k-e").text_hits, ["ttl"]);
    assert_eq!(by("k-f").text_hits, ["redis stack"]);
    assert_eq!(by("k-g").text_hits, ["ttl"]);
    assert!(by("k-e").matched_terms.is_empty());
}

#[test]
fn element_desc_is_searched() {
    let mut cs = cards();
    cs[3].reusable_elements[0].desc = "갱신하는 방법 zebra-marker 포함".into();
    let q = vec![Query { qid: "Z".into(), query: "x".into(), terms: Some(vec!["zebra-marker".into()]) }];
    let res = collect(&cs, &vocab(), &q, 40);
    assert_eq!(keys(&res[0]), ["k-d"]);
}

#[test]
fn query_without_terms_is_tokenized_and_short_tokens_dropped() {
    let q = &run(40)[1];
    assert_eq!(keys(q), ["k-g", "k-a"]);
    assert_eq!(q.candidates[0].matched_terms, ["langchain"]);
    assert_eq!(q.candidates[0].text_hits, ["tips"]);
    assert!(q.candidates[1].text_hits.is_empty());
}

#[test]
fn tokenizer_splits_on_non_letter_non_digit_unicode_and_counts_utf16_length() {
    let mut cs = cards();
    cs[4].title = Some("Zeta 한글검색 notes".into());
    cs[5].title = Some("𝒜 astral letter".into()); // one letter but 2 UTF-16 units: JS keeps it as a token
    let q = vec![Query { qid: "T".into(), query: "한글검색, ok! 𝒜 😀".into(), terms: None }]; // the emoji is not a letter: a separator
    let res = collect(&cs, &vocab(), &q, 40);
    let mut got = keys(&res[0]);
    got.sort();
    assert_eq!(got, ["k-e", "k-f"]);
    assert_eq!(res[0].candidates.iter().find(|c| c.card_key == "k-e").unwrap().text_hits, ["한글검색"]);
}

#[test]
fn limit_truncates_after_ranking() {
    let q = &run(2)[0];
    assert_eq!(keys(q), ["k-a", "k-g"]);
    assert_eq!(q.scope.l1_cards, 7);
    assert_eq!(run(40)[0].candidates.len(), 7);
}

#[test]
fn empty_result_when_nothing_matches() {
    let q = vec![Query { qid: "Z".into(), query: "zzz".into(), terms: Some(vec!["nothing-here".into()]) }];
    let res = collect(&cards(), &vocab(), &q, 40);
    assert_eq!(res, vec![QueryResult { qid: "Z".into(), scope: ScopeCount { l1_cards: 7 }, candidates: vec![], unknown_terms: vec!["nothing-here".into()] }]);
}

#[test]
fn deterministic() { assert_eq!(run(40), run(40)); }

#[test]
fn ties_use_utf16_code_unit_order_for_card_key() {
    // U+FF5E (BMP) sorts AFTER U+1F600 in UTF-16 order but BEFORE it in UTF-8 byte order.
    let mut x = base("x"); x.card_key = "k-\u{ff5e}".into(); x.title = Some("zzmark".into());
    let mut y = base("y"); y.card_key = "k-\u{1f600}".into(); y.title = Some("zzmark".into());
    let q = vec![Query { qid: "U".into(), query: "x".into(), terms: Some(vec!["zzmark".into()]) }];
    let res = collect(&[x, y], &vocab(), &q, 40);
    assert_eq!(keys(&res[0]), ["k-\u{1f600}", "k-\u{ff5e}"]);
}

#[test]
fn parse_queries_validates_like_the_node_collector() {
    let ok = parse_queries(&json!([{"qid": "A", "query": "q"}, {"qid": "B", "query": "q", "terms": ["x"]}])).unwrap();
    assert_eq!(ok[0].terms, None);
    assert_eq!(ok[1].terms, Some(vec!["x".to_string()]));
    let msg = |v: Value| parse_queries(&v).unwrap_err().to_string();
    assert!(msg(json!({"qid": "A"})).contains("array"));
    assert!(msg(json!([{"query": "no qid"}])).contains("qid"));
    assert!(msg(json!([{"qid": "", "query": "q"}])).contains("qid"));
    assert!(msg(json!([{"qid": "A", "query": 5}])).contains("query"));
    assert!(msg(json!([{"qid": "A", "query": "q", "terms": "x"}])).contains("terms"));
    assert!(msg(json!([{"qid": "A", "query": "q", "terms": [1]}])).contains("terms"));
}

#[test]
fn query_terms_are_trimmed_lowercased_and_deduped() {
    let q = vec![Query { qid: "D".into(), query: "x".into(), terms: Some(vec!["  TTL ".into(), "ttl".into(), " ".into()]) }];
    let res = collect(&cards(), &vocab(), &q, 40);
    let c = res[0].candidates.iter().find(|c| c.card_key == "k-g").unwrap();
    assert_eq!((c.matched_terms.clone(), c.text_hits.clone()), (vec!["ttl".to_string()], vec!["ttl".to_string()]));
}

#[test]
fn load_cards_reads_md_recursively_in_sorted_order_and_ignores_other_files() {
    let dir = std::env::temp_dir().join(format!("ks-load-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("t2")).unwrap();
    std::fs::create_dir_all(dir.join("t1")).unwrap();
    for c in cards().iter().take(3) {
        let sub = if c.card_key == "k-b" { "t2" } else { "t1" };
        std::fs::write(dir.join(sub).join(format!("{}.md", c.id)), c.render()).unwrap();
    }
    std::fs::write(dir.join("t1").join("notes.txt"), "ignored").unwrap();
    let loaded = load_cards(&dir).unwrap();
    assert_eq!(loaded.len(), 3);
    let mut want: Vec<String> = loaded.iter().map(|c| c.card_key.clone()).collect();
    want.sort();
    assert_eq!(want, ["k-a", "k-b", "k-c"]);
    std::fs::write(dir.join("t1").join("broken.md"), "not a card").unwrap();
    assert!(load_cards(&dir).unwrap_err().to_string().contains("broken.md"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn card_walk_never_follows_symlinks() {
    let root = std::env::temp_dir().join(format!("ks-links-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (dir, outside) = (root.join("cards"), root.join("outside"));
    std::fs::create_dir_all(dir.join("t1")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let cs = cards();
    std::fs::write(dir.join("t1").join(format!("{}.md", cs[0].id)), cs[0].render()).unwrap();
    // a card file outside the card directory, reachable only through links
    std::fs::write(outside.join(format!("{}.md", cs[1].id)), cs[1].render()).unwrap();
    std::os::unix::fs::symlink(outside.join(format!("{}.md", cs[1].id)), dir.join("t1").join(format!("{}.md", cs[1].id))).unwrap();
    std::os::unix::fs::symlink(&outside, dir.join("linked-topic")).unwrap();
    // a loop: t1/loop -> cards
    std::os::unix::fs::symlink(&dir, dir.join("t1").join("loop")).unwrap();

    let paths = md_paths(&dir).unwrap();
    assert_eq!(paths, vec![dir.join("t1").join(format!("{}.md", cs[0].id))], "{paths:?}");
    let loaded = load_cards(&dir).unwrap();
    assert_eq!(loaded.iter().map(|c| c.card_key.as_str()).collect::<Vec<_>>(), ["k-a"]);
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn query_input_is_capped() {
    let terms = |n: usize, len: usize| json!((0..n).map(|i| format!("{i:0>len$}")).collect::<Vec<_>>());
    let ok = |v: Value| parse_queries(&json!([v])).is_ok();
    assert!(ok(json!({"qid": "A", "query": "q", "terms": terms(32, 64)})));
    assert!(!ok(json!({"qid": "A", "query": "q", "terms": terms(33, 2)})), "more than 32 terms");
    assert!(!ok(json!({"qid": "A", "query": "q", "terms": terms(1, 65)})), "a term longer than 64 characters");
    assert!(ok(json!({"qid": "A", "query": "가".repeat(512)})), "512 characters (not bytes)");
    assert!(!ok(json!({"qid": "A", "query": "a".repeat(513)})), "a query longer than 512 characters");
    assert!(check_query_limits(&Query { qid: "A".into(), query: "x".into(), terms: Some(vec!["t".repeat(65)]) }).is_err());
}

#[test]
fn unknown_terms_lists_the_query_terms_the_vocabulary_does_not_resolve() {
    let unknown = |terms: Option<&[&str]>, query: &str| {
        let q = vec![Query { qid: "U".into(), query: query.into(), terms: terms.map(|t| t.iter().map(|s| s.to_string()).collect()) }];
        collect(&cards(), &vocab(), &q, 40).remove(0).unknown_terms
    };
    // alias ("redis stack" -> redis-stack), plural fold ("ttls"), alias of another term and case/space trimming resolve; the rest stay
    assert_eq!(unknown(Some(&["Redis Stack", "ttls", " LangChainJS ", "Zzz Term", "nothing-here", "zzz term"]), "x"), ["zzz term", "nothing-here"]);
    // no terms: the words of the query text are the terms
    assert_eq!(unknown(None, "a LangChainJS tips x"), ["tips"]);
    assert!(unknown(Some(&["redis", "ttl"]), "x").is_empty());
    // the field is the last one of a result, so the earlier fields keep their order
    let q = vec![Query { qid: "U".into(), query: "x".into(), terms: Some(vec!["zzz".into()]) }];
    let s = serde_json::to_string(&collect(&cards(), &vocab(), &q, 40)[0]).unwrap();
    assert!(s.ends_with(r#""candidates":[],"unknown_terms":["zzz"]}"#), "{s}");
}
