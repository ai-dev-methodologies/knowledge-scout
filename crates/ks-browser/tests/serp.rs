//! SERP extraction on hand-written fixtures (no network).
use ks_browser::serp::{extract, search_url, SerpResult};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn r(rank: usize, title: &str, url: &str, snippet: &str) -> SerpResult {
    SerpResult { rank, title: title.into(), url: url.into(), snippet: snippet.into() }
}

#[test]
fn extracts_organic_results_in_order_without_ads_internal_links_or_duplicates() {
    let got = extract(&fixture("serp_basic.html"), 10);
    assert_eq!(
        got,
        vec![
            r(1, "EXPIRE | Docs", "https://redis.io/docs/latest/commands/expire/", "Set a timeout on key. After the timeout has expired, the key will automatically be deleted."),
            r(2, "TTL guide & tips", "https://example.org/ttl-guide?a=1", "Learn how TTL works."),
            r(3, "No snippet result", "https://blog.example.net/no-snippet", ""),
            r(4, "한국어 제목", "https://ko.example.kr/%EA%B8%80", "한국어 요약입니다."),
        ]
    );
}

#[test]
fn limit_truncates_and_ranks_stay_contiguous() {
    let got = extract(&fixture("serp_basic.html"), 2);
    assert_eq!(got.iter().map(|x| x.rank).collect::<Vec<_>>(), [1, 2]);
    assert_eq!(got[1].url, "https://example.org/ttl-guide?a=1");
}

#[test]
fn pages_without_a_results_container_yield_nothing() {
    assert!(extract(&fixture("captcha_sorry.html"), 10).is_empty());
    assert!(extract(&fixture("consent.html"), 10).is_empty());
    assert!(extract("", 10).is_empty());
}

fn urls_for_link(href: &str) -> Vec<String> {
    extract(&format!(r#"<div id="rso"><div class="g"><a href="{href}"><h3>T</h3></a></div></div>"#), 10).into_iter().map(|x| x.url).collect()
}

#[test]
fn only_google_com_hosts_count_as_google_links() {
    // Google's own links: /url is unwrapped, /search and /sorry are dropped
    assert_eq!(urls_for_link("https://www.google.com/url?q=https://x.example/"), ["https://x.example/"]);
    assert!(urls_for_link("https://google.com/search?q=x").is_empty());
    assert!(urls_for_link("https://www.google.com/sorry/index").is_empty());
    // look-alike hosts are ordinary results, kept as they are
    for href in [
        "https://www.google.evil.com/search?q=x",
        "https://www.google.evil.com/url?q=https://x.example/",
        "https://google.evil.com/search?q=x",
        "https://google.com.evil.com/url?q=https://x.example/",
        "https://www.google.co.kr/search?q=x",
    ] {
        assert_eq!(urls_for_link(href), [href], "{href}");
    }
}

#[test]
fn search_url_encodes_the_query() {
    assert_eq!(search_url("redis ttl & expiry/한글"), "https://www.google.com/search?q=redis+ttl+%26+expiry%2F%ED%95%9C%EA%B8%80");
}
