//! Where the daemon keeps its state. Everything lives under `KS_HOME` (default `~/.ks`):
//! `run/` (0700: socket, token, status files, daemon log), `profiles/<name>/` (0700: the Chrome
//! profile, a credential) and `events.log` (0600).
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Paths {
    pub home: PathBuf,
}

impl Paths {
    pub fn new(home: impl Into<PathBuf>) -> Paths {
        Paths { home: home.into() }
    }
    /// `KS_HOME` when set (must be absolute, and not the home directory itself: the state directory
    /// is made 0700, which must never happen to $HOME), else `$HOME/.ks`.
    pub fn from_env() -> Result<Paths, String> {
        if let Some(h) = std::env::var_os("KS_HOME").filter(|h| !h.is_empty()) {
            let p = PathBuf::from(h);
            if !p.is_absolute() {
                return Err(format!("KS_HOME must be an absolute path, got {}", p.display()));
            }
            let same = |a: &Path, b: &Path| matches!((fs::metadata(a), fs::metadata(b)), (Ok(x), Ok(y)) if x.dev() == y.dev() && x.ino() == y.ino());
            if std::env::var_os("HOME").filter(|h| !h.is_empty()).is_some_and(|home| p == Path::new(&home) || same(&p, Path::new(&home))) {
                return Err(format!("KS_HOME must not be your home directory ({}); point it at a dedicated directory such as ~/.ks", p.display()));
            }
            return Ok(Paths::new(p));
        }
        let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).ok_or("HOME is not set")?;
        Ok(Paths::new(PathBuf::from(home).join(".ks")))
    }
    pub fn run_dir(&self) -> PathBuf {
        self.home.join("run")
    }
    pub fn profiles_dir(&self) -> PathBuf {
        self.home.join("profiles")
    }
    pub fn profile_dir(&self, profile: &str) -> PathBuf {
        self.profiles_dir().join(profile)
    }
    fn run_file(&self, profile: &str, ext: &str) -> PathBuf {
        self.run_dir().join(format!("{profile}.{ext}"))
    }
    pub fn socket(&self, profile: &str) -> PathBuf {
        self.run_file(profile, "sock")
    }
    pub fn token(&self, profile: &str) -> PathBuf {
        self.run_file(profile, "token")
    }
    pub fn lock(&self, profile: &str) -> PathBuf {
        self.run_file(profile, "lock")
    }
    pub fn pid_file(&self, profile: &str) -> PathBuf {
        self.run_file(profile, "pid")
    }
    pub fn version_file(&self, profile: &str) -> PathBuf {
        self.run_file(profile, "version")
    }
    pub fn mode_file(&self, profile: &str) -> PathBuf {
        self.run_file(profile, "mode")
    }
    pub fn pending_file(&self, profile: &str) -> PathBuf {
        self.run_file(profile, "pending")
    }
    pub fn quota_file(&self, profile: &str) -> PathBuf {
        self.run_file(profile, "quota")
    }
    pub fn daemon_log(&self, profile: &str) -> PathBuf {
        self.run_file(profile, "log")
    }
    pub fn events_log(&self) -> PathBuf {
        self.home.join("events.log")
    }
}

/// 1-32 characters of `[A-Za-z0-9_-]`, so a profile name can never escape `profiles/` or `run/`.
pub fn validate_profile_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty() && name.len() <= 32 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(format!("invalid profile name {name:?}: use 1-32 letters, digits, '-' or '_'"))
    }
}

/// Create `path` (and parents) and make it 0700. Refuses a symlink or a directory owned by someone else.
pub fn ensure_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    let md = fs::symlink_metadata(path)?;
    if md.file_type().is_symlink() || !md.is_dir() {
        return Err(io::Error::other(format!("{}: not a plain directory", path.display())));
    }
    if md.uid() != unsafe { libc::getuid() } {
        return Err(io::Error::other(format!("{}: owned by another user", path.display())));
    }
    if md.permissions().mode() & 0o777 != 0o700 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Replace `path` with a new 0600 file holding `contents`, atomically: the contents go to a new
/// temporary file in the same directory (created exclusively, without following links) and that
/// file is renamed over the path. Readers see the old file or the new one, never a missing or
/// half-written one; whatever was at the path (including a planted symlink) is replaced, never
/// followed. A temporary file is removed when the write fails; a crash can leave one
/// (`.<name>.<pid>.<n>.tmp`).
pub fn write_private_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let name = path.file_name().ok_or_else(|| io::Error::other(format!("{}: not a file path", path.display())))?;
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_file_name(format!(".{}.{}.{n}.tmp", name.to_string_lossy(), std::process::id()));
    let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(&tmp)?;
    let written = f.write_all(contents).and_then(|_| f.sync_all()).and_then(|_| fs::rename(&tmp, path));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}
