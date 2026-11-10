//! Human-intervention detection, run on every page BEFORE any extraction. Structural markers
//! (URL, form ids) always count; text phrases count only on pages that are not result pages,
//! because a result snippet can quote "unusual traffic" without being a CAPTCHA.
use crate::events::Event;
use crate::serp::{is_google_host, text_of};
use scraper::{Html, Selector};
use url::Url;

#[derive(Debug, Clone, Default)]
pub struct PageSnapshot {
    pub url: String,
    pub title: String,
    /// HTTP status of the main document (0 when unknown, e.g. file://).
    pub status: u16,
    pub html: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    pub event: Event,
    pub reason: String,
}

struct Page {
    host: String,
    path: String,
    doc: Html,
}

impl Page {
    fn new(p: &PageSnapshot) -> Page {
        let u = Url::parse(&p.url).ok();
        Page {
            host: u.as_ref().and_then(|u| u.host_str()).unwrap_or("").to_ascii_lowercase(),
            path: u.as_ref().map(|u| u.path().to_string()).unwrap_or_default(),
            doc: Html::parse_document(&p.html),
        }
    }
    fn has(&self, css: &str) -> bool {
        self.doc.select(&Selector::parse(css).expect("static selector")).next().is_some()
    }
    fn body_text(&self) -> String {
        self.doc.select(&Selector::parse("body").expect("static selector")).next().map(text_of).unwrap_or_default().to_lowercase()
    }
}

fn hit(event: Event, reason: impl Into<String>) -> Option<Detection> {
    Some(Detection { event, reason: reason.into() })
}

pub fn detect(page: &PageSnapshot) -> Option<Detection> {
    let p = Page::new(page);
    let is_serp = p.has("#rso") || p.has("#search");
    let text = if is_serp { String::new() } else { p.body_text() };
    let http = if page.status == 429 { " (HTTP 429)" } else { "" };

    if (is_google_host(&p.host) && p.path.starts_with("/sorry"))
        || p.has("form#captcha-form")
        || p.has(".g-recaptcha")
        || p.has("iframe[src*=\"recaptcha\"]")
        || text.contains("unusual traffic")
        || text.contains("비정상적인 트래픽")
    {
        return hit(Event::Captcha, format!("CAPTCHA / unusual-traffic page{http}"));
    }
    if page.status == 429 {
        return hit(Event::Blocked, "HTTP 429 Too Many Requests");
    }
    if p.host == "consent.google.com"
        || p.has("form[action*=\"consent.google.com\"]")
        || text.contains("before you continue to google")
        || text.contains("google 서비스를 계속 이용하기 전에")
    {
        return hit(Event::Consent, "cookie / terms consent page");
    }
    if p.host == "accounts.google.com" && p.path.contains("/challenge") {
        return hit(Event::MfaPrompt, "2-Step Verification prompt");
    }
    if p.host == "accounts.google.com" || p.has("input#identifierId") || p.has("input[name=\"identifier\"]") {
        return hit(Event::AuthRequired, "Google sign-in page (login wall)");
    }
    None
}

/// True only on myaccount.google.com (not its signed-out /intro page) when the account menu or
/// sign-out link is present. Reads the page only; never cookies.
pub fn is_signed_in(page: &PageSnapshot) -> bool {
    let p = Page::new(page);
    p.host == "myaccount.google.com"
        && !p.path.starts_with("/intro")
        && (p.has("a[href*=\"accounts.google.com/SignOutOptions\"]")
            || p.has("[aria-label^=\"Google Account:\"]")
            || p.has("[aria-label^=\"Google 계정:\"]"))
}

/// Why a page cannot be used as a Google result page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageProblem {
    /// The main document answered with HTTP 400 or above.
    HttpStatus(u16),
    /// The page is not on www.google.com / google.com (the host, empty when there is none).
    NotGoogle(String),
    /// No `#rso` / `#search` results container: an unknown page, not a result page.
    NoResults,
}

impl std::fmt::Display for PageProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PageProblem::HttpStatus(s) => write!(f, "HTTP {s} on the result page"),
            PageProblem::NotGoogle(h) if h.is_empty() => write!(f, "the page is not on a Google host"),
            PageProblem::NotGoogle(h) => write!(f, "the page is on {h}, not on a Google host"),
            PageProblem::NoResults => write!(f, "the page has no Google results container"),
        }
    }
}

/// Fail closed: a search answers `ok` only for a page on www.google.com (where `search_url`
/// points) or google.com, with no HTTP error and a results container. Run after `detect`.
pub fn verify_result_page(page: &PageSnapshot) -> Result<(), PageProblem> {
    if page.status >= 400 {
        return Err(PageProblem::HttpStatus(page.status));
    }
    let p = Page::new(page);
    if !matches!(p.host.as_str(), "www.google.com" | "google.com") {
        return Err(PageProblem::NotGoogle(p.host));
    }
    if !(p.has("#rso") || p.has("#search")) {
        return Err(PageProblem::NoResults);
    }
    Ok(())
}
