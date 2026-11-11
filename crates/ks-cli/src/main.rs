mod browser;
mod http;
mod mcp;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use ks_core::auth::{KeyStore, Role};
use ks_core::card::{card_id, card_key_of, parse_kit_line, Card, Scope};
use ks_core::collect::{collect, load_cards, md_paths, parse_queries};
use ks_core::index::{cards_fingerprint, Application, Fingerprints, Index};
use ks_core::normalize::js_trim;
use ks_core::validate::validate_card;
use ks_core::vocab::Vocab;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "ks", version, about = "knowledge-scout: search a private pool of technical knowledge cards")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Card storage operations
    #[command(subcommand)]
    Card(CardCmd),
    /// Collect ranked candidate cards for each query
    Search(SearchArgs),
    /// Redis index operations
    #[command(subcommand)]
    Index(IndexCmd),
    /// Record how a card was judged for a task (XADD to the application stream), or dump the records
    #[command(args_conflicts_with_subcommands = true)]
    Record(RecordCmd),
    /// MCP server
    #[command(subcommand)]
    Mcp(McpCmd),
    /// API keys for `ks mcp serve --http` (Redis keeps only their SHA-256 and metadata)
    #[command(subcommand)]
    Key(KeyCmd),
    /// Browser daemon for the google-browser engine
    #[command(subcommand)]
    Browser(browser::BrowserCmd),
    /// Sign the browser profile in or out (you sign in yourself in a visible window)
    #[command(subcommand)]
    Auth(browser::AuthCmd),
    /// Bring up the browser window so you can handle an interrupt (CAPTCHA, consent, 2-step prompt)
    Handoff {
        /// AUTH_REQUIRED, AUTH_EXPIRED, CAPTCHA, CONSENT, MFA_PROMPT, BLOCKED, ENGINE_SWITCH or DAEMON_REPLACE
        event: String,
        #[command(flatten)]
        p: browser::ProfileArg,
    },
    /// Re-read the page after you handled an interrupt and hand control back
    Resume {
        #[command(flatten)]
        p: browser::ProfileArg,
    },
    /// Browser daemon state, unresolved interrupt and the last 10 events
    Status {
        #[command(flatten)]
        p: browser::ProfileArg,
    },
    /// Web collection
    #[command(subcommand)]
    Research(browser::ResearchCmd),
}

#[derive(Subcommand)]
enum IndexCmd {
    /// Drop and recreate the Redis index from every card under <data>/cards
    Rebuild(RebuildArgs),
}

#[derive(Subcommand)]
enum McpCmd {
    /// Serve ks_search, ks_get, ks_record and ks_search_web over stdio, or with --http only ks_search and ks_get over stateless HTTP
    Serve(ServeArgs),
}

