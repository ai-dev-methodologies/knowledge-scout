//! Pure text/URL/date helpers (port of normalize.mjs). Semantics follow JavaScript where it differs from Rust.
use regex::Regex;
use std::cmp::Ordering;
use std::sync::LazyLock;
use url::Url;

/// JavaScript `\s` / `String.prototype.trim` whitespace (differs from Rust: includes U+FEFF, excludes U+0085).
pub fn is_js_space(c: char) -> bool {
    matches!(c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}'
        | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

pub fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

/// JavaScript `String.length`: UTF-16 code units.
pub fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// JavaScript `<` / `>` string order: UTF-16 code units, not UTF-8 bytes.
pub fn js_cmp(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// `.replace(/[\s_]+/g, '-').replace(/-+/g, '-')`
pub(crate) fn hyphenate(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_dash = false;
    for c in s.chars() {
        if is_js_space(c) || c == '_' || c == '-' {
            if !last_dash {
                out.push('-');
                last_dash = true;
            }
        } else {
            out.push(c);
            last_dash = false;
        }
    }
    out
}

pub fn normalize_url(raw: &str) -> Option<String> {
    let s = js_trim(raw);
    if s.is_empty() {
        return None;
    }
    let u = Url::parse(s).ok()?;
    if u.scheme() != "http" && u.scheme() != "https" {
        return None;
    }
    let mut p = u.path().to_string();
    if p.len() > 1 && p.ends_with('/') {
        p = p.trim_end_matches('/').to_string();
        if p.is_empty() {
            p.push('/');
        }
    }
    let host = u.host_str()?;
    let hostport = match u.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    };
    Some(format!("{}://{}{}", u.scheme(), hostport, p))
}

pub fn norm_keyword(s: &str) -> String {
    hyphenate(&js_trim(s).to_lowercase())
}

// JS `\s` as an explicit class (Rust's `\s` is the Unicode White_Space set), ASCII-only `\d`, and `.` that excludes all JS line terminators.
const WS: &str = r"[\t\n\x0b\x0c\r \x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]";

static ISO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([0-9]{4})-([0-9]{2})-([0-9]{2})(?:[T ][^\n\r\x{2028}\x{2029}]*)?$").unwrap());
static MDY: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"^([A-Za-z]{{3,5}})\.?{WS}+([0-9]{{1,2}}),?{WS}+([0-9]{{4}})$")).unwrap());
static DMY: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"^([0-9]{{1,2}}){WS}+([A-Za-z]{{3,5}})\.?,?{WS}+([0-9]{{4}})$")).unwrap());

fn month(name: &str) -> Option<u32> {
    Some(match name.to_ascii_lowercase().as_str() {
        "jan" => 1, "feb" => 2, "mar" => 3, "apr" => 4, "may" => 5, "jun" => 6,
        "jul" => 7, "aug" => 8, "sep" | "sept" => 9, "oct" => 10, "nov" => 11, "dec" => 12,
        _ => return None,
    })
}

fn ymd(y: u32, m: u32, d: u32) -> Option<String> {
    // JS `Date.UTC` maps years 0..=99 to 1900..=1999, so the node kit's round-trip check rejects them.
    if y < 100 || !(1..=12).contains(&m) || d < 1 {
        return None;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = match m {
        2 => if leap { 29 } else { 28 },
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if d > dim {
        return None;
    }
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

fn num(s: &str) -> u32 {
    s.parse().unwrap()
}

pub fn iso_date(raw: &str) -> Option<String> {
    let s = js_trim(raw);
    if let Some(c) = ISO.captures(s) {
        return ymd(num(&c[1]), num(&c[2]), num(&c[3]));
    }
    if let Some(c) = MDY.captures(s) {
        if let Some(m) = month(&c[1]) {
            return ymd(num(&c[3]), m, num(&c[2]));
        }
    }
    if let Some(c) = DMY.captures(s) {
        if let Some(m) = month(&c[2]) {
            return ymd(num(&c[3]), m, num(&c[1]));
        }
    }
    None
}
