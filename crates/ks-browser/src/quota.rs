//! Per-profile search quota, enforced by the daemon before a query is sent. The daemon is the one
//! place every search of a profile goes through (CLI and MCP alike), and the history of sent
//! queries lives in `run/<profile>.quota` (0600, one unix timestamp per line, pruned to the last
//! 24 hours), so it is shared by both and survives daemon restarts. Limits: KS_QUOTA_HOURLY
//! (default 20) and KS_QUOTA_DAILY (default 100), read when the daemon starts.
use crate::paths::write_private_file;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub const DEFAULT_HOURLY: u32 = 20;
pub const DEFAULT_DAILY: u32 = 100;
pub const ENV_HOURLY: &str = "KS_QUOTA_HOURLY";
pub const ENV_DAILY: &str = "KS_QUOTA_DAILY";
const HOUR: u64 = 3600;
const DAY: u64 = 86_400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quota {
    pub hourly: u32,
    pub daily: u32,
}

impl Default for Quota {
    fn default() -> Quota {
        Quota { hourly: DEFAULT_HOURLY, daily: DEFAULT_DAILY }
    }
}

impl Quota {
    /// Limits from the two setting values (None = default); each must be a positive integer.
    pub fn from_values(hourly: Option<&str>, daily: Option<&str>) -> Result<Quota, String> {
        let parse = |v: Option<&str>, name: &str, default: u32| match v {
            None => Ok(default),
            Some(s) => s.parse::<u32>().ok().filter(|n| *n > 0).ok_or_else(|| format!("{name} must be a positive integer, got {s:?}")),
        };
        Ok(Quota { hourly: parse(hourly, ENV_HOURLY, DEFAULT_HOURLY)?, daily: parse(daily, ENV_DAILY, DEFAULT_DAILY)? })
    }

    pub fn from_env() -> Result<Quota, String> {
        let get = |k: &str| std::env::var(k).ok();
        Quota::from_values(get(ENV_HOURLY).as_deref(), get(ENV_DAILY).as_deref())
    }

    /// None when one more query may be sent at `now`; otherwise the reason (it starts with "quota").
    /// Timestamps from the future (the clock moved back) count as recent.
    pub fn exceeded(&self, history: &[u64], now: u64) -> Option<String> {
        let within = |window: u64| history.iter().filter(|t| now.saturating_sub(**t) < window).count();
        if within(HOUR) >= self.hourly as usize {
            return Some(format!("quota: {} searches in the last hour reached the hourly limit ({ENV_HOURLY}={})", within(HOUR), self.hourly));
        }
        if within(DAY) >= self.daily as usize {
            return Some(format!("quota: {} searches in the last 24 hours reached the daily limit ({ENV_DAILY}={})", within(DAY), self.daily));
        }
        None
    }

    /// Let one query through at `now`: count it in `path` and return Ok, or return the reason it is
    /// refused (the quota is reached, or the history cannot be read or written) with nothing counted.
    pub fn admit(&self, path: &Path, now: u64) -> Result<(), String> {
        if let Some(reason) = self.exceeded(&load(path)?, now) {
            return Err(reason);
        }
        record(path, now)
    }
}

/// The recorded query times, oldest first. No file means no history and blank lines are skipped,
/// but a file that exists and cannot be read (not UTF-8, a symlink, no permission, not a regular
/// file) or has a line that is not a timestamp fails closed: the error says what the human can do,
/// and no search may run.
pub fn load(path: &Path) -> Result<Vec<u64>, String> {
    let refuse = |why: String| {
        format!(
            "the search quota history {p} cannot be read ({why}), so searches are refused. Check what is at that path; to start a new history run `rm {p}` (the hourly and daily limits then count from zero)",
            p = path.display()
        )
    };
    let mut text = String::new();
    let opened = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path);
    match opened.and_then(|mut f| f.read_to_string(&mut text)) {
        Ok(_) => text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
            .map(|(i, l)| l.trim().parse().map_err(|_| refuse(format!("line {} is not a timestamp", i + 1))))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(refuse(e.to_string())),
    }
}

/// Add one query at `now`, dropping entries older than a day, and rewrite the file 0600.
pub fn record(path: &Path, now: u64) -> Result<(), String> {
    let mut history: Vec<u64> = load(path)?.into_iter().filter(|t| now.saturating_sub(*t) < DAY).collect();
    history.push(now);
    write_private_file(path, history.iter().map(|t| format!("{t}\n")).collect::<String>().as_bytes()).map_err(|e| format!("quota file {}: {e}", path.display()))
}
