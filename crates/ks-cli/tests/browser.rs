//! `ks browser|auth|handoff|resume|status|research` without a browser: argument checks, the
//! profile-deletion guard, and that nothing spawns a daemon (or Chrome) when it must not.
//! The real-browser end-to-end runs only with KS_TEST_BROWSER=1 and opens no website.
use serde_json::Value;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::FromRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static N: AtomicUsize = AtomicUsize::new(0);

struct Home(PathBuf);
impl Home {
    fn new() -> Home {
        let p = PathBuf::from(format!("/tmp/ks-cb-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Home(p)
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn ks(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ks"))
        .args(args)
        .env("KS_HOME", home)
        .env("KS_NOTIFY", "0")
        .env_remove("KS_BROWSER_DAEMON")
        .output()
        .unwrap()
}
fn json(o: &Output) -> Value {
    serde_json::from_str(&String::from_utf8_lossy(&o.stdout)).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&o.stdout)))
}
/// Both output streams of a `ks` run, for assertion messages.
fn outputs(o: &Output) -> String {
    format!("stdout: {} stderr: {}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}
fn no_daemon(home: &Path) {
    assert!(!home.join("run/default.sock").exists(), "a daemon was started");
    assert!(!home.join("run/default.pid").exists(), "a daemon was started");
}

#[test]
fn status_without_a_daemon() {
    let h = Home::new();
    let o = ks(&h.0, &["status"]);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    let v = json(&o);
    assert_eq!(v["daemon"], "stopped");
    assert_eq!(v["profile"], "default");
    assert!(v["recent_events"].as_array().unwrap().is_empty());
    no_daemon(&h.0);
    assert_eq!(ks(&h.0, &["status", "--profile", "../x"]).status.code(), Some(2));
}

#[test]
fn logout_needs_yes_and_deletes_only_our_profile_dir() {
    let h = Home::new();
    let prof = h.0.join("profiles/default");
    std::fs::create_dir_all(&prof).unwrap();
    std::fs::write(prof.join("Cookies"), "x").unwrap();
    std::fs::create_dir_all(h.0.join("profiles/other")).unwrap();

    let o = ks(&h.0, &["auth", "logout"]);
    assert_eq!(o.status.code(), Some(2), "{}", outputs(&o));
    assert!(String::from_utf8_lossy(&o.stderr).contains("--yes"), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(prof.join("Cookies").exists(), "deleted without --yes");

    let o = ks(&h.0, &["auth", "logout", "--yes"]);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!prof.exists());
    assert!(h.0.join("profiles/other").exists(), "another profile was touched");
    // nothing to delete is not an error
    let o = ks(&h.0, &["auth", "logout", "--yes"]);
    assert_eq!(o.status.code(), Some(0), "{}", outputs(&o));
}

#[test]
fn logout_refuses_a_symlinked_profile_dir() {
    let h = Home::new();
    let elsewhere = h.0.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("keep"), "x").unwrap();
    std::fs::create_dir_all(h.0.join("profiles")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, h.0.join("profiles/default")).unwrap();
    let o = ks(&h.0, &["auth", "logout", "--yes"]);
    assert_eq!(o.status.code(), Some(2), "{}", outputs(&o));
    assert!(elsewhere.join("keep").exists());
}

#[test]
fn handoff_and_resume_need_a_running_daemon_and_valid_events() {
    let h = Home::new();
    assert_eq!(ks(&h.0, &["handoff", "NOT_AN_EVENT"]).status.code(), Some(2));
    let o = ks(&h.0, &["handoff", "CAPTCHA"]);
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
    let o = ks(&h.0, &["resume"]);
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
    let o = ks(&h.0, &["browser", "disconnect"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(json(&o)["daemon"], "not running");
    no_daemon(&h.0);
}

#[test]
fn research_search_validates_before_starting_anything() {
    let h = Home::new();
    for args in [
        &["research", "search", "--engine", "google-browser", "--query", "x", "--limit", "0"][..],
        &["research", "search", "--engine", "google-browser", "--query", "x", "--limit", "101"],
        &["research", "search", "--engine", "google-browser", "--query", "x", "--max-queries", "0"],
        &["research", "search", "--engine", "google-browser", "--query", "  "],
        &["research", "search", "--engine", "google-browser"],
        &["research", "search", "--engine", "bing", "--query", "x"],
        &["research", "search", "--engine", "google-browser", "--mode", "visible", "--query", "x"],
    ] {
        let o = ks(&h.0, args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", String::from_utf8_lossy(&o.stdout));
    }
    no_daemon(&h.0);
}

fn mode(p: &Path) -> u32 {
    std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn real_daemon_spawn_connect_status_disconnect() {
    if std::env::var("KS_TEST_BROWSER").ok().as_deref() != Some("1") {
        eprintln!("skipped: set KS_TEST_BROWSER=1 to run");
        return;
    }
    let h = Home::new();
    let o = ks(&h.0, &["browser", "connect"]);
    assert_eq!(o.status.code(), Some(0), "{} {}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    let s = json(&ks(&h.0, &["status"]));
    assert_eq!((s["daemon"].as_str(), s["mode"].as_str(), s["browser"].as_bool()), (Some("running"), Some("headless"), Some(true)), "{s}");
    assert_eq!(mode(&h.0.join("run")), 0o700);
    assert_eq!(mode(&h.0.join("profiles/default")), 0o700);
    assert_eq!(mode(&h.0.join("run/default.sock")), 0o600);
    assert_eq!(mode(&h.0.join("run/default.token")), 0o600);
    // detached: the daemon leads its own session/process group
    let pid = s["pid"].as_u64().unwrap().to_string();
    let pgid = String::from_utf8(Command::new("/bin/ps").args(["-o", "pgid=", "-p", &pid]).output().unwrap().stdout).unwrap();
    assert_eq!(pgid.trim(), pid, "daemon is not in its own session");
    // a second connect reuses the same daemon
    assert_eq!(ks(&h.0, &["browser", "connect"]).status.code(), Some(0));
    assert_eq!(json(&ks(&h.0, &["status"]))["pid"].as_u64().unwrap().to_string(), pid);

    let o = ks(&h.0, &["browser", "disconnect"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(json(&o)["daemon"], "stopped");
    assert_eq!(json(&ks(&h.0, &["status"]))["daemon"], "stopped");
    let alive = Command::new("/bin/kill").args(["-0", &pid]).output().unwrap().status.success();
    assert!(!alive, "daemon process still alive");
    assert!(std::fs::symlink_metadata(h.0.join("profiles/default/SingletonLock")).is_err(), "Chrome did not exit cleanly");
}

#[test]
fn logout_refuses_while_a_live_chrome_holds_the_profile() {
    let h = Home::new();
    let prof = h.0.join("profiles/default");
    std::fs::create_dir_all(&prof).unwrap();
    // Chrome's lock is a symlink to "<host>-<pid>"; this test process stands in for a live Chrome
    std::os::unix::fs::symlink(format!("somehost-{}", std::process::id()), prof.join("SingletonLock")).unwrap();
    let o = ks(&h.0, &["auth", "logout", "--yes"]);
    assert_eq!(o.status.code(), Some(2), "{}", outputs(&o));
    assert!(String::from_utf8_lossy(&o.stderr).contains("in use"), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(prof.exists());
}

#[test]
fn ks_home_that_is_the_home_directory_is_refused_and_never_chmodded() {
    let h = Home::new();
    let home = h.0.join("user-home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).unwrap();
    let link = h.0.join("home-link");
    std::os::unix::fs::symlink(&home, &link).unwrap();
    for ks_home in [&home, &link] {
        let o = Command::new(env!("CARGO_BIN_EXE_ks"))
            .args(["browser", "connect"])
            .env("HOME", &home)
            .env("KS_HOME", ks_home)
            .env("KS_NOTIFY", "0")
            .env("KS_CHROME", "/nonexistent/chrome") // never launch a browser here
            .env("KS_BROWSER_IDLE_SECS", "1")
            .env_remove("KS_BROWSER_DAEMON")
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
        assert_eq!(mode(&home), 0o755, "the home directory was chmodded");
        assert!(!home.join("run").exists(), "state was written into the home directory");
    }
}

#[test]
fn max_queries_is_capped_at_50() {
    let h = Home::new();
    let o = Command::new(env!("CARGO_BIN_EXE_ks"))
        .args(["research", "search", "--engine", "google-browser", "--query", "x", "--max-queries", "51"])
        .env("KS_HOME", &h.0)
        .env("KS_NOTIFY", "0")
        .env("KS_CHROME", "/nonexistent/chrome") // never launch a browser here
        .env("KS_BROWSER_IDLE_SECS", "1") // a daemon started by mistake goes away quickly
        .env_remove("KS_BROWSER_DAEMON")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
    assert!(String::from_utf8_lossy(&o.stderr).contains("50"), "{}", String::from_utf8_lossy(&o.stderr));
    no_daemon(&h.0);
}

/// Set on the child that `LockHolder` starts: the KS_HOME whose default profile lock it holds.
const HOLD_LOCK: &str = "KS_TEST_HOLD_PROFILE_LOCK";

/// The default profile lock of a home, held by another process until dropped: this test binary,
/// re-run as `test`, which takes the lock and holds it until its stdin closes. Not held in this
/// process: while another test thread is inside posix_spawn, the new child holds a copy of every
/// descriptor of this process (close-on-exec ones too) until its exec completes, so a lock dropped
/// here can stay held a few ms longer and the next `ks` finds the profile in use.
struct LockHolder(Child);
impl LockHolder {
    fn start(home: &Path, test: &str) -> LockHolder {
        let mut c = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture", "--test-threads=1"])
            .env(HOLD_LOCK, home)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // the marker comes on stderr: the test harness writes its own progress to stdout
        let mut err = std::io::BufReader::new(c.stderr.take().unwrap());
        let mut seen = String::new();
        loop {
            let mut line = String::new();
            if std::io::BufRead::read_line(&mut err, &mut line).unwrap() == 0 {
                let _ = c.wait();
                panic!("the lock holder exited without the lock: {seen}");
            }
            if line == "lock held\n" {
                return LockHolder(c);
            }
            seen.push_str(&line);
        }
    }

    /// In the child started by `start`: hold the lock until stdin closes, then exit.
    fn run_if_child() {
        let Some(home) = std::env::var_os(HOLD_LOCK) else { return };
        let paths = ks_browser::paths::Paths::new(Path::new(&home));
        let _lock = ks_browser::daemon::lock_profile(&paths, "default").unwrap();
        eprintln!("lock held");
        let _ = std::io::stdin().read_to_end(&mut Vec::new());
        std::process::exit(0);
    }
}
impl Drop for LockHolder {
    fn drop(&mut self) {
        // end of input: the holder exits, and its exit releases the lock before wait returns
        drop(self.0.stdin.take());
        let _ = self.0.wait();
    }
}

#[test]
fn logout_refuses_while_the_profile_lock_is_held() {
    LockHolder::run_if_child();
    let h = Home::new();
    let prof = h.0.join("profiles/default");
    std::fs::create_dir_all(&prof).unwrap();
    std::fs::write(prof.join("Cookies"), "x").unwrap();
    // a daemon that is starting up holds the lock but does not answer pings yet
    let paths = ks_browser::paths::Paths::new(&h.0);
    ks_browser::paths::ensure_private_dir(&paths.run_dir()).unwrap();
    let lock = LockHolder::start(&h.0, "logout_refuses_while_the_profile_lock_is_held");
    let o = ks(&h.0, &["auth", "logout", "--yes"]);
    assert_eq!(o.status.code(), Some(2), "{}", outputs(&o));
    assert!(prof.join("Cookies").exists(), "deleted while the profile lock was held");
    drop(lock);
    let o = ks(&h.0, &["auth", "logout", "--yes"]);
    assert_eq!(o.status.code(), Some(0), "{}", outputs(&o));
    assert!(!prof.exists());
}

#[test]
fn logout_refuses_a_symlinked_profiles_dir() {
    let h = Home::new();
    let elsewhere = h.0.join("elsewhere");
    std::fs::create_dir_all(elsewhere.join("default")).unwrap();
    std::fs::write(elsewhere.join("default/keep"), "x").unwrap();
    std::os::unix::fs::symlink(&elsewhere, h.0.join("profiles")).unwrap();
    let o = ks(&h.0, &["auth", "logout", "--yes"]);
    assert_eq!(o.status.code(), Some(2), "{}", outputs(&o));
    assert!(elsewhere.join("default/keep").exists(), "deleted through a symlinked profiles directory");
}

#[test]
fn a_daemon_from_a_different_ks_build_is_replaced_or_needs_the_human() {
    // same crate version, different executable: a rebuilt `ks` must not keep talking to the old daemon
    let h = Home::new();
    // declared after `h`, so it runs first: a failed assertion must not leave a daemon running from
    // the deleted copy
    struct StopDaemon(PathBuf);
    impl Drop for StopDaemon {
        fn drop(&mut self) {
            if let Ok(p) = std::fs::read_to_string(self.0.join("run/default.pid")) {
                let _ = Command::new("/bin/kill").arg(p.trim()).output();
            }
        }
    }
    let _stop = StopDaemon(h.0.clone());
    let old = h.0.join("ks-old");
    std::fs::copy(env!("CARGO_BIN_EXE_ks"), &old).unwrap();
    let new = Path::new(env!("CARGO_BIN_EXE_ks"));
    let run = |exe: &Path, args: &[&str]| {
        Command::new(exe)
            .args(args)
            .env("KS_HOME", &h.0)
            .env("KS_NOTIFY", "0")
            .env("KS_CHROME", "/nonexistent/chrome") // never launch a browser here; the daemon stays up
            .env("KS_BROWSER_IDLE_SECS", "60")
            .env_remove("KS_BROWSER_DAEMON")
            .output()
            .unwrap()
    };
    let pid = || std::fs::read_to_string(h.0.join("run/default.pid")).unwrap().trim().to_string();
    let wait_gone = |p: &str| {
        let t = std::time::Instant::now();
        while Command::new("/bin/kill").args(["-0", p]).output().unwrap().status.success() {
            assert!(t.elapsed() < std::time::Duration::from_secs(30), "daemon {p} did not exit");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    };

    // unfinished work in the old daemon: replacing it needs the human
    let paths = ks_browser::paths::Paths::new(&h.0);
    ks_browser::paths::ensure_private_dir(&paths.run_dir()).unwrap();
    std::fs::write(paths.pending_file("default"), "CAPTCHA\n").unwrap();
    assert_eq!(run(&old, &["browser", "connect"]).status.code(), Some(2), "Chrome cannot start here");
    let p_old = pid();
    let o = run(new, &["browser", "connect"]);
    assert_eq!(o.status.code(), Some(3), "{}", String::from_utf8_lossy(&o.stdout));
    assert_eq!(json(&o)["event"], "DAEMON_REPLACE");
    assert_eq!(pid(), p_old, "a daemon with unfinished work was replaced");
    assert_eq!(run(&old, &["browser", "disconnect"]).status.code(), Some(0));
    wait_gone(&p_old);
    std::fs::remove_file(paths.pending_file("default")).unwrap();

    // an idle headless daemon of another build is replaced; the same build is reused
    assert_eq!(run(&old, &["browser", "connect"]).status.code(), Some(2));
    let p_old = pid();
    assert_eq!(run(&old, &["browser", "connect"]).status.code(), Some(2));
    assert_eq!(pid(), p_old, "the same build must reuse its daemon");
    assert_eq!(run(new, &["browser", "connect"]).status.code(), Some(2));
    let p_new = pid();
    assert_ne!(p_new, p_old, "the daemon of the other build was reused");
    wait_gone(&p_old);
    assert_eq!(run(new, &["browser", "disconnect"]).status.code(), Some(0));
    wait_gone(&p_new);
}

#[test]
fn the_guidance_for_a_daemon_of_another_build_leads_out() {
    // DAEMON_REPLACE from a daemon of another build must not send the human to a command that runs
    // into the same DAEMON_REPLACE again
    let h = Home::new();
    struct StopDaemon(PathBuf);
    impl Drop for StopDaemon {
        fn drop(&mut self) {
            if let Ok(p) = std::fs::read_to_string(self.0.join("run/default.pid")) {
                let _ = Command::new("/bin/kill").arg(p.trim()).output();
            }
        }
    }
    let _stop = StopDaemon(h.0.clone());
    let old = h.0.join("ks-old");
    std::fs::copy(env!("CARGO_BIN_EXE_ks"), &old).unwrap();
    let new = Path::new(env!("CARGO_BIN_EXE_ks"));
    let run = |exe: &Path, args: &[&str]| {
        Command::new(exe)
            .args(args)
            .env("KS_HOME", &h.0)
            .env("KS_NOTIFY", "0")
            .env("KS_CHROME", "/nonexistent/chrome") // never launch a browser here; the daemon stays up
            .env("KS_BROWSER_IDLE_SECS", "60")
            .env_remove("KS_BROWSER_DAEMON")
            .output()
            .unwrap()
    };
    let pid = || std::fs::read_to_string(h.0.join("run/default.pid")).unwrap().trim().to_string();
    let wait_gone = |p: &str| {
        let t = std::time::Instant::now();
        while Command::new("/bin/kill").args(["-0", p]).output().unwrap().status.success() {
            assert!(t.elapsed() < std::time::Duration::from_secs(30), "daemon {p} did not exit");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    };
    let paths = ks_browser::paths::Paths::new(&h.0);
    ks_browser::paths::ensure_private_dir(&paths.run_dir()).unwrap();
    std::fs::write(paths.pending_file("default"), "CAPTCHA\n").unwrap();
    assert_eq!(run(&old, &["browser", "connect"]).status.code(), Some(2), "Chrome cannot start here");
    let p_old = pid();
    let o = run(new, &["browser", "connect"]);
    assert_eq!(o.status.code(), Some(3), "{}", String::from_utf8_lossy(&o.stdout));
    let v = json(&o);
    assert_eq!(v["event"], "DAEMON_REPLACE", "{v}");
    // the resume_cmd, run as printed by the new build, reaches the old daemon instead of looping
    let cmd: Vec<&str> = v["resume_cmd"].as_str().unwrap().split_whitespace().collect();
    assert_eq!(cmd[0], "ks", "{v}");
    let o = run(new, &cmd[1..]);
    let r = json(&o);
    assert_ne!(r["event"], "DAEMON_REPLACE", "following resume_cmd {cmd:?} ran into DAEMON_REPLACE again: {r}");
    assert_eq!(pid(), p_old, "the daemon with unfinished work was replaced");
    // the other way out the steps name: disconnect keeps the unresolved interrupt for the new build
    assert!(v["steps"].to_string().contains("ks browser disconnect"), "{v}");
    assert_eq!(run(new, &["browser", "disconnect"]).status.code(), Some(0));
    wait_gone(&p_old);
    assert_eq!(run(new, &["browser", "connect"]).status.code(), Some(2), "Chrome cannot start here");
    let s = json(&run(new, &["status"]));
    assert_eq!((s["pending_event"].as_str(), s["daemon"].as_str()), (Some("CAPTCHA"), Some("running")), "{s}");
    let p_new = pid();
    assert_ne!(p_new, p_old);
    assert_eq!(run(new, &["browser", "disconnect"]).status.code(), Some(0));
    wait_gone(&p_new);
    std::fs::remove_file(paths.pending_file("default")).unwrap();

    // a headful daemon of another build whose Chrome never started holds no window: it is replaced
    assert_eq!(run(&old, &["browser", "connect", "--headed"]).status.code(), Some(2), "Chrome cannot start here");
    let p_old = pid();
    let o = run(new, &["browser", "connect"]);
    assert_eq!(o.status.code(), Some(2), "a daemon without a window was not replaced: {}", String::from_utf8_lossy(&o.stdout));
    let p_new = pid();
    assert_ne!(p_new, p_old);
    wait_gone(&p_old);
    assert_eq!(run(new, &["browser", "disconnect"]).status.code(), Some(0));
    wait_gone(&p_new);
}

/// A pseudo-terminal (master, slave), both close-on-exec: `ks auth google` runs only with a
/// terminal on stdin. Opened in-process; no `script` command and no shell.
fn pty() -> (File, File) {
    extern "C" {
        fn openpty(amaster: *mut i32, aslave: *mut i32, name: *mut std::ffi::c_char, termp: *mut std::ffi::c_void, winp: *mut std::ffi::c_void) -> i32;
        fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    }
    const F_SETFD: i32 = 2;
    const FD_CLOEXEC: i32 = 1;
    let (mut m, mut s) = (-1, -1);
    // SAFETY: openpty fills in two new descriptors, which the Files below then own
    unsafe {
        assert_eq!(openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()), 0, "openpty: {}", std::io::Error::last_os_error());
        fcntl(m, F_SETFD, FD_CLOEXEC);
        fcntl(s, F_SETFD, FD_CLOEXEC);
        (File::from_raw_fd(m), File::from_raw_fd(s))
    }
}

/// `ks auth google` with extra environment and a terminal on stdin. The terminal already holds
/// Ctrl+D, so a run that gets as far as waiting for Enter reads end-of-input instead of hanging.
fn ks_auth(home: &Path, env: &[(&str, &str)]) -> Output {
    let (mut term, slave) = pty();
    term.write_all(&[4]).unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_ks"));
    c.args(["auth", "google"]).env("KS_HOME", home).env("KS_NOTIFY", "0").env("KS_BROWSER_IDLE_SECS", "1").env_remove("KS_BROWSER_DAEMON").stdin(slave);
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn alive(pid: &str) -> bool {
    Command::new("/bin/kill").args(["-0", pid]).output().unwrap().status.success()
}

/// `ks auth google` waiting at its prompt on a terminal. KS_CHROME is /usr/bin/yes, a compiled
/// system binary that runs until it is signalled: it stands in for the sign-in Chrome, so no
/// browser opens and no website is contacted.
struct Login {
    ks: Child,
    term: File,
    /// pid of the stand-in sign-in window
    window: String,
}

impl Login {
    fn start(home: &Path) -> Login {
        let (term, slave) = pty();
        let ks = Command::new(env!("CARGO_BIN_EXE_ks"))
            .args(["auth", "google"])
            .env("KS_HOME", home)
            .env("KS_NOTIFY", "0")
            .env("KS_CHROME", "/usr/bin/yes")
            .env("KS_BROWSER_IDLE_SECS", "1")
            .env_remove("KS_BROWSER_DAEMON")
            .stdin(slave)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut l = Login { ks, term, window: String::new() };
        let t = Instant::now();
        while l.window.is_empty() {
            if let Some(s) = l.ks.try_wait().unwrap() {
                let (_, out, err) = l.finish();
                panic!("ks auth google exited ({s}) before opening the window: {out} {err}");
            }
            assert!(t.elapsed() < Duration::from_secs(10), "the sign-in window never started");
            let kids = Command::new("/usr/bin/pgrep").args(["-P", &l.ks.id().to_string()]).output().unwrap();
            l.window = String::from_utf8_lossy(&kids.stdout).split_whitespace().next().unwrap_or("").to_string();
            std::thread::sleep(Duration::from_millis(50));
        }
        l
    }

    /// Wait for ks to exit: (exit code, stdout, stderr).
    fn finish(&mut self) -> (Option<i32>, String, String) {
        let t = Instant::now();
        let status = loop {
            if let Some(s) = self.ks.try_wait().unwrap() {
                break s;
            }
            assert!(t.elapsed() < Duration::from_secs(40), "ks auth google did not exit");
            std::thread::sleep(Duration::from_millis(50));
        };
        let (mut out, mut err) = (String::new(), String::new());
        self.ks.stdout.take().unwrap().read_to_string(&mut out).unwrap();
        self.ks.stderr.take().unwrap().read_to_string(&mut err).unwrap();
        (status.code(), out, err)
    }
}

impl Drop for Login {
    fn drop(&mut self) {
        let _ = self.ks.kill();
        let _ = self.ks.wait();
        // a stand-in window that outlived ks (a failed assertion) must not keep running
        let comm = Command::new("/bin/ps").args(["-o", "comm=", "-p", &self.window]).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        if !self.window.is_empty() && comm.is_ok_and(|c| c.ends_with("yes")) {
            let _ = Command::new("/bin/kill").arg(&self.window).output();
        }
    }
}

/// Stops the daemon a test started, also when an assertion fails first.
struct StopDaemonOf(PathBuf);
impl Drop for StopDaemonOf {
    fn drop(&mut self) {
        if let Ok(p) = std::fs::read_to_string(self.0.join("run/default.pid")) {
            let _ = Command::new("/bin/kill").arg(p.trim()).output();
        }
    }
}

#[test]
fn auth_google_without_a_terminal_changes_nothing() {
    // an agent or the MCP server running the printed resume_cmd: refused before anything happens
    let run = |home: &Path, stdin: Stdio| {
        Command::new(env!("CARGO_BIN_EXE_ks"))
            .args(["auth", "google"])
            .env("KS_HOME", home)
            .env("KS_NOTIFY", "0")
            .env("KS_CHROME", "/usr/bin/yes") // would run (and show up) if a window were opened
            .env("KS_BROWSER_IDLE_SECS", "60")
            .env_remove("KS_BROWSER_DAEMON")
            .stdin(stdin)
            .output()
            .unwrap()
    };
    let check = |o: &Output| {
        assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
        let e = String::from_utf8_lossy(&o.stderr);
        assert!(e.contains("terminal") && e.contains("ks auth google"), "{e}");
        assert!(!e.contains("is open on the Google sign-in page"), "a sign-in window was opened: {e}");
    };

    // no daemon: nothing is created (no lock, no profile directory, no window)
    let h = Home::new();
    for stdin in [Stdio::null(), Stdio::piped()] {
        check(&run(&h.0, stdin));
        assert!(!h.0.join("run").exists() && !h.0.join("profiles").exists(), "state was created");
    }

    // a running daemon (the human's window, say) is left running
    let h = Home::new();
    let _stop = StopDaemonOf(h.0.clone());
    let o = Command::new(env!("CARGO_BIN_EXE_ks"))
        .args(["browser", "connect"])
        .env("KS_HOME", &h.0)
        .env("KS_NOTIFY", "0")
        .env("KS_CHROME", "/nonexistent/chrome") // never launch a browser here; the daemon stays up
        .env("KS_BROWSER_IDLE_SECS", "60")
        .env_remove("KS_BROWSER_DAEMON")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2), "Chrome cannot start here");
    let pid = std::fs::read_to_string(h.0.join("run/default.pid")).unwrap();
    for stdin in [Stdio::null(), Stdio::piped()] {
        check(&run(&h.0, stdin));
        assert_eq!(std::fs::read_to_string(h.0.join("run/default.pid")).ok(), Some(pid.clone()), "the daemon was stopped");
        assert!(alive(pid.trim()), "the daemon was stopped");
    }
    assert_eq!(ks(&h.0, &["browser", "disconnect"]).status.code(), Some(0));
}

#[test]
fn auth_google_holds_the_profile_while_the_window_is_open_and_closes_it_after_enter() {
    let h = Home::new();
    let paths = ks_browser::paths::Paths::new(&h.0);
    // the sign-in check after Enter must start no daemon (and no Chrome) here: the daemon log is a
    // symlink, which the client refuses to open, so the check fails at once
    ks_browser::paths::ensure_private_dir(&paths.run_dir()).unwrap();
    std::os::unix::fs::symlink(h.0.join("elsewhere"), paths.daemon_log("default")).unwrap();

    let mut l = Login::start(&h.0);
    // the profile lock is held while the window runs: no daemon (and no daemon Chrome) can start
    let lock = ks_browser::daemon::lock_profile(&paths, "default");
    let free = lock.is_ok();
    drop(lock);
    assert!(!free, "the profile lock is free while the sign-in window runs");
    assert!(alive(&l.window), "premise: the window runs");

    l.term.write_all(b"\n").unwrap();
    let (code, out, err) = l.finish();
    assert!(!alive(&l.window), "the sign-in window still runs after Enter: {err}");
    assert_eq!(code, Some(2), "{out} {err}");
    assert!(out.contains("daemon log"), "premise: the check ran and failed at the daemon log: {out} {err}");
    assert!(ks_browser::daemon::lock_profile(&paths, "default").is_ok(), "the profile lock is still held");
    no_daemon(&h.0);
}

#[test]
fn auth_google_ctrl_d_cancels_closes_the_window_and_skips_the_check() {
    let h = Home::new();
    let mut l = Login::start(&h.0);
    l.term.write_all(&[4]).unwrap();
    let (code, out, err) = l.finish();
    assert!(!alive(&l.window), "the sign-in window still runs after Ctrl+D: {err}");
    assert_eq!(code, Some(2), "{out} {err}");
    assert!(err.contains("cancel") && err.contains("not checked") && err.contains("ks auth google"), "{err}");
    assert!(out.is_empty(), "the sign-in was checked: {out}");
    no_daemon(&h.0);
    let paths = ks_browser::paths::Paths::new(&h.0);
    assert!(ks_browser::daemon::lock_profile(&paths, "default").is_ok(), "the profile lock is still held");
}

#[test]
fn commands_during_the_sign_in_window_say_the_profile_is_in_use() {
    let h = Home::new();
    let _stop = StopDaemonOf(h.0.clone());
    let mut l = Login::start(&h.0);

    // a search starts a daemon, which cannot take the profile
    let o = Command::new(env!("CARGO_BIN_EXE_ks"))
        .args(["research", "search", "--engine", "google-browser", "--query", "x"])
        .env("KS_HOME", &h.0)
        .env("KS_NOTIFY", "0")
        .env("KS_CHROME", "/nonexistent/chrome")
        .env("KS_BROWSER_IDLE_SECS", "1")
        .env_remove("KS_BROWSER_DAEMON")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
    let e = json(&o)["error"].as_str().unwrap_or_default().to_string();
    assert!(e.contains("in use") && e.contains("ks auth google"), "{e}");
    no_daemon(&h.0);

    // a second `ks auth google`
    let o = ks_auth(&h.0, &[("KS_CHROME", "/usr/bin/yes")]);
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
    let e = String::from_utf8_lossy(&o.stderr);
    assert!(e.contains("in use") && e.contains("ks auth google") && !e.contains("disconnect"), "{e}");
    assert!(alive(&l.window), "the first sign-in window was touched");

    l.term.write_all(&[4]).unwrap();
    l.finish();
    assert!(!alive(&l.window));
}

#[test]
fn auth_google_reports_a_missing_chrome_without_starting_a_daemon() {
    let h = Home::new();
    let o = ks_auth(&h.0, &[("KS_CHROME", "/nonexistent/chrome")]);
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
    assert!(String::from_utf8_lossy(&o.stderr).contains("Chrome not found"), "{}", String::from_utf8_lossy(&o.stderr));
    no_daemon(&h.0);
}

#[test]
fn auth_google_refuses_a_profile_another_chrome_holds() {
    let h = Home::new();
    let prof = h.0.join("profiles/default");
    std::fs::create_dir_all(&prof).unwrap();
    // Chrome's lock is a symlink to "<host>-<pid>"; this test process stands in for a live Chrome.
    // KS_CHROME is a compiled binary that exists (never a script): it must not be started at all.
    std::os::unix::fs::symlink(format!("somehost-{}", std::process::id()), prof.join("SingletonLock")).unwrap();
    let o = ks_auth(&h.0, &[("KS_CHROME", env!("CARGO_BIN_EXE_ks"))]);
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
    let e = String::from_utf8_lossy(&o.stderr);
    assert!(e.contains("in use") && e.contains(&std::process::id().to_string()), "{e}");
    no_daemon(&h.0);
    assert!(std::fs::symlink_metadata(prof.join("SingletonLock")).is_ok(), "the lock of another Chrome was touched");
}

#[test]
fn auth_google_refuses_while_the_daemon_is_busy() {
    let h = Home::new();
    struct StopDaemon(PathBuf);
    impl Drop for StopDaemon {
        fn drop(&mut self) {
            if let Ok(p) = std::fs::read_to_string(self.0.join("run/default.pid")) {
                let _ = Command::new("/bin/kill").arg(p.trim()).output();
            }
        }
    }
    let _stop = StopDaemon(h.0.clone());
    // a search keeps the daemon busy: the profile is signed in (marker) but held by another
    // Chrome (this test process), so the daemon waits up to 15 s for it and never starts Chrome
    let prof = h.0.join("profiles/default");
    std::fs::create_dir_all(&prof).unwrap();
    std::fs::set_permissions(&prof, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(prof.join(".ks-google-auth"), "").unwrap();
    std::os::unix::fs::symlink(format!("somehost-{}", std::process::id()), prof.join("SingletonLock")).unwrap();
    let home = h.0.clone();
    let search = std::thread::spawn(move || {
        Command::new(env!("CARGO_BIN_EXE_ks"))
            .args(["research", "search", "--engine", "google-browser", "--query", "x"])
            .env("KS_HOME", &home)
            .env("KS_NOTIFY", "0")
            .env("KS_CHROME", env!("CARGO_BIN_EXE_ks"))
            .env("KS_BROWSER_IDLE_SECS", "60")
            .env_remove("KS_BROWSER_DAEMON")
            .output()
            .unwrap()
    });
    let c = ks_browser::client::Client::new(ks_browser::paths::Paths::new(&h.0), "default");
    let t = std::time::Instant::now();
    while c.ping().is_none_or(|p| p["busy"] != true) {
        assert!(t.elapsed() < std::time::Duration::from_secs(10), "the daemon never got busy");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let pid = std::fs::read_to_string(h.0.join("run/default.pid")).unwrap();

    let t = std::time::Instant::now();
    let o = ks_auth(&h.0, &[("KS_CHROME", env!("CARGO_BIN_EXE_ks"))]);
    assert!(t.elapsed() < std::time::Duration::from_secs(5), "it waited for the busy daemon instead of refusing");
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
    let v = json(&o);
    assert!(v["error"].as_str().is_some_and(|e| e.contains("busy") && e.contains("ks auth google")), "{v}");
    assert_eq!(std::fs::read_to_string(h.0.join("run/default.pid")).unwrap(), pid, "the busy daemon was stopped");

    let _ = search.join();
    assert_eq!(ks(&h.0, &["browser", "disconnect"]).status.code(), Some(0));
}

#[test]
fn auth_check_starts_its_own_browser() {
    // the sign-in check runs in a browser the daemon starts itself (headless), after the plain
    // sign-in Chrome has exited: with no browser running it tries to start one
    let h = Home::new();
    struct StopDaemon(PathBuf);
    impl Drop for StopDaemon {
        fn drop(&mut self) {
            if let Ok(p) = std::fs::read_to_string(self.0.join("run/default.pid")) {
                let _ = Command::new("/bin/kill").arg(p.trim()).output();
            }
        }
    }
    let _stop = StopDaemon(h.0.clone());
    let o = Command::new(env!("CARGO_BIN_EXE_ks"))
        .args(["browser", "connect"])
        .env("KS_HOME", &h.0)
        .env("KS_NOTIFY", "0")
        .env("KS_CHROME", "/nonexistent/chrome") // never launch a browser here; the daemon stays up
        .env("KS_BROWSER_IDLE_SECS", "60")
        .env_remove("KS_BROWSER_DAEMON")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2), "Chrome cannot start here");
    let c = ks_browser::client::Client::new(ks_browser::paths::Paths::new(&h.0), "default");
    let v = c.request(serde_json::json!({"cmd": "auth_check"})).unwrap();
    assert_eq!(v["status"], "error", "{v}");
    assert!(v["error"].as_str().unwrap().contains("Chrome not found"), "auth_check did not try to start a browser: {v}");
    assert_eq!(std::fs::read_to_string(h.0.join("run/default.mode")).unwrap().trim(), "headless");
    assert_eq!(ks(&h.0, &["browser", "disconnect"]).status.code(), Some(0));
}

#[test]
fn logout_waits_briefly_for_a_profile_lock_that_is_being_released() {
    LockHolder::run_if_child();
    let h = Home::new();
    let prof = h.0.join("profiles/default");
    std::fs::create_dir_all(&prof).unwrap();
    std::fs::write(prof.join("Cookies"), "x").unwrap();
    let paths = ks_browser::paths::Paths::new(&h.0);
    let run = paths.run_dir();
    ks_browser::paths::ensure_private_dir(&run).unwrap();
    // a daemon that was just shut down releases the profile lock last, a moment after `shutdown` returns
    let lock = LockHolder::start(&h.0, "logout_waits_briefly_for_a_profile_lock_that_is_being_released");
    // `ks auth logout` makes the run directory 0700 right before its first try for the lock (nothing in between waits),
    // so a run directory that turns 0700 again says that `ks` is at the lock; a start-up of any length cannot race it
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o750)).unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let mut child = Command::new(env!("CARGO_BIN_EXE_ks"))
        .args(["auth", "logout", "--yes"])
        .env("KS_HOME", &h.0)
        .env("KS_NOTIFY", "0")
        .env_remove("KS_BROWSER_DAEMON")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let t = Instant::now();
    while mode(&run) != 0o700 {
        assert!(t.elapsed() < Duration::from_secs(30), "`ks auth logout` never reached the profile lock");
        std::thread::sleep(Duration::from_millis(5));
    }
    // its first try fails on the held lock right away; well inside its 2 s of retries, it must still be waiting
    std::thread::sleep(Duration::from_millis(300));
    assert!(child.try_wait().unwrap().is_none(), "`ks auth logout` gave up on a held profile lock instead of retrying it");
    drop(lock);
    let o = child.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", outputs(&o));
    assert!(!prof.exists(), "the profile was not deleted");
}
