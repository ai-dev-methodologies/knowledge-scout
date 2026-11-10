//! A persistent cookie written by one headless Chrome is read back by the next headless Chrome on the
//! same profile: the OS keychain encrypts and decrypts it across a restart. (Headful<->headless
//! sharing needs a window, so it is checked by hand at manual sign-in.)
//! Opt-in with KS_TEST_BROWSER=1: headless Chrome on a throwaway profile loads only pages served on
//! 127.0.0.1 by this test; it never contacts Google or any other site.
use ks_browser::session::Session;
use ks_browser::Mode;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

/// A local HTTP server: `/set` sets a persistent cookie; every request's path and Cookie header
/// (empty when none) are recorded.
fn serve_local() -> (String, Arc<Mutex<Vec<(String, String)>>>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(vec![]));
    let log = seen.clone();
    std::thread::spawn(move || {
        for mut s in l.incoming().flatten() {
            let mut buf = [0u8; 8192];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or_default().to_string();
            let cookie = req
                .lines()
                .find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("cookie")).map(|(_, v)| v.trim().to_string()))
                .unwrap_or_default();
            log.lock().unwrap().push((path.clone(), cookie));
            let set = if path == "/set" { "Set-Cookie: ks_keep=yes; Max-Age=86400; Path=/\r\n" } else { "" };
            let body = "<html><head><title>local</title></head><body>local</body></html>";
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n{set}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    (base, seen)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_persistent_cookie_survives_a_headless_restart_on_the_same_profile() {
    if std::env::var("KS_TEST_BROWSER").ok().as_deref() != Some("1") {
        eprintln!("skipped: set KS_TEST_BROWSER=1 to run");
        return;
    }
    let dir = std::path::PathBuf::from(format!("/tmp/ks-cookie-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let profile = dir.join("profile");
    let (base, seen) = serve_local();
    let cookie_for = |path: &str| seen.lock().unwrap().iter().filter(|(p, _)| p == path).map(|(_, c)| c.clone()).collect::<Vec<_>>();

    let s = Session::launch(&profile, Mode::Headless).await.expect("launch headless chrome");
    assert_eq!(s.navigate(&format!("{base}/set")).await, Ok(200));
    assert_eq!(cookie_for("/set"), vec![String::new()], "a fresh profile already sent a cookie");
    s.close().await;
    assert!(profile.join("Default").join("Cookies").exists(), "no cookie store written to the profile");

    let s = Session::launch(&profile, Mode::Headless).await.expect("relaunch headless chrome");
    assert_eq!(s.navigate(&format!("{base}/check")).await, Ok(200));
    assert_eq!(cookie_for("/check"), vec!["ks_keep=yes".to_string()], "the cookie did not survive the restart");
    s.close().await;
    std::fs::remove_dir_all(&dir).unwrap();
}
