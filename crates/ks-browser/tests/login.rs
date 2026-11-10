//! The sign-in window of `ks auth google`: a plain Chrome with no remote control and no automation
//! flag, closed with SIGTERM (never SIGKILL) so it saves the sign-in. No test here opens Chrome.
use ks_browser::login::{login_args, stop};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn the_sign_in_window_is_a_plain_chrome_without_remote_control() {
    let a = login_args(std::path::Path::new("/tmp/ks-profile-x"));
    for x in &a {
        assert!(!x.starts_with("--remote-debugging"), "{x}");
        assert!(!x.contains("automation") && !x.contains("Automation"), "{x}");
        assert!(!x.starts_with("--password-store") && x != "--use-mock-keychain", "{x}");
        assert!(!x.starts_with("--headless"), "{x}");
        assert!(!x.contains("--user-agent") && !x.contains("excludeSwitches") && !x.contains("--disable-blink-features"), "{x}");
    }
    // --disable-sync: accepting Chrome's sync offer would bring the personal account's passwords and
    // extensions into the ks profile. It hides nothing about the browser.
    assert_eq!(a, ["--user-data-dir=/tmp/ks-profile-x", "--no-first-run", "--no-default-browser-check", "--disable-sync", "https://accounts.google.com/"]);
}

#[test]
fn stop_ends_the_process_with_sigterm() {
    let mut c = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    stop(&mut c, Duration::from_secs(10)).expect("sleep exits on SIGTERM");
    assert_eq!(c.wait().unwrap().signal(), Some(libc::SIGTERM));
    // a window the human already closed: nothing to do
    let mut done = Command::new("/bin/sleep").arg("0").spawn().unwrap();
    done.wait().unwrap();
    stop(&mut done, Duration::from_secs(1)).expect("an exited process is fine");
}

#[test]
fn stop_never_kills_a_process_that_does_not_exit() {
    // SIG_IGN survives exec: this sleep ignores SIGTERM, like a Chrome that hangs on shutdown
    let mut c = unsafe {
        Command::new("/bin/sleep")
            .arg("30")
            .pre_exec(|| {
                libc::signal(libc::SIGTERM, libc::SIG_IGN);
                Ok(())
            })
            .spawn()
            .unwrap()
    };
    let t = Instant::now();
    let why = stop(&mut c, Duration::from_secs(1)).expect_err("it did not exit");
    assert!(t.elapsed() >= Duration::from_secs(1));
    assert!(c.try_wait().unwrap().is_none(), "the process was killed");
    assert!(why.contains(&c.id().to_string()) && why.contains("yourself"), "{why}");
    c.kill().unwrap();
    c.wait().unwrap();
}