#[derive(Subcommand)]
enum KeyCmd {
    /// Issue a key. It is printed once (or written to a new 0600 file with --out) and never stored: Redis keeps its SHA-256 and the metadata
    #[command(visible_alias = "create")]
    Issue(IssueArgs),
    /// List the keys' metadata (never a key or its hash)
    List,
    /// Revoke a key by its id; refused from the next request on
    Revoke {
        id: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum RoleArg {
    /// public cards only, without source fields
    Reader,
    /// every card, full Markdown
    Admin,
    /// every card, full Markdown (the pool's owner)
    Owner,
}

impl RoleArg {
    fn role(self) -> Role {
        match self {
            RoleArg::Reader => Role::Reader,
            RoleArg::Admin => Role::Admin,
            RoleArg::Owner => Role::Owner,
        }
    }
}

#[derive(Args)]
struct IssueArgs {
    #[arg(long, value_enum)]
    role: RoleArg,
    /// What the key is for (kept with the key's metadata on the operator's side)
    #[arg(long, visible_alias = "label")]
    purpose: String,
    /// Who the key is for (kept with the key's metadata on the operator's side)
    #[arg(long, visible_alias = "subject")]
    user: String,
    /// Write the key to this new file (mode 0600; an existing file is refused) instead of printing it
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum Backend {
    Redis,
    /// For tests and cross-checks only; product search requires Redis
    #[value(hide = true)]
    File,
}

#[derive(Args)]
struct RebuildArgs {
    #[arg(long)]
    data: PathBuf,
    /// Vocabulary file the card terms are classified with (use the one `search` and `mcp serve` use); default <data>/vocab/terms.yaml|yml|json
    #[arg(long)]
    vocab: Option<PathBuf>,
}

#[derive(Args)]
struct ServeArgs {
    #[arg(long)]
    data: PathBuf,
    /// Vocabulary file; default <data>/vocab/terms.yaml|yml|json
    #[arg(long)]
    vocab: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "redis")]
    backend: Backend,
    /// Serve over stateless Streamable HTTP at <ADDR:PORT> (endpoint /mcp) with only ks_search and ks_get, for callers with a key
    #[arg(long, value_name = "ADDR:PORT")]
    http: Option<String>,
    /// Also accept this Host header (repeat, or comma-separate; empty values are ignored). localhost, 127.0.0.1 and ::1 are always accepted
    #[arg(long, value_name = "HOST", value_delimiter = ',', requires = "http")]
    allowed_host: Vec<String>,
    /// Accept requests with this Origin header (e.g. https://app.example:443); any other Origin is refused
    #[arg(long, value_name = "ORIGIN", requires = "http")]
    allowed_origin: Vec<String>,
    /// The lowest key role served (default reader). A reader-facing pool must hold only `scope: public` cards
    #[arg(long, value_enum, requires = "http")]
    min_role: Option<RoleArg>,
    /// No keys: every request is a reader. Only on a loopback address
    #[arg(long, requires = "http")]
    no_auth: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum Verdict {
    #[value(name = "A")]
    A,
    #[value(name = "B")]
    B,
    #[value(name = "C")]
    C,
    #[value(name = "excluded")]
    Excluded,
    #[value(name = "unknown")]
    Unknown,
}

#[derive(Clone, Copy, ValueEnum)]
enum Outcome {
    Success,
    Failed,
    Partial,
}

#[derive(Args)]
struct RecordCmd {
    #[command(subcommand)]
    sub: Option<RecordSub>,
    // Required unless `dump` is given; clap cannot express that on a struct that also holds the subcommand, so `record()` checks.
    #[arg(long)]
    task: Option<String>,
    #[arg(long)]
    card: Option<String>,
    #[arg(long, value_enum)]
    verdict: Option<Verdict>,
    #[arg(long)]
    reason: Option<String>,
    #[arg(long, value_enum)]
    outcome: Option<Outcome>,
}

#[derive(Subcommand)]
enum RecordSub {
    /// Append the records that are not yet in <out>/applications.jsonl (existing lines are never rewritten)
    Dump(DumpArgs),
}

#[derive(Args)]
struct DumpArgs {
    #[arg(long)]
    out: PathBuf,
}

#[derive(Subcommand)]
enum CardCmd {
    /// Convert kit topic .jsonl files (L1 cards only) into <out>/<topic>/<id>.md
    Import(ImportArgs),
    /// Validate every card under <data>/cards
    Validate(ValidateArgs),
    /// Print the card_key and card id a source gets, as JSON {"card_key", "id"}
    Id(IdArgs),
}

#[derive(Clone, Copy, ValueEnum)]
enum ScopeArg {
    Private,
    Team,
    Public,
}

#[derive(Args)]
struct ImportArgs {
    #[arg(long)]
    jsonl_dir: PathBuf,
    /// Vocabulary file (.json, or .yaml/.yml)
    #[arg(long)]
    vocab: PathBuf,
    /// Cards directory, normally <data>/cards
    #[arg(long)]
    out: PathBuf,
    #[arg(long, value_enum, default_value = "private")]
    scope: ScopeArg,
}

#[derive(Args)]
struct ValidateArgs {
    #[arg(long)]
    data: PathBuf,
}

#[derive(Args)]
struct IdArgs {
    /// An http(s) URL (normalized: no query, fragment or trailing slash), or else a local path or name, used as `path:<source>`
    #[arg(long)]
    source: String,
}

#[derive(Args)]
struct SearchArgs {
    #[arg(long)]
    data: PathBuf,
    #[arg(long)]
    vocab: PathBuf,
    #[arg(long)]
    queries: PathBuf,
    #[arg(long)]
    out: PathBuf,
    #[arg(long, default_value_t = 40)]
    limit: u64,
    /// Where candidates are fetched from
    #[arg(long, value_enum, default_value = "redis")]
    backend: Backend,
}

fn main() -> ExitCode {
    // `ks` re-executes itself as the per-profile browser daemon (see ks_browser::client::Client::ensure)
    if ks_browser::daemon::is_daemon_invocation() {
        return ExitCode::from(ks_browser::daemon::run_from_env() as u8);
    }
    let cli = Cli::parse();
    let res = match cli.cmd {
        Cmd::Card(CardCmd::Import(a)) => import(a),
        Cmd::Card(CardCmd::Validate(a)) => validate(a),
        Cmd::Card(CardCmd::Id(a)) => card_id_of(a),
        Cmd::Search(a) => search(a),
        Cmd::Index(IndexCmd::Rebuild(a)) => rebuild(a),
        Cmd::Record(r) => record(r),
        Cmd::Mcp(McpCmd::Serve(a)) => mcp_serve(a),
        Cmd::Key(c) => key(c),
        Cmd::Browser(c) => browser::browser(c),
        Cmd::Auth(c) => browser::auth(c),
        Cmd::Handoff { event, p } => browser::handoff(event, p),
        Cmd::Resume { p } => browser::resume(p),
        Cmd::Status { p } => browser::status(p),
        Cmd::Research(c) => browser::research(c),
    };
    match res {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).expect("json serializes"));
}

fn safe_topic(t: &str) -> bool {
    !t.is_empty() && t != "." && t != ".." && !t.contains(['/', '\\', '\0'])
}

fn import(a: ImportArgs) -> Result<ExitCode> {
    let vocab = Vocab::load(&a.vocab)?;
    let scope = match a.scope {
        ScopeArg::Private => Scope::Private,
        ScopeArg::Team => Scope::Team,
        ScopeArg::Public => Scope::Public,
    };
    let mut files: Vec<PathBuf> = fs::read_dir(&a.jsonl_dir)
        .with_context(|| format!("{}: cannot read directory", a.jsonl_dir.display()))?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<_>>()?;
    files.retain(|p| p.is_file() && p.extension().is_some_and(|x| x == "jsonl") && p.file_name().is_some_and(|n| n != "rejects.jsonl"));
    files.sort();

    let (mut imported, mut skipped_non_l1) = (0usize, 0usize);
    let mut errors: Vec<Value> = vec![];
    for f in files {
        let src = fs::read_to_string(&f).with_context(|| format!("{}: cannot read", f.display()))?;
        for (i, line) in src.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let v = parse_kit_line(line).with_context(|| format!("{}:{}", f.display(), i + 1))?;
            if v.get("tier").and_then(Value::as_str) != Some("L1") {
                skipped_non_l1 += 1;
                continue;
            }
            let key = v.get("card_key").and_then(Value::as_str).unwrap_or("").to_string();
            let outcome = Card::from_kit(&v, &vocab, scope).map_err(|e| e.to_string()).and_then(|card| {
                let topic = card.topic.clone().unwrap_or_default();
                if !safe_topic(&topic) {
                    return Err(format!("{key}: unsafe or missing topic {topic:?}"));
                }
                write_card(&a.out.join(&topic), &card).map_err(|e| format!("{key}: {e:#}"))
            });
            match outcome {
                Ok(()) => imported += 1,
                Err(e) => errors.push(json!({"file": f.display().to_string(), "line": i + 1, "error": e})),
            }
        }
    }
    let failed = !errors.is_empty();
    print_json(&json!({"imported": imported, "skipped_non_l1": skipped_non_l1, "errors": errors}));
    Ok(if failed { ExitCode::from(1) } else { ExitCode::SUCCESS })
}

