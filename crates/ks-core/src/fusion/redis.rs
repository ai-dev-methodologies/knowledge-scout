//! The fusion index in Redis and its searcher (r4-unified-redis.mjs indexCorpus / candidates / searchRedis).
//!
//! Keys, all under one prefix (default `ksf:`; only `ksf:` and `ksf-<name>:` are accepted, never `ks:` or `ks-eval-*`, and
//! never a prefix overlapping KS_KEY_PREFIX):
//!   <P>c:<n>  HASH per card (n = ordinal): f0..f7 token codes (TEXT NOSTEM, weights 3 3 1 2 2 1 1 1), v = vocabulary term
//!             codes (weight 3), optionally x = enrichment text codes (weight --enrich-weight), p = payload, id / key /
//!             title / scope (not indexed), optionally e = the card vector (1,024 FLOAT16, not indexed)
//!   <P>t      HASH token table: token code -> df + association row
//!   <P>m      HASH meta: schema ks-fusion-index, build id, recipe, model id and sha256, card count, vectors, average field
//!             lengths, the cards fingerprint, the vocabulary, alias dictionary and model files the index was built with,
//!             complete = 1 (written last; a build writes complete = 0 with its schema before the first card)
//!   <P>idx    the RediSearch index over <P>c:, created after the hashes exist (one background scan; creating it first was
//!             measured to leave transient scores), then waited on until it has settled.
//! A query: HMGET token table, one FT.SEARCH ... SCORER BM25STD RETURN p id key title [e] LIMIT 0 400, then the ranking
//! in process. After every search the meta is read again (one HMGET): a changed build id or an incomplete index drops the
//! result with an error, as `ks search` does for its index.
//! A build writes only to an empty prefix or over a fusion index (<P>m says so) with no key of plain ks under it
//! (<P>meta, <P>apply, <P>rebuild-lock, <P>card:*, <P>auth:*) and no key of a shape the build does not write itself
//! (it writes only <P>m, <P>t, <P>c:<n>); everything it needs is read and checked before the old index is deleted.
//! A build that fails while writing leaves an incomplete index (complete = 0) that searches refuse: run the build again.
//! Ceiling: a rebuild is not locked against a concurrent rebuild of the same prefix (one operator, one build).
use super::analyzer::{code, tokenize, Linker};
use super::build::{BuiltIndex, STAGE1_VT, STAGE1_W};
use super::embed::Embedder;
use super::model::{Model, CANDIDATES, DIM, RECIPE};
use super::payload::{decode_payload, from_f16_bytes, to_f16_bytes, unpack_token_row, vector_ok, Payload, NF};
use super::plan::{expand_plan, fill_idf, has_terms, plan_query, stage1_query, stage1_string};
use super::rank::rank_unified;
use super::switch::{begin_path, settle_path, Switch};
use crate::index::rerr;
use crate::vocab::Vocab;
use crate::{Error, Result};
use redis::{Connection, Value};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

pub const DEFAULT_PREFIX: &str = "ksf:";

/// The fusion namespace only: `ksf:` or `ksf-<name>:` (lowercase letters, digits, hyphens). `ks:` and `ks-eval-<name>:`
/// belong to plain ks (its index, cards, application stream and API keys live there) and are refused.
pub fn check_prefix(p: &str) -> Result<()> {
    let name_ok = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && !s.starts_with('-');
    if p == DEFAULT_PREFIX || p.strip_prefix("ksf-").and_then(|r| r.strip_suffix(':')).is_some_and(name_ok) {
        Ok(())
    } else {
        Err(Error::msg(format!("fusion: refusing the key prefix {p:?} (allowed: ksf: or ksf-<name>:)")))
    }
}

/// `check_prefix`, and the prefix must not equal or overlap the key prefix plain ks uses (`KS_KEY_PREFIX`, default `ks:`):
/// neither may start with the other.
pub fn check_prefix_with(p: &str, ks_prefix: Option<&str>) -> Result<()> {
    check_prefix(p)?;
    if let Some(k) = ks_prefix.filter(|k| !k.is_empty()) {
        if p.starts_with(k) || k.starts_with(p) {
            return Err(Error::msg(format!("fusion: the key prefix {p:?} overlaps the ks key prefix {k:?} (KS_KEY_PREFIX)")));
        }
    }
    Ok(())
}

