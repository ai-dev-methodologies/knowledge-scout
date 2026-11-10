//! Controlled vocabulary (port of vocab.mjs).
use crate::normalize::{hyphenate, js_len, js_trim};
use crate::{Error, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Facet {
    Entity,
    Technique,
    Domain,
}

impl Facet {
    pub fn as_str(self) -> &'static str {
        match self {
            Facet::Entity => "entity",
            Facet::Technique => "technique",
            Facet::Domain => "domain",
        }
    }
    fn parse(s: &str) -> Option<Facet> {
        match s {
            "entity" => Some(Facet::Entity),
            "technique" => Some(Facet::Technique),
            "domain" => Some(Facet::Domain),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub term: String,
    pub facet: Facet,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermEntry {
    pub term: String,
    pub facet: Facet,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Classified {
    pub entities: Vec<String>,
    pub techniques: Vec<String>,
    pub domains: Vec<String>,
    pub new_terms: Vec<String>,
}

#[derive(Debug)]
pub struct Vocab {
    pub terms: Vec<TermEntry>,
    /// normalized canonical term or alias -> (canonical term, facet)
    index: HashMap<String, Resolved>,
}

const ARTICLES: [&str; 3] = ["a", "an", "the"];

pub fn normalize_term(s: &str) -> String {
    let h = hyphenate(&js_trim(s).to_lowercase());
    let h = h.strip_prefix('-').unwrap_or(&h);
    let t = h.strip_suffix('-').unwrap_or(h);
    if let Some(i) = t.find('-') {
        // A leading article is dropped only when a real word follows: "a-b-testing" keeps its "a-".
        let rest = &t[i + 1..];
        if i > 0 && ARTICLES.contains(&&t[..i]) && js_len(rest.split('-').next().unwrap_or("")) >= 2 {
            return rest.to_string();
        }
    }
    t.to_string()
}

/// Strip one trailing 's' only when the singular form satisfies `has`.
pub fn fold_plural(term: &str, has: impl Fn(&str) -> bool) -> String {
    if term.chars().count() > 1 && term.ends_with('s') && has(&term[..term.len() - 1]) {
        term[..term.len() - 1].to_string()
    } else {
        term.to_string()
    }
}

impl Vocab {
    /// Load `.yaml`/`.yml` as YAML, anything else as JSON.
    pub fn load(path: &Path) -> Result<Vocab> {
        Ok(Vocab::load_with_fingerprint(path)?.0)
    }

    /// `load`, plus the sha256 (hex) of the file bytes that were parsed: the vocabulary fingerprint an index is built
    /// with and checked against (same value as `index::Fingerprints::of_files` gives for an unchanged file).
    pub fn load_with_fingerprint(path: &Path) -> Result<(Vocab, String)> {
        let label = path.display().to_string();
        let src = std::fs::read_to_string(path).map_err(|e| Error::msg(format!("{label}: cannot read vocabulary ({e})")))?;
        let fp = hex::encode(Sha256::digest(src.as_bytes()));
        let vocab = match path.extension().and_then(|e| e.to_str()) {
            Some("yaml") | Some("yml") => Vocab::from_yaml(&src, &label),
            _ => Vocab::from_json(&src, &label),
        }?;
        Ok((vocab, fp))
    }

    pub fn from_json(src: &str, label: &str) -> Result<Vocab> {
        let doc: Value = serde_json::from_str(src).map_err(|e| Error::msg(format!("{label}: cannot read vocabulary JSON ({e})")))?;
        Vocab::from_value(&doc, label)
    }

    pub fn from_yaml(src: &str, label: &str) -> Result<Vocab> {
        let doc: Value = serde_yaml::from_str(src).map_err(|e| Error::msg(format!("{label}: cannot read vocabulary YAML ({e})")))?;
        Vocab::from_value(&doc, label)
    }

    fn from_value(doc: &Value, label: &str) -> Result<Vocab> {
        let version_ok = doc.get("version").and_then(Value::as_f64) == Some(1.0);
        let list = doc.get("terms").and_then(Value::as_array);
        let list = match (version_ok, list) {
            (true, Some(l)) => l,
            _ => return Err(Error::msg(format!("{label}: expected {{\"version\": 1, \"terms\": [...]}}; unsupported version"))),
        };
        let mut index: HashMap<String, Resolved> = HashMap::new();
        let mut claim = |key: &str, entry: &TermEntry, what: &str| -> Result<()> {
            if let Some(prev) = index.get(key) {
                return Err(Error::msg(format!("{label}: {what} \"{key}\" collides with \"{}\"", prev.term)));
            }
            index.insert(key.to_string(), Resolved { term: entry.term.clone(), facet: entry.facet });
            Ok(())
        };
        let mut terms = Vec::with_capacity(list.len());
        for e in list {
            let term = match e.get("term").and_then(Value::as_str) {
                Some(t) if !js_trim(t).is_empty() => t,
                _ => return Err(Error::msg(format!("{label}: entry with empty term"))),
            };
            if normalize_term(term) != term {
                return Err(Error::msg(format!("{label}: term \"{term}\" is not in normalized form (\"{}\")", normalize_term(term))));
            }
            let facet = e.get("facet").and_then(Value::as_str).and_then(Facet::parse);
            let facet = facet.ok_or_else(|| Error::msg(format!("{label}: term \"{term}\" has invalid facet {}", e.get("facet").map_or("undefined".into(), |f| f.to_string()))))?;
            let aliases = match e.get("aliases") {
                None | Some(Value::Null) => vec![],
                Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
                Some(_) => return Err(Error::msg(format!("{label}: term \"{term}\" aliases must be an array"))),
            };
            terms.push(TermEntry { term: term.to_string(), facet, aliases });
        }
        for e in &terms {
            claim(&e.term, e, "term")?;
        }
        for e in &terms {
            for al in &e.aliases {
                let key = normalize_term(al);
                if !key.is_empty() && key != e.term {
                    claim(&key, e, "alias")?;
                }
            }
        }
        Ok(Vocab { terms, index })
    }

    pub fn resolve(&self, term: &str) -> Option<Resolved> {
        let t = normalize_term(term);
        if t.is_empty() {
            return None;
        }
        let hit = self.index.get(&t).or_else(|| self.index.get(&fold_plural(&t, |s| self.index.contains_key(s))));
        hit.cloned()
    }

    /// Resolve raw terms into deduped canonical facet buckets plus unresolved normalized terms.
    pub fn classify_terms<S: AsRef<str>>(&self, terms: &[S]) -> Classified {
        let mut got = Classified::default();
        for raw in terms {
            let raw = raw.as_ref();
            match self.resolve(raw) {
                Some(r) => {
                    let bucket = match r.facet {
                        Facet::Entity => &mut got.entities,
                        Facet::Technique => &mut got.techniques,
                        Facet::Domain => &mut got.domains,
                    };
                    if !bucket.contains(&r.term) {
                        bucket.push(r.term);
                    }
                }
                None => {
                    let n = normalize_term(raw);
                    if !n.is_empty() && !got.new_terms.contains(&n) {
                        got.new_terms.push(n);
                    }
                }
            }
        }
        got
    }
}
