//! Human-intervention detection on hand-written fixtures (no network). Detection runs before extraction.
use ks_browser::detect::{detect, is_signed_in, verify_result_page, PageProblem, PageSnapshot};
use ks_browser::events::Event;

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}
fn page(url: &str, status: u16, file: &str) -> PageSnapshot {
    PageSnapshot { url: url.into(), title: String::new(), status, html: if file.is_empty() { String::new() } else { fixture(file) } }
}
fn ev(p: &PageSnapshot) -> Option<Event> {
    detect(p).map(|d| d.event)
}

#[test]
fn a_normal_result_page_is_not_an_interrupt_even_when_snippets_quote_interrupt_phrases() {
    assert_eq!(ev(&page("https://www.google.com/search?q=redis", 200, "serp_basic.html")), None);
    assert_eq!(ev(&page("https://www.google.com/search?q=google+unusual+traffic", 200, "serp_mentions_interrupts.html")), None);
}

#[test]
fn captcha_sorry_page_is_captcha_even_with_http_429() {
    let p = page("https://www.google.com/sorry/index?continue=https://www.google.com/search%3Fq%3Dredis&q=x", 429, "captcha_sorry.html");
    let d = detect(&p).unwrap();
    assert_eq!(d.event, Event::Captcha);
    assert!(d.reason.contains("429"), "{}", d.reason);
    // the form alone is enough, wherever it is served from
    assert_eq!(ev(&page("https://www.google.com/search?q=redis", 200, "captcha_sorry.html")), Some(Event::Captcha));
    // and so is the "unusual traffic" text on a page that is not a result page
    assert_eq!(ev(&page("https://www.google.com/search?q=redis", 200, "unusual_traffic_text_only.html")), Some(Event::Captcha));
}

#[test]
fn sorry_path_is_captcha_only_on_google_com_hosts() {
    assert_eq!(ev(&page("https://www.google.com/sorry/index?continue=x", 200, "")), Some(Event::Captcha));
    for url in ["https://www.google.evil.com/sorry/index", "https://google.evil.com/sorry/index", "https://google.com.evil.com/sorry/index"] {
        assert_eq!(ev(&page(url, 200, "")), None, "{url}");
    }
}

#[test]
fn http_429_without_captcha_is_blocked() {
    assert_eq!(ev(&page("https://www.google.com/search?q=redis", 429, "")), Some(Event::Blocked));
    assert_eq!(ev(&page("https://www.google.com/search?q=redis", 429, "serp_basic.html")), Some(Event::Blocked));
}

#[test]
fn consent_page_and_consent_overlay() {
    assert_eq!(ev(&page("https://consent.google.com/ml?continue=https://www.google.com/search?q%3Dredis", 200, "consent.html")), Some(Event::Consent));
    assert_eq!(ev(&page("https://www.google.com/search?q=redis", 200, "serp_with_consent_overlay.html")), Some(Event::Consent));
}

#[test]
fn login_wall_and_mfa_prompt() {
    assert_eq!(ev(&page("https://accounts.google.com/v3/signin/identifier?continue=https%3A%2F%2Fwww.google.com", 200, "signin.html")), Some(Event::AuthRequired));
    assert_eq!(ev(&page("https://accounts.google.com/ServiceLogin?continue=x", 200, "")), Some(Event::AuthRequired));
    // the sign-in form embedded on another host is still a login wall
    assert_eq!(ev(&page("https://www.google.com/search?q=redis", 200, "signin.html")), Some(Event::AuthRequired));
    assert_eq!(ev(&page("https://accounts.google.com/v3/signin/challenge/ipp?TL=x", 200, "mfa_challenge.html")), Some(Event::MfaPrompt));
    assert_eq!(ev(&page("https://accounts.google.com/signin/v2/challenge/totp?x=1", 200, "")), Some(Event::MfaPrompt));
}

#[test]
fn non_google_pages_are_no_interrupt_but_never_a_usable_result_page() {
    // detection only looks for interrupts: these pages show none
    assert_eq!(ev(&page("file:///tmp/x.html", 0, "serp_basic.html")), None);
    assert_eq!(ev(&page("about:blank", 0, "")), None);
    // but a search must fail closed on them instead of answering "ok" with whatever is there
    assert_eq!(verify_result_page(&page("about:blank", 0, "")), Err(PageProblem::NotGoogle(String::new())));
    assert_eq!(verify_result_page(&page("file:///tmp/x.html", 0, "serp_basic.html")), Err(PageProblem::NotGoogle(String::new())));
    assert_eq!(verify_result_page(&page("https://www.google.evil.com/search?q=x", 200, "serp_basic.html")), Err(PageProblem::NotGoogle("www.google.evil.com".into())));
    assert_eq!(verify_result_page(&page("https://evil.example/search?q=x", 200, "serp_basic.html")), Err(PageProblem::NotGoogle("evil.example".into())));
}

#[test]
fn a_result_page_must_be_google_with_a_results_container_and_no_http_error() {
    assert_eq!(verify_result_page(&page("https://www.google.com/search?q=redis", 200, "serp_basic.html")), Ok(()));
    assert_eq!(verify_result_page(&page("https://www.google.com/search?q=redis", 0, "serp_basic.html")), Ok(()));
    for s in [400, 403, 404, 500, 503] {
        assert_eq!(verify_result_page(&page("https://www.google.com/search?q=redis", s, "serp_basic.html")), Err(PageProblem::HttpStatus(s)), "{s}");
    }
    // a Google page without #rso / #search (an unknown interstitial, an empty page) is not a result page
    assert_eq!(verify_result_page(&page("https://www.google.com/search?q=redis", 200, "consent.html")), Err(PageProblem::NoResults));
    assert_eq!(verify_result_page(&page("https://www.google.com/search?q=redis", 200, "")), Err(PageProblem::NoResults));
}

#[test]
fn signed_in_marker_on_my_account() {
    assert!(is_signed_in(&page("https://myaccount.google.com/?pli=1", 200, "myaccount_signed_in.html")));
    // redirected to sign-in, the landing page for signed-out users, or no marker: not signed in
    assert!(!is_signed_in(&page("https://accounts.google.com/v3/signin/identifier?continue=https://myaccount.google.com/", 200, "signin.html")));
    assert!(!is_signed_in(&page("https://myaccount.google.com/intro", 200, "myaccount_intro.html")));
    assert!(!is_signed_in(&page("https://myaccount.google.com/", 200, "myaccount_intro.html")));
    // the marker on any other host does not count
    assert!(!is_signed_in(&page("https://evil.example.com/myaccount.google.com", 200, "myaccount_signed_in.html")));
}
