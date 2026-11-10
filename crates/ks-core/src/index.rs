//! Redis index: cards as RedisJSON documents, a Query Engine index over them, and the application-record stream.
//!
//! Search parity: Redis only fetches a candidate set, the final ranking is `collect::rank_entries` (the same code the
//! file backend uses). The candidate set is a guaranteed superset of the cards that can match:
//!
//! * canonical terms: TAG match on the resolved query terms. The `terms` TAG holds `collect::card_terms(card, vocab)`
//!   computed at `rebuild` (the same function `Entry::new` uses), not the terms stored in the card file;
//! * text terms: `text.contains(term)` is a substring test, so every letter/digit run ("piece") of the term must
//!   occur inside one Redis token. Each piece becomes an infix query `*piece*` over the TEXT fields, which are
//!   indexed with STOPWORDS 0, NOSTEM and WITHSUFFIXTRIE. A term with no piece of 2+ characters (`x`, `++`) can
//!   match anywhere, so every eligible card is fetched.
//!
//! The Rust matcher then re-checks every candidate, so extra candidates cost time but never change the output.
use crate::card::Card;
use crate::collect::{card_terms, md_paths, query_terms, rank_entries, unknown_terms, Entry, Found, Query, QueryTerms, Ranking, NON_WORD};
use crate::vocab::Vocab;
use crate::{Error, Result};
use redis::{Connection, Value};
use serde::Serialize;
use serde_json::{json, Map};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

pub const SCHEMA_VERSION: &str = "1";
pub const VERDICTS: [&str; 5] = ["A", "B", "C", "excluded", "unknown"];
pub const OUTCOMES: [&str; 3] = ["success", "failed", "partial"];
pub const MAX_TEXT_BYTES: usize = 2048;
pub const MAX_APPLICATIONS: usize = 100_000;
const DEFAULT_PREFIX: &str = "ks:";
const TEXT_FIELDS: [&str; 5] = ["title", "summary", "problems", "limits", "elements"];
const PAGE: usize = 1000;
const BATCH: usize = 500;
/// How long `<prefix>rebuild-lock` lives. A rebuild of about a thousand cards takes well under a second, and even a
/// pool twenty times larger takes seconds; one Redis call may wait up to the 300 s read timeout before it fails.
/// 15 minutes outlasts any rebuild that can still succeed by a wide margin, and is the longest a crashed `ks` (which
/// never releases it) blocks the next rebuild.
const REBUILD_LOCK_MS: u64 = 15 * 60 * 1000;
/// Delete the lock only while it still holds the caller's token, in one step (a lock that expired and was taken by
/// another rebuild is left alone).
const UNLOCK_LUA: &str = "if redis.call('GET', KEYS[1]) == ARGV[1] then return redis.call('DEL', KEYS[1]) else return 0 end";
/// Replace the meta hash (KEYS[2]) with the field/value pairs ARGV[2..] only while the lock (KEYS[1]) still holds the
/// caller's token ARGV[1], in one step; returns 1 when written. Deleted first, so no field of an earlier meta is left.
const WRITE_META_LUA: &str = "if redis.call('GET', KEYS[1]) ~= ARGV[1] then return 0 end \
     redis.call('DEL', KEYS[2]) redis.call('HSET', KEYS[2], unpack(ARGV, 2)) return 1";

/// 128 random bits as hex, for rebuild lock tokens and build ids.
fn random_hex() -> Result<String> {
    use std::io::Read;
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).map_err(|e| Error::msg(format!("/dev/urandom: {e}")))?;
    Ok(hex::encode(b))
}

fn rerr(e: redis::RedisError) -> Error {
    // RedisError's Display never contains the connection URL (and so never the password).
    Error::msg(format!("redis: {e}"))
}

pub struct Index {
    con: Connection,
    prefix: String,
    search_config_checked: bool,
}

/// The `meta` that `check_fresh` accepted, for `search_checked`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked(BTreeMap<String, String>);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RebuildReport {
    pub cards: usize,
    pub eligible: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<String>,
}

/// What an index was built from, as two cheap fingerprints: `rebuild_with` stores them in `meta`, `check_fresh` compares
/// them with the files as they are now, so a search never runs against an index that no longer matches its inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprints {
    /// sha256 (hex) of the vocabulary file's bytes.
    pub vocab_fp: String,
    /// sha256 (hex) over the sorted card file list with each file's size and modification time (see `cards_fingerprint`).
    pub cards_fp: String,
}

