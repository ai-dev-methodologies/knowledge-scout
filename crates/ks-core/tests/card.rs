use ks_core::card::*;
use ks_core::vocab::Vocab;
use serde_json::json;

fn full() -> Card {
    let mut c = Card::new("https://example.com/notes/widget-cache-1");
    c.title = Some("Widget Cache Notes: a \"field report\" # 1".into());
    c.source_url = Some("https://example.com/notes/widget-cache-1".into());
    c.scope = Scope::Team;
    c.access = Access::A;
    c.topic = Some("example-topic".into());
    c.status = Some("synthesized".into());
    c.evidence_type = Some("opinion".into());
    c.published_at = Some("2019-07-08".into());
    c.collected_at = Some("2019-08-09".into());
    c.sha256 = Some("1111111111111111111111111111111111111111111111111111111111111111".into());
    c.source_path = Some("sources/example/topic/sample-001/x.md".into());
    c.learning_record = Some("docs/lr.md".into());
    c.learning_record_state = Some("draft".into());
    c.entities = vec!["widgetdb".into(), "mcp".into()];
    c.techniques = vec!["cache-expiry".into()];
    c.domains = vec!["agent".into()];
    c.new_terms = vec!["widget-ttl-policy".into()];
    c.problems = vec!["가상 위젯 캐시의 만료 누락".into(), "캐시 재생성 중 응답 지연".into()];
    c.tech_keywords = vec!["widgetdb".into(), "example-lib".into(), "c++".into(), "node.js".into()];
    c.summary = vec![
        "가상의 위젯 캐시에서 만료 정책을 바꾼 과정을 정리한 글이다.".into(),
        "- 로 시작하는 줄도 그대로 보존되어야 한다: yes # no".into(),
        "수치는 모두 가상의 예시이며 측정한 값이 아니다. key: value".into(),
    ];
    c.limits = "가상의 서비스 하나를 가정한 예시라 다른 환경에 그대로 옮길 수 없다.".into();
    c.reusable_elements = vec![
        Element { term: "key-prefix".into(), kind: "method".into(), desc: "키 이름 앞에 접두사를 붙인다.".into(), anchor: "Key prefix namespacing".into(), requires: Some("redis 7 이상".into()) },
        Element { term: "ttl".into(), kind: "pitfall".into(), desc: "만료 시간을 갱신하지 않는다.".into(), anchor: "TTL never refreshed".into(), requires: None },
    ];
    c
}

#[test]
fn card_id_is_first_16_hex_of_sha256_of_card_key() {
    assert_eq!(card_id("https://m.com/a"), "09893fb7eba66489"); // shasum -a 256, first 16 hex chars
    assert_eq!(card_id(""), "e3b0c44298fc1c14");
    assert_eq!(Card::new("https://m.com/a").id, "09893fb7eba66489");
}

#[test]
fn new_card_defaults() {
    let c = Card::new("k");
    assert_eq!((c.schema.as_str(), c.card_key.as_str(), c.scope, c.access), ("card-v1", "k", Scope::Private, Access::Unknown));
    assert!(c.skip_reason.is_none() && c.summary.is_empty() && c.limits.is_empty());
}

#[test]
fn round_trip_full_card() {
    let c = full();
    let md = c.render();
    assert_eq!(Card::parse(&md).unwrap(), c, "{md}");
    assert_eq!(Card::parse(&md).unwrap().render(), md, "render is stable");
}

#[test]
fn round_trip_minimal_and_skipped_cards() {
    let m = Card::new("path:sources/x.md");
    assert_eq!(Card::parse(&m.render()).unwrap(), m);
    let mut s = Card::new("https://m.com/s");
    s.skip_reason = Some("not_technical".into());
    s.access = Access::B;
    assert_eq!(Card::parse(&s.render()).unwrap(), s);
}

#[test]
fn round_trip_yaml_hostile_strings() {
    for t in ["null", "true", "~", "123", "1e3", "0x1f", "  padded  ", "a: b", "- x", "# c", "'q'", "\"dq\"", "multi\nline title", "한글: 제목 #1", "---", "2019-07-08", ""] {
        let mut c = full();
        c.title = Some(t.into());
        c.sha256 = Some(t.into());
        c.topic = Some(t.into());
        c.problems = vec![t.into()];
        c.reusable_elements[0].desc = t.into();
        assert_eq!(Card::parse(&c.render()).unwrap(), c, "title {t:?}\n{}", c.render());
    }
}

#[test]
fn rendered_format_has_frontmatter_and_body_sections() {
    let md = full().render();
    assert!(md.starts_with("---\nschema: card-v1\nid: "), "{md}");
    assert!(md.contains("\nscope: team\n") && md.contains("\naccess: A\n"), "{md}");
    assert!(md.contains("\nskip_reason: null\n"), "{md}");
    let body = md.split("\n---\n").nth(1).unwrap();
    assert!(body.contains("## 요약\n\n- 가상의 위젯 캐시에서"), "{body}");
    assert!(body.contains("\n## 한계\n\n가상의 서비스 하나를"), "{body}");
    assert!(!md.contains("reusable_elements") || md.contains("requires: redis"));
    assert!(!Card::new("k").render().contains("reusable_elements"), "v1 cards omit the optional field");
}

#[test]
fn parse_rejects_malformed_files() {
    assert!(Card::parse("no frontmatter").is_err());
    assert!(Card::parse("---\nschema: card-v1\n").is_err(), "unterminated frontmatter");
    let md = full().render();
    assert!(Card::parse(&md.replace("schema: card-v1", "schema: card-v9")).is_err());
    assert!(Card::parse(&md.replace("scope: team", "scope: public")).is_err());
    assert!(Card::parse(&md.replace("access: A", "access: Z")).is_err());
    let stray = md.replace("## 요약\n\n", "## 요약\n\nstray prose line\n");
    assert!(Card::parse(&stray).is_err(), "non-bullet line inside 요약");
}

