//! Candidate collection (port of collect_candidates.mjs).
use crate::normalize::{js_cmp, js_len, js_trim};
use crate::{card::Card, vocab::Vocab, Error, Result};
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub qid: String,
    pub query: String,
    pub terms: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Candidate {
    pub card_key: String,
    pub path: Option<String>,
    pub title: Option<String>,
    pub matched_terms: Vec<String>,
    pub text_hits: Vec<String>,
    pub rank: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScopeCount {
    pub l1_cards: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QueryResult {
    pub qid: String,
    pub scope: ScopeCount,
    pub candidates: Vec<Candidate>,
    /// Query terms the vocabulary did not resolve (normalization and aliases included); they can only match as text.
    pub unknown_terms: Vec<String>,
}

/// Validate `[{qid, query, terms?}]` with the same rules as the node collector.
pub fn parse_queries(v: &Value) -> Result<Vec<Query>> {
    let list = v.as_array().ok_or_else(|| Error::msg("--queries must be a JSON array of {qid, query, terms?}"))?;
    let mut out = Vec::with_capacity(list.len());
    for (i, q) in list.iter().enumerate() {
        let qid = match q.get("qid").and_then(Value::as_str) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return Err(Error::msg(format!("query {i}: qid required"))),
        };
        let query = q.get("query").and_then(Value::as_str).ok_or_else(|| Error::msg(format!("query {qid}: query must be a string")))?.to_string();
        let terms = match q.get("terms") {
            None => None,
            Some(Value::Array(a)) if a.iter().all(Value::is_string) => Some(a.iter().map(|t| t.as_str().unwrap().to_string()).collect()),
            Some(_) => return Err(Error::msg(format!("query {qid}: terms must be an array of strings"))),
        };
        let q = Query { qid, query, terms };
        check_query_limits(&q)?;
        out.push(q);
    }
    Ok(out)
}

pub const MAX_TERMS: usize = 32;
pub const MAX_TERM_CHARS: usize = 64;
pub const MAX_QUERY_CHARS: usize = 512;

/// Input caps for one query (CLI and MCP): at most 32 terms of at most 64 characters each, and a query
/// text of at most 512 characters. Every term (or query word) becomes an infix clause in the Redis
/// query, so unbounded input would mean unbounded server work.
pub fn check_query_limits(q: &Query) -> Result<()> {
    if q.query.chars().count() > MAX_QUERY_CHARS {
        return Err(Error::msg(format!("query {}: query must be at most {MAX_QUERY_CHARS} characters", q.qid)));
    }
    if let Some(ts) = &q.terms {
        if ts.len() > MAX_TERMS {
            return Err(Error::msg(format!("query {}: at most {MAX_TERMS} terms", q.qid)));
        }
        if ts.iter().any(|t| t.chars().count() > MAX_TERM_CHARS) {
            return Err(Error::msg(format!("query {}: each term must be at most {MAX_TERM_CHARS} characters", q.qid)));
        }
    }
    Ok(())
}

pub(crate) static NON_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\p{L}\p{N}]+").unwrap());

/// `s.toLowerCase().split(/[^\p{L}\p{N}]+/u).filter(t => t.length >= 2)` (length in UTF-16 units).
fn tokenize(s: &str) -> Vec<String> {
    NON_WORD.split(&s.to_lowercase()).filter(|t| js_len(t) >= 2).map(str::to_string).collect()
}

fn uniq(xs: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    xs.into_iter().filter(|x| seen.insert(x.clone())).collect()
}

/// A query reduced to what matching uses: lowercased text terms and the canonical vocabulary terms they resolve to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryTerms {
    pub text_terms: Vec<String>,
    pub canon: Vec<String>,
}

