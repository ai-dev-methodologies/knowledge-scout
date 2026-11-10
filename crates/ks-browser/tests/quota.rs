//! Per-profile search quota: hourly and daily limits over a persisted history (run/<profile>.quota).
use ks_browser::quota::{self, Quota};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);
fn tmp() -> std::path::PathBuf {
    let p = std::path::PathBuf::from(format!("/tmp/ks-quota-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}
const NOW: u64 = 1_800_000_000;

#[test]
fn defaults_and_configuration() {
    assert_eq!(Quota::default(), Quota { hourly: 20, daily: 100 });
    assert_eq!(Quota::from_values(None, None).unwrap(), Quota { hourly: 20, daily: 100 });
    assert_eq!(Quota::from_values(Some("5"), Some("40")).unwrap(), Quota { hourly: 5, daily: 40 });
    for bad in ["0", "-1", "x", "", "1.5"] {
        assert!(Quota::from_values(Some(bad), None).is_err(), "hourly {bad:?}");
        assert!(Quota::from_values(None, Some(bad)).is_err(), "daily {bad:?}");
    }
}

#[test]
fn hourly_limit() {
    let q = Quota { hourly: 3, daily: 100 };
    let recent = |n: u64| (0..n).map(|i| NOW - 60 * i).collect::<Vec<_>>();
    assert_eq!(q.exceeded(&recent(2), NOW), None);
    let why = q.exceeded(&recent(3), NOW).expect("3 in the last hour reaches an hourly limit of 3");
    assert!(why.starts_with("quota"), "{why}");
    // older than an hour no longer counts for the hourly limit
    assert_eq!(q.exceeded(&[NOW - 3600, NOW - 3700, NOW - 5000], NOW), None);
}

#[test]
fn daily_limit() {
    let q = Quota { hourly: 100, daily: 5 };
    let spread: Vec<u64> = (1..=5).map(|h| NOW - 3600 * h * 4).collect(); // 4 h .. 20 h ago
    let why = q.exceeded(&spread, NOW).expect("5 in the last day reaches a daily limit of 5");
    assert!(why.starts_with("quota"), "{why}");
    assert_eq!(q.exceeded(&spread[..4], NOW), None);
    assert_eq!(q.exceeded(&[NOW - 86_400, NOW - 90_000, NOW - 100_000, NOW - 200_000, NOW - 300_000], NOW), None, "older than a day");
    // a timestamp from the future (clock moved back) still counts
    assert!(Quota { hourly: 1, daily: 1 }.exceeded(&[NOW + 50], NOW).is_some());
}

#[test]
fn history_file_is_0600_pruned_to_a_day_and_survives_reload() {
    let d = tmp();
    let f = d.join("default.quota");
    assert!(quota::load(&f).unwrap().is_empty(), "no file: no history");
    // (a line that is not a timestamp fails closed: a_line_that_is_not_a_timestamp_fails_closed...)
    std::fs::write(&f, format!("{}\n{}\n{}\n", NOW - 200_000, NOW - 100, NOW - 50)).unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
    quota::record(&f, NOW).unwrap();
    assert_eq!(quota::load(&f).unwrap(), vec![NOW - 100, NOW - 50, NOW]);
    assert_eq!(std::fs::symlink_metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_history_that_exists_but_cannot_be_read_fails_closed() {
    let d = tmp();
    let f = d.join("default.quota");
    let q = Quota { hourly: 100, daily: 100 };
    let check = |what: &str| {
        let e = quota::load(&f).expect_err(what);
        assert!(e.contains(&f.display().to_string()) && e.contains("rm "), "{what}: {e}");
        assert!(q.admit(&f, NOW).is_err(), "{what}: admitted");
        assert!(quota::record(&f, NOW).is_err(), "{what}: recorded over it");
    };
    std::fs::write(&f, b"1799999000\n\xff\n").unwrap();
    check("not UTF-8");
    std::fs::remove_file(&f).unwrap();
    std::fs::write(d.join("real"), "1799999000\n").unwrap();
    std::os::unix::fs::symlink(d.join("real"), &f).unwrap();
    check("symlink");
    assert!(std::fs::symlink_metadata(&f).unwrap().file_type().is_symlink(), "the symlink was replaced");
    std::fs::remove_file(&f).unwrap();
    std::fs::write(&f, "1799999000\n").unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o000)).unwrap();
    check("no permission");
    std::fs::remove_file(&f).unwrap();
    std::fs::create_dir(&f).unwrap();
    check("a directory");
    std::fs::remove_dir(&f).unwrap();
    // missing: an empty history, and admit counts the query
    assert_eq!(q.admit(&f, NOW), Ok(()));
    assert_eq!(quota::load(&f).unwrap(), vec![NOW]);
    assert!(Quota { hourly: 1, daily: 100 }.admit(&f, NOW).unwrap_err().starts_with("quota"));
    assert_eq!(quota::load(&f).unwrap(), vec![NOW], "a refused query is not counted");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_line_that_is_not_a_timestamp_fails_closed_like_an_unreadable_file() {
    let d = tmp();
    let f = d.join("default.quota");
    let q = Quota { hourly: 100, daily: 100 };
    for text in ["garbage\n", "1799999000\ngarbage\n", "17999x9000\n", "1799999000\n-5\n"] {
        std::fs::write(&f, text).unwrap();
        let e = quota::load(&f).expect_err(text);
        assert!(e.contains(&f.display().to_string()) && e.contains("rm "), "{text:?}: {e}");
        assert!(q.admit(&f, NOW).is_err(), "{text:?}: admitted");
        assert!(quota::record(&f, NOW).is_err(), "{text:?}: recorded over it");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), text, "{text:?}: the file was rewritten");
    }
    // blank lines and surrounding spaces are not damage
    std::fs::write(&f, format!("\n {} \n\n{}\n", NOW - 10, NOW - 5)).unwrap();
    assert_eq!(quota::load(&f).unwrap(), vec![NOW - 10, NOW - 5]);
    std::fs::remove_dir_all(&d).unwrap();
}
