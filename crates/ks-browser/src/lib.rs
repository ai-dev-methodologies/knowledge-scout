//! Browser collection adapter for knowledge-scout: a per-profile daemon that drives Chrome over CDP,
//! extracts Google result pages, and stops for a human whenever a login wall, CAPTCHA, consent page,
//! 2-step prompt or rate limit appears. It never solves CAPTCHAs and adds no anti-detection flags.
pub mod cdp;
pub mod client;
pub mod daemon;
pub mod detect;
pub mod events;
pub mod login;
pub mod paths;
pub mod quota;
pub mod serp;
pub mod session;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The environment for the daemon and for Chrome: the spawn sites call `env_clear()` and pass only
/// this. Allowed: HOME, PATH, TMPDIR, LANG, the Linux desktop session variables Chrome needs to open
/// a window and reach the secret store (DISPLAY, WAYLAND_DISPLAY, XAUTHORITY, XDG_RUNTIME_DIR,
/// DBUS_SESSION_BUS_ADDRESS, XDG_CURRENT_DESKTOP; no secrets) and `KS_*`, except names containing
/// `REDIS` (KS_REDIS_PASSWORD, and KS_REDIS_URL / KS_TEST_REDIS_URL, which can embed the same password).
pub fn child_env(vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    vars.into_iter()
        .filter(|(k, _)| {
            let Some(k) = k.to_str() else { return false };
            const SESSION: [&str; 6] = ["DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS", "XDG_CURRENT_DESKTOP"];
            matches!(k, "HOME" | "PATH" | "TMPDIR" | "LANG") || SESSION.contains(&k) || (k.starts_with("KS_") && !k.contains("REDIS"))
        })
        .collect()
}

/// Browser window mode. Both modes use the same profile directory and the same OS keychain key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Headless,
    Headful,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Headless => "headless",
            Mode::Headful => "headful",
        }
    }
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "headless" => Some(Mode::Headless),
            "headful" => Some(Mode::Headful),
            _ => None,
        }
    }
}

/// Which executable file `path` is: device, inode, size and modification time of the file it leads to.
/// The path is left out, so the same file reached through a symlink is the same build. A rebuilt or
/// replaced `ks` differs in at least one of them even when the crate version is the same. The
/// daemon reports its own (taken at start) in `ping`; the client compares it with the `ks` it would
/// start. Null when the file cannot be read.
pub fn exe_identity(path: &std::path::Path) -> serde_json::Value {
    use std::os::unix::fs::MetadataExt;
    match std::fs::metadata(path) {
        Ok(m) => serde_json::json!({"dev": m.dev(), "ino": m.ino(), "size": m.size(), "mtime": m.mtime(), "mtime_nsec": m.mtime_nsec()}),
        Err(_) => serde_json::Value::Null,
    }
}