/// Write `<dir>/<id>.md`, leaving an identical existing file untouched (idempotent re-import).
fn write_card(dir: &Path, card: &Card) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("{}: cannot create", dir.display()))?;
    let path = dir.join(format!("{}.md", card.id));
    let md = card.render();
    if fs::read_to_string(&path).is_ok_and(|old| old == md) {
        return Ok(());
    }
    fs::write(&path, md).with_context(|| format!("{}: cannot write", path.display()))
}

fn validate(a: ValidateArgs) -> Result<ExitCode> {
    let dir = a.data.join("cards");
    let paths = md_paths(&dir)?;
    let mut errors: Vec<Value> = vec![];
    for p in &paths {
        let errs = match fs::read_to_string(p).map_err(|e| e.to_string()).and_then(|s| Card::parse(&s).map_err(|e| e.to_string())) {
            Ok(card) => {
                let mut errs = validate_card(&card);
                errs.extend(location_errors(&dir, p, &card));
                errs
            }
            Err(e) => vec![e],
        };
        if !errs.is_empty() {
            errors.push(json!({"path": p.display().to_string(), "errors": errs}));
        }
    }
    let invalid = errors.len();
    print_json(&json!({"cards": paths.len(), "valid": paths.len() - invalid, "invalid": invalid, "errors": errors}));
    Ok(if invalid > 0 { ExitCode::from(1) } else { ExitCode::SUCCESS })
}

