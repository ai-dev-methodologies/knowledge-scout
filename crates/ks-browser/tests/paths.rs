//! State directory permissions and profile names.
use ks_browser::paths::{ensure_private_dir, validate_profile_name, write_private_file, Paths};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);
fn tmp() -> std::path::PathBuf {
    let p = std::path::PathBuf::from(format!("/tmp/ks-paths-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&p);
    p
}
fn mode(p: &std::path::Path) -> u32 {
    std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn layout_under_home() {
    let p = Paths::new("/x/.ks");
    assert_eq!(p.run_dir(), std::path::PathBuf::from("/x/.ks/run"));
    assert_eq!(p.socket("default"), std::path::PathBuf::from("/x/.ks/run/default.sock"));
    assert_eq!(p.token("default"), std::path::PathBuf::from("/x/.ks/run/default.token"));
    assert_eq!(p.pid_file("default"), std::path::PathBuf::from("/x/.ks/run/default.pid"));
    assert_eq!(p.version_file("default"), std::path::PathBuf::from("/x/.ks/run/default.version"));
    assert_eq!(p.mode_file("default"), std::path::PathBuf::from("/x/.ks/run/default.mode"));
    assert_eq!(p.profile_dir("work"), std::path::PathBuf::from("/x/.ks/profiles/work"));
    assert_eq!(p.events_log(), std::path::PathBuf::from("/x/.ks/events.log"));
}

#[test]
fn private_dir_is_created_0700_and_loose_existing_dirs_are_tightened() {
    let t = tmp();
    let d = t.join("a/run");
    ensure_private_dir(&d).unwrap();
    assert_eq!(mode(&d), 0o700);
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
    ensure_private_dir(&d).unwrap();
    assert_eq!(mode(&d), 0o700);
    std::fs::remove_dir_all(&t).unwrap();
}

#[test]
fn private_dir_refuses_a_symlink() {
    let t = tmp();
    std::fs::create_dir_all(t.join("real")).unwrap();
    std::os::unix::fs::symlink(t.join("real"), t.join("link")).unwrap();
    assert!(ensure_private_dir(&t.join("link")).is_err());
    std::fs::remove_dir_all(&t).unwrap();
}

#[test]
fn private_file_is_0600_replaces_old_content_and_does_not_follow_symlinks() {
    let t = tmp();
    ensure_private_dir(&t).unwrap();
    let f = t.join("token");
    std::fs::write(&f, "old").unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
    write_private_file(&f, b"new").unwrap();
    assert_eq!(mode(&f), 0o600);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "new");
    // a symlink planted at the path is replaced, its target is untouched
    let target = t.join("target");
    std::fs::write(&target, "keep").unwrap();
    std::fs::remove_file(&f).unwrap();
    std::os::unix::fs::symlink(&target, &f).unwrap();
    write_private_file(&f, b"x").unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
    assert!(!std::fs::symlink_metadata(&f).unwrap().file_type().is_symlink());
    std::fs::remove_dir_all(&t).unwrap();
}

#[test]
fn profile_names_are_restricted() {
    for ok in ["default", "work-2", "a_b", "A1"] {
        assert!(validate_profile_name(ok).is_ok(), "{ok}");
    }
    for bad in ["", ".", "..", "a/b", "../x", "a b", "x.y", &"a".repeat(33)] {
        assert!(validate_profile_name(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_private_file_is_replaced_atomically_and_leaves_no_temporary_file() {
    // a reader (or a crash) between the steps of a rewrite must never find the file missing
    let t = tmp();
    ensure_private_dir(&t).unwrap();
    let f = t.join("default.quota");
    write_private_file(&f, b"1\n").unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader = {
        let (f, stop) = (f.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut missing = 0;
            while !stop.load(Ordering::SeqCst) {
                if std::fs::symlink_metadata(&f).is_err() {
                    missing += 1;
                }
            }
            missing
        })
    };
    for i in 0..300 {
        write_private_file(&f, format!("{i}\n").as_bytes()).unwrap();
    }
    stop.store(true, Ordering::SeqCst);
    assert_eq!(reader.join().unwrap(), 0, "the file was missing while it was rewritten");
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "299\n");
    assert_eq!(mode(&f), 0o600);
    let names: Vec<String> = std::fs::read_dir(&t).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["default.quota"], "temporary files left behind");
    std::fs::remove_dir_all(&t).unwrap();
}
