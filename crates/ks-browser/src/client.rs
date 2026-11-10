//! Talking to the daemon from the CLI and the MCP server (blocking). The client spawns the daemon by
//! re-executing the `ks` binary with `KS_BROWSER_DAEMON=1` in a new session (setsid), so it outlives
//! the command that started it, and authenticates every request with the token file.
use crate::daemon::{ENV_FLAG, ENV_MODE, ENV_PROFILE};
use crate::events::{other_build_daemon, tail_log};
use crate::paths::{ensure_private_dir, validate_profile_name, Paths};
use crate::Mode;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
const PING_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Client {
    pub paths: Paths,
    pub profile: String,
}

/// Send one JSON line and read the one-line JSON answer.
pub fn send_line(socket: &Path, line: &str, timeout: Duration) -> Result<Value, String> {
    let mut s = UnixStream::connect(socket).map_err(|e| format!("cannot reach the browser daemon: {e}"))?;
    s.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
    s.write_all(format!("{line}\n").as_bytes()).map_err(|e| e.to_string())?;
    let mut out = String::new();
    s.read_to_string(&mut out).map_err(|e| format!("browser daemon did not answer: {e}"))?;
    serde_json::from_str(out.trim()).map_err(|e| format!("browser daemon sent a bad answer: {e}"))
}

fn err(msg: impl Into<String>) -> Value {
    json!({"status": "error", "error": msg.into()})
}

/// The daemon (ping answer `p`) runs the same build as `exe`: the same crate version and executable file.
fn same_build(p: &Value, exe: &Path) -> bool {
    p["version"] == crate::VERSION && !p["exe"].is_null() && p["exe"] == crate::exe_identity(exe)
}

impl Client {
    pub fn new(paths: Paths, profile: &str) -> Client {
        Client { paths, profile: profile.to_string() }
    }

    /// The daemon token, only from a regular file we own that nobody else can read or write. Opened
    /// without following links and checked on the open descriptor (fstat), so the file that is
    /// checked is the file that is read.
    pub fn read_token(&self) -> Result<String, String> {
        let path = self.paths.token(&self.profile);
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let md = f.metadata().map_err(|e| format!("{}: {e}", path.display()))?;
        if !md.file_type().is_file() || md.uid() != unsafe { libc::getuid() } || md.mode() & 0o077 != 0 {
            return Err(format!("{}: refusing a token file that is not a private (0600) file owned by you", path.display()));
        }
        let mut t = String::new();
        f.read_to_string(&mut t).map_err(|e| format!("{}: {e}", path.display()))?;
        let t = t.trim().to_string();
        if t.is_empty() {
            return Err(format!("{}: empty token", path.display()));
        }
        Ok(t)
    }

    fn request_with(&self, mut req: Value, timeout: Duration) -> Result<Value, String> {
        let token = self.read_token()?;
        req.as_object_mut().ok_or("request must be a JSON object")?.insert("token".into(), Value::String(token));
        send_line(&self.paths.socket(&self.profile), &req.to_string(), timeout)
    }

    pub fn request(&self, req: Value) -> Result<Value, String> {
        self.request_with(req, REQUEST_TIMEOUT)
    }

    /// The daemon's ping answer, or None when no authenticated daemon answers.
    pub fn ping(&self) -> Option<Value> {
        self.request_with(json!({"cmd": "ping"}), PING_TIMEOUT).ok().filter(|v| v["status"] == "ok")
    }