/// A card lives at exactly `<cards>/<topic>/<id>.md`, where `import` writes it. (`ks_get` finds a card by its file
/// name anywhere below `<cards>`, recursively, so the file name must be the id.)
fn location_errors(cards: &Path, path: &Path, card: &Card) -> Vec<String> {
    let mut errs = vec![];
    let want = format!("{}.md", card.id);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if name != want {
        errs.push(format!("path: the file is named {name} but the card id is {} (expected {want})", card.id));
    }
    let rel: Vec<String> = path.strip_prefix(cards).unwrap_or(path).iter().map(|c| c.to_string_lossy().into_owned()).collect();
    let at = rel.join("/");
    match card.topic.as_deref() {
        Some(t) if rel.len() == 2 && rel[0] == t => {}
        Some(t) => errs.push(format!("path: the card is at {at} under the cards directory but its topic is {t} (expected {t}/{want})")),
        None => errs.push(format!("path: the card has no topic, so it belongs in no topic directory (it is at {at})")),
    }
    errs
}

/// The card_key `ks card import` gives a source (the kit's rule: the normalized URL, else `path:<source>`) and its id.
/// Surrounding whitespace is dropped first, with the same trim `normalize_url` applies to a URL.
fn card_id_of(a: IdArgs) -> Result<ExitCode> {
    let source = js_trim(&a.source);
    if source.is_empty() {
        bail!("--source must not be empty");
    }
    let card_key = card_key_of(source);
    print_json(&json!({"card_key": card_key, "id": card_id(&card_key)}));
    Ok(ExitCode::SUCCESS)
}

fn search(a: SearchArgs) -> Result<ExitCode> {
    if a.limit < 1 {
        bail!("--limit must be a positive integer");
    }
    let (vocab, vocab_fp) = Vocab::load_with_fingerprint(&a.vocab)?;
    let raw = fs::read_to_string(&a.queries).with_context(|| format!("{}: cannot read queries JSON", a.queries.display()))?;
    let doc: Value = serde_json::from_str(&raw).with_context(|| format!("{}: cannot read queries JSON", a.queries.display()))?;
    let queries = parse_queries(&doc)?;
    let result = match a.backend {
        Backend::File => collect(&load_cards(&a.data.join("cards"))?, &vocab, &queries, a.limit as usize),
        Backend::Redis => {
            let mut idx = Index::from_env()?;
            // fail closed: never search an index built from other vocabulary or card files than the ones in use now
            let now = Fingerprints { vocab_fp, cards_fp: cards_fingerprint(&a.data.join("cards"))? };
            let checked = idx.check_fresh(&now, &rebuild_hint(&a.data, &a.vocab))?;
            idx.search_checked(&checked, &vocab, &queries, a.limit as usize)?.into_iter().map(|r| r.into_result()).collect()
        }
    };
    if let Some(parent) = a.out.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| format!("{}: cannot create", parent.display()))?;
    }
    fs::write(&a.out, serde_json::to_string_pretty(&result)? + "\n").with_context(|| format!("{}: cannot write", a.out.display()))?;
    Ok(ExitCode::SUCCESS)
}

fn value_name<T: ValueEnum>(v: T) -> String {
    v.to_possible_value().expect("no skipped variants").get_name().to_string()
}

/// The command that rebuilds the index for these inputs, as shown in the errors of a refused search.
pub(crate) fn rebuild_hint(data: &Path, vocab: &Path) -> String {
    format!("ks index rebuild --data {} --vocab {}", data.display(), vocab.display())
}

