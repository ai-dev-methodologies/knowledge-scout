//! L1 card validation (port of validate.mjs).
use crate::card::{card_id, Card};
use crate::normalize::{js_len, js_trim, norm_keyword};
use regex::Regex;
use serde_json::{json, Value};
use std::sync::LazyLock;

pub const EVIDENCE_TYPES: [&str; 7] = ["author_experience", "measurement", "official_doc", "tutorial", "opinion", "announcement", "mixed"];
pub const ELEMENT_KINDS: [&str; 3] = ["method", "tool_usage", "pitfall"];
pub const SKIP_REASONS: [&str; 4] = ["not_technical", "not_an_article", "unreadable", "duplicate_content"];

// `(?i-u)`: ASCII-only case folding, like a JS /i regex without the u flag.
static URL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i-u)https?://").unwrap());
static MEDIUM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i-u)medium\.com").unwrap());
static INJECT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i-u)ignore (all|previous|the above)").unwrap());
static TECH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9+.#-]*$").unwrap());

fn is_hangul(c: char) -> bool {
    ('\u{3131}'..='\u{318e}').contains(&c) || ('\u{ac00}'..='\u{d7a3}').contains(&c)
}

/// JSON.stringify(v), with a missing value printed as `undefined`.
fn show(v: Option<&Value>) -> String {
    v.map_or("undefined".to_string(), |v| v.to_string())
}

fn guard_text(field: &str, s: &str, errs: &mut Vec<String>) {
    if URL_RE.is_match(s) {
        errs.push(format!("{field}: contains a URL"));
    }
    if MEDIUM_RE.is_match(s) {
        errs.push(format!("{field}: contains a medium.com fragment"));
    }
    if INJECT_RE.is_match(s) {
        errs.push(format!("{field}: contains an instruction-injection phrase"));
    }
}

fn tech_ok(n: &str) -> bool {
    !n.is_empty() && js_len(n) <= 40 && TECH_RE.is_match(n)
}

pub fn validate_l1(card: &Value) -> Vec<String> {
    let mut errs = vec![];
    if !card.is_object() {
        return vec!["card: not an object".to_string()];
    }
    match card.get("card_key").and_then(Value::as_str) {
        Some(k) if !js_trim(k).is_empty() => {}
        _ => errs.push("card_key: required string".to_string()),
    }
    match card.get("skip_reason") {
        None | Some(Value::Null) => {}
        Some(skip) => {
            if !skip.as_str().is_some_and(|s| SKIP_REASONS.contains(&s)) {
                errs.push(format!("skip_reason: must be null or one of {}", SKIP_REASONS.join("|")));
            }
            return errs;
        }
    }

    match card.get("tech_keywords").and_then(Value::as_array) {
        Some(tk) if (3..=8).contains(&tk.len()) => {
            let mut seen: Vec<String> = vec![];
            for k in tk {
                let n = k.as_str().map(norm_keyword).unwrap_or_default();
                if !tech_ok(&n) {
                    errs.push(format!("tech_keywords: invalid keyword {}", k));
                } else if seen.contains(&n) {
                    errs.push(format!("tech_keywords: duplicate after normalization {}", Value::String(n.clone())));
                }
                seen.push(n);
            }
        }
        _ => errs.push("tech_keywords: need an array of 3..8 items".to_string()),
    }

    match card.get("problem_keywords").and_then(Value::as_array) {
        Some(pk) if (2..=5).contains(&pk.len()) => {
            for k in pk {
                match k.as_str() {
                    Some(s) if !js_trim(s).is_empty() && js_len(s) <= 40 && s.chars().any(is_hangul) => guard_text("problem_keywords", s, &mut errs),
                    _ => errs.push(format!("problem_keywords: invalid keyword {k} (<=40 chars, needs Hangul)")),
                }
            }
        }
        _ => errs.push("problem_keywords: need an array of 2..5 items".to_string()),
    }

    match card.get("summary_3l").and_then(Value::as_array) {
        Some(s3) if s3.len() == 3 => {
            for l in s3 {
                match l.as_str() {
                    Some(s) if (10..=160).contains(&js_len(s)) => guard_text("summary_3l", s, &mut errs),
                    _ => errs.push("summary_3l: each line must be 10..160 chars".to_string()),
                }
            }
        }
        _ => errs.push("summary_3l: need exactly 3 strings".to_string()),
    }

    if !card.get("evidence_type").and_then(Value::as_str).is_some_and(|e| EVIDENCE_TYPES.contains(&e)) {
        errs.push(format!("evidence_type: must be one of {}", EVIDENCE_TYPES.join("|")));
    }

    match card.get("limits").and_then(Value::as_str) {
        Some(l) if (10..=200).contains(&js_len(l)) => guard_text("limits", l, &mut errs),
        _ => errs.push("limits: need a string of 10..200 chars".to_string()),
    }

    if let Some(els) = card.get("reusable_elements") {
        validate_elements(els, &mut errs);
    }
    errs
}

// Optional card-v2 field. Errors keep the "reusable_elements:" prefix so the merge report groups them.
fn validate_elements(els: &Value, errs: &mut Vec<String>) {
    let list = match els.as_array() {
        Some(l) if l.len() <= 5 => l,
        _ => {
            errs.push("reusable_elements: need an array of 0..5 items".to_string());
            return;
        }
    };
    for (i, e) in list.iter().enumerate() {
        let at = format!("reusable_elements: item {i}");
        if !e.is_object() {
            errs.push(format!("{at} is not an object"));
            continue;
        }
        let n = e.get("term").and_then(Value::as_str).map(norm_keyword).unwrap_or_default();
        if !tech_ok(&n) {
            errs.push(format!("{at} term invalid {}", show(e.get("term"))));
        }
        if !e.get("kind").and_then(Value::as_str).is_some_and(|k| ELEMENT_KINDS.contains(&k)) {
            errs.push(format!("{at} kind must be one of {}", ELEMENT_KINDS.join("|")));
        }
        match e.get("desc").and_then(Value::as_str) {
            Some(d) if (10..=120).contains(&js_len(d)) => guard_text(&format!("reusable_elements: item {i} desc"), d, errs),
            _ => errs.push(format!("{at} desc needs 10..120 chars")),
        }
        if !e.get("anchor").and_then(Value::as_str).is_some_and(|a| (8..=80).contains(&js_len(a))) {
            errs.push(format!("{at} anchor needs 8..80 chars"));
        }
        if let Some(r) = e.get("requires") {
            match r.as_str() {
                Some(s) if (5..=80).contains(&js_len(s)) => guard_text(&format!("reusable_elements: item {i} requires"), s, errs),
                _ => errs.push(format!("{at} requires needs 5..80 chars")),
            }
        }
    }
}

/// Validate a parsed Card: the L1 rules over its L1 fields, plus id/card_key consistency.
pub fn validate_card(card: &Card) -> Vec<String> {
    let mut v = json!({
        "card_key": card.card_key,
        "skip_reason": card.skip_reason,
        "tech_keywords": card.tech_keywords,
        "problem_keywords": card.problems,
        "summary_3l": card.summary,
        "evidence_type": card.evidence_type,
        "limits": card.limits,
    });
    if !card.reusable_elements.is_empty() {
        v["reusable_elements"] = serde_json::to_value(&card.reusable_elements).unwrap();
    }
    let mut errs = validate_l1(&v);
    if card.id != card_id(&card.card_key) {
        errs.push(format!("id: {} does not match sha256(card_key) prefix {}", card.id, card_id(&card.card_key)));
    }
    errs
}