/// What a key under a fusion prefix is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    /// a key a fusion build writes: <p>m, <p>t, <p>c:<card ordinal>
    Fusion,
    /// a key plain ks writes: <p>meta, <p>apply, <p>rebuild-lock, <p>card:*, <p>auth:*
    Ks,
    /// anything else
    Foreign,
}

/// The kind of `key` under `prefix`. The fusion build writes exactly <p>m, <p>t and <p>c:<n> (n = decimal card ordinal),
/// plus the RediSearch index <p>idx, which is not a key.
pub fn classify(prefix: &str, key: &str) -> KeyKind {
    let Some(rest) = key.strip_prefix(prefix) else { return KeyKind::Foreign };
    if rest == "m" || rest == "t" || rest.strip_prefix("c:").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) {
        return KeyKind::Fusion;
    }
    if ["meta", "apply", "rebuild-lock"].contains(&rest) || rest.starts_with("card:") || rest.starts_with("auth:") {
        return KeyKind::Ks;
    }
    KeyKind::Foreign
}

/// What is already under a prefix (ks_keys and foreign list at most 20 keys each).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Found {
    /// some key exists under the prefix
    pub any_key: bool,
    /// <prefix>m says schema ks-fusion-index
    pub fusion_meta: bool,
    /// a RediSearch index named <prefix>idx exists
    pub index: bool,
    /// keys plain ks writes
    pub ks_keys: Vec<String>,
    /// keys neither a fusion build nor plain ks writes
    pub foreign: Vec<String>,
}

/// A build may replace what is under a prefix only when it is empty or a fusion index (its meta says so) and every key
/// there has a shape the fusion build writes. Ownership is positive: a fusion meta does not make other keys deletable.
pub fn may_replace(f: &Found) -> Result<()> {
    if !f.ks_keys.is_empty() {
        return Err(Error::msg(format!("fusion: refusing to write: keys of plain ks exist under this prefix ({}); nothing was deleted", f.ks_keys.join(", "))));
    }
    if !f.foreign.is_empty() {
        return Err(Error::msg(format!("fusion: refusing to write: keys a fusion build does not write exist under this prefix ({}); nothing was deleted", f.foreign.join(", "))));
    }
    if (f.any_key || f.index) && !f.fusion_meta {
        return Err(Error::msg("fusion: refusing to write: the prefix holds keys or an index that are not a fusion index (no <prefix>m with schema ks-fusion-index); nothing was deleted"));
    }
    Ok(())
}

/// Look at what is under a (checked) prefix: one SCAN over every key there, each classified.
pub fn inspect(con: &mut Connection, prefix: &str) -> Result<Found> {
    check_prefix(prefix)?;
    let schema: Option<String> = redis::cmd("HGET").arg(format!("{prefix}m")).arg("schema").query(con).map_err(rerr)?;
    let list: Vec<String> = redis::cmd("FT._LIST").query(con).map_err(rerr)?;
    let keys = scan(con, &format!("{prefix}*"))?;
    let mut f = Found { any_key: !keys.is_empty(), fusion_meta: schema.as_deref() == Some("ks-fusion-index"), index: list.iter().any(|n| *n == format!("{prefix}idx")), ..Found::default() };
    for k in keys {
        match classify(prefix, &k) {
            KeyKind::Fusion => {}
            KeyKind::Ks if f.ks_keys.len() < 20 => f.ks_keys.push(k),
            KeyKind::Foreign if f.foreign.len() < 20 => f.foreign.push(k),
            _ => {}
        }
    }
    f.ks_keys.sort();
    f.foreign.sort();
    Ok(f)
}

