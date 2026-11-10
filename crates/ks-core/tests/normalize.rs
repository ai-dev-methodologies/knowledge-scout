use ks_core::normalize::*;
use std::cmp::Ordering;

#[test]
fn normalize_url_drops_query_fragment_lowercases_host_strips_trailing_slash() {
    assert_eq!(normalize_url("  HTTPS://Medium.COM/@a/Post-1/?x=1#frag ").as_deref(), Some("https://medium.com/@a/Post-1"));
    assert_eq!(normalize_url("https://medium.com/").as_deref(), Some("https://medium.com/"));
    assert_eq!(normalize_url("https://medium.com").as_deref(), Some("https://medium.com/"));
    assert_eq!(normalize_url("https://medium.com/a/b?source=x").as_deref(), Some("https://medium.com/a/b"));
}

#[test]
fn normalize_url_keeps_non_default_port_and_collapses_trailing_slashes() {
    assert_eq!(normalize_url("http://Example.com:8080/a//").as_deref(), Some("http://example.com:8080/a"));
    assert_eq!(normalize_url("https://example.com:443/a/").as_deref(), Some("https://example.com/a"));
}

#[test]
fn normalize_url_none_for_empty_invalid_or_non_http() {
    assert_eq!(normalize_url(""), None);
    assert_eq!(normalize_url("   "), None);
    assert_eq!(normalize_url("not a url"), None);
    assert_eq!(normalize_url("ftp://example.com/x"), None);
    assert_eq!(normalize_url("mailto:a@b.c"), None);
}

#[test]
fn iso_date_cases() {
    assert_eq!(iso_date("2026-08-12").as_deref(), Some("2026-08-12"));
    assert_eq!(iso_date("Aug 12, 2026").as_deref(), Some("2026-08-12"));
    assert_eq!(iso_date("2026-08-12T10:11:12Z").as_deref(), Some("2026-08-12"));
    assert_eq!(iso_date("12/08/2026"), None);
    assert_eq!(iso_date("2026-13-45"), None);
    assert_eq!(iso_date("yesterday"), None);
    assert_eq!(iso_date(""), None);
}

#[test]
fn iso_date_month_name_forms_and_calendar_validity() {
    assert_eq!(iso_date("12 Sept. 2026").as_deref(), Some("2026-09-12"));
    assert_eq!(iso_date("sep 3 2026").as_deref(), Some("2026-09-03"));
    assert_eq!(iso_date("Feb 29, 2024").as_deref(), Some("2024-02-29"));
    assert_eq!(iso_date("Feb 29, 2026"), None);
    assert_eq!(iso_date("2026-02-30"), None);
    assert_eq!(iso_date("Foo 12, 2026"), None);
}

#[test]
fn iso_date_two_digit_years_are_rejected_like_js_date_utc() {
    // JS Date.UTC maps years 0..99 to 1900..1999, so the round-trip check fails and the node kit returns null.
    assert_eq!(iso_date("0050-01-01"), None);
    assert_eq!(iso_date("0000-01-01"), None);
    assert_eq!(iso_date("0100-01-01").as_deref(), Some("0100-01-01"));
}

#[test]
fn norm_keyword_cases() {
    assert_eq!(norm_keyword("  Agentic Coding "), "agentic-coding");
    assert_eq!(norm_keyword("retry__budget  x"), "retry-budget-x");
    assert_eq!(norm_keyword("a--b"), "a-b");
    assert_eq!(norm_keyword("-a- -"), "-a-");
}

#[test]
fn js_helpers() {
    assert_eq!(js_trim("\u{feff} x\u{a0}"), "x");
    assert_eq!(js_trim("\u{85}x"), "\u{85}x"); // NEL is not JS whitespace
    assert_eq!(js_len("a😀"), 3); // UTF-16 units
    // UTF-16 code-unit order: U+FF5E (BMP) sorts after U+1F600 (surrogate pair D83D..), unlike UTF-8 byte order.
    assert_eq!(js_cmp("\u{ff5e}", "\u{1f600}"), Ordering::Greater);
    assert_eq!(js_cmp("a", "b"), Ordering::Less);
    assert_eq!(js_cmp("a", "a"), Ordering::Equal);
}
