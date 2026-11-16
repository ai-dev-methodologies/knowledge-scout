//! `ks fusion build|search|batch`: the fusion ranker (Redis + CPU, switchable CPU embedding) on its own key prefix.
use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use ks_core::card::Card;
use ks_core::collect::{check_query_limits, md_paths, Query};
use ks_core::fusion::analyzer::Linker;
use ks_core::fusion::build::{build_index, eligible_last_wins, CardInput};
use ks_core::fusion::embed::{CacheEmbedder, Embedder, HttpEmbedder, NoEmbedder};
use ks_core::fusion::model::{Model, DIM};
use ks_core::fusion::payload::{f16_to_f32, vector_ok};
use ks_core::fusion::redis::{check_prefix_with, check_target, connect, embed_cards, write_index, BuildSources, IndexState, Searcher, DEFAULT_PREFIX};
use ks_core::fusion::switch::{Mode, Switch, SystemClock};
use ks_core::index::{cards_fingerprint, env_url_and_prefix};
use ks_core::vocab::Vocab;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

#[derive(Subcommand)]
pub enum FusionCmd {
    /// Build the fusion index from the eligible cards under <data>/cards into Redis under --prefix (ksf: or ksf-<name>:; a fusion index there is replaced)
    Build(BuildArgs),
    /// Search the fusion index; prints JSON with the ranked cards and the path the query took (on = embedding, off = lexical)
    Search(SearchArgs),
    /// Read queries as JSON lines ({"qid", "query", "terms"?}) on stdin, write one ranking per line on stdout
    Batch(BatchArgs),
}

#[derive(Args)]
pub struct BuildArgs {
    #[arg(long)]
    data: PathBuf,
    /// Vocabulary file; default <data>/vocab/terms.yaml|yml|json
    #[arg(long)]
    vocab: Option<PathBuf>,
    /// Alias dictionary ({"terms": {term: {"ko": [...], "en": [...]}}})
    #[arg(long)]
    aliases: Option<PathBuf>,
    /// Model file (fusion-model-r4.json from the search lab's algos/r4-unified-export.mjs)
    #[arg(long)]
    model: PathBuf,
    /// Long summaries: <dir>/<topic>/<card id>/summary.ko.md, indexed when present
    #[arg(long)]
    long_dir: Option<PathBuf>,
    /// Card vectors: records of 16 ASCII id bytes + 1,024 little-endian FLOAT16 values
    #[arg(long, conflicts_with = "embed_url")]
    vectors: Option<PathBuf>,
    /// Compute the card vectors with this local llama-server (http://127.0.0.1:<port>, bge-m3)
    #[arg(long)]
    embed_url: Option<String>,
    /// Enrichment sidecar: JSON lines {"id", "questions_ko"?, "title_en"?, "summary_en"?}; indexed as one more text field
    #[arg(long, requires = "enrich_weight")]
    enrich: Option<PathBuf>,
    /// First-stage weight of the enrichment field (integer >= 1)
    #[arg(long, requires = "enrich")]
    enrich_weight: Option<u32>,
    #[arg(long, default_value = DEFAULT_PREFIX)]
    prefix: String,
    /// Also write a digest of what was written (per-card payload / field sha256, token table sha256) to this file
    #[arg(long)]
    digest_out: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum SwitchArg {
    Auto,
    On,
    Off,
}

#[derive(Clone, Copy, ValueEnum, PartialEq)]
pub enum View {
    /// the shipped rankers
    Shipped,
    /// the search lab's out-of-fold rankers for its dev queries (benchmarks only)
    Cv,
}

#[derive(Args)]
pub struct EngineArgs {
    /// Embedding switch: auto (embedding while the server answers within 700 ms, lexical otherwise), on (strict), off
    #[arg(long = "switch", value_enum, default_value = "auto")]
    switch: SwitchArg,
    /// Local llama-server for the query embedding (http://127.0.0.1:<port>)
    #[arg(long)]
    embed_url: Option<String>,
    /// Read query vectors from this cache directory (<sha256(text)>.f32) instead of a server (benchmarks)
    #[arg(long, conflicts_with = "embed_url", hide = true)]
    query_cache: Option<PathBuf>,
    #[arg(long, default_value = DEFAULT_PREFIX)]
    prefix: String,
    /// Rank with this model file instead of the one stored with the index
    #[arg(long)]
    model: Option<PathBuf>,
    /// Refuse to search when the cards under <data>/cards are not the ones the index was built from
    #[arg(long)]
    data: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "shipped", hide = true)]
    view: View,
    #[arg(long, default_value_t = 40)]
    limit: usize,
}

