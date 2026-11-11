//! Collection queue: the URLs a search discovered, appended to a JSONL file that another program reads.
//! One line per URL, `{"v":1,"card_key","url","title","engine","query","found_at"}`; see ARCHITECTURE.md, "Collection queue".
use crate::card::card_key_of;
use crate::collect::load_cards;
use crate::normalize::normalize_url;
use crate::{Error, Result};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueEntry {
    pub card_key: String,
    pub url: String,
    pub title: String,
    pub engine: String,
    pub query: String,
    pub found_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Enqueued {
    pub enqueued: usize,
    /// Entries not written because their card_key was already in the queue file, already has a card, or came twice.
    pub skipped_known: usize,
}

/// The line as written. Field order is the format's key order.
#[derive(Serialize)]
struct Line<'a> {
    v: u8,
    card_key: &'a str,
    url: &'a str,
    title: &'a str,
    engine: &'a str,
    query: &'a str,
    found_at: &'a str,
}

impl QueueEntry {
    pub fn new(card_key: &str, url: &str, title: &str, engine: &str, query: &str, found_at: &str) -> QueueEntry {
        QueueEntry { card_key: card_key.into(), url: url.into(), title: title.into(), engine: engine.into(), query: query.into(), found_at: found_at.into() }
    }

    /// The JSON line, without the trailing newline (JSON escapes any newline inside a field).
    pub fn to_line(&self) -> String {
        let l = Line { v: 1, card_key: &self.card_key, url: &self.url, title: &self.title, engine: &self.engine, query: &self.query, found_at: &self.found_at };
        serde_json::to_string(&l).expect("a struct of strings serializes")
    }
}

/// The queue file's parent directory must already exist; it is never created.
pub fn check_parent(path: &Path) -> Result<()> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    match std::fs::metadata(parent) {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => Err(Error::msg(format!("{}: not a directory (the parent of the queue file {})", parent.display(), path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(Error::msg(format!("{}: directory does not exist (the queue file's parent is not created automatically)", parent.display())))
        }
        Err(e) => Err(Error::msg(format!("{}: {e}", parent.display()))),
    }
}

/// The card_keys already in the file. A line that is not a JSON object with a string `card_key` is an error:
/// skipping it would hide damage and could queue a URL twice.
fn keys_in(text: &str, path: &Path) -> Result<HashSet<String>> {
    let mut keys = HashSet::new();
    for (i, line) in text.split('\n').enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let key = serde_json::from_str::<Value>(line).ok().and_then(|v| v.get("card_key")?.as_str().map(String::from));
        match key {
            Some(k) => {
                keys.insert(k);
            }
            None => return Err(Error::msg(format!("{}: line {} is not a queue entry (a JSON object with a string card_key); nothing was written", path.display(), i + 1))),
        }
    }
    Ok(keys)
}

/// Append the entries whose card_key is neither in the file nor in `known` (the card_keys that already have a card),
/// as one write under an exclusive flock. The file is created with mode 0600 (an existing file keeps its mode).
pub fn enqueue(path: &Path, entries: &[QueueEntry], known: &HashSet<String>) -> Result<Enqueued> {
    check_parent(path)?;
    let io = |e: std::io::Error| Error::msg(format!("{}: {e}", path.display()));
    let mut f = std::fs::OpenOptions::new().read(true).append(true).create(true).mode(0o600).open(path).map_err(io)?;
    // blocks until another writer is done; released when `f` is dropped
    while unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(io(e));
        }
    }
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes).map_err(io)?;
    let text = String::from_utf8(bytes).map_err(|_| Error::msg(format!("{}: not valid UTF-8; nothing was written", path.display())))?;
    let mut seen = keys_in(&text, path)?;

    let mut out = String::new();
    let (mut enqueued, mut skipped_known) = (0, 0);
    for e in entries {
        if known.contains(&e.card_key) || !seen.insert(e.card_key.clone()) {
            skipped_known += 1;
            continue;
        }
        out.push_str(&e.to_line());
        out.push('\n');
        enqueued += 1;
    }
    if enqueued > 0 {
        // a last line without its newline is valid JSON here (a torn one failed above): finish it first
        if !text.is_empty() && !text.ends_with('\n') {
            out.insert(0, '\n');
        }
        f.write_all(out.as_bytes()).map_err(io)?;
    }
    Ok(Enqueued { enqueued, skipped_known })
}

/// Queue entries for the `results` array of one search answer (`{title,url,engine,fetched_at,..}` each).
pub fn entries_from_results(query: &str, results: &Value) -> Result<Vec<QueueEntry>> {
    let arr = results.as_array().ok_or_else(|| Error::msg("search results are not an array"))?;
    arr.iter()
        .map(|r| {
            let field = |k: &str| r.get(k).and_then(Value::as_str).ok_or_else(|| Error::msg(format!("search result has no string {k}: {r}")));
            let raw = field("url")?;
            let url = normalize_url(raw).ok_or_else(|| Error::msg(format!("search result url is not an http(s) URL: {raw:?}")))?;
            Ok(QueueEntry::new(&card_key_of(&url), &url, field("title")?, field("engine")?, query, field("fetched_at")?))
        })
        .collect()
}

/// The card_keys of the cards under `<data>/cards`.
pub fn known_card_keys(data: &Path) -> Result<HashSet<String>> {
    Ok(load_cards(&data.join("cards"))?.into_iter().map(|c| c.card_key).collect())
}

/// Accumulates what one `ks research search` run queues, search by search.
pub struct QueueWriter {
    path: std::path::PathBuf,
    known: HashSet<String>,
    total: Enqueued,
}

impl QueueWriter {
    /// Fails now if the file's parent directory is missing, so no search runs for nothing.
    pub fn new(path: &Path, known: HashSet<String>) -> Result<QueueWriter> {
        check_parent(path)?;
        Ok(QueueWriter { path: path.to_path_buf(), known, total: Enqueued { enqueued: 0, skipped_known: 0 } })
    }

    /// Queue the `results` array of one finished search.
    pub fn add(&mut self, query: &str, results: &Value) -> Result<()> {
        let r = enqueue(&self.path, &entries_from_results(query, results)?, &self.known)?;
        self.total.enqueued += r.enqueued;
        self.total.skipped_known += r.skipped_known;
        Ok(())
    }

    /// The `queue` member of the command's JSON output.
    pub fn report(&self) -> Value {
        serde_json::json!({"path": self.path.display().to_string(), "enqueued": self.total.enqueued, "skipped_known": self.total.skipped_known})
    }
}