/// Everything a build checks about its target before any work: the prefix rules, the ks prefix, and that what is
/// there may be replaced.
pub fn check_target(con: &mut Connection, prefix: &str, ks_prefix: Option<&str>) -> Result<()> {
    check_prefix_with(prefix, ks_prefix)?;
    may_replace(&inspect(con, prefix)?)
}

pub fn connect(url: &str) -> Result<Connection> {
    let client = redis::Client::open(url).map_err(rerr)?;
    let con = client.get_connection_with_timeout(Duration::from_secs(5)).map_err(rerr)?;
    con.set_read_timeout(Some(Duration::from_secs(120))).map_err(rerr)?;
    Ok(con)
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::BulkString(b) => String::from_utf8(b.clone()).ok(),
        Value::SimpleString(s) => Some(s.clone()),
        Value::Int(n) => Some(n.to_string()),
        Value::Double(d) => Some(d.to_string()),
        _ => None,
    }
}

fn scan(con: &mut Connection, pattern: &str) -> Result<Vec<String>> {
    let mut keys = vec![];
    let mut cursor = "0".to_string();
    loop {
        let (next, page): (String, Vec<String>) = redis::cmd("SCAN").arg(&cursor).arg("MATCH").arg(pattern).arg("COUNT").arg(2000).query(con).map_err(rerr)?;
        keys.extend(page);
        cursor = next;
        if cursor == "0" {
            return Ok(keys);
        }
    }
}

/// Drop the fusion index and its keys under the prefix. The guard runs here, so no caller can skip it: refused unless
/// `may_replace` passes, and only keys of the shapes a fusion build writes are deleted (a key that appears in between is
/// left alone). Returns the number of keys deleted.
fn purge(con: &mut Connection, prefix: &str) -> Result<usize> {
    check_prefix(prefix)?;
    may_replace(&inspect(con, prefix)?)?;
    let r: redis::RedisResult<()> = redis::cmd("FT.DROPINDEX").arg(format!("{prefix}idx")).query(con);
    if let Err(e) = r {
        let m = e.to_string().to_lowercase();
        if !(m.contains("not found") || m.contains("unknown index") || m.contains("no such index")) {
            return Err(rerr(e));
        }
    }
    let keys: Vec<String> = scan(con, &format!("{prefix}*"))?.into_iter().filter(|k| classify(prefix, k) == KeyKind::Fusion).collect();
    for chunk in keys.chunks(500) {
        let _: () = redis::cmd("UNLINK").arg(chunk).query(con).map_err(rerr)?;
    }
    Ok(keys.len())
}

/// What a build writes besides the cards: the inputs the searcher needs (as file contents) and the optional extras.
pub struct BuildSources<'a> {
    pub vocab_json: &'a str,
    pub aliases_json: Option<&'a str>,
    pub model: &'a Model,
    /// one vector (or none) per card, in card order
    pub vectors: Option<Vec<Option<Vec<f32>>>>,
    /// enrichment text per card ordinal and its first-stage field weight (>= 1)
    pub enrich: Option<(HashMap<usize, String>, u32)>,
    pub extra_meta: Vec<(String, String)>,
}

#[derive(Debug, Serialize)]
pub struct BuildReport {
    pub prefix: String,
    pub cards: usize,
    pub vectors: usize,
    pub enriched: usize,
    pub tokens: usize,
    pub seconds: f64,
}

fn random_hex() -> Result<String> {
    use std::io::Read;
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).map_err(|e| Error::msg(format!("/dev/urandom: {e}")))?;
    Ok(hex::encode(b))
}

