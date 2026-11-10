//! Human-intervention events: the needs_human response, events.log and the desktop notification.
//! Nothing here ever records cookies, credentials, account names or query text.
use crate::paths::{ensure_private_dir, Paths};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Event {
    AuthRequired,
    AuthExpired,
    Captcha,
    Consent,
    MfaPrompt,
    Blocked,
    EngineSwitch,
    DaemonReplace,
}

impl Event {
    pub const ALL: [Event; 8] = [
        Event::AuthRequired,
        Event::AuthExpired,
        Event::Captcha,
        Event::Consent,
        Event::MfaPrompt,
        Event::Blocked,
        Event::EngineSwitch,
        Event::DaemonReplace,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Event::AuthRequired => "AUTH_REQUIRED",
            Event::AuthExpired => "AUTH_EXPIRED",
            Event::Captcha => "CAPTCHA",
            Event::Consent => "CONSENT",
            Event::MfaPrompt => "MFA_PROMPT",
            Event::Blocked => "BLOCKED",
            Event::EngineSwitch => "ENGINE_SWITCH",
            Event::DaemonReplace => "DAEMON_REPLACE",
        }
    }
    pub fn parse(s: &str) -> Option<Event> {
        Event::ALL.into_iter().find(|e| e.as_str() == s)
    }
}

fn profile_flag(profile: &str) -> String {
    if profile == "default" {
        String::new()
    } else {
        format!(" --profile {profile}")
    }
}

/// `{status: "needs_human", event, reason, steps[], resume_cmd}`: what the CLI prints (exit 3) and
/// what the MCP tool returns unchanged.
pub fn needs_human(event: Event, reason: &str, profile: &str) -> Value {
    let p = profile_flag(profile);
    let handoff = format!("Run `ks handoff {}{p}` to bring up the browser window on the blocked page.", event.as_str());
    let resume = format!("ks resume{p}");
    let (steps, resume_cmd): (Vec<String>, String) = match event {
        // Google refuses sign-in in the browser ks drives over CDP: signing in (2-Step Verification
        // included) always happens in the plain Chrome window of `ks auth google`
        Event::AuthRequired | Event::AuthExpired | Event::MfaPrompt => (
            vec![
                format!("Run `ks auth google{p}`: it closes the browser ks controls for this profile and opens a normal Chrome window (not controlled by ks) on the Google sign-in page. Google refuses sign-in in the browser ks controls."),
                "Sign in yourself in that window, including 2-Step Verification. knowledge-scout never asks for, types or stores your password.".into(),
                "Press Enter in the terminal: ks closes that window so Chrome saves the sign-in, checks the signed-in state headless and prints only yes or no.".into(),
                "Run the search again.".into(),
            ],
            format!("ks auth google{p}"),
        ),
        Event::Captcha => (
            vec![
                handoff,
                "Solve the CAPTCHA yourself in that window. knowledge-scout never solves or bypasses CAPTCHAs.".into(),
                format!("Run `{resume}` to hand control back."),
                "If this is the second CAPTCHA today, stop collecting for today.".into(),
            ],
            resume,
        ),
        Event::Consent => (
            vec![handoff, "Make your own choice on the consent screen in that window.".into(), format!("Run `{resume}` to hand control back.")],
            resume,
        ),
        // the local quota (reason "quota…") sent nothing to Google and is not pending: it lifts by itself
        Event::Blocked if reason.starts_with("quota") => (
            vec![
                "No request was sent to Google: this profile reached its own search quota (see the reason).".into(),
                format!("Wait until older searches leave the window (1 hour for the hourly limit, 24 hours for the daily one), or raise KS_QUOTA_HOURLY / KS_QUOTA_DAILY: run `ks browser disconnect{p}` and search again with the new value set."),
                "Then run the search again; there is nothing to resume.".into(),
            ],
            format!("ks research search --engine google-browser --query <query>{p}"),
        ),
        // pending: searches stay refused until `ks resume` finds a normal result page
        Event::Blocked => (
            vec![
                "Stop collecting now: Google is refusing or rate-limiting searches from this browser.".into(),
                "Wait before trying again (for example until tomorrow), and decide whether to lower the query frequency.".into(),
                format!("When you want to continue, run `ks handoff BLOCKED{p}` and check in that window yourself that a Google search shows normal results (no CAPTCHA, no error page)."),
                format!("Then run `{resume}`: searches stay refused until it finds a normal Google result page."),
            ],
            resume,
        ),
        Event::EngineSwitch => (
            vec![
                "Decide whether to switch from google-browser to another engine (an API engine uses your key and quota).".into(),
                "Re-run the search with the engine you approve.".into(),
            ],
            format!("ks research search --engine <engine> --query <query>{p}"),
        ),
        // the same build with a visible window (another build: `other_build_daemon`)
        Event::DaemonReplace => (
            vec![
                "The browser for this profile is running in a visible window; switching it to headless closes that window and its tabs. The login stays in the profile.".into(),
                "Finish what you are doing in that window.".into(),
                format!("Run `ks browser connect{p}` to switch to headless, or re-run with --mode headful."),
            ],
            format!("ks browser connect{p}"),
        ),
    };
    json!({"status": "needs_human", "event": event.as_str(), "reason": reason, "steps": steps, "resume_cmd": resume_cmd})
}

