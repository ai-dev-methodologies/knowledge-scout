//! Interrupt bookkeeping: every stop is pending until `ks resume` (BLOCKED included), and the same
//! interrupt three searches in a row escalates to BLOCKED.
use ks_browser::daemon::Interrupts;
use ks_browser::events::Event;

#[test]
fn blocked_is_pending_like_every_other_interrupt() {
    let mut i = Interrupts::default();
    assert_eq!(i.search_stopped(Event::Blocked), Event::Blocked);
    assert_eq!(i.pending, Some(Event::Blocked), "BLOCKED must stick until ks resume");
    let mut i = Interrupts::default();
    assert_eq!(i.search_stopped(Event::Consent), Event::Consent);
    assert_eq!(i.pending, Some(Event::Consent));
}

#[test]
fn the_same_interrupt_three_times_in_a_row_is_blocked() {
    let mut i = Interrupts::default();
    assert_eq!(i.search_stopped(Event::Captcha), Event::Captcha);
    i.pending = None; // the human ran ks resume
    assert_eq!(i.search_stopped(Event::Captcha), Event::Captcha);
    i.pending = None;
    assert_eq!(i.search_stopped(Event::Captcha), Event::Blocked, "third CAPTCHA in a row");
    assert_eq!(i.pending, Some(Event::Blocked));
}

#[test]
fn a_different_interrupt_or_a_clean_search_restarts_the_count() {
    let mut i = Interrupts::default();
    i.search_stopped(Event::Captcha);
    i.search_stopped(Event::Captcha);
    i.search_ok();
    assert_eq!(i.search_stopped(Event::Captcha), Event::Captcha, "a clean search in between resets the streak");
    i.search_stopped(Event::Captcha);
    assert_eq!(i.search_stopped(Event::Consent), Event::Consent, "a different event resets the streak");
    assert_eq!(i.search_stopped(Event::Consent), Event::Consent);
    assert_eq!(i.search_stopped(Event::Consent), Event::Blocked);
}

#[test]
fn a_restored_pending_interrupt_is_kept() {
    assert_eq!(Interrupts::new(Some(Event::Captcha)).pending, Some(Event::Captcha));
}

/// `ks resume` clears an interrupt only on a page that proves it is resolved.
#[test]
fn resume_clears_only_on_a_page_that_proves_the_interrupt_is_resolved() {
    use ks_browser::daemon::resume_may_clear;
    use ks_browser::detect::PageSnapshot;
    let fixture = |f: &str| std::fs::read_to_string(format!("{}/tests/fixtures/{f}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let page = |url: &str, status: u16, f: &str| PageSnapshot { url: url.into(), title: String::new(), status, html: if f.is_empty() { String::new() } else { fixture(f) } };
    let serp = page("https://www.google.com/search?q=redis", 200, "serp_basic.html");
    let serp_503 = page("https://www.google.com/search?q=redis", 503, "serp_basic.html");
    let blank = page("about:blank", 0, "");
    let my_account = page("https://myaccount.google.com/?pli=1", 200, "myaccount_signed_in.html");
    let local = page("file:///tmp/x.html", 0, "serp_basic.html");

    // search-blocking interrupts: only a usable Google result page
    for e in [Event::Captcha, Event::Blocked, Event::Consent, Event::EngineSwitch, Event::DaemonReplace] {
        assert_eq!(resume_may_clear(e, &serp, "default"), Ok(()), "{e:?}");
        for (name, p) in [("503", &serp_503), ("about:blank", &blank), ("myaccount", &my_account), ("file", &local)] {
            let why = resume_may_clear(e, p, "default").expect_err(&format!("{e:?} cleared on {name}"));
            assert!(why.contains("ks resume"), "{e:?} on {name}: say what to do: {why}");
        }
    }
    // sign-in interrupts: the signed-in check `ks auth google` uses
    for e in [Event::AuthRequired, Event::AuthExpired, Event::MfaPrompt] {
        assert_eq!(resume_may_clear(e, &my_account, "default"), Ok(()), "{e:?}");
        for (name, p) in [("serp", &serp), ("about:blank", &blank)] {
            let why = resume_may_clear(e, p, "default").expect_err(&format!("{e:?} cleared on {name}"));
            assert!(why.contains("ks auth google"), "{e:?} on {name}: {why}");
        }
    }
}

/// The steps `ks resume` prints name the profile they act on (none for the default profile).
#[test]
fn resume_may_clear_names_a_non_default_profile() {
    use ks_browser::daemon::resume_may_clear;
    use ks_browser::detect::PageSnapshot;
    let serp = PageSnapshot { url: "https://www.google.com/search?q=redis".into(), title: String::new(), status: 200, html: std::fs::read_to_string(format!("{}/tests/fixtures/serp_basic.html", env!("CARGO_MANIFEST_DIR"))).unwrap() };
    let blank = PageSnapshot { url: "about:blank".into(), title: String::new(), status: 0, html: String::new() };

    let why = resume_may_clear(Event::AuthExpired, &serp, "work").unwrap_err();
    assert!(why.contains("ks auth google --profile work"), "{why}");
    let why = resume_may_clear(Event::Captcha, &blank, "work").unwrap_err();
    assert!(why.contains("ks handoff CAPTCHA --profile work") && why.contains("ks resume --profile work"), "{why}");

    for (e, p) in [(Event::AuthExpired, &serp), (Event::Captcha, &blank)] {
        let why = resume_may_clear(e, p, "default").unwrap_err();
        assert!(!why.contains("--profile"), "{why}");
    }
}