#[derive(Args)]
pub struct SearchArgs {
    #[arg(long)]
    query: String,
    /// Client terms (repeat): switches to terms mode
    #[arg(long = "term")]
    terms: Vec<String>,
    #[command(flatten)]
    engine: EngineArgs,
}

#[derive(Args)]
pub struct BatchArgs {
    #[command(flatten)]
    engine: EngineArgs,
}

pub fn run(cmd: FusionCmd) -> Result<ExitCode> {
    match cmd {
        FusionCmd::Build(a) => build(a),
        FusionCmd::Search(a) => search(a),
        FusionCmd::Batch(a) => batch(a),
    }
}

fn redis_url() -> Result<String> {
    Ok(env_url_and_prefix()?.0)
}

/// The key prefix plain ks uses (KS_KEY_PREFIX, default ks:): a fusion prefix must not overlap it.
fn ks_prefix() -> Result<String> {
    Ok(env_url_and_prefix()?.1)
}

fn default_vocab(data: &Path) -> Result<PathBuf> {
    ["terms.yaml", "terms.yml", "terms.json"].iter().map(|f| data.join("vocab").join(f)).find(|p| p.exists()).with_context(|| format!("no vocabulary under {}/vocab (pass --vocab)", data.display()))
}

/// Card id -> vector; a record with an infinite or NaN value is left out (that card gets no vector). Returns the map and
/// the number of records left out.
fn read_vectors(path: &Path) -> Result<(HashMap<String, Vec<f32>>, usize)> {
    let b = std::fs::read(path).with_context(|| format!("{}: cannot read the vector file", path.display()))?;
    let rec = 16 + DIM * 2;
    if b.len() % rec != 0 {
        bail!("{}: not a whole number of records of {rec} bytes", path.display());
    }
    let mut out = HashMap::new();
    let mut refused = 0;
    for r in b.chunks_exact(rec) {
        let id = std::str::from_utf8(&r[..16]).context("vector file: card id is not ASCII")?.to_string();
        let v: Vec<f32> = r[16..].chunks_exact(2).map(|c| f16_to_f32(u16::from_le_bytes([c[0], c[1]]))).collect();
        if vector_ok(&v, DIM) {
            out.insert(id, v);
        } else {
            out.remove(&id);
            refused += 1;
        }
    }
    Ok((out, refused))
}

fn enrich_text(v: &Value) -> String {
    let mut parts = vec![];
    for k in ["questions_ko", "title_en", "summary_en"] {
        match v.get(k) {
            Some(Value::String(s)) => parts.push(s.clone()),
            Some(Value::Array(a)) => parts.extend(a.iter().filter_map(|x| x.as_str().map(str::to_string))),
            _ => {}
        }
    }
    parts.join("\n")
}

