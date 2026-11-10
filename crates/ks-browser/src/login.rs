//! The sign-in window of `ks auth google`: the Chrome executable the daemon uses, started as a plain
//! browser on the profile directory, without a remote-debugging connection or any automation flag.
//! Google refuses sign-in in a browser driven over CDP ("this browser or app may not be secure"),
//! so the human signs in here. Searching still drives Chrome over CDP and hides nothing about it.
use crate::cdp::chrome::find_chrome;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const SIGN_IN_URL: &str = "https://accounts.google.com/";
/// How long `stop` waits for Chrome to exit after SIGTERM.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// The whole command line of the sign-in window (without the executable): the profile, no first-run
/// or default-browser prompts, no sync (accepting Chrome's sync offer would bring the personal
/// account's passwords and extensions into the ks profile), the sign-in page. Nothing else.
pub fn login_args(profile_dir: &Path) -> Vec<String> {
    vec![
        format!("--user-data-dir={}", profile_dir.display()),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-sync".into(),
        SIGN_IN_URL.into(),
    ]
}

/// The same executable the daemon would start (`KS_CHROME`, then the standard locations).
pub fn chrome() -> Result<PathBuf, String> {
    find_chrome().ok_or_else(|| "Chrome not found. Install Google Chrome or set KS_CHROME to its executable.".to_string())
}

/// Start the sign-in window with the same environment allowlist as the daemon's Chrome. It stays in
/// our process group, so Ctrl+C in the terminal sends SIGINT to it too. Whether Chrome saves the
/// sign-in on SIGINT is not checked; Enter closes it with SIGTERM instead (`stop`).
pub fn launch(chrome: &Path, profile_dir: &Path) -> Result<Child, String> {
    Command::new(chrome)
        .args(login_args(profile_dir))
        .env_clear()
        .envs(crate::child_env(std::env::vars_os()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start Chrome at {}: {e}", chrome.display()))
}

/// Close the sign-in window the way Chrome saves its cookies: SIGTERM, then wait up to `timeout`.
/// Already exited (the human closed it) is fine. Never SIGKILL: a killed Chrome can lose the
/// sign-in, so when it does not exit in time the human is asked to quit it.
pub fn stop(child: &mut Child, timeout: Duration) -> Result<(), String> {
    if let Ok(Some(_)) = child.try_wait() {
        return Ok(());
    }
    let pid = child.id();
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(100)),
            Ok(None) => break,
            Err(e) => return Err(format!("waiting for Chrome (pid {pid}): {e}")),
        }
    }
    Err(format!(
        "Chrome (pid {pid}) did not exit within {} s after it was asked to close. It was not killed, because that can lose the sign-in: quit that Chrome yourself (Cmd+Q)",
        timeout.as_secs()
    ))
}
