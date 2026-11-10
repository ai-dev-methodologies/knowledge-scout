use ks_core::vocab::*;
use std::collections::HashSet;

const DOC: &str = r#"{"version":1,"terms":[
 {"term":"redis","facet":"entity","aliases":["redis-stack"]},
 {"term":"llm-as-judge","facet":"technique","aliases":["llm-as-a-judge","LLM Judge"]},
 {"term":"agent","facet":"domain","aliases":[]}
]}"#;

fn v() -> Vocab { Vocab::from_json(DOC, "t.json").unwrap() }
fn r(t: &str, f: Facet) -> Option<Resolved> { Some(Resolved { term: t.into(), facet: f }) }
fn err(src: &str) -> String { Vocab::from_json(src, "t.json").unwrap_err().to_string() }

#[test]
fn normalize_term_basic() {
    assert_eq!(normalize_term("  LLM_as  Judge "), "llm-as-judge");
    assert_eq!(normalize_term("a--b___c"), "a-b-c");
    assert_eq!(normalize_term("-redis-"), "redis");
    assert_eq!(normalize_term(""), "");
    assert_eq!(normalize_term("-"), "");
}

#[test]
fn normalize_term_drops_leading_articles_only_as_whole_tokens() {
    assert_eq!(normalize_term("The Agent Loop"), "agent-loop");
    assert_eq!(normalize_term("an-agent"), "agent");
    assert_eq!(normalize_term("a-harness"), "harness");
    assert_eq!(normalize_term("theory-of-mind"), "theory-of-mind");
    assert_eq!(normalize_term("anchor"), "anchor");
    assert_eq!(normalize_term("the"), "the");
    assert_eq!(normalize_term("a"), "a");
}

#[test]
fn normalize_term_keeps_a_before_single_letter_token() {
    assert_eq!(normalize_term("A-B testing"), "a-b-testing");
    assert_eq!(normalize_term("a-b-testing"), "a-b-testing");
}

#[test]
fn fold_plural_only_when_singular_is_known() {
    let set: HashSet<&str> = ["skill", "agent"].into_iter().collect();
    let has = |s: &str| set.contains(s);
    assert_eq!(fold_plural("skills", &has), "skill");
    assert_eq!(fold_plural("agents", &has), "agent");
    assert_eq!(fold_plural("hooks", &has), "hooks");
    assert_eq!(fold_plural("skill", &has), "skill");
    assert_eq!(fold_plural("s", &has), "s");
}

#[test]
fn resolve_canonical_alias_variants_plural_unknown() {
    let v = v();
    assert_eq!(v.resolve("redis"), r("redis", Facet::Entity));
    assert_eq!(v.resolve("Redis Stack"), r("redis", Facet::Entity));
    assert_eq!(v.resolve("llm-as-a-judge"), r("llm-as-judge", Facet::Technique));
    assert_eq!(v.resolve("llm_judge"), r("llm-as-judge", Facet::Technique));
    assert_eq!(v.resolve("the agents"), r("agent", Facet::Domain));
    assert_eq!(v.resolve("postgres"), None);
    assert_eq!(v.resolve(""), None);
}

#[test]
fn load_rejects_bad_documents() {
    assert!(err(r#"{"version":2,"terms":[]}"#).contains("version"));
    assert!(err(r#"{"terms":[]}"#).contains("version"));
    assert!(err(r#"{"version":1,"terms":[{"term":"x","facet":"thing","aliases":[]}]}"#).contains("facet"));
    assert!(err(r#"{"version":1,"terms":[{"term":" ","facet":"entity","aliases":[]}]}"#).contains("term"));
    assert!(err(r#"{"version":1,"terms":[{"term":"a1","facet":"entity","aliases":["dup"]},{"term":"b1","facet":"entity","aliases":["dup"]}]}"#).contains("dup"));
    assert!(err(r#"{"version":1,"terms":[{"term":"a1","facet":"entity","aliases":[]},{"term":"a1","facet":"domain","aliases":[]}]}"#).contains("a1"));
    assert!(err("{not json").contains("JSON"));
}

#[test]
fn load_requires_normalized_canonical_terms() {
    assert!(err(r#"{"version":1,"terms":[{"term":"LLM Judge","facet":"entity","aliases":[]}]}"#).contains("normalized"));
}

#[test]
fn alias_equal_to_own_term_is_ignored_and_missing_aliases_default_empty() {
    let v = Vocab::from_json(r#"{"version":1,"terms":[{"term":"redis","facet":"entity","aliases":["Redis"]},{"term":"kafka","facet":"entity"}]}"#, "t").unwrap();
    assert_eq!(v.resolve("Redis"), r("redis", Facet::Entity));
    assert_eq!(v.resolve("kafka"), r("kafka", Facet::Entity));
}

#[test]
fn yaml_vocab_is_equivalent_to_json() {
    let yaml = "version: 1\nterms:\n  - term: redis\n    facet: entity\n    aliases: [redis-stack]\n  - term: llm-as-judge\n    facet: technique\n    aliases: [llm-as-a-judge, LLM Judge]\n  - term: agent\n    facet: domain\n";
    let y = Vocab::from_yaml(yaml, "t.yaml").unwrap();
    let j = v();
    assert_eq!(y.terms, j.terms);
    for t in ["redis", "Redis Stack", "llm_judge", "the agents", "postgres"] {
        assert_eq!(y.resolve(t), j.resolve(t), "{t}");
    }
    assert!(Vocab::from_yaml("version: 2\nterms: []\n", "t.yaml").is_err());
}

#[test]
fn load_dispatches_on_extension() {
    let dir = std::env::temp_dir().join(format!("ks-vocab-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("t.json"), DOC).unwrap();
    std::fs::write(dir.join("t.yaml"), "version: 1\nterms:\n  - {term: redis, facet: entity, aliases: [redis-stack]}\n").unwrap();
    assert_eq!(Vocab::load(&dir.join("t.json")).unwrap().resolve("redis-stack"), r("redis", Facet::Entity));
    assert_eq!(Vocab::load(&dir.join("t.yaml")).unwrap().resolve("redis-stack"), r("redis", Facet::Entity));
    assert!(Vocab::load(&dir.join("missing.json")).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn classify_terms_buckets_dedupe_and_new_terms() {
    let got = v().classify_terms(&["redis", "Redis Stack", "llm-as-a-judge", "agents", "New Thing", "new_thing", ""]);
    assert_eq!(got, Classified {
        entities: vec!["redis".into()],
        techniques: vec!["llm-as-judge".into()],
        domains: vec!["agent".into()],
        new_terms: vec!["new-thing".into()],
    });
    assert_eq!(v().classify_terms::<&str>(&[]), Classified::default());
}