pub fn query_terms(q: &Query, vocab: &Vocab) -> QueryTerms {
    let raw: Vec<String> = match &q.terms {
        Some(ts) => ts.iter().map(|t| js_trim(t).to_lowercase()).filter(|t| !t.is_empty()).collect(),
        None => tokenize(&q.query),
    };
    let text_terms = uniq(raw.iter().cloned());
    let canon = uniq(raw.iter().filter_map(|t| vocab.resolve(t).map(|r| r.term)));
    QueryTerms { text_terms, canon }
}

/// The terms of `t` the vocabulary does not know: no normalization, alias or plural fold resolves them, so they can only
/// match as text. Reported with every result so that a term missing from the vocabulary is not a silent empty result.
pub fn unknown_terms(t: &QueryTerms, vocab: &Vocab) -> Vec<String> {
    t.text_terms.iter().filter(|x| vocab.resolve(x).is_none()).cloned().collect()
}

/// One card prepared for matching: its canonical terms and its lowercased searchable text.
pub struct Entry<'a> {
    card: &'a Card,
    terms: HashSet<String>,
    text: String,
}

/// The canonical terms of a card under `vocab`: `tech_keywords` and `reusable_elements[].term` classified now, not the
/// `entities`/`techniques`/`domains` stored in the card (those are only as current as the vocabulary at import time).
/// The one definition of a card's terms: file matching (`Entry`) and the Redis `terms` TAG both use it.
pub fn card_terms(card: &Card, vocab: &Vocab) -> Vec<String> {
    let mut kw = card.tech_keywords.clone();
    kw.extend(card.reusable_elements.iter().map(|e| e.term.clone()));
    let set = vocab.classify_terms(&kw);
    set.entities.into_iter().chain(set.techniques).chain(set.domains).collect()
}