/// Write an in-process index to Redis under `prefix`, replacing what is there (only an empty prefix or a fusion index,
/// see `may_replace`). Every vector must have the model's dimension and finite values.
pub fn write_index(con: &mut Connection, prefix: &str, idx: &BuiltIndex, src: &BuildSources) -> Result<BuildReport> {
    check_prefix(prefix)?;
    if let Some(vs) = &src.vectors {
        if vs.len() != idx.n || vs.iter().flatten().any(|v| !vector_ok(v, DIM)) {
            return Err(Error::msg(format!("fusion: one vector or none per card, each of {DIM} finite values")));
        }
    }
    let t0 = Instant::now();
    purge(con, prefix)?;
    let build_id = random_hex()?;
    let _: () = redis::cmd("HSET").arg(format!("{prefix}m")).arg("schema").arg("ks-fusion-index").arg("build_id").arg(&build_id).arg("complete").arg("0").query(con).map_err(rerr)?;
    let mut nv = 0;
    let mut ne = 0;
    for start in (0..idx.n).step_by(200) {
        let mut pipe = redis::pipe();
        for i in start..(start + 200).min(idx.n) {
            let c = &idx.cards[i];
            let cmd = pipe.cmd("HSET").arg(format!("{prefix}c:{i}")).arg("p").arg(&idx.payloads[i][..]).arg("id").arg(&c.id).arg("key").arg(&c.key).arg("title").arg(c.title.as_deref().unwrap_or("")).arg("scope").arg(&c.scope);
            for (k, v) in idx.text_fields(i) {
                cmd.arg(k).arg(v);
            }
            if let Some((texts, _)) = &src.enrich {
                if let Some(t) = texts.get(&i) {
                    let codes = tokenize(t, false, false).iter().map(|t| code(super::analyzer::fnv1a32(t))).collect::<Vec<_>>().join(" ");
                    if !codes.is_empty() {
                        cmd.arg("x").arg(codes);
                        ne += 1;
                    }
                }
            }
            if let Some(Some(v)) = src.vectors.as_ref().map(|vs| &vs[i]) {
                cmd.arg("e").arg(to_f16_bytes(v));
                nv += 1;
            }
            cmd.ignore();
        }
        pipe.query::<()>(con).map_err(rerr)?;
    }
    for chunk in idx.df_order.chunks(2000) {
        let mut cmd = redis::cmd("HSET");
        cmd.arg(format!("{prefix}t"));
        for &h in chunk {
            cmd.arg(code(h)).arg(super::payload::pack_token_row(idx.df[&h], &idx.assoc_row(h)));
        }
        let _: () = cmd.query(con).map_err(rerr)?;
    }
    let mut create = redis::cmd("FT.CREATE");
    create.arg(format!("{prefix}idx")).arg("ON").arg("HASH").arg("PREFIX").arg(1).arg(format!("{prefix}c:")).arg("NOOFFSETS").arg("STOPWORDS").arg(0).arg("SCHEMA");
    for f in 0..NF {
        create.arg(format!("f{f}")).arg("TEXT").arg("NOSTEM").arg("WEIGHT").arg(STAGE1_W[f]);
    }
    create.arg("v").arg("TEXT").arg("NOSTEM").arg("WEIGHT").arg(STAGE1_VT);
    if let Some((_, w)) = &src.enrich {
        create.arg("x").arg("TEXT").arg("NOSTEM").arg("WEIGHT").arg(*w);
    }
    let _: () = create.query(con).map_err(rerr)?;
    for tries in 0.. {
        let info: Vec<Value> = redis::cmd("FT.INFO").arg(format!("{prefix}idx")).query(con).map_err(rerr)?;
        let at = |k: &str| info.iter().position(|v| text(v).as_deref() == Some(k)).and_then(|i| info.get(i + 1)).and_then(text).unwrap_or_default();
        if at("num_docs").parse::<usize>().ok() == Some(idx.n) && at("indexing") == "0" && at("percent_indexed").parse::<f64>().ok() == Some(1.0) {
            break;
        }
        if tries > 1200 {
            return Err(Error::msg(format!("fusion: {prefix}idx has {} of {} cards after 120 s", at("num_docs"), idx.n)));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let seconds = t0.elapsed().as_secs_f64();
    let avg = serde_json::to_string(&idx.avg.to_vec()).unwrap();
    let mut meta: Vec<(String, String)> = vec![
        ("schema".into(), "ks-fusion-index".into()),
        ("build_id".into(), build_id),
        ("recipe".into(), RECIPE.into()),
        ("model_id".into(), src.model.id.clone()),
        ("model_sha256".into(), src.model.sha256.clone()),
        ("cards".into(), idx.n.to_string()),
        ("vectors".into(), nv.to_string()),
        ("enriched".into(), ne.to_string()),
        ("enrich_weight".into(), src.enrich.as_ref().map_or(0, |e| e.1).to_string()),
        ("avg".into(), avg),
        ("build_seconds".into(), format!("{seconds:.2}")),
        ("vocab_json".into(), src.vocab_json.to_string()),
        ("aliases_json".into(), src.aliases_json.unwrap_or("").to_string()),
        ("model_json".into(), src.model.text.clone()),
    ];
    meta.extend(src.extra_meta.iter().cloned());
    meta.push(("complete".into(), "1".into()));
    let _: () = redis::cmd("DEL").arg(format!("{prefix}m")).query(con).map_err(rerr)?;
    let mut hset = redis::cmd("HSET");
    hset.arg(format!("{prefix}m"));
    for (k, v) in &meta {
        hset.arg(k).arg(v);
    }
    let _: () = hset.query(con).map_err(rerr)?;
    Ok(BuildReport { prefix: prefix.to_string(), cards: idx.n, vectors: nv, enriched: ne, tokens: idx.df_order.len(), seconds })
}

/// One ranked card.
#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub rank: usize,
    pub id: String,
    pub title: String,
    /// the card's scope (private | team | public). The CLI returns every card, as `ks search` does; a reader-facing
    /// integration (`ks mcp serve --http`) must keep only scope public, as its ks_search does.
    pub scope: String,
    pub score: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub mode: &'static str,
    pub path: &'static str,
    pub reason: &'static str,
    pub candidates: usize,
    pub results: Vec<Hit>,
    pub ms: BTreeMap<&'static str, f64>,
}

/// The index under a searcher's prefix compared with the one it opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexState {
    Unchanged,
    /// complete, with another build id: open a new searcher
    Rebuilt,
    /// missing or complete != 1 (a build is running, failed, or the index was purged)
    Incomplete,
}

