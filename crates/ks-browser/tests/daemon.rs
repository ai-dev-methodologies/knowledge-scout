//! Daemon transport and authentication, run in-process. None of these commands launch Chrome.
use ks_browser::client::{send_line, Client};
use ks_browser::daemon::{serve, DaemonConfig};
use ks_browser::paths::{ensure_private_dir, Paths};
use ks_browser::quota::Quota;
use ks_browser::Mode;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

static N: AtomicUsize = AtomicUsize::new(0);

struct Home(PathBuf);
impl Home {
    fn new() -> Home {
        // short path: Unix socket paths are limited to ~104 bytes on macOS
        let p = PathBuf::from(format!("/tmp/ks-d-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&p);
        Home(p)
    }
    fn paths(&self) -> Paths { Paths::new(&self.0) }
}
impl Drop for Home {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

fn start(home: &Home, idle: Duration) -> JoinHandle<Result<(), String>> {
    start_cfg(home, Mode::Headless, idle, Quota::default())
}
fn start_cfg(home: &Home, mode: Mode, idle: Duration, quota: Quota) -> JoinHandle<Result<(), String>> {
    // interrupts in these tests must not raise desktop notifications
    std::env::set_var("KS_NOTIFY", "0");
    let cfg = DaemonConfig { paths: home.paths(), profile: "default".into(), mode, idle, quota };
    let h = std::thread::spawn(move || serve(cfg));
    let c = Client::new(home.paths(), "default");
    let t = Instant::now();
    while c.ping().is_none() {
        assert!(t.elapsed() < Duration::from_secs(10), "daemon did not come up");
        std::thread::sleep(Duration::from_millis(20));
    }
    h
}
fn mode(p: &std::path::Path) -> u32 {
    std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777
}
fn raw(home: &Home, line: &str) -> Value {
    send_line(&home.paths().socket("default"), line, Duration::from_secs(5)).unwrap()
}

#[test]
fn files_have_private_permissions_and_status_files_are_written() {
    let home = Home::new();
    let p = home.paths();
    // a pre-existing, too-open run dir is tightened
    std::fs::create_dir_all(p.run_dir()).unwrap();
    std::fs::set_permissions(p.run_dir(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let h = start(&home, Duration::from_secs(600));

    assert_eq!(mode(&p.run_dir()), 0o700);
    assert_eq!(mode(&p.token("default")), 0o600);
    assert_eq!(mode(&p.socket("default")), 0o600);
    assert!(std::fs::symlink_metadata(p.socket("default")).unwrap().file_type().is_socket_like());
    let token = std::fs::read_to_string(p.token("default")).unwrap();
    assert_eq!(token.trim().len(), 64, "256-bit hex token");
    assert!(token.trim().bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(std::fs::read_to_string(p.pid_file("default")).unwrap().trim(), std::process::id().to_string());
    assert_eq!(std::fs::read_to_string(p.version_file("default")).unwrap().trim(), ks_browser::VERSION);
    assert_eq!(std::fs::read_to_string(p.mode_file("default")).unwrap().trim(), "headless");
    for f in [p.pid_file("default"), p.version_file("default"), p.mode_file("default")] {
        assert_eq!(mode(&f), 0o600, "{}", f.display());
    }

    let c = Client::new(p.clone(), "default");
    let ping = c.ping().unwrap();
    assert_eq!(ping["status"], "ok");
    assert_eq!(ping["mode"], "headless");
    assert_eq!(ping["browser"], false, "ping must not launch Chrome");
    assert!(c.shutdown().unwrap());
    h.join().unwrap().unwrap();
    for f in [p.socket("default"), p.token("default"), p.pid_file("default"), p.mode_file("default"), p.version_file("default")] {
        assert!(!f.exists(), "{} left behind", f.display());
    }
}

trait SocketLike { fn is_socket_like(&self) -> bool; }
impl SocketLike for std::fs::FileType {
    fn is_socket_like(&self) -> bool { std::os::unix::fs::FileTypeExt::is_socket(self) }
}

#[test]
fn requests_without_the_right_token_are_rejected() {
    let home = Home::new();
    let h = start(&home, Duration::from_secs(600));
    let token = std::fs::read_to_string(home.paths().token("default")).unwrap().trim().to_string();
    let unauthorized = json!({"status": "error", "error": "unauthorized"});

    assert_eq!(raw(&home, &json!({"cmd": "ping"}).to_string()), unauthorized, "missing token");
    assert_eq!(raw(&home, &json!({"cmd": "ping", "token": ""}).to_string()), unauthorized, "empty token");
    assert_eq!(raw(&home, &json!({"cmd": "ping", "token": "0".repeat(64)}).to_string()), unauthorized, "wrong token");
    let mut flipped = token.clone().into_bytes();
    flipped[63] = if flipped[63] == b'a' { b'b' } else { b'a' };
    assert_eq!(raw(&home, &json!({"cmd": "ping", "token": String::from_utf8(flipped).unwrap()}).to_string()), unauthorized, "one char off");
    assert_eq!(raw(&home, &json!({"cmd": "ping", "token": &token[..63]}).to_string()), unauthorized, "prefix");
    // a rejected shutdown leaves the daemon running
    assert_eq!(raw(&home, &json!({"cmd": "shutdown", "token": "nope"}).to_string()), unauthorized);
    assert_eq!(raw(&home, "not json"), json!({"status": "error", "error": "bad request"}));

    let ok = raw(&home, &json!({"cmd": "ping", "token": token}).to_string());
    assert_eq!(ok["status"], "ok", "{ok}");
    let unknown = raw(&home, &json!({"cmd": "rm-rf", "token": token}).to_string());
    assert_eq!(unknown["status"], "error", "{unknown}");

    assert!(Client::new(home.paths(), "default").shutdown().unwrap());
    h.join().unwrap().unwrap();
}

#[test]
fn client_refuses_a_token_file_others_can_read() {
    let home = Home::new();
    let h = start(&home, Duration::from_secs(600));
    let c = Client::new(home.paths(), "default");
    let tok = home.paths().token("default");
    std::fs::set_permissions(&tok, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(c.read_token().is_err());
    assert!(c.ping().is_none());
    std::fs::set_permissions(&tok, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(c.shutdown().unwrap());
    h.join().unwrap().unwrap();
}

#[test]
fn a_second_daemon_for_the_same_profile_refuses_to_start() {
    let home = Home::new();
    let h = start(&home, Duration::from_secs(600));
    let token_before = std::fs::read_to_string(home.paths().token("default")).unwrap();
    let cfg = DaemonConfig { paths: home.paths(), profile: "default".into(), mode: Mode::Headless, idle: Duration::from_secs(600), quota: Quota::default() };
    let err = serve(cfg).unwrap_err();
    assert!(err.contains("in use") && err.contains("ks auth google"), "{err}");
    assert_eq!(std::fs::read_to_string(home.paths().token("default")).unwrap(), token_before, "token must not be rotated by the loser");
    assert!(Client::new(home.paths(), "default").ping().is_some());
    assert!(Client::new(home.paths(), "default").shutdown().unwrap());
    h.join().unwrap().unwrap();
}

/// A daemon that was just stopped removes its pid file and socket before it releases the profile
/// lock, so `ks auth google` retries the lock for a short while instead of failing at once.
#[test]
fn the_profile_lock_is_retried_for_a_short_while() {
    let home = Home::new();
    let paths = home.paths();
    ensure_private_dir(&paths.run_dir()).unwrap();
    let held = ks_browser::daemon::lock_profile(&paths, "default").unwrap();
    let t = Instant::now();
    let err = ks_browser::daemon::lock_profile_within(&paths, "default", Duration::from_millis(500)).expect_err("held all along");
    assert!(t.elapsed() >= Duration::from_millis(500), "gave up after {:?}", t.elapsed());
    assert!(err.contains("in use"), "{err}");
    // released while it waits: taken
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        drop(held);
    });
    let lock = ks_browser::daemon::lock_profile_within(&paths, "default", Duration::from_secs(2));
    release.join().unwrap();
    assert!(lock.is_ok(), "{:?}", lock.err());
}

#[test]
fn idle_daemon_shuts_itself_down_and_cleans_up() {
    let home = Home::new();
    let h = start(&home, Duration::from_secs(1));
    let t = Instant::now();
    while !h.is_finished() {
        assert!(t.elapsed() < Duration::from_secs(10), "idle shutdown did not happen");
        std::thread::sleep(Duration::from_millis(50));
    }
    h.join().unwrap().unwrap();
    assert!(!home.paths().socket("default").exists());
    assert!(!home.paths().token("default").exists());
    assert!(Client::new(home.paths(), "default").ping().is_none());
}

#[test]
fn status_without_a_daemon_reports_stopped() {
    let home = Home::new();
    let s = Client::new(home.paths(), "default").status();
    assert_eq!(s["daemon"], "stopped", "{s}");
    assert_eq!(s["profile"], "default");
    assert!(s["recent_events"].as_array().unwrap().is_empty());
}

#[test]
fn delay_between_queries_is_random_within_3_to_8_seconds() {
    let samples: Vec<Duration> = (0..300).map(|_| ks_browser::daemon::random_delay()).collect();
    assert!(samples.iter().all(|d| *d >= Duration::from_secs(3) && *d <= Duration::from_secs(8)), "{samples:?}");
    let distinct: std::collections::HashSet<_> = samples.iter().collect();
    assert!(distinct.len() > 50, "not random: {} distinct values", distinct.len());
}

#[test]
fn client_refuses_a_symlinked_token_file() {
    let home = Home::new();
    let h = start(&home, Duration::from_secs(600));
    let c = Client::new(home.paths(), "default");
    let tok = home.paths().token("default");
    let copy = home.0.join("token-copy");
    std::fs::copy(&tok, &copy).unwrap();
    std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::rename(&tok, home.0.join("token-real")).unwrap();
    std::os::unix::fs::symlink(&copy, &tok).unwrap();
    assert!(c.read_token().is_err(), "a symlink at the token path must not be followed");
    std::fs::remove_file(&tok).unwrap();
    std::fs::rename(home.0.join("token-real"), &tok).unwrap();
    assert!(c.shutdown().unwrap());
    h.join().unwrap().unwrap();
}

fn finished_within(h: &JoinHandle<Result<(), String>>, d: Duration) -> bool {
    let t = Instant::now();
    while !h.is_finished() {
        if t.elapsed() > d {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}
fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}
fn seed(home: &Home, file: PathBuf, text: &str) {
    ensure_private_dir(&home.paths().run_dir()).unwrap();
    std::fs::write(file, text).unwrap();
}
fn search(home: &Home) -> Value {
    Client::new(home.paths(), "default").request(json!({"cmd": "search", "query": "redis ttl", "query_index": 1})).unwrap()
}

#[test]
fn pings_do_not_keep_an_idle_daemon_alive() {
    let home = Home::new();
    let h = start(&home, Duration::from_secs(2));
    let c = Client::new(home.paths(), "default");
    let t = Instant::now();
    while !h.is_finished() && t.elapsed() < Duration::from_secs(8) {
        let _ = c.ping(); // `ks status` polling must not count as activity
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(h.is_finished(), "the daemon never idled out while only being pinged");
    h.join().unwrap().unwrap();
}

#[test]
fn a_pending_interrupt_survives_a_restart_blocks_searches_and_idle_shutdown() {
    let home = Home::new();
    let p = home.paths();
    seed(&home, p.pending_file("default"), "CAPTCHA\n");
    let h = start(&home, Duration::from_secs(1));
    let c = Client::new(p.clone(), "default");
    assert_eq!(c.ping().unwrap()["pending_event"], "CAPTCHA", "pending was not loaded at start");
    let v = search(&home);
    assert_eq!((v["status"].as_str(), v["event"].as_str()), (Some("needs_human"), Some("CAPTCHA")), "{v}");
    assert!(!finished_within(&h, Duration::from_secs(3)), "idle shutdown while a human still has to act");
    assert_eq!(c.ping().unwrap()["browser"], false, "nothing may launch Chrome here");
    assert!(c.shutdown().unwrap());
    h.join().unwrap().unwrap();
    assert_eq!(std::fs::read_to_string(p.pending_file("default")).unwrap().trim(), "CAPTCHA", "pending must survive the daemon");
}

#[test]
fn a_headful_daemon_without_a_browser_idles_out() {
    // only an open window keeps the daemon up; a headful daemon whose Chrome never started or has
    // exited protects nothing and must not stay forever
    let home = Home::new();
    let h = start_cfg(&home, Mode::Headful, Duration::from_secs(1), Quota::default());
    assert!(finished_within(&h, Duration::from_secs(10)), "a headful daemon with no browser never idled out");
    h.join().unwrap().unwrap();
}

#[test]
fn pending_file_round_trip() {
    use ks_browser::daemon::{load_pending, save_pending};
    use ks_browser::events::Event;
    let home = Home::new();
    let p = home.paths();
    ensure_private_dir(&p.run_dir()).unwrap();
    assert_eq!(load_pending(&p, "default"), None);
    save_pending(&p, "default", Some(Event::Consent)).unwrap();
    assert_eq!(mode(&p.pending_file("default")), 0o600);
    assert_eq!(load_pending(&p, "default"), Some(Event::Consent));
    save_pending(&p, "default", None).unwrap();
    assert!(!p.pending_file("default").exists());
    assert_eq!(load_pending(&p, "default"), None);
    // an unreadable or unknown value fails closed
    std::fs::write(p.pending_file("default"), "garbage").unwrap();
    assert_eq!(load_pending(&p, "default"), Some(Event::Blocked));
}

#[test]
fn hourly_quota_stops_searches_before_any_browser_starts() {
    let home = Home::new();
    let p = home.paths();
    let t = now();
    seed(&home, p.quota_file("default"), &(0..3).map(|i| format!("{}\n", t - 60 * i)).collect::<String>());
    let h = start_cfg(&home, Mode::Headless, Duration::from_secs(600), Quota { hourly: 3, daily: 100 });
    let v = search(&home);
    assert_eq!((v["status"].as_str(), v["event"].as_str()), (Some("needs_human"), Some("BLOCKED")), "{v}");
    assert!(v["reason"].as_str().unwrap().starts_with("quota"), "{v}");
    let c = Client::new(p.clone(), "default");
    assert_eq!(c.ping().unwrap()["browser"], false, "Chrome was started despite the quota");
    assert_eq!(std::fs::read_to_string(p.quota_file("default")).unwrap().lines().count(), 3, "a refused search is not counted");
    assert!(c.shutdown().unwrap());
    h.join().unwrap().unwrap();
}

#[test]
fn daily_quota_counts_the_last_24_hours() {
    let home = Home::new();
    let p = home.paths();
    let t = now();
    // 4 searches spread over the last 20 hours: under the hourly limit, at the daily limit of 4
    seed(&home, p.quota_file("default"), &(1..=4).map(|i| format!("{}\n", t - 3600 * 5 * i)).collect::<String>());
    let h = start_cfg(&home, Mode::Headless, Duration::from_secs(600), Quota { hourly: 20, daily: 4 });
    let v = search(&home);
    assert_eq!((v["event"].as_str(), v["reason"].as_str().map(|r| r.starts_with("quota"))), (Some("BLOCKED"), Some(true)), "{v}");
    assert!(Client::new(p, "default").shutdown().unwrap());
    h.join().unwrap().unwrap();
}

#[test]
fn under_the_quota_a_search_goes_on_to_the_sign_in_check() {
    let home = Home::new();
    let p = home.paths();
    seed(&home, p.quota_file("default"), &format!("{}\n", now() - 10));
    let h = start_cfg(&home, Mode::Headless, Duration::from_secs(600), Quota { hourly: 3, daily: 100 });
    // the profile was never signed in: that is the next stop, and no query reached Google
    let v = search(&home);
    assert_eq!(v["event"], "AUTH_REQUIRED", "{v}");
    assert_eq!(std::fs::read_to_string(p.quota_file("default")).unwrap().lines().count(), 1);
    assert!(Client::new(p, "default").shutdown().unwrap());
    h.join().unwrap().unwrap();
}

#[test]
fn an_unreadable_quota_file_refuses_searches_instead_of_resetting_the_limit() {
    let t = now();
    let history: String = (0..3).map(|i| format!("{}\n", t - 60 * i)).collect();
    for case in ["not-utf8", "symlink", "unreadable"] {
        let home = Home::new();
        let p = home.paths();
        let f = p.quota_file("default");
        ensure_private_dir(&p.run_dir()).unwrap();
        match case {
            "not-utf8" => std::fs::write(&f, [history.as_bytes(), b"\xff\n"].concat()).unwrap(),
            "symlink" => {
                let real = home.0.join("real.quota");
                std::fs::write(&real, &history).unwrap();
                std::os::unix::fs::symlink(&real, &f).unwrap();
            }
            _ => {
                std::fs::write(&f, &history).unwrap();
                std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o000)).unwrap();
            }
        }
        // three recent searches against an hourly limit of 3: a reset history would let the next one through
        let h = start_cfg(&home, Mode::Headless, Duration::from_secs(600), Quota { hourly: 3, daily: 100 });
        let v = search(&home);
        assert_eq!(v["status"], "error", "{case}: {v}");
        let msg = v["error"].as_str().unwrap();
        assert!(msg.contains(&f.display().to_string()) && msg.contains("rm "), "{case}: the error must name the file and the fix: {msg}");
        let c = Client::new(p.clone(), "default");
        assert_eq!(c.ping().unwrap()["browser"], false, "{case}: Chrome was started");
        assert!(c.shutdown().unwrap());
        h.join().unwrap().unwrap();
        assert_eq!(std::fs::symlink_metadata(&f).unwrap().file_type().is_symlink(), case == "symlink", "{case}: the file was replaced");
    }
}

fn browser_tests() -> bool {
    let on = std::env::var("KS_TEST_BROWSER").ok().as_deref() == Some("1");
    if !on {
        eprintln!("skipped: set KS_TEST_BROWSER=1 to run");
    }
    on
}

/// Opt-in (KS_TEST_BROWSER=1): headless Chrome on a throwaway profile, no website is opened.
#[test]
fn a_chrome_that_died_is_cleared_and_connect_launches_a_new_one() {
    if !browser_tests() {
        return;
    }
    let home = Home::new();
    let h = start(&home, Duration::from_secs(600));
    let c = Client::new(home.paths(), "default");
    let connect = || c.request(json!({"cmd": "connect", "mode": "headless"})).unwrap();
    assert_eq!(connect()["status"], "ok");
    let prof = home.paths().profile_dir("default");
    let pid = ks_browser::session::singleton_holder(&prof).expect("Chrome holds the profile");
    // Chrome dies under the daemon (crash, or Cmd+Q on a visible window)
    assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
    std::thread::sleep(Duration::from_millis(1000));

    let v = c.request(json!({"cmd": "resume"})).unwrap();
    assert_eq!(v["status"], "error", "{v}");
    assert!(v["error"].as_str().unwrap().contains("not running"), "a dead browser must read as not running: {v}");
    assert_eq!(c.ping().unwrap()["browser"], false);
    // and it can be started again
    assert_eq!(connect()["status"], "ok");
    let pid2 = ks_browser::session::singleton_holder(&prof).expect("a new Chrome holds the profile");
    assert_ne!(pid, pid2);
    assert_eq!(c.request(json!({"cmd": "resume"})).unwrap()["status"], "resumed");
    assert!(c.shutdown().unwrap());
    h.join().unwrap().unwrap();
    assert!(ks_browser::session::singleton_holder(&prof).is_none(), "Chrome left running");
}

/// Opt-in (KS_TEST_BROWSER=1). A restarted browser shows about:blank: that page proves nothing, so
/// resume must keep the interrupt pending and say what to do.
#[test]
fn resume_keeps_a_pending_interrupt_until_the_page_proves_it_is_resolved() {
    if !browser_tests() {
        return;
    }
    for ev in ["CAPTCHA", "BLOCKED", "AUTH_EXPIRED"] {
        let home = Home::new();
        let p = home.paths();
        seed(&home, p.pending_file("default"), &format!("{ev}\n"));
        let h = start(&home, Duration::from_secs(600));
        let c = Client::new(p.clone(), "default");
        assert_eq!(c.request(json!({"cmd": "connect", "mode": "headless"})).unwrap()["status"], "ok");
        let v = c.request(json!({"cmd": "resume"})).unwrap();
        assert_eq!((v["status"].as_str(), v["event"].as_str()), (Some("needs_human"), Some(ev)), "{ev}: {v}");
        assert!(!v["reason"].as_str().unwrap_or("").is_empty(), "{v}");
        assert_eq!(c.ping().unwrap()["pending_event"], ev, "{ev} was cleared on about:blank");
        assert!(c.shutdown().unwrap());
        h.join().unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(p.pending_file("default")).unwrap().trim(), ev);
    }
}

#[test]
fn the_same_executable_reached_through_a_symlink_has_the_same_identity() {
    // `ks` started through a link (~/.local/bin/ks) and through its real path is one build: the
    // client must reuse that daemon, not replace it
    let home = Home::new();
    std::fs::create_dir_all(&home.0).unwrap();
    let real = home.0.join("ks-real");
    std::fs::write(&real, b"binary").unwrap();
    let link = home.0.join("ks-link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let id = ks_browser::exe_identity(&real);
    assert!(!id.is_null());
    assert_eq!(ks_browser::exe_identity(&link), id);
    // a different file is still a different build
    let copy = home.0.join("ks-copy");
    std::fs::copy(&real, &copy).unwrap();
    assert_ne!(ks_browser::exe_identity(&copy), id);
}

#[test]
fn a_quota_file_with_lines_that_are_not_timestamps_refuses_searches() {
    // corrupted into other ASCII: skipping those lines would empty the history and reset the limit
    let home = Home::new();
    let p = home.paths();
    let f = p.quota_file("default");
    let t = now();
    seed(&home, f.clone(), &(0..3).map(|i| format!("{}x\n", t - 60 * i)).collect::<String>());
    let h = start_cfg(&home, Mode::Headless, Duration::from_secs(600), Quota { hourly: 3, daily: 100 });
    let v = search(&home);
    assert_eq!(v["status"], "error", "{v}");
    let msg = v["error"].as_str().unwrap();
    assert!(msg.contains(&f.display().to_string()) && msg.contains("rm "), "the error must name the file and the fix: {msg}");
    let c = Client::new(p.clone(), "default");
    assert_eq!(c.ping().unwrap()["browser"], false, "Chrome was started");
    assert!(c.shutdown().unwrap());
    h.join().unwrap().unwrap();
}