fn rebuild(a: RebuildArgs) -> Result<ExitCode> {
    let (vocab, vocab_fp) = Vocab::load_with_fingerprint(&mcp::vocab_path(&a.data, a.vocab)?)?;
    // fingerprint the cards before reading them: a card that changes meanwhile then makes the index read as stale
    let cards_dir = a.data.join("cards");
    let fp = Fingerprints { vocab_fp, cards_fp: cards_fingerprint(&cards_dir)? };
    let cards = load_cards(&cards_dir)?;
    let report = Index::from_env()?.rebuild_with(&cards, &vocab, Some(&a.data), &fp)?;
    print_json(&serde_json::to_value(report)?);
    Ok(ExitCode::SUCCESS)
}

fn record(r: RecordCmd) -> Result<ExitCode> {
    if let Some(RecordSub::Dump(d)) = r.sub {
        print_json(&serde_json::to_value(Index::from_env()?.dump_applications(&d.out)?)?);
        return Ok(ExitCode::SUCCESS);
    }
    let need = |v: Option<String>, flag: &str| {
        v.with_context(|| format!("missing required argument {flag}\nUsage: ks record --task <TASK> --card <CARD> --verdict <A|B|C|excluded|unknown> --reason <TEXT> [--outcome <success|failed|partial>]\n       ks record dump --out <DIR>"))
    };
    let app = Application {
        task: need(r.task, "--task")?,
        card: need(r.card, "--card")?,
        verdict: need(r.verdict.map(value_name), "--verdict")?,
        reason: need(r.reason, "--reason")?,
        outcome: r.outcome.map(value_name),
    };
    app.validate()?; // before connecting: a bad record never reaches Redis
    let id = Index::from_env()?.record(&app)?;
    print_json(&json!({"id": id}));
    Ok(ExitCode::SUCCESS)
}

fn mcp_serve(a: ServeArgs) -> Result<ExitCode> {
    match a.http {
        None => mcp::serve(a.data, a.vocab, a.backend)?,
        Some(addr) => {
            let min_role = a.min_role.map_or(Role::Reader, RoleArg::role);
            let opts = http::HttpOpts { addr, allowed_hosts: a.allowed_host, allowed_origins: a.allowed_origin, min_role, no_auth: a.no_auth };
            http::serve_http(a.data, a.vocab, a.backend, opts)?
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn key(c: KeyCmd) -> Result<ExitCode> {
    let mut store = KeyStore::from_env()?;
    match c {
        KeyCmd::Issue(a) => {
            // the file first: when it cannot be created, no key is stored
            let mut file = match &a.out {
                Some(p) => {
                    use std::os::unix::fs::OpenOptionsExt;
                    let f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(p).map_err(|e| match e.kind() {
                        std::io::ErrorKind::AlreadyExists => anyhow::anyhow!("{}: the file already exists; refusing to overwrite it", p.display()),
                        _ => anyhow::anyhow!("{}: cannot create: {e}", p.display()),
                    })?;
                    Some((p.clone(), f))
                }
                None => None,
            };
            let (meta, key) = match store.issue(a.role.role(), &a.purpose, &a.user, &ks_browser::events::utc_now()) {
                Ok(x) => x,
                Err(e) => {
                    if let Some((p, _)) = &file {
                        let _ = fs::remove_file(p);
                    }
                    return Err(e.into());
                }
            };
            let mut v = meta.to_json();
            match file.as_mut() {
                Some((p, f)) => {
                    use std::io::Write;
                    if let Err(e) = f.write_all(format!("{key}\n").as_bytes()).and_then(|()| f.sync_all()) {
                        // never delivered: revoke it rather than leave a live key nobody has
                        let _ = store.revoke(&meta.id, &ks_browser::events::utc_now());
                        let _ = fs::remove_file(&*p);
                        bail!("{}: cannot write the key ({e}); key {} was revoked", p.display(), meta.id);
                    }
                    v["out"] = json!(p.display().to_string());
                }
                None => {
                    v["key"] = json!(key);
                    eprintln!("ks key: this key is shown only once and is not stored anywhere (Redis keeps its SHA-256); hand it to its user now");
                }
            }
            print_json(&v);
        }
        KeyCmd::List => print_json(&Value::Array(store.list()?.iter().map(|m| m.to_json()).collect())),
        KeyCmd::Revoke { id } => {
            if !store.revoke(&id, &ks_browser::events::utc_now())? {
                bail!("no key with id {id}");
            }
            print_json(&json!({"id": id, "revoked": true}));
        }
    }
    Ok(ExitCode::SUCCESS)
}