pub struct Searcher {
    con: Connection,
    prefix: String,
    pub n: usize,
    pub vectors: usize,
    pub avg: [f64; NF],
    pub model: Model,
    linker: Linker,
    vocab: Vocab,
    pub meta: BTreeMap<String, String>,
    build_id: String,
}

impl Searcher {
    /// Open the index under `prefix`. `model`: a model file to rank with instead of the one stored at build time.
    pub fn open(mut con: Connection, prefix: &str, model: Option<Model>) -> Result<Searcher> {
        check_prefix(prefix)?;
        let meta: BTreeMap<String, String> = redis::cmd("HGETALL").arg(format!("{prefix}m")).query(&mut con).map_err(rerr)?;
        if meta.get("complete").map(String::as_str) != Some("1") {
            return Err(Error::msg(format!("fusion: no complete index under {prefix} (run `ks fusion build`)")));
        }
        if meta.get("recipe").map(String::as_str) != Some(RECIPE) {
            return Err(Error::msg(format!("fusion: the index under {prefix} has another recipe; rebuild it")));
        }
        let get = |k: &str| meta.get(k).cloned().ok_or_else(|| Error::msg(format!("fusion: index meta without {k}")));
        let avg_v: Vec<f64> = serde_json::from_str(&get("avg")?).map_err(|_| Error::msg("fusion: bad avg in the index meta"))?;
        let avg: [f64; NF] = avg_v.try_into().map_err(|_| Error::msg("fusion: bad avg in the index meta"))?;
        let vocab = Vocab::from_json(&get("vocab_json")?, "index meta vocab_json")?;
        let aliases_text = get("aliases_json")?;
        let aliases: Option<serde_json::Value> = if aliases_text.is_empty() { None } else { Some(serde_json::from_str(&aliases_text).map_err(|_| Error::msg("fusion: bad alias dictionary in the index meta"))?) };
        let linker = Linker::new(&vocab, aliases.as_ref());
        let model = match model {
            Some(m) => m,
            None => Model::parse(&get("model_json")?)?,
        };
        let n = get("cards")?.parse().map_err(|_| Error::msg("fusion: bad card count in the index meta"))?;
        let vectors = get("vectors")?.parse().map_err(|_| Error::msg("fusion: bad vector count in the index meta"))?;
        let build_id = get("build_id")?;
        Ok(Searcher { con, prefix: prefix.to_string(), n, vectors, avg, model, linker, vocab, meta, build_id })
    }