impl Fingerprints {
    /// Fingerprints of the files as they are on disk now. Take them before reading the files for a rebuild: a file that
    /// changes in between then makes the index read as stale instead of passing as current.
    pub fn of_files(vocab_file: &Path, cards_dir: &Path) -> Result<Fingerprints> {
        let bytes = std::fs::read(vocab_file).map_err(|e| Error::msg(format!("{}: cannot read vocabulary ({e})", vocab_file.display())))?;
        Ok(Fingerprints { vocab_fp: hex::encode(Sha256::digest(bytes)), cards_fp: cards_fingerprint(cards_dir)? })
    }
}

/// sha256 over every `*.md` file under `dir` (the files `collect::load_cards` reads): relative path, size and modification
/// time in nanoseconds, in sorted path order. Only `stat` calls, no file is read. Ceiling: a card rewritten with the same
/// size inside the filesystem's timestamp granularity (or with its mtime restored) is not noticed; upgrade to hashing
/// the contents if that ever happens in practice.
pub fn cards_fingerprint(dir: &Path) -> Result<String> {
    let mut h = Sha256::new();
    for p in md_paths(dir)? {
        let md = std::fs::symlink_metadata(&p).map_err(|e| Error::msg(format!("{}: {e}", p.display())))?;
        let mtime = md.modified().map_err(|e| Error::msg(format!("{}: {e}", p.display())))?;
        let nanos = mtime.duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let rel = p.strip_prefix(dir).unwrap_or(&p).to_string_lossy();
        h.update(format!("{rel}\0{}\0{nanos}\n", md.len()));
    }
    Ok(hex::encode(h.finalize()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Application {
    pub task: String,
    pub card: String,
    pub verdict: String,
    pub reason: String,
    pub outcome: Option<String>,
}

impl Application {
    /// Checks done before anything is sent: task, card and reason present; card a 16-hex card id;
    /// task and reason at most 2048 bytes each; known verdict and outcome.
    pub fn validate(&self) -> Result<()> {
        let blank = |s: &str| s.trim().is_empty();
        if blank(&self.task) || blank(&self.card) || blank(&self.reason) {
            return Err(Error::msg("record: task, card and reason must not be empty"));
        }
        if self.card.len() != 16 || !self.card.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::msg("record: card must be a card id of 16 hex characters"));
        }
        if self.task.len() > MAX_TEXT_BYTES || self.reason.len() > MAX_TEXT_BYTES {
            return Err(Error::msg(format!("record: task and reason must be at most {MAX_TEXT_BYTES} bytes each")));
        }
        if !VERDICTS.contains(&self.verdict.as_str()) {
            return Err(Error::msg(format!("record: verdict must be one of {}", VERDICTS.join("|"))));
        }
        if let Some(o) = &self.outcome {
            if !OUTCOMES.contains(&o.as_str()) {
                return Err(Error::msg(format!("record: outcome must be one of {}", OUTCOMES.join("|"))));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Dump {
    pub appended: usize,
    pub total: usize,
}

/// A card as stored in `$.card`: the frontmatter fields plus the two body sections.
pub fn card_to_json(card: &Card) -> serde_json::Value {
    let mut v = serde_json::to_value(card).expect("card serializes");
    v["summary"] = json!(card.summary);
    v["limits"] = json!(card.limits);
    v
}

pub fn card_from_json(v: &serde_json::Value) -> Result<Card> {
    let mut card: Card = serde_json::from_value(v.clone()).map_err(|e| Error::msg(format!("indexed card: {e}")))?;
    card.summary = v.get("summary").and_then(|s| s.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
    card.limits = v.get("limits").and_then(|s| s.as_str()).unwrap_or_default().to_string();
    Ok(card)
}

/// Backslash-escape everything Redis query syntax treats as punctuation or space (ASCII), except `_`.
pub fn escape_tag(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if (c.is_ascii_punctuation() && c != '_') || c.is_whitespace() {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The Redis query that fetches a superset of the cards matching `t` (see the module docs), or None when the query
/// has no terms at all.
pub fn candidate_query(t: &QueryTerms) -> Option<String> {
    let mut clauses: Vec<String> = vec![];
    if !t.canon.is_empty() {
        clauses.push(format!("@terms:{{{}}}", t.canon.iter().map(|c| escape_tag(c)).collect::<Vec<_>>().join("|")));
    }
    for term in &t.text_terms {
        let pieces: Vec<String> = NON_WORD.split(term).filter(|p| p.chars().count() >= 2).map(|p| format!("*{p}*")).collect();
        if pieces.is_empty() {
            return Some("@eligible:{1}".to_string());
        }
        clauses.push(format!("@{}:({})", TEXT_FIELDS.join("|"), pieces.join(" ")));
    }
    if clauses.is_empty() {
        return None;
    }
    Some(format!("@eligible:{{1}} ({})", clauses.join(" | ")))
}

fn lower(s: &str) -> String {
    s.to_lowercase()
}

fn doc(card: &Card, vocab: &Vocab) -> serde_json::Value {
    let terms = card_terms(card, vocab);
    json!({
        "id": card.id,
        "eligible": if card.skip_reason.is_none() { "1" } else { "0" },
        "terms": terms,
        "title": lower(card.title.as_deref().unwrap_or("")),
        "summary": lower(&card.summary.join("\n")),
        "problems": lower(&card.problems.join("\n")),
        "limits": lower(&card.limits),
        "elements": lower(&card.reusable_elements.iter().map(|e| e.desc.as_str()).collect::<Vec<_>>().join("\n")),
        "card": card_to_json(card),
    })
}

fn as_string(v: &Value) -> Option<String> {
    match v {
        Value::BulkString(b) => String::from_utf8(b.clone()).ok(),
        Value::SimpleString(s) => Some(s.clone()),
        _ => None,
    }
}

fn git_sha(dir: &Path) -> Option<String> {
    let git = |args: &[&str]| -> Option<String> {
        let o = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
        o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty())
    };
    let top = std::fs::canonicalize(git(&["rev-parse", "--show-toplevel"])?).ok()?;
    (top == std::fs::canonicalize(dir).ok()?).then(|| git(&["rev-parse", "HEAD"])).flatten()
}

fn glob_escape(s: &str) -> String {
    s.chars().flat_map(|c| if "*?[]\\".contains(c) { vec!['\\', c] } else { vec![c] }).collect()
}

impl Index {
    pub fn connect(url: &str, prefix: &str) -> Result<Index> {
        if prefix.is_empty() {
            return Err(Error::msg("redis key prefix must not be empty"));
        }
        let client = redis::Client::open(url).map_err(rerr)?;
        let con = client.get_connection_with_timeout(Duration::from_secs(5)).map_err(rerr)?;
        con.set_read_timeout(Some(Duration::from_secs(300))).map_err(rerr)?;
        Ok(Index { con, prefix: prefix.to_string(), search_config_checked: false })
    }

    /// `KS_REDIS_URL`, or `redis://[:KS_REDIS_PASSWORD@]KS_REDIS_HOST:KS_REDIS_PORT` (127.0.0.1:6379 by default).
    /// Key prefix: `KS_KEY_PREFIX` (default `ks:`). The password is only ever read from the environment.
    pub fn from_env() -> Result<Index> {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let url = match env("KS_REDIS_URL") {
            Some(u) => u,
            None => {
                let mut u = url::Url::parse(&format!("redis://{}:{}", env("KS_REDIS_HOST").unwrap_or_else(|| "127.0.0.1".into()), env("KS_REDIS_PORT").unwrap_or_else(|| "6379".into())))
                    .map_err(|_| Error::msg("redis: invalid KS_REDIS_HOST/KS_REDIS_PORT"))?;
                if let Some(pw) = env("KS_REDIS_PASSWORD") {
                    let _ = u.set_password(Some(&pw));
                }
                u.to_string()
            }
        };
        Index::connect(&url, &env("KS_KEY_PREFIX").unwrap_or_else(|| DEFAULT_PREFIX.into()))
    }

    fn k(&mut self, suffix: &str) -> String {
        format!("{}{suffix}", self.prefix)
    }

    fn q<T: redis::FromRedisValue>(&mut self, cmd: &redis::Cmd) -> Result<T> {
        cmd.query(&mut self.con).map_err(rerr)
    }

    fn drop_index(&mut self) -> Result<()> {
        let name = self.k("idx");
        match redis::cmd("FT.DROPINDEX").arg(&name).query::<()>(&mut self.con) {
            Ok(()) => Ok(()),
            Err(e) if ["index not found", "unknown index", "no such index"].iter().any(|m| e.to_string().to_lowercase().contains(m)) => Ok(()),
            Err(e) => Err(rerr(e)),
        }
    }

    fn unlink_matching(&mut self, pattern: &str) -> Result<()> {
        let mut cursor: u64 = 0;
        loop {
            let (next, keys): (u64, Vec<String>) = redis::cmd("SCAN").arg(cursor).arg("MATCH").arg(pattern).arg("COUNT").arg(1000).query(&mut self.con).map_err(rerr)?;
            if !keys.is_empty() {
                redis::cmd("UNLINK").arg(&keys).query::<()>(&mut self.con).map_err(rerr)?;
            }
            if next == 0 {
                return Ok(());
            }
            cursor = next;
        }
    }

    /// Drop and recreate the index and every card document, then write `meta`. The application stream is kept.
    /// The `terms` TAG is `collect::card_terms(card, vocab)`, the same terms file matching uses, so the vocabulary at
    /// rebuild time is the one `search` must be given: after the vocabulary changes, rebuild.
    /// The `meta` written here has no fingerprints, so `check_fresh` refuses it; `ks` uses `rebuild_with`.
    pub fn rebuild(&mut self, cards: &[Card], vocab: &Vocab, data_dir: Option<&Path>) -> Result<RebuildReport> {
        self.rebuild_inner(cards, vocab, data_dir, None)
    }

    /// `rebuild`, and record `fp` (the fingerprints of the vocabulary file and card files that `cards` and `vocab` were
    /// read from, taken before reading them) in `meta` for `check_fresh`.
    pub fn rebuild_with(&mut self, cards: &[Card], vocab: &Vocab, data_dir: Option<&Path>, fp: &Fingerprints) -> Result<RebuildReport> {
        self.rebuild_inner(cards, vocab, data_dir, Some(fp))
    }

    /// Take `<prefix>rebuild-lock` (`SET NX PX` with a fresh random token) and return the token for `unlock_rebuild`.
    /// Refused while another rebuild holds it. `rebuild` and `rebuild_with` take and release it themselves.
    pub fn lock_rebuild(&mut self) -> Result<String> {
        let key = self.k("rebuild-lock");
        let token = random_hex()?;
        let got: Option<String> = self.q(redis::cmd("SET").arg(&key).arg(&token).arg("NX").arg("PX").arg(REBUILD_LOCK_MS))?;
        match got {
            Some(_) => Ok(token),
            None => Err(Error::msg(format!(
                "redis: another `ks index rebuild` is running on this index ({key} is held); wait until it ends, then rebuild again; \
                 if no other rebuild is running, the lock was left by a rebuild that crashed: it expires by itself at most {} minutes after it was taken",
                REBUILD_LOCK_MS / 60_000
            ))),
        }
    }

    /// Release `<prefix>rebuild-lock` if it still holds `token`; returns whether it did.
    pub fn unlock_rebuild(&mut self, token: &str) -> Result<bool> {
        let key = self.k("rebuild-lock");
        let n: i64 = self.q(redis::cmd("EVAL").arg(UNLOCK_LUA).arg(1).arg(&key).arg(token))?;
        Ok(n == 1)
    }

    fn rebuild_inner(&mut self, cards: &[Card], vocab: &Vocab, data_dir: Option<&Path>, fp: Option<&Fingerprints>) -> Result<RebuildReport> {
        let token = self.lock_rebuild()?;
        let res = self.rebuild_locked(&token, cards, vocab, data_dir, fp);
        let unlocked = self.unlock_rebuild(&token);
        let report = res?;
        match unlocked {
            Ok(true) => Ok(report),
            Ok(false) => Err(Error::msg("redis: the rebuild lock expired before this rebuild ended, so another rebuild may have run at the same time; rebuild again")),
            Err(e) => Err(Error::msg(format!("{e} (the index was rebuilt, but its rebuild lock could not be released; it expires by itself)"))),
        }
    }

    fn rebuild_locked(&mut self, token: &str, cards: &[Card], vocab: &Vocab, data_dir: Option<&Path>, fp: Option<&Fingerprints>) -> Result<RebuildReport> {
        let (idx, card_prefix, meta) = (self.k("idx"), self.k("card:"), self.k("meta"));
        // first: a rebuild that fails half way must not leave the old meta (and its fingerprints) vouching for a partial
        // index, and a search that read it before must see it gone (see `search_checked`)
        self.q::<()>(redis::cmd("DEL").arg(&meta))?;
        self.drop_index()?;
        let card_pat = format!("{}card:*", glob_escape(&self.prefix));
        self.unlink_matching(&card_pat)?;
        let mut create = redis::cmd("FT.CREATE");
        create.arg(&idx).arg("ON").arg("JSON").arg("PREFIX").arg(1).arg(&card_prefix).arg("STOPWORDS").arg(0).arg("SCHEMA");
        create.arg("$.terms[*]").arg("AS").arg("terms").arg("TAG");
        create.arg("$.eligible").arg("AS").arg("eligible").arg("TAG");
        for f in TEXT_FIELDS {
            create.arg(format!("$.{f}")).arg("AS").arg(f).arg("TEXT").arg("NOSTEM").arg("WITHSUFFIXTRIE");
        }
        self.q::<()>(&create)?;

        let mut seen: HashSet<&str> = HashSet::new();
        let unique: Vec<&Card> = cards.iter().rev().filter(|c| seen.insert(c.id.as_str())).collect();
        for chunk in unique.chunks(BATCH) {
            let mut pipe = redis::pipe();
            for c in chunk {
                pipe.cmd("JSON.SET").arg(format!("{card_prefix}{}", c.id)).arg("$").arg(doc(c, vocab).to_string()).ignore();
            }
            pipe.query::<()>(&mut self.con).map_err(rerr)?;
        }
        let eligible = unique.iter().filter(|c| c.skip_reason.is_none()).count();
        let sha = data_dir.and_then(git_sha);
        let mut fields: Vec<(&str, String)> = vec![("schema_version", SCHEMA_VERSION.to_string()), ("card_count", unique.len().to_string()), ("eligible_count", eligible.to_string())];
        // new on every rebuild, so `search_checked` notices a rebuild even from the very same files
        fields.push(("build_id", random_hex()?));
        if let Some(s) = &sha {
            fields.push(("git_sha", s.clone()));
        }
        if let Some(fp) = fp {
            fields.push(("vocab_fp", fp.vocab_fp.clone()));
            fields.push(("cards_fp", fp.cards_fp.clone()));
        }
        self.write_meta(token, &fields)?;
        Ok(RebuildReport { cards: unique.len(), eligible, git_sha: sha })
    }

    /// Replace `meta` with `fields` while `<prefix>rebuild-lock` still holds `token` (`WRITE_META_LUA`). A rebuild whose
    /// lock expired (and may have been taken by another rebuild meanwhile) writes nothing and fails.
    fn write_meta(&mut self, token: &str, fields: &[(&str, String)]) -> Result<()> {
        let (lock, meta) = (self.k("rebuild-lock"), self.k("meta"));
        let mut cmd = redis::cmd("EVAL");
        cmd.arg(WRITE_META_LUA).arg(2).arg(&lock).arg(&meta).arg(token);
        for (k, v) in fields {
            cmd.arg(*k).arg(v);
        }
        let written: i64 = self.q(&cmd)?;
        if written != 1 {
            return Err(Error::msg(
                "redis: the rebuild lock expired before this rebuild could write the index meta, so another rebuild may have run at the same time; the meta was not written; rebuild again",
            ));
        }
        Ok(())
    }

    pub fn meta(&mut self) -> Result<BTreeMap<String, String>> {
        let key = self.k("meta");
        self.q(redis::cmd("HGETALL").arg(key))
    }

    /// Fail closed when the index no longer matches the vocabulary file and card files it was built from: `current` are
    /// their fingerprints now (`Fingerprints::of_files`), `rebuild_hint` is the command that rebuilds (shown in the
    /// error). An index without fingerprints (built by `rebuild`, or by an older `ks`) or without a build id (an older
    /// `ks`: `search_checked` could not tell a rebuild from the same files) cannot be checked and is refused too. Call it
    /// right before `search_checked` on every Redis search; the file backend has no index to go stale.
    pub fn check_fresh(&mut self, current: &Fingerprints, rebuild_hint: &str) -> Result<Checked> {
        let meta = self.meta()?;
        if meta.is_empty() {
            return Err(Error::msg(format!("redis: the card index does not exist; run `{rebuild_hint}`")));
        }
        let (Some(vocab_fp), Some(cards_fp), Some(_)) = (meta.get("vocab_fp"), meta.get("cards_fp"), meta.get("build_id")) else {
            return Err(Error::msg(format!(
                "redis: the card index was built without fingerprints or a build id (by an older `ks index rebuild`), so it cannot be checked against the vocab file and card files; run `{rebuild_hint}`"
            )));
        };
        let changed: Vec<&str> = [("the vocab file", *vocab_fp != current.vocab_fp), ("the card files", *cards_fp != current.cards_fp)]
            .into_iter()
            .filter(|(_, c)| *c)
            .map(|(what, _)| what)
            .collect();
        if changed.is_empty() {
            return Ok(Checked(meta));
        }
        Err(Error::msg(format!("redis: the card index is stale: {} changed since it was built; run `{rebuild_hint}`", changed.join(" and "))))
    }

    /// Queries are only a superset filter if wildcard expansion is not capped and a timeout fails instead of returning
    /// partial results (both are silent defaults in the Query Engine); the timeout itself must be finite so a runaway
    /// query cannot hang the server. These come from the server's own config (docker-compose.yml): verify, never change.
    fn ensure_search_config(&mut self) -> Result<()> {
        if self.search_config_checked {
            return Ok(());
        }
        type Check = (&'static str, &'static str, fn(&str) -> bool);
        let checks: [Check; 3] = [
            ("search-max-prefix-expansions", "at least 1000000", |v| v.parse::<u64>().is_ok_and(|n| n >= 1_000_000)),
            ("search-timeout", "a finite timeout above 0 ms (10000)", |v| v.parse::<u64>().is_ok_and(|n| n > 0)),
            ("search-on-timeout", "FAIL", |v| v.eq_ignore_ascii_case("fail")),
        ];
        for (name, want, ok) in checks {
            let cur: Vec<String> = self.q(redis::cmd("CONFIG").arg("GET").arg(name))?;
            let cur = cur.get(1).cloned().unwrap_or_default();
            if !ok(&cur) {
                return Err(Error::msg(format!(
                    "redis: refusing to search: {name} is {cur:?} but must be {want}. The settings come from docker-compose.yml \
                     (loadmodule ... MAXPREFIXEXPANSIONS 1000000000 TIMEOUT 10000 ON_TIMEOUT FAIL); restart the container with them: \
                     docker compose up -d --force-recreate redis"
                )));
            }
        }
        self.search_config_checked = true;
        Ok(())
    }

    fn search_cmd(&mut self, query: &str, offset: usize, count: usize) -> Result<(usize, Vec<String>)> {
        let idx = self.k("idx");
        let card_prefix = self.k("card:");
        let reply: Vec<Value> = redis::cmd("FT.SEARCH").arg(idx).arg(query).arg("NOCONTENT").arg("LIMIT").arg(offset).arg(count).arg("DIALECT").arg(2).query(&mut self.con).map_err(|e| {
            if e.to_string().to_lowercase().contains("index not found") {
                Error::msg("redis: the card index does not exist; run `ks index rebuild --data <dir>` first (it reads <dir>/vocab/terms.yaml|yml|json, or pass --vocab <file>)")
            } else {
                rerr(e)
            }
        })?;
        let total = match reply.first() {
            Some(Value::Int(n)) => *n as usize,
            _ => return Err(Error::msg("redis: unexpected FT.SEARCH reply")),
        };
        let ids = reply[1..].iter().filter_map(as_string).map(|k| k.strip_prefix(&card_prefix).unwrap_or(&k).to_string()).collect();
        Ok((total, ids))
    }

    fn candidate_ids(&mut self, query: &str) -> Result<Vec<String>> {
        let mut ids: Vec<String> = vec![];
        loop {
            let (total, page) = self.search_cmd(query, ids.len(), PAGE)?;
            let done = page.is_empty();
            ids.extend(page);
            if done || ids.len() >= total {
                return Ok(ids);
            }
        }
    }

    fn count(&mut self, query: &str) -> Result<usize> {
        Ok(self.search_cmd(query, 0, 0)?.0)
    }

    fn fetch_cards(&mut self, ids: &[String]) -> Result<Vec<Card>> {
        let mut out = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(BATCH) {
            let mut cmd = redis::cmd("JSON.MGET");
            for id in chunk {
                cmd.arg(self.k(&format!("card:{id}")));
            }
            cmd.arg("$.card");
            let replies: Vec<Option<String>> = self.q(&cmd)?;
            for r in replies.into_iter().flatten() {
                let v: serde_json::Value = serde_json::from_str(&r).map_err(|e| Error::msg(format!("indexed card: {e}")))?;
                let v = match v {
                    serde_json::Value::Array(mut a) if a.len() == 1 => a.remove(0),
                    v => v,
                };
                out.push(card_from_json(&v)?);
            }
        }
        Ok(out)
    }

    /// `search`, kept only if `meta` is still the one `check_fresh` accepted. A rebuild deletes `meta` before it touches
    /// the index and writes a new `build_id` when it ends, so a changed or missing `meta` means the search may have read a
    /// half-built index: the results are dropped and the search fails.
    pub fn search_checked(&mut self, checked: &Checked, vocab: &Vocab, queries: &[Query], limit: usize) -> Result<Vec<Ranking>> {
        let out = self.search(vocab, queries, limit)?;
        if self.meta()? != checked.0 {
            return Err(Error::msg("redis: the card index was rebuilt while this search ran, so its results were dropped; search again"));
        }
        Ok(out)
    }

    /// Same results as `collect::collect_ranked` over the cards that were indexed (see the module docs).
    pub fn search(&mut self, vocab: &Vocab, queries: &[Query], limit: usize) -> Result<Vec<Ranking>> {
        self.ensure_search_config()?;
        let terms: Vec<QueryTerms> = queries.iter().map(|q| query_terms(q, vocab)).collect();
        let mut ids_per_query: Vec<Vec<String>> = vec![];
        for t in &terms {
            ids_per_query.push(match candidate_query(t) {
                Some(q) => self.candidate_ids(&q)?,
                None => vec![],
            });
        }
        let mut union: Vec<String> = vec![];
        let mut seen: HashSet<&str> = HashSet::new();
        for id in ids_per_query.iter().flatten() {
            if seen.insert(id) {
                union.push(id.clone());
            }
        }
        let cards = self.fetch_cards(&union)?;
        let entries: Vec<Entry> = cards.iter().map(|c| Entry::new(c, vocab)).collect();
        let by_id: HashMap<&str, usize> = cards.iter().enumerate().map(|(i, c)| (c.id.as_str(), i)).collect();

        let mut df: HashMap<String, usize> = HashMap::new();
        for t in terms.iter().flat_map(|t| &t.canon) {
            if !df.contains_key(t) {
                let n = self.count(&format!("@eligible:{{1}} @terms:{{{}}}", escape_tag(t)))?;
                df.insert(t.clone(), n);
            }
        }
        let l1_cards = self.count("@eligible:{1}")?;
        Ok(queries
            .iter()
            .zip(&terms)
            .zip(&ids_per_query)
            .map(|((q, t), ids)| {
                let subset: Vec<&Entry> = ids.iter().filter_map(|id| by_id.get(id.as_str()).map(|&i| &entries[i])).collect();
                let found: Vec<Found> = rank_entries(&subset, t, limit, &|term| df.get(term).copied().unwrap_or(0));
                Ranking { qid: q.qid.clone(), l1_cards, found, unknown_terms: unknown_terms(t, vocab) }
            })
            .collect())
    }

    /// XADD one application record to `<prefix>apply`; returns the stream id.
    pub fn record(&mut self, a: &Application) -> Result<String> {
        a.validate()?;
        let key = self.k("apply");
        let mut cmd = redis::cmd("XADD");
        // bounded: older records are trimmed (approximately) once the stream passes MAX_APPLICATIONS
        cmd.arg(key).arg("MAXLEN").arg("~").arg(MAX_APPLICATIONS).arg("*").arg("task").arg(&a.task).arg("card").arg(&a.card).arg("verdict").arg(&a.verdict).arg("reason").arg(&a.reason);
        if let Some(o) = &a.outcome {
            cmd.arg("outcome").arg(o);
        }
        self.q(&cmd)
    }

    /// Append stream entries that are not yet in `<out_dir>/applications.jsonl`. Existing lines are never rewritten;
    /// a file with a line that is not a JSON object with an `id` is refused untouched.
    pub fn dump_applications(&mut self, out_dir: &Path) -> Result<Dump> {
        use std::io::Write;
        let path = out_dir.join("applications.jsonl");
        let existing = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(Error::msg(format!("{}: {e}", path.display()))),
        };
        let mut have: HashSet<String> = HashSet::new();
        let mut lines = 0usize;
        for (n, l) in existing.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            let id = serde_json::from_str::<serde_json::Value>(l).ok().and_then(|v| v.get("id").and_then(|i| i.as_str().map(str::to_string)));
            match id {
                Some(id) => {
                    have.insert(id);
                    lines += 1;
                }
                None => return Err(Error::msg(format!("{}:{}: not an application record; refusing to touch the file", path.display(), n + 1))),
            }
        }
        let key = self.k("apply");
        let mut fresh = String::new();
        let mut appended = 0usize;
        let mut start = "-".to_string();
        loop {
            let page: Vec<(String, Vec<String>)> = self.q(redis::cmd("XRANGE").arg(&key).arg(&start).arg("+").arg("COUNT").arg(PAGE))?;
            let Some(last) = page.last().map(|(id, _)| id.clone()) else { break };
            let n = page.len();
            for (id, fields) in page {
                if have.contains(&id) {
                    continue;
                }
                let mut m = Map::new();
                m.insert("id".into(), json!(id));
                for kv in fields.chunks(2) {
                    if let [k, v] = kv {
                        m.insert(k.clone(), json!(v));
                    }
                }
                fresh.push_str(&serde_json::Value::Object(m).to_string());
                fresh.push('\n');
                appended += 1;
            }
            if n < PAGE {
                break;
            }
            start = format!("({last}");
        }
        if appended > 0 {
            std::fs::create_dir_all(out_dir).map_err(|e| Error::msg(format!("{}: {e}", out_dir.display())))?;
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| Error::msg(format!("{}: {e}", path.display())))?;
            if !existing.is_empty() && !existing.ends_with('\n') {
                fresh.insert(0, '\n');
            }
            f.write_all(fresh.as_bytes()).map_err(|e| Error::msg(format!("{}: {e}", path.display())))?;
        }
        Ok(Dump { appended, total: lines + appended })
    }

    /// Delete the index and every key under this prefix, including the application stream. For tests and manual cleanup.
    pub fn purge(&mut self) -> Result<()> {
        self.drop_index()?;
        let pat = format!("{}*", glob_escape(&self.prefix));
        self.unlink_matching(&pat)
    }
}

#[cfg(test)]
mod tests {
    //! Needs the private meta write, so it lives here. Runs only when KS_TEST_REDIS_URL is set; its key prefix is purged
    //! afterwards.
    use super::*;

    struct Purge(String, String);
    impl Drop for Purge {
        fn drop(&mut self) {
            if let Ok(mut i) = Index::connect(&self.0, &self.1) {
                let _ = i.purge();
            }
        }
    }

    fn meta_of(idx: &mut Index) -> Vec<(String, String)> {
        idx.meta().unwrap().into_iter().collect()
    }

    #[test]
    fn meta_is_written_whole_and_only_while_the_rebuild_lock_still_holds_the_token() {
        let Some(url) = std::env::var("KS_TEST_REDIS_URL").ok().filter(|u| !u.is_empty()) else { return };
        let prefix = format!("kstest-unit-{}:", std::process::id());
        let _purge = Purge(url.clone(), prefix.clone());
        let mut idx = Index::connect(&url, &prefix).unwrap();
        let (lock, meta) = (format!("{prefix}rebuild-lock"), format!("{prefix}meta"));
        let mut con = redis::Client::open(url.as_str()).unwrap().get_connection().unwrap();
        let old = vec![("stale".to_string(), "1".to_string())];
        let new = [("card_count", "3".to_string()), ("build_id", "b".to_string())];
        redis::cmd("HSET").arg(&meta).arg("stale").arg("1").query::<()>(&mut con).unwrap();

        // the lock expired: nobody holds it
        let e = idx.write_meta("mine", &new).unwrap_err().to_string();
        assert!(e.contains("lock expired") && e.contains("rebuild again"), "{e}");
        assert_eq!(meta_of(&mut idx), old, "meta written without the lock");
        // the lock expired and another rebuild took it
        redis::cmd("SET").arg(&lock).arg("theirs").query::<()>(&mut con).unwrap();
        assert!(idx.write_meta("mine", &new).is_err());
        assert_eq!(meta_of(&mut idx), old, "meta written under another rebuild's lock");
        // still ours: the meta is replaced whole, no field of the old one is left
        redis::cmd("SET").arg(&lock).arg("mine").query::<()>(&mut con).unwrap();
        idx.write_meta("mine", &new).unwrap();
        assert_eq!(meta_of(&mut idx), vec![("build_id".to_string(), "b".to_string()), ("card_count".to_string(), "3".to_string())]);

        // a whole rebuild whose lock is gone by the time it writes meta leaves no meta behind
        redis::cmd("SET").arg(&lock).arg("theirs").query::<()>(&mut con).unwrap();
        let card = Card::new("path:x");
        let vocab = Vocab::from_json(r#"{"version":1,"terms":[]}"#, "t").unwrap();
        let e = idx.rebuild_locked("mine", &[card], &vocab, None, None).unwrap_err().to_string();
        assert!(e.contains("lock expired"), "{e}");
        assert!(idx.meta().unwrap().is_empty(), "meta written by a rebuild that lost its lock");
    }
}