fn build(a: BuildArgs) -> Result<ExitCode> {
    // the target first: nothing is read or embedded for a prefix that may not be written
    check_prefix_with(&a.prefix, Some(&ks_prefix()?))?;
    let mut con = connect(&redis_url()?)?;
    check_target(&mut con, &a.prefix, Some(&ks_prefix()?))?;
    let vocab_path = match &a.vocab {
        Some(p) => p.clone(),
        None => default_vocab(&a.data)?,
    };
    let vocab_text = std::fs::read_to_string(&vocab_path).with_context(|| format!("{}: cannot read", vocab_path.display()))?;
    if vocab_path.extension().is_some_and(|e| e != "json") {
        bail!("the fusion index stores the vocabulary as JSON: pass a .json vocabulary");
    }
    let vocab = Vocab::from_json(&vocab_text, &vocab_path.display().to_string())?;
    let aliases_text = match &a.aliases {
        Some(p) => Some(std::fs::read_to_string(p).with_context(|| format!("{}: cannot read", p.display()))?),
        None => None,
    };
    let aliases: Option<Value> = aliases_text.as_deref().map(serde_json::from_str).transpose().context("alias dictionary is not JSON")?;
    let model = Model::load(&a.model)?;
    let cards_dir = a.data.join("cards");
    let cards_fp = cards_fingerprint(&cards_dir)?;
    let mut parsed = vec![];
    for p in md_paths(&cards_dir)? {
        let src = std::fs::read_to_string(&p).with_context(|| format!("{}: cannot read", p.display()))?;
        let card = Card::parse(&src).map_err(|e| anyhow::anyhow!("{}: {e}", p.display()))?;
        let topic = p.parent().and_then(|d| d.file_name()).and_then(|s| s.to_str()).unwrap_or("").to_string();
        parsed.push((card, topic));
    }
    let read = parsed.len();
    // what ks search searches: the last card of an id wins, cards with a skip_reason are not eligible
    let (parsed, skipped, duplicates) = eligible_last_wins(parsed, |x| x.0.id.as_str(), |x| x.0.skip_reason.is_none());
    let mut cards = vec![];
    for (card, topic) in parsed {
        let long = match &a.long_dir {
            Some(d) => {
                let f = d.join(&topic).join(&card.id).join("summary.ko.md");
                if f.exists() { Some(std::fs::read_to_string(&f).with_context(|| format!("{}: cannot read", f.display()))?) } else { None }
            }
            None => None,
        };
        cards.push(CardInput::from_card(&card, &vocab, long));
    }
    let linker = Linker::new(&vocab, aliases.as_ref());
    let idx = build_index(cards, &linker)?;
    let mut refused = 0;
    let vectors = if let Some(p) = &a.vectors {
        let (m, r) = read_vectors(p)?;
        refused = r;
        Some(idx.cards.iter().map(|c| m.get(&c.id).cloned()).collect())
    } else if let Some(u) = &a.embed_url {
        let e = HttpEmbedder::new(u).map_err(|m| anyhow::anyhow!(m))?;
        Some(embed_cards(&e, &idx.cards.iter().map(|c| c.embed_text()).collect::<Vec<_>>())?)
    } else {
        None
    };
    let enrich = match (&a.enrich, a.enrich_weight) {
        (Some(p), Some(w)) => {
            if w < 1 {
                bail!("--enrich-weight must be an integer >= 1 (RediSearch floors a field weight at 1)");
            }
            let at: HashMap<&str, usize> = idx.cards.iter().enumerate().map(|(i, c)| (c.id.as_str(), i)).collect();
            let mut texts = HashMap::new();
            let f = std::fs::File::open(p).with_context(|| format!("{}: cannot read", p.display()))?;
            for (n, line) in std::io::BufReader::new(f).lines().enumerate() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let v: Value = serde_json::from_str(&line).with_context(|| format!("{}:{}: not JSON", p.display(), n + 1))?;
                let id = v.get("id").and_then(Value::as_str).with_context(|| format!("{}:{}: no id", p.display(), n + 1))?;
                if let Some(&i) = at.get(id) {
                    texts.insert(i, enrich_text(&v));
                }
            }
            Some((texts, w))
        }
        _ => None,
    };
    if let Some(p) = &a.digest_out {
        std::fs::write(p, serde_json::to_string(&idx.digest())? + "\n").with_context(|| format!("{}: cannot write", p.display()))?;
    }
    let src = BuildSources {
        vocab_json: &vocab_text,
        aliases_json: aliases_text.as_deref(),
        model: &model,
        vectors,
        enrich,
        extra_meta: vec![("cards_fp".into(), cards_fp), ("vocab_file".into(), vocab_path.display().to_string())],
    };
    let r = write_index(&mut con, &a.prefix, &idx, &src)?;
    let mut v = serde_json::to_value(&r)?;
    v["cards_read"] = json!(read);
    v["skipped_not_eligible"] = json!(skipped);
    v["duplicate_ids_dropped"] = json!(duplicates);
    v["vectors_refused"] = json!(refused);
    crate::print_json(&v);
    Ok(ExitCode::SUCCESS)
}

struct Engine {
    searcher: Searcher,
    sw: Switch,
    cv: bool,
    limit: usize,
}

fn open_searcher(a: &EngineArgs) -> Result<Searcher> {
    check_prefix_with(&a.prefix, Some(&ks_prefix()?))?;
    let model = a.model.as_deref().map(Model::load).transpose()?;
    let searcher = Searcher::open(connect(&redis_url()?)?, &a.prefix, model)?;
    if let Some(d) = &a.data {
        searcher.check_cards_fp(&cards_fingerprint(&d.join("cards"))?)?;
    }
    Ok(searcher)
}

