//! Card model and its Markdown file format (YAML frontmatter + `## 요약` / `## 한계`).
use crate::normalize::{js_trim, normalize_url};
use crate::{vocab::Vocab, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope { Private, Team }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Access {
    A,
    B,
    #[serde(rename = "unknown")]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Element {
    pub term: String,
    pub kind: String,
    pub desc: String,
    pub anchor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    pub schema: String,
    pub id: String,
    pub card_key: String,
    pub title: Option<String>,
    pub source_url: Option<String>,
    pub scope: Scope,
    pub access: Access,
    pub topic: Option<String>,
    pub status: Option<String>,
    pub evidence_type: Option<String>,
    pub published_at: Option<String>,
    pub collected_at: Option<String>,
    pub sha256: Option<String>,
    pub source_path: Option<String>,
    pub learning_record: Option<String>,
    pub learning_record_state: Option<String>,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default)]
    pub techniques: Vec<String>,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub new_terms: Vec<String>,
    #[serde(default)]
    pub problems: Vec<String>,
    #[serde(default)]
    pub tech_keywords: Vec<String>,
    pub skip_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reusable_elements: Vec<Element>,
    #[serde(skip)]
    pub summary: Vec<String>,
    #[serde(skip)]
    pub limits: String,
}

/// The card_key a source gets (the kit's rule): the normalized URL, else `path:<source>`. Surrounding whitespace is
/// dropped first, with the same trim `normalize_url` applies to a URL.
pub fn card_key_of(source: &str) -> String {
    let source = js_trim(source);
    normalize_url(source).unwrap_or_else(|| format!("path:{source}"))
}

/// First 16 hex chars of sha256(card_key).
pub fn card_id(card_key: &str) -> String {
    hex::encode(&Sha256::digest(card_key.as_bytes())[..8])
}

const SCHEMA: &str = "card-v1";
const H_SUMMARY: &str = "## 요약";
const H_LIMITS: &str = "## 한계";

impl Card {
    pub fn new(card_key: &str) -> Card {
        Card {
            schema: SCHEMA.to_string(),
            id: card_id(card_key),
            card_key: card_key.to_string(),
            title: None,
            source_url: None,
            scope: Scope::Private,
            access: Access::Unknown,
            topic: None,
            status: None,
            evidence_type: None,
            published_at: None,
            collected_at: None,
            sha256: None,
            source_path: None,
            learning_record: None,
            learning_record_state: None,
            entities: vec![],
            techniques: vec![],
            domains: vec![],
            new_terms: vec![],
            problems: vec![],
            tech_keywords: vec![],
            skip_reason: None,
            reusable_elements: vec![],
            summary: vec![],
            limits: String::new(),
        }
    }

    /// YAML frontmatter, then `## 요약` (one `- ` bullet per line) and `## 한계` (free text to end of file).
    pub fn render(&self) -> String {
        let yaml = serde_yaml::to_string(self).expect("card frontmatter serializes");
        let mut md = format!("---\n{yaml}---\n\n{H_SUMMARY}\n\n");
        for l in &self.summary {
            md.push_str(&format!("- {l}\n"));
        }
        md.push_str(&format!("\n{H_LIMITS}\n\n{}\n", self.limits));
        md
    }

    pub fn parse(md: &str) -> Result<Card> {
        let rest = md.strip_prefix("---\n").ok_or_else(|| Error::msg("card: missing `---` frontmatter"))?;
        let mut pos = 0;
        let mut split = None;
        for line in rest.split_inclusive('\n') {
            if line.trim_end_matches('\n') == "---" {
                split = Some((pos, pos + line.len()));
                break;
            }
            pos += line.len();
        }
        let (yaml_end, body_start) = split.ok_or_else(|| Error::msg("card: unterminated frontmatter"))?;
        let mut card: Card = serde_yaml::from_str(&rest[..yaml_end]).map_err(|e| Error::msg(format!("card: invalid frontmatter ({e})")))?;
        if card.schema != SCHEMA {
            return Err(Error::msg(format!("card: unsupported schema {:?} (expected {SCHEMA})", card.schema)));
        }

        #[derive(PartialEq)]
        enum Sec { Head, Summary, Limits }
        let (mut sec, mut seen_summary, mut seen_limits) = (Sec::Head, false, false);
        let mut limits: Vec<&str> = vec![];
        for line in rest[body_start..].lines() {
            if line == H_SUMMARY || line == H_LIMITS {
                let (flag, next) = if line == H_SUMMARY { (&mut seen_summary, Sec::Summary) } else { (&mut seen_limits, Sec::Limits) };
                if *flag {
                    return Err(Error::msg(format!("card: duplicate section {line:?}")));
                }
                *flag = true;
                sec = next;
                continue;
            }
            match sec {
                Sec::Limits => limits.push(line),
                _ if line.trim().is_empty() => {}
                Sec::Summary => match line.strip_prefix("- ").or(if line == "-" { Some("") } else { None }) {
                    Some(item) => card.summary.push(item.to_string()),
                    None => return Err(Error::msg(format!("card: unexpected line in {H_SUMMARY}: {line:?}"))),
                },
                Sec::Head => return Err(Error::msg(format!("card: text before the first section: {line:?}"))),
            }
        }
        card.limits = limits.join("\n").trim_matches('\n').to_string();
        Ok(card)
    }