impl<'a> Entry<'a> {
    pub fn new(card: &'a Card, vocab: &Vocab) -> Entry<'a> {
        let mut parts: Vec<&str> = vec![];
        parts.extend(card.title.as_deref());
        parts.extend(card.summary.iter().map(String::as_str));
        parts.extend(card.problems.iter().map(String::as_str));
        parts.push(&card.limits);
        parts.extend(card.reusable_elements.iter().map(|e| e.desc.as_str()));
        Entry { card, terms: card_terms(card, vocab).into_iter().collect(), text: parts.join("\n").to_lowercase() }
    }
}

/// A ranked card with the evidence that matched it.
#[derive(Debug, Clone)]
pub struct Found {
    pub card: Card,
    pub matched_terms: Vec<String>,
    pub text_hits: Vec<String>,
    pub rank: usize,
}

/// The ranked candidates of one query and the size of the searched pool.
#[derive(Debug, Clone)]
pub struct Ranking {
    pub qid: String,
    pub l1_cards: usize,
    pub found: Vec<Found>,
    /// The query's terms that `vocab.resolve` could not resolve, in query order (see `QueryResult::unknown_terms`).
    pub unknown_terms: Vec<String>,
}

impl Ranking {
    pub fn into_result(self) -> QueryResult {
        QueryResult {
            qid: self.qid,
            scope: ScopeCount { l1_cards: self.l1_cards },
            candidates: self
                .found
                .into_iter()
                .map(|f| Candidate { card_key: f.card.card_key, path: f.card.source_path, title: f.card.title, matched_terms: f.matched_terms, text_hits: f.text_hits, rank: f.rank })
                .collect(),
            unknown_terms: self.unknown_terms,
        }
    }
}

struct Ranked<'e, 'a> {
    e: &'e Entry<'a>,
    matched: Vec<String>,
    hits: Vec<String>,
    rarest: usize,
}

/// Rank `entries` for one query: matched terms desc, text hits desc, rarest matched term (lowest document
/// frequency, from `df`) asc, card_key asc (UTF-16 order). Truncate to `limit` after ranking. Entries that
/// match nothing are dropped, so `entries` may be any superset of the matching cards.
pub fn rank_entries(entries: &[&Entry], terms: &QueryTerms, limit: usize, df: &dyn Fn(&str) -> usize) -> Vec<Found> {
    let mut ranked: Vec<Ranked> = vec![];
    for &e in entries {
        let matched: Vec<String> = terms.canon.iter().filter(|t| e.terms.contains(*t)).cloned().collect();
        let hits: Vec<String> = terms.text_terms.iter().filter(|t| e.text.contains(t.as_str())).cloned().collect();
        if matched.is_empty() && hits.is_empty() {
            continue;
        }
        let rarest = matched.iter().map(|t| df(t)).min().unwrap_or(usize::MAX);
        ranked.push(Ranked { e, matched, hits, rarest });
    }
    ranked.sort_by(|x, y| {
        y.matched.len().cmp(&x.matched.len())
            .then(y.hits.len().cmp(&x.hits.len()))
            .then(x.rarest.cmp(&y.rarest))
            .then_with(|| js_cmp(&x.e.card.card_key, &y.e.card.card_key))
    });
    ranked
        .into_iter()
        .take(limit)
        .enumerate()
        .map(|(i, r)| Found { card: r.e.card.clone(), matched_terms: r.matched, text_hits: r.hits, rank: i + 1 })
        .collect()
}

/// Candidate pool = cards with `skip_reason` null, ranked as described on [`rank_entries`].
pub fn collect_ranked(cards: &[Card], vocab: &Vocab, queries: &[Query], limit: usize) -> Vec<Ranking> {
    let entries: Vec<Entry> = cards.iter().filter(|c| c.skip_reason.is_none()).map(|c| Entry::new(c, vocab)).collect();
    let mut df: HashMap<&str, usize> = HashMap::new();
    for e in &entries {
        for t in &e.terms {
            *df.entry(t.as_str()).or_default() += 1;
        }
    }
    let all: Vec<&Entry> = entries.iter().collect();
    queries
        .iter()
        .map(|q| {
            let t = query_terms(q, vocab);
            Ranking { qid: q.qid.clone(), l1_cards: entries.len(), found: rank_entries(&all, &t, limit, &|term| df[term]), unknown_terms: unknown_terms(&t, vocab) }
        })
        .collect()
}

pub fn collect(cards: &[Card], vocab: &Vocab, queries: &[Query], limit: usize) -> Vec<QueryResult> {
    collect_ranked(cards, vocab, queries, limit).into_iter().map(Ranking::into_result).collect()
}

/// Paths of every `*.md` under `dir`, recursive, in sorted path order. Symlinks (to files or
/// directories) are skipped, never followed: cards are only the regular files inside `dir`.
pub fn md_paths(dir: &Path) -> Result<Vec<PathBuf>> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        let rd = std::fs::read_dir(dir).map_err(|e| Error::msg(format!("{}: {e}", dir.display())))?;
        let mut entries: Vec<PathBuf> = rd.map(|e| e.map(|e| e.path())).collect::<std::io::Result<_>>().map_err(|e| Error::msg(format!("{}: {e}", dir.display())))?;
        entries.sort();
        for p in entries {
            let ft = std::fs::symlink_metadata(&p).map_err(|e| Error::msg(format!("{}: {e}", p.display())))?.file_type();
            if ft.is_dir() {
                walk(&p, out)?;
            } else if ft.is_file() && p.extension().is_some_and(|x| x == "md") {
                out.push(p);
            }
        }
        Ok(())
    }
    let mut out = vec![];
    walk(dir, &mut out)?;
    Ok(out)
}

/// Load every `*.md` card under `dir` (recursive, sorted by path). A file that fails to parse is an error.
pub fn load_cards(dir: &Path) -> Result<Vec<Card>> {
    md_paths(dir)?
        .into_iter()
        .map(|p| {
            let src = std::fs::read_to_string(&p).map_err(|e| Error::msg(format!("{}: {e}", p.display())))?;
            Card::parse(&src).map_err(|e| Error::msg(format!("{}: {e}", p.display())))
        })
        .collect()
}