#[test]
fn from_kit_maps_fields_and_classifies_terms() {
    let vocab = Vocab::from_json(r#"{"version":1,"terms":[
        {"term":"redis","facet":"entity","aliases":["redis-stack"]},
        {"term":"ttl","facet":"technique","aliases":[]},
        {"term":"agent-memory","facet":"domain","aliases":[]}]}"#, "t").unwrap();
    let kit = json!({
        "schema": "card-v1", "card_key": "https://m.com/a", "tier": "L1", "topic": "t1", "status": "synthesized",
        "path": "sources/example/t1/a.md", "sha256": "abc", "title": "Title A", "author": "Secret Author",
        "source_url": "https://m.com/a?utm=1", "published_at": "2019-07-08", "collected_at": null, "access_tier": "A",
        "learning_record": null, "learning_record_state": "draft", "skip_reason": null,
        "tech_keywords": ["redis-stack", "agent-memory", "brand-new"], "problem_keywords": ["가나", "다라"],
        "summary_3l": ["하나 요약입니다 열 자 이상.", "둘 요약입니다 열 자 이상.", "셋 요약입니다 열 자 이상."],
        "evidence_type": "tutorial", "limits": "한계 설명 한 줄입니다 열 자 이상.",
        "reusable_elements": [{"term": "TTL", "kind": "method", "desc": "만료 시간을 갱신한다 열 자 이상", "anchor": "anchor text", "requires": "redis 필요함"}],
    });
    let c = Card::from_kit(&kit, &vocab, Scope::Private).unwrap();
    assert_eq!(c.id, card_id("https://m.com/a"));
    assert_eq!((c.card_key.as_str(), c.title.as_deref(), c.source_url.as_deref()), ("https://m.com/a", Some("Title A"), Some("https://m.com/a?utm=1")));
    assert_eq!((c.scope, c.access), (Scope::Private, Access::A));
    assert_eq!((c.topic.as_deref(), c.status.as_deref(), c.sha256.as_deref(), c.source_path.as_deref()), (Some("t1"), Some("synthesized"), Some("abc"), Some("sources/example/t1/a.md")));
    assert_eq!((c.published_at.as_deref(), c.collected_at.as_deref(), c.learning_record_state.as_deref()), (Some("2019-07-08"), None, Some("draft")));
    assert_eq!(c.entities, ["redis"]);
    assert_eq!(c.techniques, ["ttl"]);
    assert_eq!(c.domains, ["agent-memory"]);
    assert_eq!(c.new_terms, ["brand-new"]);
    assert_eq!(c.problems, ["가나", "다라"]);
    assert_eq!(c.tech_keywords, ["redis-stack", "agent-memory", "brand-new"]);
    assert_eq!(c.summary.len(), 3);
    assert_eq!(c.limits, "한계 설명 한 줄입니다 열 자 이상.");
    assert_eq!(c.reusable_elements[0].requires.as_deref(), Some("redis 필요함"));
    assert!(!c.render().contains("Secret Author"), "author never reaches the card");
    assert_eq!(Card::parse(&c.render()).unwrap(), c);
}

#[test]
fn from_kit_access_tier_and_scope() {
    let vocab = Vocab::from_json(r#"{"version":1,"terms":[]}"#, "t").unwrap();
    for (tier, want) in [("A", Access::A), ("B", Access::B), ("unknown", Access::Unknown), ("weird", Access::Unknown)] {
        let c = Card::from_kit(&json!({"card_key": "k", "access_tier": tier}), &vocab, Scope::Team).unwrap();
        assert_eq!((c.access, c.scope), (want, Scope::Team), "{tier}");
    }
    assert!(Card::from_kit(&json!({"title": "no key"}), &vocab, Scope::Private).is_err());
}

#[test]
fn from_kit_rejects_content_the_markdown_format_cannot_represent() {
    let vocab = Vocab::from_json(r#"{"version":1,"terms":[]}"#, "t").unwrap();
    let e = Card::from_kit(&json!({"card_key": "k", "summary_3l": ["line one\nline two", "b", "c"]}), &vocab, Scope::Private).unwrap_err();
    assert!(e.to_string().contains("not representable"), "{e}");
    assert!(Card::from_kit(&json!({"card_key": "k", "limits": "\nstarts with a newline"}), &vocab, Scope::Private).is_err());
    assert!(Card::from_kit(&json!({"card_key": "k", "limits": "fake\n## 요약\nheading inside limits"}), &vocab, Scope::Private).is_err());
    assert!(Card::from_kit(&json!({"card_key": "k", "limits": "  padded but fine  "}), &vocab, Scope::Private).is_ok());
}

#[test]
fn parse_kit_line_replaces_lone_surrogate_escapes_but_keeps_valid_pairs_and_escaped_backslashes() {
    let v = parse_kit_line(r#"{"a":"x \ud835","b":"pair 𝒜 ok","c":"lone low \udc9c","d":"esc \\ud835 stays","e":"\ud835A"}"#).unwrap();
    assert_eq!(v["a"], "x \u{fffd}");
    assert_eq!(v["b"], "pair \u{1d49c} ok");
    assert_eq!(v["c"], "lone low \u{fffd}");
    assert_eq!(v["d"], "esc \\ud835 stays");
    assert_eq!(v["e"], "\u{fffd}A");
    assert_eq!(parse_kit_line(r#"{"plain":"한글 😀"}"#).unwrap()["plain"], "한글 😀");
    assert!(parse_kit_line("{not json").is_err());
}
