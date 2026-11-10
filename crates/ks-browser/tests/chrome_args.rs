//! The Chrome command line: the real OS keychain in both modes (no keychain override flags), no
//! anti-detection flags.
use ks_browser::cdp::chrome::{chrome_args, LaunchOptions};

fn args(headless: bool) -> Vec<String> {
    chrome_args(&LaunchOptions { headless, user_data_dir: "/tmp/ks-profile-x".into(), executable_path: None })
}

#[test]
fn cookies_use_the_real_keychain_in_headless_and_headful() {
    for a in [args(true), args(false)] {
        assert!(!a.iter().any(|x| x == "--use-mock-keychain"), "{a:?}");
        assert!(!a.iter().any(|x| x.starts_with("--password-store")), "{a:?}");
        assert!(a.iter().any(|x| x == "--user-data-dir=/tmp/ks-profile-x"), "{a:?}");
    }
    let mut h = args(true);
    h.retain(|x| x != "--headless=new");
    assert_eq!(h, args(false), "the only difference between the modes is --headless");
    assert!(!args(false).iter().any(|x| x.starts_with("--headless")));
}

#[test]
fn no_stealth_or_anti_detection_flags() {
    for a in [args(true), args(false)] {
        for x in &a {
            assert!(!x.contains("AutomationControlled"), "{x}");
            assert!(!x.contains("--user-agent"), "{x}");
            assert!(!x.contains("excludeSwitches"), "{x}");
            assert!(!x.contains("--disable-web-security"), "{x}");
        }
    }
}

#[test]
fn cdp_runs_over_a_pipe_never_a_tcp_port() {
    for a in [args(true), args(false)] {
        assert!(a.iter().any(|x| x == "--remote-debugging-pipe"), "{a:?}");
        assert!(!a.iter().any(|x| x.starts_with("--remote-debugging-port")), "{a:?}");
        assert!(!a.iter().any(|x| x.starts_with("--remote-debugging-address")), "{a:?}");
    }
}

#[test]
fn popup_blocking_stays_on() {
    for a in [args(true), args(false)] {
        assert!(!a.iter().any(|x| x == "--disable-popup-blocking"), "{a:?}");
    }
}
