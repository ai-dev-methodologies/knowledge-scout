//! Real-browser smoke test, opt-in with KS_TEST_BROWSER=1. Launches headless Chrome on a throwaway
//! profile and opens only a local file:// page; it never contacts Google or any other site.
use ks_browser::detect::detect;
use ks_browser::serp::extract;
use ks_browser::session::Session;
use ks_browser::Mode;

#[tokio::test(flavor = "multi_thread")]
async fn headless_chrome_reads_a_local_result_page() {
    if std::env::var("KS_TEST_BROWSER").ok().as_deref() != Some("1") {
        eprintln!("skipped: set KS_TEST_BROWSER=1 to run");
        return;
    }
    let profile = std::path::PathBuf::from(format!("/tmp/ks-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&profile);
    let page = format!("file://{}/tests/fixtures/serp_basic.html", env!("CARGO_MANIFEST_DIR"));

    let s = Session::launch(&profile, Mode::Headless).await.expect("launch headless chrome");
    s.navigate(&page).await.expect("navigate");
    let snap = s.snapshot().await.expect("snapshot");
    assert_eq!(snap.url, page);
    assert_eq!(snap.title, "redis ttl - Google Search");
    assert!(detect(&snap).is_none());
    let results = extract(&snap.html, 10);
    assert_eq!(results.len(), 4, "{results:?}");
    assert_eq!(results[0].title, "EXPIRE | Docs");
    s.close().await;
    // a graceful close releases the profile lock
    assert!(std::fs::symlink_metadata(profile.join("SingletonLock")).is_err());
    std::fs::remove_dir_all(&profile).unwrap();
}

/// Same opt-in and throwaway profile as above. A page script cannot change what the snapshot reports
/// (the snapshot runs in an isolated world), and CDP never listens on a TCP port (pipe transport).
#[tokio::test(flavor = "multi_thread")]
async fn snapshot_ignores_page_scripts_and_cdp_uses_no_tcp_port() {
    if std::env::var("KS_TEST_BROWSER").ok().as_deref() != Some("1") {
        eprintln!("skipped: set KS_TEST_BROWSER=1 to run");
        return;
    }
    let profile = std::path::PathBuf::from(format!("/tmp/ks-smoke2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&profile);
    let page = format!("file://{}/tests/fixtures/hostile_page.html", env!("CARGO_MANIFEST_DIR"));

    let s = Session::launch(&profile, Mode::Headless).await.expect("launch headless chrome");
    // pipe transport: Chrome writes no DevToolsActivePort and listens on no TCP port
    assert!(!profile.join("DevToolsActivePort").exists(), "DevToolsActivePort written: CDP is on a TCP port");
    let pid = s.browser_pid().to_string();
    let lsof = std::process::Command::new("/usr/sbin/lsof").args(["-nP", "-a", "-p", &pid, "-iTCP", "-sTCP:LISTEN"]).output().unwrap();
    assert!(String::from_utf8_lossy(&lsof.stdout).trim().is_empty(), "Chrome listens on TCP: {}", String::from_utf8_lossy(&lsof.stdout));

    s.navigate(&page).await.expect("navigate");
    let snap = s.snapshot().await.expect("snapshot");
    assert_eq!(snap.url, page);
    assert_eq!(snap.title, "real title");
    assert!(snap.html.contains("real body"), "{}", snap.html);
    s.close().await;
    std::fs::remove_dir_all(&profile).unwrap();
}
