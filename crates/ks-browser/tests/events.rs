//! needs_human responses and events.log (no cookies, credentials or query text).
use ks_browser::events::{append_log, needs_human, tail_log, utc_now, Event, LogEntry};
use ks_browser::paths::Paths;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);
fn tmp() -> std::path::PathBuf {
    let p = std::path::PathBuf::from(format!("/tmp/ks-ev-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&p);
    p
}

#[test]
fn event_names_round_trip() {
    let names: Vec<&str> = Event::ALL.iter().map(|e| e.as_str()).collect();
    assert_eq!(names, ["AUTH_REQUIRED", "AUTH_EXPIRED", "CAPTCHA", "CONSENT", "MFA_PROMPT", "BLOCKED", "ENGINE_SWITCH", "DAEMON_REPLACE"]);
    for e in Event::ALL {
        assert_eq!(Event::parse(e.as_str()), Some(e));
        assert_eq!(serde_json::to_value(e).unwrap(), serde_json::json!(e.as_str()));
    }
    assert_eq!(Event::parse("captcha"), None);
}

#[test]
fn needs_human_has_the_contract_shape_for_every_event() {
    for e in Event::ALL {
        let v = needs_human(e, "why", "default");
        assert_eq!(v["status"], "needs_human", "{v}");
        assert_eq!(v["event"], e.as_str());
        assert_eq!(v["reason"], "why");
        let steps = v["steps"].as_array().unwrap();
        assert!(!steps.is_empty() && steps.iter().all(|s| s.as_str().is_some_and(|s| !s.is_empty())), "{v}");
        let resume = v["resume_cmd"].as_str().unwrap();
        assert!(resume.starts_with("ks "), "{resume}");
        assert!(!resume.contains("--profile"), "{resume}");
    }
    assert_eq!(needs_human(Event::Captcha, "x", "default")["resume_cmd"], "ks resume");
    assert_eq!(needs_human(Event::AuthRequired, "x", "default")["resume_cmd"], "ks auth google");
    assert_eq!(needs_human(Event::Captcha, "x", "work")["resume_cmd"], "ks resume --profile work");
    let steps = needs_human(Event::Captcha, "x", "default")["steps"].to_string().to_lowercase();
    assert!(steps.contains("ks handoff captcha") && steps.contains("yourself"), "{steps}");
}

#[test]
fn log_lines_are_json_in_a_0600_file_and_tail_returns_the_last_n() {
    let home = tmp();
    let paths = Paths::new(&home);
    for i in 1..=12 {
        let e = LogEntry {
            ts: utc_now(), event: Some(Event::Captcha), profile: "default".into(), mode: "headless".into(),
            query_index: Some(i), result: "stopped".into(), reason: "CAPTCHA page".into(),
        };
        append_log(&paths, &e).unwrap();
    }
    let log = paths.events_log();
    assert_eq!(std::fs::metadata(&log).unwrap().permissions().mode() & 0o777, 0o600);
    let text = std::fs::read_to_string(&log).unwrap();
    assert_eq!(text.lines().count(), 12);
    let first: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    let mut keys: Vec<&str> = first.as_object().unwrap().keys().map(|k| k.as_str()).collect();
    keys.sort();
    assert_eq!(keys, ["event", "mode", "profile", "query_index", "reason", "result", "ts"]);
    let tail = tail_log(&paths, 10);
    assert_eq!(tail.len(), 10);
    assert_eq!(tail.last().unwrap().query_index, Some(12));
    assert_eq!(tail.first().unwrap().query_index, Some(3));
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn utc_now_is_rfc3339_utc() {
    let t = utc_now();
    assert_eq!(t.len(), 20, "{t}");
    assert!(t.starts_with("20") && t.ends_with('Z') && &t[10..11] == "T", "{t}");
}

#[test]
fn blocked_guidance_matches_what_actually_unblocks_and_names_the_cause() {
    // a BLOCKED stop stays pending: searching again is refused until `ks resume` passes
    let v = needs_human(Event::Blocked, "HTTP 429 Too Many Requests", "default");
    assert_eq!(v["resume_cmd"], "ks resume", "{v}");
    let steps = v["steps"].to_string();
    assert!(steps.contains("ks handoff BLOCKED") && steps.contains("ks resume"), "{steps}");
    assert_eq!(needs_human(Event::Blocked, "HTTP 503 on the result page", "work")["resume_cmd"], "ks resume --profile work");
    // the local quota sent nothing to Google, is not pending, and lifts by itself
    let q = needs_human(Event::Blocked, "quota: 3 searches in the last hour reached the hourly limit (KS_QUOTA_HOURLY=3)", "default");
    let steps = q["steps"].to_string().to_lowercase();
    assert!(!steps.contains("rate-limiting"), "the quota is ours, not Google's: {steps}");
    assert!(steps.contains("no request was sent to google") && steps.contains("ks_quota_hourly") && !steps.contains("ks resume"), "{steps}");
    assert!(q["resume_cmd"].as_str().unwrap().starts_with("ks research search"), "{q}");
}

#[test]
fn sign_in_guidance_leads_to_ks_auth_google_never_the_controlled_window() {
    // Google refuses sign-in in the window ks drives over CDP: every sign-in event leads to
    // `ks auth google` (a plain Chrome), never to handing off that window and `ks resume`
    for e in [Event::AuthRequired, Event::AuthExpired, Event::MfaPrompt] {
        let v = needs_human(e, "why", "default");
        assert_eq!(v["resume_cmd"], "ks auth google", "{e:?}: {v}");
        let steps = v["steps"].to_string();
        assert!(steps.contains("ks auth google"), "{e:?}: {steps}");
        assert!(!steps.contains("ks handoff") && !steps.contains("ks resume"), "{e:?}: {steps}");
        assert_eq!(needs_human(e, "why", "work")["resume_cmd"], "ks auth google --profile work", "{e:?}");
    }
    // a daemon of another build holding a sign-in event: the way out ends at `ks auth google`
    use ks_browser::events::other_build_daemon;
    let v = other_build_daemon(false, Some("AUTH_EXPIRED"), "work");
    let steps = v["steps"].to_string();
    assert_eq!(v["resume_cmd"], "ks browser disconnect --profile work", "{v}");
    assert!(steps.contains("ks auth google --profile work") && !steps.contains("ks handoff") && !steps.contains("ks resume"), "{steps}");
}

#[test]
fn daemon_replace_guidance_names_the_cause_and_a_way_out() {
    use ks_browser::events::other_build_daemon;
    // the same build with a visible window: an explicit connect may close it
    let same = needs_human(Event::DaemonReplace, "the browser is open in a visible window; switching to headless would close it", "default");
    let text = same.to_string().to_lowercase();
    assert!(!text.contains("version") && !text.contains("build"), "a window case must not blame the build: {text}");
    // another build: the way out must never be a command that replaces the daemon (connect, search,
    // auth), since that answers DAEMON_REPLACE again
    let loops = |cmd: &str| ["ks browser connect", "ks research", "ks auth google"].iter().any(|c| cmd.starts_with(c));
    for (busy, pending, want_resume) in [(true, None, "ks status"), (false, Some("CAPTCHA"), "ks resume"), (false, None, "ks browser disconnect")] {
        let v = other_build_daemon(busy, pending, "default");
        assert_eq!((v["status"].as_str(), v["event"].as_str()), (Some("needs_human"), Some("DAEMON_REPLACE")), "{v}");
        let reason = v["reason"].as_str().unwrap();
        assert!(reason.contains("another ks build"), "{reason}");
        let cmd = v["resume_cmd"].as_str().unwrap();
        assert_eq!(cmd, want_resume, "{v}");
        assert!(!loops(cmd), "{v}");
        let steps = v["steps"].to_string();
        assert!(!steps.contains("ks browser connect") && !steps.contains("--mode headful"), "{steps}");
    }
    let v = other_build_daemon(false, Some("CAPTCHA"), "work");
    let steps = v["steps"].to_string();
    assert!(v["reason"].as_str().unwrap().contains("CAPTCHA"), "{v}");
    assert!(steps.contains("ks handoff CAPTCHA --profile work") && steps.contains("ks resume --profile work") && steps.contains("ks browser disconnect --profile work"), "{steps}");
    let window = other_build_daemon(false, None, "default");
    assert!(window["reason"].as_str().unwrap().contains("window"), "{window}");
}