    /// Reuse the running daemon or start one. A daemon is reused only when it runs the same build:
    /// the same crate version and the same executable file as `exe` (device, inode, size, mtime), so a
    /// rebuilt `ks` does not keep talking to a daemon whose binary is gone. `Err` carries the JSON to
    /// print (an error, or needs_human DAEMON_REPLACE when a daemon of another build holds a visible
    /// window or unfinished work).
    pub fn ensure(&self, exe: &Path, mode: Mode) -> Result<(), Value> {
        validate_profile_name(&self.profile).map_err(err)?;
        for d in [self.paths.home.clone(), self.paths.run_dir()] {
            ensure_private_dir(&d).map_err(|e| err(format!("{}: {e}", d.display())))?;
        }
        if let Some(p) = self.ping() {
            if same_build(&p, exe) {
                return Ok(());
            }
            if let Some(v) = self.other_build_refusal(&p) {
                return Err(v);
            }
            self.shutdown().map_err(err)?;
        }
        let log = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.paths.daemon_log(&self.profile))
            .map_err(|e| err(format!("daemon log: {e}")))?;
        let mut cmd = Command::new(exe);
        cmd.env_clear()
            .envs(crate::child_env(std::env::vars_os()))
            .env(ENV_FLAG, "1")
            .env(ENV_PROFILE, &self.profile)
            .env(ENV_MODE, mode.as_str())
            .env("KS_HOME", &self.paths.home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log));
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: setsid is async-signal-safe; it detaches the daemon from our terminal/session.
            unsafe {
                cmd.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
        }
        let mut child = cmd.spawn().map_err(|e| err(format!("cannot start the browser daemon: {e}")))?;
        let start = Instant::now();
        loop {
            if self.ping().is_some() {
                // reap it whenever it exits so a long-lived parent (the MCP server) keeps no zombie
                std::thread::spawn(move || child.wait());
                return Ok(());
            }
            if let Ok(Some(status)) = child.try_wait() {
                // lost a start race to another daemon that is now answering, or failed
                if self.ping().is_some() {
                    return Ok(());
                }
                if status.code() == Some(crate::daemon::EXIT_PROFILE_IN_USE) {
                    return Err(err(format!("{}; finish or close that first, then run the command again", crate::daemon::profile_in_use(&self.profile))));
                }
                return Err(err(format!("the browser daemon exited at start; see {}", self.paths.daemon_log(&self.profile).display())));
            }
            if start.elapsed() > Duration::from_secs(15) {
                return Err(err(format!("the browser daemon did not answer within 15 s; see {}", self.paths.daemon_log(&self.profile).display())));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// A daemon of another build (ping answer `p`) is replaced only when it is idle, holds no visible
    /// window and no unresolved interrupt. Some(answer for the human) otherwise.
    fn other_build_refusal(&self, p: &Value) -> Option<Value> {
        // a headful daemon whose Chrome is gone holds no window (a missing `browser` counts as one)
        let window = p["mode"] != "headless" && p["browser"] != false;
        (p["busy"] == true || window || !p["pending_event"].is_null()).then(|| other_build_daemon(p["busy"] == true, p["pending_event"].as_str(), &self.profile))
    }

    /// Before `ks auth google` opens its plain sign-in Chrome: no daemon browser may run on the
    /// profile (two Chromes cannot share it). A busy daemon is refused, a daemon of another build
    /// follows `ensure`'s rule, and any other daemon is stopped, which closes its browser gracefully;
    /// an unresolved interrupt stays saved in run/<profile>.pending.
    pub fn stop_for_login(&self, exe: &Path) -> Result<(), Value> {
        let Some(p) = self.ping() else { return Ok(()) };
        if !same_build(&p, exe) {
            if let Some(v) = self.other_build_refusal(&p) {
                return Err(v);
            }
        } else if p["busy"] == true {
            let f = if self.profile == "default" { String::new() } else { format!(" --profile {}", self.profile) };
            return Err(err(format!(
                "the browser daemon for profile {} is busy with a command (a search, say), so it was not stopped; wait until it finishes (`ks status{f}`), then run `ks auth google{f}` again",
                self.profile
            )));
        }
        self.shutdown().map(|_| ()).map_err(err)
    }

    /// Ask the daemon to close its browser gracefully and exit; waits until it is gone.
    /// Ok(false) when none was running.
    pub fn shutdown(&self) -> Result<bool, String> {
        if self.ping().is_none() {
            return Ok(false);
        }
        self.request(json!({"cmd": "shutdown"}))?;
        let start = Instant::now();
        while self.paths.socket(&self.profile).exists() || self.paths.pid_file(&self.profile).exists() {
            if start.elapsed() > Duration::from_secs(30) {
                return Err("the browser daemon did not stop within 30 s".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(true)
    }

    /// Daemon state, the unresolved interrupt and the last 10 events of this profile. Never spawns.
    pub fn status(&self) -> Value {
        let read = |p: std::path::PathBuf| std::fs::read_to_string(p).ok().map(|s| s.trim().to_string());
        let ping = self.ping();
        let daemon = match (&ping, self.paths.socket(&self.profile).exists()) {
            (Some(_), _) => "running",
            (None, true) => "unresponsive",
            (None, false) => "stopped",
        };
        let events: Vec<_> = tail_log(&self.paths, 1000).into_iter().filter(|e| e.profile == self.profile).collect();
        let recent = &events[events.len().saturating_sub(10)..];
        let pending = ping.as_ref().and_then(|p| p["pending_event"].as_str().map(String::from));
        json!({
            "profile": self.profile,
            "daemon": daemon,
            "pid": ping.as_ref().map(|p| p["pid"].clone()).unwrap_or(Value::Null),
            "version": read(self.paths.version_file(&self.profile)),
            "mode": read(self.paths.mode_file(&self.profile)),
            "browser": ping.as_ref().map(|p| p["browser"].clone()).unwrap_or(Value::Null),
            "pending_event": pending,
            "recent_events": recent,
            "events_log": self.paths.events_log(),
        })
    }
}