    /// Refuse an index built from other card files than the ones under the data dir now (`cards_fingerprint`).
    pub fn check_cards_fp(&self, current: &str) -> Result<()> {
        match self.meta.get("cards_fp") {
            Some(fp) if fp == current => Ok(()),
            _ => Err(Error::msg(format!("fusion: the index under {} was built from other card files than the ones there now; rebuild it with `ks fusion build`", self.prefix))),
        }
    }

    /// Whether the index is still the one this searcher opened (one HMGET of the meta).
    pub fn index_state(&mut self) -> Result<IndexState> {
        let (complete, build_id): (Option<String>, Option<String>) = redis::cmd("HMGET").arg(format!("{}m", self.prefix)).arg("complete").arg("build_id").query(&mut self.con).map_err(rerr)?;
        Ok(if complete.as_deref() != Some("1") {
            IndexState::Incomplete
        } else if build_id.as_deref() != Some(self.build_id.as_str()) {
            IndexState::Rebuilt
        } else {
            IndexState::Unchanged
        })
    }

    /// The index is still the one this searcher opened (same build id, complete).
    fn check_unchanged(&mut self) -> Result<()> {
        match self.index_state()? {
            IndexState::Unchanged => Ok(()),
            IndexState::Rebuilt => Err(Error::msg(format!("fusion: the index under {} was rebuilt, so this result was dropped; search again", self.prefix))),
            IndexState::Incomplete => Err(Error::msg(format!("fusion: the index under {} is incomplete (a build is running or failed), so this result was dropped; rebuild it or search again later", self.prefix))),
        }
    }