    /// Convert one kit JSONL card (card-v1 L1 record) into a Card. Fails when the content cannot survive a
    /// render/parse round trip (e.g. a newline inside a summary line), so nothing is silently altered.
    pub fn from_kit(v: &Value, vocab: &Vocab, scope: Scope) -> Result<Card> {
        let key = match v.get("card_key").and_then(Value::as_str) {
            Some(k) if !k.trim().is_empty() => k,
            _ => return Err(Error::msg("card_key: required string")),
        };
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let list = |k: &str| -> Vec<String> {
            v.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
        };
        let mut c = Card::new(key);
        c.title = s("title");
        c.source_url = s("source_url");
        c.scope = scope;
        c.access = match v.get("access_tier").and_then(Value::as_str) {
            Some("A") => Access::A,
            Some("B") => Access::B,
            _ => Access::Unknown,
        };
        c.topic = s("topic");
        c.status = s("status");
        c.evidence_type = s("evidence_type");
        c.published_at = s("published_at");
        c.collected_at = s("collected_at");
        c.sha256 = s("sha256");
        c.source_path = s("path");
        c.learning_record = s("learning_record");
        c.learning_record_state = s("learning_record_state");
        c.skip_reason = s("skip_reason");
        c.tech_keywords = list("tech_keywords");
        c.problems = list("problem_keywords");
        c.summary = list("summary_3l");
        c.limits = s("limits").unwrap_or_default();
        if let Some(els) = v.get("reusable_elements").filter(|e| !e.is_null()) {
            c.reusable_elements = serde_json::from_value(els.clone()).map_err(|e| Error::msg(format!("reusable_elements: {e}")))?;
        }
        let mut terms = c.tech_keywords.clone();
        terms.extend(c.reusable_elements.iter().map(|e| e.term.clone()));
        let got = vocab.classify_terms(&terms);
        (c.entities, c.techniques, c.domains, c.new_terms) = (got.entities, got.techniques, got.domains, got.new_terms);
        match Card::parse(&c.render()) {
            Ok(back) if back == c => Ok(c),
            _ => Err(Error::msg(format!("{key}: content is not representable in card-v1 markdown (newline in a summary line, or limits that start/end with a blank line or contain a section heading)"))),
        }
    }
}

/// Parse one JSONL line of the kit. JavaScript's JSON.parse accepts a lone UTF-16 surrogate escape
/// (e.g. a title truncated inside an emoji: `\ud835`), which Rust strings cannot hold; such an escape
/// becomes U+FFFD, valid surrogate pairs are untouched.
pub fn parse_kit_line(line: &str) -> Result<Value> {
    serde_json::from_str(&replace_lone_surrogates(line)).map_err(|e| Error::msg(format!("invalid JSON ({e})")))
}

fn hex4(s: &str) -> Option<u32> {
    if s.len() == 4 && s.bytes().all(|b| b.is_ascii_hexdigit()) { u32::from_str_radix(s, 16).ok() } else { None }
}

fn replace_lone_surrogates(line: &str) -> String {
    let b = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            let c = line[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
            continue;
        }
        let code = if b.get(i + 1) == Some(&b'u') { line.get(i + 2..i + 6).and_then(hex4) } else { None };
        match code {
            Some(0xD800..=0xDBFF) => {
                let low = if line.get(i + 6..i + 8) == Some("\\u") { line.get(i + 8..i + 12).and_then(hex4) } else { None };
                if low.is_some_and(|l| (0xDC00..=0xDFFF).contains(&l)) {
                    out.push_str(&line[i..i + 12]);
                    i += 12;
                } else {
                    out.push_str("\\ufffd");
                    i += 6;
                }
            }
            Some(0xDC00..=0xDFFF) => {
                out.push_str("\\ufffd");
                i += 6;
            }
            Some(_) => {
                out.push_str(&line[i..i + 6]);
                i += 6;
            }
            None => {
                // Any other escape: keep the backslash and the escaped ASCII char together so an escaped backslash is not re-read as an escape start.
                out.push('\\');
                i += 1;
                if let Some(&n) = b.get(i) {
                    if n.is_ascii() {
                        out.push(n as char);
                        i += 1;
                    }
                }
            }
        }
    }
    out
}