fn engine(a: &EngineArgs) -> Result<Engine> {
    let searcher = open_searcher(a)?;
    let embed: Arc<dyn Embedder> = match (&a.embed_url, &a.query_cache) {
        (Some(u), _) => Arc::new(HttpEmbedder::new(u).map_err(|m| anyhow::anyhow!(m))?),
        (None, Some(d)) => Arc::new(CacheEmbedder { dir: d.clone() }),
        (None, None) => Arc::new(NoEmbedder),
    };
    let mode = match a.switch {
        SwitchArg::Auto => Mode::Auto,
        SwitchArg::On => Mode::On,
        SwitchArg::Off => Mode::Off,
    };
    let sw = Switch::new(mode, &searcher.model.switch, embed, Arc::new(SystemClock::new()), DIM);
    if a.limit == 0 || a.limit > 400 {
        bail!("--limit must be between 1 and 400");
    }
    Ok(Engine { searcher, sw, cv: a.view == View::Cv, limit: a.limit })
}

fn checked(qid: &str, query: &str, terms: &[String]) -> Result<()> {
    check_query_limits(&Query { qid: qid.to_string(), query: query.to_string(), terms: if terms.is_empty() { None } else { Some(terms.to_vec()) } })?;
    Ok(())
}

fn search(a: SearchArgs) -> Result<ExitCode> {
    checked("cli", &a.query, &a.terms)?;
    let mut e = engine(&a.engine)?;
    let terms = if a.terms.is_empty() { None } else { Some(a.terms.as_slice()) };
    let r = e.searcher.search(&e.sw, &a.query, terms, e.limit, e.cv)?;
    let mut v = serde_json::to_value(&r)?;
    v["query"] = json!(a.query);
    v["terms"] = json!(a.terms);
    v["switch"] = json!(e.sw.mode.as_str());
    v["model"] = json!(e.searcher.model.id);
    v["index"] = json!({"prefix": a.engine.prefix, "cards": e.searcher.n, "vectors": e.searcher.vectors});
    crate::print_json(&v);
    Ok(ExitCode::SUCCESS)
}

fn batch(a: BatchArgs) -> Result<ExitCode> {
    let mut e = engine(&a.engine)?;
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for (n, line) in stdin.lock().lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let row = (|| -> Result<Value> {
            let v: Value = serde_json::from_str(&line).with_context(|| format!("line {}: not JSON", n + 1))?;
            let qid = v.get("qid").map(|q| q.as_str().map(str::to_string).unwrap_or_else(|| q.to_string())).unwrap_or_else(|| (n + 1).to_string());
            let query = v.get("query").and_then(Value::as_str).with_context(|| format!("line {}: query must be a string", n + 1))?;
            let terms: Vec<String> = match v.get("terms") {
                None | Some(Value::Null) => vec![],
                Some(Value::Array(t)) if t.iter().all(Value::is_string) => t.iter().map(|x| x.as_str().unwrap().to_string()).collect(),
                _ => bail!("line {}: terms must be an array of strings", n + 1),
            };
            if let Err(err) = checked(&qid, query, &terms) {
                return Ok(json!({"qid": qid, "error": format!("{err:#}")}));
            }
            // one query's failure (or a bug that panics) is that line's error; the run goes on
            let t = if terms.is_empty() { None } else { Some(terms.as_slice()) };
            let mut r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| e.searcher.search(&e.sw, query, t, e.limit, e.cv)));
            // the index was rebuilt (complete, new build id) since this run opened it: reopen once and retry this line.
            // An incomplete index keeps the per-line error (nothing to reopen until a build completes).
            if matches!(r, Ok(Err(_))) && matches!(e.searcher.index_state(), Ok(IndexState::Rebuilt)) {
                match open_searcher(&a.engine) {
                    Ok(s) => {
                        e.searcher = s;
                        r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| e.searcher.search(&e.sw, query, t, e.limit, e.cv)));
                    }
                    Err(err) => r = Ok(Err(ks_core::Error::msg(format!("the index was rebuilt and could not be reopened: {err:#}")))),
                }
            }
            Ok(match r {
                Ok(Ok(r)) => json!({"qid": qid, "ids": r.results.iter().map(|h| &h.id).collect::<Vec<_>>(), "scores": r.results.iter().map(|h| h.score).collect::<Vec<_>>(), "mode": r.mode, "path": r.path, "reason": r.reason, "candidates": r.candidates, "ms": r.ms}),
                Ok(Err(err)) => json!({"qid": qid, "error": err.to_string()}),
                Err(_) => json!({"qid": qid, "error": "internal error while ranking this query (see stderr)"}),
            })
        })()
        .unwrap_or_else(|err| json!({"line": n + 1, "error": format!("{err:#}")}));
        writeln!(out, "{row}")?;
        out.flush()?;
    }
    Ok(ExitCode::SUCCESS)
}
