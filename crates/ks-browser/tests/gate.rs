//! The one gate for daemon navigations that can send a query (searches, and the page reopened after
//! a mode switch): quota checked fail-closed, the query counted before it is sent, the pause kept.
//! Opt-in with KS_TEST_BROWSER=1: headless Chrome on a throwaway profile loads only a page served on
//! 127.0.0.1 by this test; it never contacts Google or any other site.
use ks_browser::daemon::{gated_navigate, GateError};
use ks_browser::quota::Quota;
use ks_browser::session::Session;
use ks_browser::Mode;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A local HTTP server that answers every request with a tiny page and records the request paths.
fn serve_local() -> (String, Arc<Mutex<Vec<String>>>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(vec![]));
    let log = seen.clone();
    std::thread::spawn(move || {
        for mut s in l.incoming().flatten() {
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            if let Some(path) = req.split_whitespace().nth(1) {
                log.lock().unwrap().push(path.to_string());
            }
            let body = "<html><head><title>local</title></head><body>local</body></html>";
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    (base, seen)
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_gated_navigation_is_refused_over_the_quota_and_counted_under_it() {
    if std::env::var("KS_TEST_BROWSER").ok().as_deref() != Some("1") {
        eprintln!("skipped: set KS_TEST_BROWSER=1 to run");
        return;
    }
    let dir = std::path::PathBuf::from(format!("/tmp/ks-gate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let quota_file = dir.join("default.quota");
    let (base, seen) = serve_local();
    let hits = |path: &str| seen.lock().unwrap().iter().filter(|p| *p == path).count();
    let q = Quota { hourly: 2, daily: 100 };
    let s = Session::launch(&dir.join("profile"), Mode::Headless).await.expect("launch headless chrome");

    // at the hourly limit: nothing is sent and nothing is counted
    let t = now();
    std::fs::write(&quota_file, format!("{}\n{}\n", t - 10, t - 20)).unwrap();
    let mut next = Instant::now();
    let r = gated_navigate(&s, &format!("{base}/over"), &q, &quota_file, &mut next).await;
    assert!(matches!(&r, Err(GateError::Refused(why)) if why.starts_with("quota")), "{r:?}");
    assert_eq!(hits("/over"), 0, "a refused navigation reached the server");
    assert_eq!(std::fs::read_to_string(&quota_file).unwrap().lines().count(), 2, "a refused navigation was counted");

    // an unreadable history refuses too (fail closed)
    std::fs::write(&quota_file, b"\xff\n").unwrap();
    let r = gated_navigate(&s, &format!("{base}/broken"), &q, &quota_file, &mut next).await;
    assert!(matches!(&r, Err(GateError::Refused(_))), "{r:?}");
    assert_eq!(hits("/broken"), 0);

    // under the limit: counted before it is sent, and the next one waits for the pause
    std::fs::remove_file(&quota_file).unwrap();
    let r = gated_navigate(&s, &format!("{base}/ok"), &q, &quota_file, &mut next).await;
    assert_eq!(r, Ok(200));
    assert_eq!(hits("/ok"), 1);
    assert_eq!(std::fs::read_to_string(&quota_file).unwrap().lines().count(), 1, "the navigation was not counted");
    let pause = next.saturating_duration_since(Instant::now());
    assert!(pause > Duration::from_secs(2) && pause <= Duration::from_secs(8), "no pause before the next query: {pause:?}");

    s.close().await;
    std::fs::remove_dir_all(&dir).unwrap();
}