    /// One search end to end: the switch decides before the round trips whether vectors are read, the embedding call runs
    /// beside them, the ranker of the path orders the candidates. `cv`: the lab's out-of-fold rankers (benchmarks only).
    pub fn search(&mut self, sw: &Switch, query: &str, terms: Option<&[String]>, limit: usize, cv: bool) -> Result<SearchResult> {
        let t = Instant::now();
        let mode = if has_terms(terms) { 1 } else { 0 };
        let rankers = self.model.rankers_for(query, cv).clone();
        let b = begin_path(sw, &rankers, mode, self.vectors as f64 / self.n.max(1) as f64, query, terms)?;
        let want_vectors = b.want_vectors;
        let mut plan = plan_query(&self.linker, &self.vocab, query, terms);
        let mut want: Vec<u32> = plan.toks.iter().map(|x| x.h).chain(plan.vterms.iter().map(|v| v.h)).collect();
        let mut seen = std::collections::HashSet::new();
        want.retain(|h| seen.insert(*h));
        let t1 = Instant::now();
        let table: HashMap<u32, (u32, super::payload::TokenRow)> = if want.is_empty() {
            HashMap::new()
        } else {
            let mut cmd = redis::cmd("HMGET");
            cmd.arg(format!("{}t", self.prefix));
            for &h in &want {
                cmd.arg(code(h));
            }
            let rows: Vec<Option<Vec<u8>>> = cmd.query(&mut self.con).map_err(rerr)?;
            let mut m = HashMap::new();
            for (h, r) in want.iter().zip(rows) {
                if let Some(b) = r {
                    m.insert(*h, unpack_token_row(&b)?);
                }
            }
            m
        };
        fill_idf(&mut plan, |h| table.get(&h).map_or(0, |x| x.0), self.n);
        expand_plan(&mut plan, |h| table.get(&h).map(|x| x.1.clone()));
        let t2 = Instant::now();
        // (id, key, title, scope, payload, vector)
        let mut cand: Vec<(String, String, String, String, Payload, Option<Vec<f32>>)> = vec![];
        if let Some(q) = stage1_string(&stage1_query(&plan)) {
            let mut cmd = redis::cmd("FT.SEARCH");
            cmd.arg(format!("{}idx", self.prefix)).arg(q).arg("SCORER").arg("BM25STD").arg("RETURN");
            if want_vectors {
                cmd.arg(6).arg("p").arg("id").arg("key").arg("title").arg("scope").arg("e");
            } else {
                cmd.arg(5).arg("p").arg("id").arg("key").arg("title").arg("scope");
            }
            cmd.arg("LIMIT").arg(0).arg(CANDIDATES).arg("DIALECT").arg(2);
            let reply: Vec<Value> = cmd.query(&mut self.con).map_err(rerr)?;
            for pair in reply.get(1..).unwrap_or(&[]).chunks(2) {
                let Some(Value::Array(fields)) = pair.get(1) else { continue };
                let (mut p, mut id, mut key, mut title, mut scope, mut e) = (None, None, None, None, None, None);
                for kv in fields.chunks(2) {
                    let (Some(Value::BulkString(k)), Some(Value::BulkString(v))) = (kv.first(), kv.get(1)) else { continue };
                    match k.as_slice() {
                        b"p" => p = Some(v.clone()),
                        b"id" => id = String::from_utf8(v.clone()).ok(),
                        b"key" => key = String::from_utf8(v.clone()).ok(),
                        b"title" => title = String::from_utf8(v.clone()).ok(),
                        b"scope" => scope = String::from_utf8(v.clone()).ok(),
                        // a stored vector of another dimension or with a non-finite value counts as no vector
                        b"e" => e = from_f16_bytes(v, DIM),
                        _ => {}
                    }
                }
                let (Some(p), Some(id), Some(key)) = (p, id, key) else { continue };
                cand.push((id, key, title.unwrap_or_default(), scope.unwrap_or_default(), decode_payload(p)?, e));
            }
        }
        let t3 = Instant::now();
        let r0 = b.wait()?;
        let t4 = Instant::now();
        let mut ms = BTreeMap::new();
        ms.insert("plan", (t1 - t).as_secs_f64() * 1000.0);
        ms.insert("tokens", (t2 - t1).as_secs_f64() * 1000.0);
        ms.insert("search", (t3 - t2).as_secs_f64() * 1000.0);
        ms.insert("embed", r0.embed_ms);
        let mode_s = if mode == 1 { "terms" } else { "query" };
        if cand.is_empty() {
            self.check_unchanged()?;
            ms.insert("rank", 0.0);
            ms.insert("total", t.elapsed().as_secs_f64() * 1000.0);
            return Ok(SearchResult { mode: mode_s, path: r0.path, reason: r0.reason, candidates: 0, results: vec![], ms });
        }
        let vecs: Vec<Option<Vec<f32>>> = cand.iter().map(|c| c.5.clone()).collect();
        let r = settle_path(sw, r0, &vecs)?;
        let ranker = if r.q.is_some() { &rankers.on[mode] } else { &rankers.off[mode] };
        let payloads: Vec<Payload> = cand.iter().map(|c| c.4.clone()).collect();
        let keys: Vec<String> = cand.iter().map(|c| c.1.clone()).collect();
        let (order, scores) = rank_unified(ranker, &self.avg, &plan, &payloads, &keys, limit, r.q.as_deref(), &vecs)?;
        ms.insert("rank", t4.elapsed().as_secs_f64() * 1000.0);
        self.check_unchanged()?;
        ms.insert("total", t.elapsed().as_secs_f64() * 1000.0);
        let results = order.iter().enumerate().map(|(k, &i)| Hit { rank: k + 1, id: cand[i].0.clone(), title: cand[i].2.clone(), scope: cand[i].3.clone(), score: scores[i] }).collect();
        Ok(SearchResult { mode: mode_s, path: r.path, reason: r.reason, candidates: cand.len(), results, ms })
    }
}

