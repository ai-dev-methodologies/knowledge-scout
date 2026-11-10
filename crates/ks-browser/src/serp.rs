//! Google result page parsing. The daemon reads the page through CDP `Runtime.evaluate`
//! (outerHTML); everything here is pure so it can be tested on saved fixtures.
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerpResult {
    pub rank: usize,
    pub title: String,
    pub url: String,
    pub snippet: String,
}

pub fn search_url(query: &str) -> String {
    format!("https://www.google.com/search?q={}", url::form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>())
}

fn sel(css: &str) -> Selector {
    Selector::parse(css).expect("static selector")
}

pub(crate) fn text_of(e: ElementRef) -> String {
    e.text().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn is_google_host(host: &str) -> bool {
    host == "google.com" || host.ends_with(".google.com")
}

/// The organic result's destination: unwraps `/url?q=`, keeps only http(s), drops Google's own
/// search/redirect links (related searches, pagination).
fn result_url(href: &str) -> Option<String> {
    let base = Url::parse("https://www.google.com/").expect("static url");
    let mut u = base.join(href).ok()?;
    if is_google_host(u.host_str().unwrap_or("")) && u.path() == "/url" {
        let target = u.query_pairs().find(|(k, _)| k == "q" || k == "url").map(|(_, v)| v.into_owned())?;
        u = Url::parse(&target).ok()?;
    }
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    let host = u.host_str().unwrap_or("");
    if is_google_host(host) && ["/search", "/url", "/sorry"].iter().any(|p| u.path().starts_with(p)) {
        return None;
    }
    Some(u.to_string())
}

/// Organic results in page order, at most `limit`. Only `#rso` (or `#search`) is read, so ads in
/// `#tads` / `#bottomads` never count. A result is an `<a href>` around an `<h3>`; its block is the
/// largest ancestor that still holds only that one heading, and the snippet comes from that block.
pub fn extract(html: &str, limit: usize) -> Vec<SerpResult> {
    let doc = Html::parse_document(html);
    let Some(root) = doc.select(&sel("#rso")).next().or_else(|| doc.select(&sel("#search")).next()) else {
        return vec![];
    };
    let (h3, link, snippet) = (sel("h3"), sel("a[href]"), sel(".VwiC3b, [data-sncf], div[style*=\"line-clamp\"]"));
    let mut seen = HashSet::new();
    let mut out = vec![];
    for h in root.select(&h3) {
        if out.len() >= limit {
            break;
        }
        let Some(a) = h.ancestors().filter_map(ElementRef::wrap).take_while(|e| e.id() != root.id()).find(|e| link.matches(e)) else {
            continue;
        };
        let Some(url) = a.value().attr("href").and_then(result_url) else { continue };
        let title = text_of(h);
        if title.is_empty() || !seen.insert(url.clone()) {
            continue;
        }
        let mut block = a;
        while let Some(parent) = block.parent().and_then(ElementRef::wrap) {
            if parent.id() == root.id() || parent.select(&h3).count() != 1 {
                break;
            }
            block = parent;
        }
        let snippet = block.select(&snippet).next().map(text_of).unwrap_or_default();
        out.push(SerpResult { rank: out.len() + 1, title, url, snippet });
    }
    out
}
