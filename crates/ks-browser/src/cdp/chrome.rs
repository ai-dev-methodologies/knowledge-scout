// Vendored from vercel-labs/agent-browser (Apache-2.0), cli/src/native/cdp/chrome.rs at commit
// e7519957b9067c541825afa21a792fbe1f483047. See NOTICE and third_party/agent-browser/LICENSE.
// Modifications: trimmed to launching one Chrome on a persistent profile directory we own. CDP runs
// over --remote-debugging-pipe (fds 3 and 4) instead of a localhost TCP port, so no other local
// process can attach to the browser; the DevToolsActivePort discovery was removed with it. Chrome
// gets a cleared environment with an allowlist (crate::child_env). Removed temp profiles, copying
// the user's real Chrome profiles, Xvfb, NSS trust stores, proxies, extensions, WebGPU, Windows
// support, the stderr URL fallback and --disable-popup-blocking. No stealth / anti-detection flags.
// 2026-11-10: removed --password-store=basic and --use-mock-keychain (an earlier version here set
// them in both modes so headful and headless shared one key). Cookies are now encrypted with the
// real OS keychain key in both modes, not a public fixed key.
use std::fs::File;
use std::os::unix::io::{FromRawFd, RawFd};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct ChromeProcess {
    child: Child,
    /// Our ends of the CDP pipe: we write commands to `cdp_in` (Chrome's fd 3) and read
    /// responses from `cdp_out` (Chrome's fd 4). Taken once by the CDP client.
    pipes: Option<(File, File)>,
    /// The process group ID used to kill the entire Chrome process tree.
    pgid: i32,
}

impl ChromeProcess {
    /// Our ends of the CDP pipe, `(write commands, read responses)`. `None` after the first call.
    pub fn take_pipes(&mut self) -> Option<(File, File)> {
        self.pipes.take()
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        // Kill the entire process group so Chrome helper processes (GPU,
        // renderer, utility, crashpad) are also terminated.
        unsafe {
            libc::kill(-self.pgid, libc::SIGKILL);
        }
        let _ = self.child.wait();
    }

    /// Returns the OS process ID of the Chrome child process.
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Non-blocking check whether Chrome has exited.
    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)) | Err(_))
    }

    /// Wait for Chrome to exit on its own (after Browser.close CDP command),
    /// falling back to kill() if it doesn't exit within the timeout.
    /// This allows Chrome to flush cookies and other state to the user-data-dir.
    pub fn wait_or_kill(&mut self, timeout: Duration) {
        let start = Instant::now();
        while start.elapsed() < timeout {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => break,
            }
        }
        self.kill();
    }
}

impl Drop for ChromeProcess {
    fn drop(&mut self) {
        if !self.has_exited() {
            self.kill();
        }
    }
}

#[derive(Clone, Debug)]
pub struct LaunchOptions {
    pub headless: bool,
    pub user_data_dir: PathBuf,
    pub executable_path: Option<PathBuf>,
}

/// The full Chrome command line (without the executable). Public so tests can check no keychain
/// override flag and no anti-detection flag is ever present.
pub fn chrome_args(options: &LaunchOptions) -> Vec<String> {
    let mut args = vec![
        "--remote-debugging-pipe".to_string(),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--disable-background-networking".to_string(),
        "--disable-backgrounding-occluded-windows".to_string(),
        "--disable-component-update".to_string(),
        "--disable-default-apps".to_string(),
        "--disable-hang-monitor".to_string(),
        "--disable-prompt-on-repost".to_string(),
        "--disable-sync".to_string(),
        "--metrics-recording-only".to_string(),
        // No --password-store / --use-mock-keychain: cookies are encrypted with the OS keychain key
        // in both modes, never a public fixed key.
    ];
    if options.headless {
        args.push("--headless=new".to_string());
    }
    args.push(format!("--user-data-dir={}", options.user_data_dir.display()));
    args.push("about:blank".to_string());
    args
}

/// A pipe whose two ends are close-on-exec: only the copies dup2'ed onto fds 3/4 reach Chrome.
fn cloexec_pipe() -> Result<(RawFd, RawFd), String> {
    let mut fds = [0 as RawFd; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(format!("pipe: {}", std::io::Error::last_os_error()));
    }
    for fd in fds {
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    Ok((fds[0], fds[1]))
}

pub fn launch_chrome(options: &LaunchOptions) -> Result<ChromeProcess, String> {
    let chrome_path = match &options.executable_path {
        Some(p) => p.clone(),
        None => find_chrome().ok_or_else(|| {
            "Chrome not found. Install Google Chrome or set KS_CHROME to its executable.".to_string()
        })?,
    };

    // --remote-debugging-pipe: Chrome reads NUL-terminated JSON commands from fd 3 and writes
    // responses/events to fd 4. Wrapping the raw fds in File right away closes them on every path.
    let (cmd_r, cmd_w) = cloexec_pipe()?;
    let (cmd_r, cmd_w) = unsafe { (File::from_raw_fd(cmd_r), File::from_raw_fd(cmd_w)) };
    let (resp_r, resp_w) = cloexec_pipe()?;
    let (resp_r, resp_w) = unsafe { (File::from_raw_fd(resp_r), File::from_raw_fd(resp_w)) };
    let (child_in, child_out): (RawFd, RawFd) = {
        use std::os::unix::io::AsRawFd;
        (cmd_r.as_raw_fd(), resp_w.as_raw_fd())
    };

    let mut cmd = Command::new(&chrome_path);
    cmd.args(chrome_args(options))
        .env_clear()
        .envs(crate::child_env(std::env::vars_os()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // Place Chrome in its own process group so we can kill the entire tree
    // (main process + helpers) with a single killpg(), and hand it the pipe as fds 3 and 4.
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: pre_exec runs between fork() and exec() in the child; setpgid, fcntl and dup2
        // are async-signal-safe. The ends are first copied above fd 4 (close-on-exec) so dup2
        // cannot clobber one with the other; dup2 clears close-on-exec on fds 3 and 4.
        unsafe {
            cmd.pre_exec(move || {
                libc::setpgid(0, 0);
                let a = libc::fcntl(child_in, libc::F_DUPFD_CLOEXEC, 10);
                let b = libc::fcntl(child_out, libc::F_DUPFD_CLOEXEC, 10);
                if a < 0 || b < 0 || libc::dup2(a, 3) < 0 || libc::dup2(b, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("Failed to launch Chrome at {:?}: {}", chrome_path, e))?;
    // Chrome holds its own copies now; dropping ours lets EOF reach the reader when Chrome exits.
    drop(cmd_r);
    drop(resp_w);
    let pgid = child.id() as i32;
    Ok(ChromeProcess { child, pipes: Some((cmd_w, resp_r)), pgid })
}

/// `KS_CHROME` first, then the standard macOS application paths, then Chrome/Chromium on PATH.
pub fn find_chrome() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("KS_CHROME").map(PathBuf::from) {
        return p.is_file().then_some(p);
    }
    let candidates = [
        "Google Chrome.app/Contents/MacOS/Google Chrome",
        "Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
        "Chromium.app/Contents/MacOS/Chromium",
    ];
    let home_apps = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Applications"));
    for apps in std::iter::once(PathBuf::from("/Applications")).chain(home_apps) {
        for c in &candidates {
            let p = apps.join(c);
            if p.exists() {
                return Some(p);
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for name in ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"] {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}