/// Card vectors from an embedding server (batches of 32 texts), for a build without a vector file.
pub fn embed_cards(e: &dyn Embedder, texts: &[String]) -> Result<Vec<Option<Vec<f32>>>> {
    let mut out = Vec::with_capacity(texts.len());
    for chunk in texts.chunks(32) {
        let vs = e.embed(chunk, Duration::from_secs(300)).map_err(|m| Error::msg(format!("fusion: card embedding failed ({m})")))?;
        if vs.len() != chunk.len() || vs.iter().any(|v| !vector_ok(v, DIM)) {
            return Err(Error::msg(format!("fusion: the embedding server returned vectors that are not {DIM} finite values")));
        }
        out.extend(vs.into_iter().map(Some));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_fusion_prefixes_are_accepted() {
        for p in ["ksf:", "ksf-demo:", "ksf-eval-rust:", "ksf-a1:"] {
            assert!(check_prefix(p).is_ok(), "{p}");
        }
        for p in ["ks:", "ks", "ksf", "ks:fusion:", "ksf-:", "ks-eval-:", "ksf-Demo:", "ksf-a*:", "", "ksf-x:y:", "ks-eval-x", "ksf--a:"] {
            assert!(check_prefix(p).is_err(), "{p}");
        }
    }

    #[test]
    fn only_the_fusion_namespace_is_accepted() {
        // ks-eval-<name>: belongs to plain ks (KS_KEY_PREFIX of the eval indexes): refused
        for p in ["ks-eval-base:", "ks-eval-new:", "ks-eval-fusion-rust:"] {
            assert!(check_prefix(p).is_err(), "{p}");
        }
        assert!(check_prefix("ksf-eval-rust:").is_ok());
    }

    #[test]
    fn the_prefix_must_not_overlap_the_ks_prefix() {
        assert!(check_prefix_with("ksf:", Some("ks:")).is_ok());
        assert!(check_prefix_with("ksf-a:", Some("ks-eval-base:")).is_ok());
        for (p, ks) in [("ksf-x:", "ksf-x:"), ("ksf:", "ks"), ("ksf-a:", "ksf-a:b:"), ("ksf-ab:", "ksf-a")] {
            assert!(check_prefix_with(p, Some(ks)).is_err(), "{p} vs {ks}");
        }
    }

    #[test]
    fn only_the_key_shapes_a_build_writes_are_fusion_keys() {
        let p = "ksf-team:";
        for k in ["ksf-team:m", "ksf-team:t", "ksf-team:c:0", "ksf-team:c:5606"] {
            assert_eq!(classify(p, k), KeyKind::Fusion, "{k}");
        }
        for k in ["ksf-team:meta", "ksf-team:apply", "ksf-team:card:ab", "ksf-team:auth:key:x", "ksf-team:rebuild-lock"] {
            assert_eq!(classify(p, k), KeyKind::Ks, "{k}");
        }
        for k in ["ksf-team:session:1", "ksf-team:c:", "ksf-team:c:1x", "ksf-team:m2", "ksf-team:tt", "ksf-team:idx"] {
            assert_eq!(classify(p, k), KeyKind::Foreign, "{k}");
        }
        // a planted fusion meta does not make other keys deletable
        let f = Found { any_key: true, fusion_meta: true, foreign: vec!["ksf-team:session:1".into()], ..Found::default() };
        let e = may_replace(&f).unwrap_err().to_string();
        assert!(e.contains("ksf-team:session:1"), "{e}");
    }

    #[test]
    fn replacing_requires_a_fusion_owned_prefix() {
        let none = Found::default();
        assert!(may_replace(&none).is_ok(), "an empty prefix may be written");
        let fusion = Found { any_key: true, fusion_meta: true, ..Found::default() };
        assert!(may_replace(&fusion).is_ok());
        for (f, why) in [
            (Found { any_key: true, ..Found::default() }, "keys without fusion meta"),
            (Found { any_key: true, fusion_meta: true, ks_keys: vec!["ksf-x:meta".into()], ..Found::default() }, "ks keys"),
            (Found { index: true, ..Found::default() }, "an index without fusion meta"),
        ] {
            assert!(may_replace(&f).is_err(), "{why}");
        }
    }
}