/// DAEMON_REPLACE for a daemon of another `ks` build that cannot be replaced without the human:
/// it is busy, holds an unresolved interrupt (`pending`), or else has a visible window open. The
/// steps lead out instead of back into this answer: `ks status`, `ks handoff` and `ks resume` talk
/// to that daemon without replacing it, and `ks browser disconnect` stops it; none of them starts a
/// daemon. Only the steps after that disconnect start this build's daemon (`ks auth google` for
/// its sign-in check, or running the command again).
pub fn other_build_daemon(busy: bool, pending: Option<&str>, profile: &str) -> Value {
    let p = profile_flag(profile);
    let disconnect = format!("ks browser disconnect{p}");
    let (reason, steps, resume_cmd): (String, Vec<String>, String) = if busy {
        (
            "the browser daemon of another ks build is busy with a command; it is replaced only when idle".into(),
            vec![
                "A browser daemon started by another ks build (an older or rebuilt binary) is running a command right now.".into(),
                format!("Wait a few seconds until `ks status{p}` answers again, then run your command again. An idle headless daemon of another build is replaced by itself."),
            ],
            format!("ks status{p}"),
        )
    } else if let Some(e) = pending.filter(|e| matches!(Event::parse(e), Some(Event::AuthRequired | Event::AuthExpired | Event::MfaPrompt))) {
        // signing in never happens in that daemon's window (Google refuses it there): stop it, then
        // `ks auth google`, which starts no daemon of its own until the sign-in window is closed
        (
            format!("the browser daemon of another ks build holds an unresolved {e}"),
            vec![
                format!("Run `{disconnect}` to close its window and stop the old daemon. The unresolved {e} is kept."),
                format!("Then run `ks auth google{p}` and sign in in the normal Chrome window it opens; that clears {e}."),
            ],
            disconnect,
        )
    } else if let Some(e) = pending {
        (
            format!("the browser daemon of another ks build holds an unresolved {e}; replacing it now would close the page it stopped on"),
            vec![
                format!("Run `ks handoff {e}{p}`: the running daemon opens its window on that page (it is not replaced)."),
                "Handle it yourself in that window.".into(),
                format!("Run `ks resume{p}` to confirm it is resolved."),
                format!("Then run `{disconnect}` to close that window and stop the old daemon, and run your command again: this build starts its own daemon."),
                format!("Or run `{disconnect}` right away: the unresolved {e} is kept and this build's daemon refuses searches until it is resolved."),
            ],
            format!("ks resume{p}"),
        )
    } else {
        (
            "the browser daemon of another ks build has a visible window open; replacing it would close that window and its tabs".into(),
            vec![
                "Finish what you are doing in that window.".into(),
                format!("Run `{disconnect}` to close it and stop the old daemon. The login stays in the profile."),
                "Run your command again: this build starts its own daemon.".into(),
            ],
            disconnect,
        )
    };
    json!({"status": "needs_human", "event": Event::DaemonReplace.as_str(), "reason": reason, "steps": steps, "resume_cmd": resume_cmd})
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LogEntry {
    pub ts: String,
    pub event: Option<Event>,
    pub profile: String,
    pub mode: String,
    /// Position of the query in its run, never the query text.
    pub query_index: Option<usize>,
    /// stopped | handoff | resumed
    pub result: String,
    pub reason: String,
}

/// Append one JSON line to `<KS_HOME>/events.log` (0600).
pub fn append_log(paths: &Paths, entry: &LogEntry) -> std::io::Result<()> {
    ensure_private_dir(&paths.home)?;
    let path = paths.events_log();
    let mut f = fs::OpenOptions::new().append(true).create(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(&path)?;
    if f.metadata()?.permissions().mode() & 0o777 != 0o600 {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }
    let mut line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
    line.push('\n');
    f.write_all(line.as_bytes())
}

/// The last `n` parseable entries. Reads the whole file: fine while the log is small (a few lines per
/// day); switch to reading from the end if it ever grows to megabytes.
pub fn tail_log(paths: &Paths, n: usize) -> Vec<LogEntry> {
    let Ok(text) = fs::read_to_string(paths.events_log()) else { return vec![] };
    let all: Vec<LogEntry> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    all[all.len().saturating_sub(n)..].to_vec()
}

/// Current UTC time as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn utc_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil_from_days (H. Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// macOS desktop notification through the osascript binary (no shell). Disabled with KS_NOTIFY=0.
/// The text is built from fixed strings and the event name only.
pub fn notify(event: Event, profile: &str) {
    if std::env::var("KS_NOTIFY").as_deref() == Ok("0") || !cfg!(target_os = "macos") {
        return;
    }
    let msg = format!("{} on profile {profile}: run ks status", event.as_str());
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!("display notification \"{}\" with title \"knowledge-scout needs you\"", esc(&msg));
    let _ = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}
