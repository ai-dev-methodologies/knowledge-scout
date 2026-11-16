//! Analyzer of the fusion ranker: typed tokens (Hangul character bigrams `k:`, folded Latin words `w:`, trigrams `t:`,
//! word pairs `p:`), their 32-bit FNV-1a hashes, and the alias linker. JS semantics where they differ from Rust: string
//! lengths and slices are UTF-16 code units, the clause split is the JS regex's (r4-unified-core.mjs).
use crate::collect::NON_WORD;
use crate::normalize::{is_js_space, js_cmp, js_len};
use crate::vocab::Vocab;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

pub const STOP_WORDS: &str = "a an the of to in on for and or nor but is are was were be been being am with by at as it its this that these those from how what why when where which who whom whose \
do does did doing done i we you he she they me us them my our your his her their can could should would will shall may might must not no yes vs via into onto about so if then than too very just \
also only even still yet such some any each every all both more most much many few less other another own same there here up down out off over under again once \
have has had having get gets got want wants need needs like without within while during before after between through because until against among per etc ie eg";

static STOP: LazyLock<HashSet<&'static str>> = LazyLock::new(|| STOP_WORDS.split_whitespace().collect());

pub fn stop_words() -> Vec<&'static str> {
    STOP_WORDS.split_whitespace().collect()
}

/// A precomposed Hangul syllable (U+AC00..U+D7A3), the `[가-힣]` of the JS.
pub fn is_hangul(c: char) -> bool {
    ('\u{ac00}'..='\u{d7a3}').contains(&c)
}

/// `run.match(/[가-힣]+|[^가-힣]+/g)`: maximal runs of Hangul syllables and of everything else.
pub(crate) fn segments(run: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut start = 0;
    let mut cur: Option<bool> = None;
    for (i, c) in run.char_indices() {
        let h = is_hangul(c);
        if cur.is_some_and(|x| x != h) {
            out.push(&run[start..i]);
            start = i;
        }
        cur = Some(h);
    }
    if start < run.len() {
        out.push(&run[start..]);
    }
    out
}

fn seg_is_hangul(s: &str) -> bool {
    s.chars().next().is_some_and(is_hangul)
}

/// Plural folding of a Latin word (r4-unified-core.mjs foldPlural; lengths in UTF-16 units).
pub fn fold_plural(s: &str) -> String {
    if js_len(s) <= 3 || s.bytes().any(|b| b.is_ascii_digit()) {
        return s.to_string();
    }
    if js_len(s) > 4 && s.ends_with("ies") {
        return format!("{}y", &s[..s.len() - 3]);
    }
    if s.ends_with("sses") {
        return s[..s.len() - 2].to_string();
    }
    if ["ches", "shes", "xes", "zes"].iter().any(|e| s.ends_with(e)) {
        return s[..s.len() - 2].to_string();
    }
    if s.ends_with('s') && !["ss", "us", "is"].iter().any(|e| s.ends_with(e)) {
        return s[..s.len() - 1].to_string();
    }
    s.to_string()
}

fn is_clause_char(c: char) -> bool {
    matches!(c, ',' | ':' | ';' | '!' | '?' | '\n' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '"' | '\u{201c}' | '\u{201d}' | '\u{2018}' | '\u{2019}' | '\u{b7}' | '\u{2026}')
}

/// `text.split(/[,:;!?\n()[\]{}|"“”‘’·…]+|\.(?=\s|$)/u)` (empty clauses dropped: the tokenizer skips them anyway).
fn clauses(text: &str) -> Vec<&str> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = vec![];
    let mut start = 0;
    for (k, &(i, c)) in chars.iter().enumerate() {
        let sep = is_clause_char(c) || (c == '.' && chars.get(k + 1).is_none_or(|&(_, n)| is_js_space(n)));
        if sep {
            if start < i {
                out.push(&text[start..i]);
            }
            start = i + c.len_utf8();
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// Typed tokens of a text, in order (per segment: its bigrams, then its trigrams when `tri`). `pair`: also one `p:` token
/// per two neighbouring units of a clause (a unit = a Latin word, or the first two syllables of a Hangul word).
pub fn tokenize(text: &str, tri: bool, pair: bool) -> Vec<String> {
    let lower = text.to_lowercase();
    let parts: Vec<&str> = if pair { clauses(&lower) } else { vec![lower.as_str()] };
    let mut out = vec![];
    for clause in parts {
        if clause.is_empty() {
            continue;
        }
        let mut prev: Option<String> = None;
        for run in NON_WORD.split(clause) {
            if run.is_empty() {
                continue;
            }
            let segs = segments(run);
            for s in &segs {
                let mut unit: Option<String> = None;
                if seg_is_hangul(s) {
                    let cs: Vec<char> = s.chars().collect();
                    if cs.len() == 1 {
                        if segs.len() == 1 {
                            out.push(format!("k:{s}"));
                        }
                        continue;
                    }
                    for w in cs.windows(2) {
                        out.push(format!("k:{}{}", w[0], w[1]));
                    }
                    if tri {
                        for w in cs.windows(3) {
                            out.push(format!("t:{}{}{}", w[0], w[1], w[2]));
                        }
                    }
                    unit = Some(cs[..2].iter().collect());
                } else if js_len(s) >= 2 && !STOP.contains(s) {
                    let u = fold_plural(s);
                    out.push(format!("w:{u}"));
                    unit = Some(u);
                }
                if pair {
                    if let Some(u) = unit {
                        if let Some(p) = &prev {
                            out.push(format!("p:{p}_{u}"));
                        }
                        prev = Some(u);
                    }
                }
            }
        }
    }
    out
}

/// `t:` trigram or `p:` pair token: they go to the payload's set table, not to the token table or the text fields.
pub fn is_set_token(t: &str) -> bool {
    t.starts_with("t:") || t.starts_with("p:")
}

/// 32-bit FNV-1a over the UTF-8 bytes of a typed token.
pub fn fnv1a32(s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in s.bytes() {
        h = (h ^ b as u32).wrapping_mul(0x01000193);
    }
    h
}

/// Token code as stored in the TEXT fields and the token table: "z" + the hash in base 36 (RediSearch never sees Korean text).
pub fn code(h: u32) -> String {
    let mut digits = vec![];
    let mut x = h;
    loop {
        digits.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[(x % 36) as usize]);
        x /= 36;
        if x == 0 {
            break;
        }
    }
    digits.reverse();
    format!("z{}", String::from_utf8(digits).unwrap())
}

/// `term.replace(/[-_]+/g, ' ')`
pub fn opened(term: &str) -> String {
    let mut out = String::with_capacity(term.len());
    let mut last = false;
    for c in term.chars() {
        if c == '-' || c == '_' {
            if !last {
                out.push(' ');
            }
            last = true;
        } else {
            out.push(c);
            last = false;
        }
    }
    out
}

fn alias_key(s: &str) -> Vec<u16> {
    NON_WORD.replace_all(&s.to_lowercase(), "").encode_utf16().collect()
}

pub const FACET_ENTITY: u8 = 0;
pub const FACET_TECHNIQUE: u8 = 1;
pub const FACET_DOMAIN: u8 = 2;
pub const FACET_NEW: u8 = 3;

/// The alias linker: vocabulary terms, their aliases and the Korean / English candidate aliases of the alias dictionary
/// (`{terms: {term: {ko: [...], en: [...]}}}`), matched in a text across spacing and hyphens.
pub struct Linker {
    map: HashMap<Vec<u16>, Vec<String>>,
    facet: HashMap<String, u8>,
    /// term -> every surface form (the opened term first)
    pub texts: HashMap<String, Vec<String>>,
    prefixes: HashMap<[u16; 2], Vec<usize>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub term: String,
    pub facet: u8,
    pub w: f64,
    pub key: String,
}

struct Match {
    p: usize,
    e: usize,
    terms: Vec<String>,
    key: Vec<u16>,
    inside: bool,
}

impl Linker {
    pub fn new(vocab: &Vocab, aliases: Option<&Value>) -> Linker {
        let mut l = Linker { map: HashMap::new(), facet: HashMap::new(), texts: HashMap::new(), prefixes: HashMap::new() };
        for e in &vocab.terms {
            let extra = aliases.and_then(|a| a.get("terms")).and_then(|t| t.get(&e.term));
            let list = |k: &str| -> Vec<String> { extra.and_then(|x| x.get(k)).and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default() };
            let mut surfaces = vec![opened(&e.term)];
            surfaces.extend(e.aliases.iter().map(|a| opened(a)));
            surfaces.extend(list("ko"));
            surfaces.extend(list("en"));
            l.facet.insert(e.term.clone(), e.facet as u8);
            for s in &surfaces {
                l.put(s, &e.term);
            }
            l.texts.insert(e.term.clone(), surfaces);
        }
        for lens in l.prefixes.values_mut() {
            lens.sort_by(|a, b| b.cmp(a));
        }
        l
    }

    fn put(&mut self, surface: &str, term: &str) {
        let key = alias_key(surface);
        if key.len() < 2 {
            return;
        }
        let pre = [key[0], key[1]];
        let len = key.len();
        let list = self.map.entry(key).or_default();
        if !list.iter().any(|t| t == term) {
            list.push(term.to_string());
        }
        let lens = self.prefixes.entry(pre).or_default();
        if !lens.contains(&len) {
            lens.push(len);
        }
    }

    pub fn facet_of(&self, term: &str) -> Option<u8> {
        self.facet.get(term).copied()
    }

    /// Every match of a text. A match starts at the start of a word (inside a Hangul word only for keys of 4+ characters)
    /// and ends at the end of a word, before a Hangul ending, or before a Latin plural s / es.
    fn matches(&self, text: &str) -> Vec<Match> {
        let mut s: Vec<u16> = vec![];
        let mut seg_of: Vec<usize> = vec![];
        let mut segs: Vec<(usize, usize, bool)> = vec![];
        let lower = text.to_lowercase();
        for run in NON_WORD.split(&lower) {
            if run.is_empty() {
                continue;
            }
            for sg in segments(run) {
                let u: Vec<u16> = sg.encode_utf16().collect();
                segs.push((s.len(), s.len() + u.len(), seg_is_hangul(sg)));
                seg_of.extend(std::iter::repeat_n(segs.len() - 1, u.len()));
                s.extend(u);
            }
        }
        let mut found: Vec<Match> = vec![];
        let mut p = 0;
        while p + 2 <= s.len() {
            if let Some(lens) = self.prefixes.get(&[s[p], s[p + 1]]) {
                let (s0, _, h0) = segs[seg_of[p]];
                let at_start = p == s0;
                if at_start || h0 {
                    for &len in lens {
                        if len > s.len() - p {
                            continue;
                        }
                        if !at_start && len < 4 {
                            break;
                        }
                        let key = &s[p..p + len];
                        let Some(terms) = self.map.get(key) else { continue };
                        let e = p + len;
                        let (_, e1, h1) = segs[seg_of[e - 1]];
                        if e != e1 && !h1 {
                            let rest = &s[e..e1];
                            if rest != [b's' as u16] && rest != [b'e' as u16, b's' as u16] {
                                continue;
                            }
                        }
                        found.push(Match { p, e, terms: terms.clone(), key: key.to_vec(), inside: false });
                    }
                }
            }
            p += 1;
        }
        // ordered by start, then longest first: a match can only lie inside one that starts no later
        for i in 0..found.len() {
            let (mp, me) = (found[i].p, found[i].e);
            let mut inside = false;
            for j in 0..found.len() {
                if found[j].p > mp {
                    break;
                }
                if j != i && found[j].e >= me && found[j].e - found[j].p > me - mp {
                    inside = true;
                    break;
                }
            }
            found[i].inside = inside;
        }
        found
    }

    /// Vocabulary terms named in a text, best first: w = 1 for a match no longer match contains, 0.5 for one inside a
    /// longer match, both shared between the terms of an ambiguous alias.
    pub fn link_text(&self, text: &str) -> Vec<Link> {
        let mut order: Vec<String> = vec![];
        let mut best: HashMap<String, (f64, Vec<u16>)> = HashMap::new();
        for m in self.matches(text) {
            let w = (if m.inside { 0.5 } else { 1.0 }) / m.terms.len() as f64;
            for t in &m.terms {
                let cur = best.get(t).map_or(0.0, |x| x.0);
                if w > cur {
                    if !best.contains_key(t) {
                        order.push(t.clone());
                    }
                    best.insert(t.clone(), (w, m.key.clone()));
                }
            }
        }
        let mut out: Vec<Link> = order
            .into_iter()
            .map(|t| {
                let (w, key) = best.remove(&t).unwrap();
                Link { facet: self.facet.get(&t).copied().unwrap_or(FACET_NEW), term: t, w, key: String::from_utf16_lossy(&key) }
            })
            .collect();
        out.sort_by(|a, b| super::desc(a.w, b.w).then_with(|| js_cmp(&a.term, &b.term)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    // Expected values: r4-unified-core.mjs tokenize / fnv1a32 on the same strings (fusion-model-r4.json probes).
    #[test]
    fn tokens_match_js_on_mixed_text() {
        let t = "LLM의 분산 추적을 OpenTelemetry로 구현하기";
        assert_eq!(tokenize(t, false, false), s(&["w:llm", "k:분산", "k:추적", "k:적을", "w:opentelemetry", "k:구현", "k:현하", "k:하기"]));
        assert_eq!(
            tokenize(t, true, true),
            s(&["w:llm", "k:분산", "p:llm_분산", "k:추적", "k:적을", "t:추적을", "p:분산_추적", "w:opentelemetry", "p:추적_opentelemetry", "k:구현", "k:현하", "k:하기", "t:구현하", "t:현하기", "p:opentelemetry_구현"])
        );
    }

    #[test]
    fn tokens_fold_plurals_drop_stop_words_and_split_clauses() {
        let t = "Kubernetes pods crashes: retries, queues; and the caches.";
        assert_eq!(tokenize(t, false, false), s(&["w:kubernete", "w:pod", "w:crash", "w:retry", "w:queue", "w:cach"]));
        assert_eq!(tokenize(t, true, true), s(&["w:kubernete", "w:pod", "p:kubernete_pod", "w:crash", "p:pod_crash", "w:retry", "w:queue", "w:cach"]));
        let t = "A/B testing vs. canary releases... why? \"feature-flags\" · rollout…";
        assert_eq!(tokenize(t, true, true), s(&["w:testing", "w:canary", "w:release", "p:canary_release", "w:feature", "w:flag", "p:feature_flag", "w:rollout"]));
        let t = "p99 latency 2x 감소 gpt-4o vs claude-3.5 benchmarks";
        assert_eq!(
            tokenize(t, true, true),
            s(&["w:p99", "w:latency", "p:p99_latency", "w:2x", "p:latency_2x", "k:감소", "p:2x_감소", "w:gpt", "p:감소_gpt", "w:4o", "p:gpt_4o", "w:claude", "p:4o_claude", "w:benchmark", "p:claude_benchmark"])
        );
    }

    #[test]
    fn hashes_match_js() {
        let toks = tokenize("LLM의 분산 추적을 OpenTelemetry로 구현하기", false, false);
        let h: Vec<u32> = toks.iter().map(|t| fnv1a32(t)).collect();
        assert_eq!(h, vec![257964131, 2698586343, 1306752817, 2528489056, 1209004771, 2843051858, 3160780839, 1997260744]);
    }
    fn linker(vocab: &str, aliases: &str) -> Linker {
        let v = crate::vocab::Vocab::from_json(vocab, "t.json").unwrap();
        let a: serde_json::Value = serde_json::from_str(aliases).unwrap();
        Linker::new(&v, Some(&a))
    }
    fn links(l: &Linker, t: &str) -> Vec<(String, u8, f64, String)> {
        l.link_text(t).into_iter().map(|x| (x.term, x.facet, x.w, x.key)).collect()
    }
    fn lk(t: &str, f: u8, w: f64, k: &str) -> (String, u8, f64, String) {
        (t.into(), f, w, k.into())
    }

    // Expected values: r4-unified-core.mjs linkText (the probes of fusion-model-r4.json, reduced to the terms involved).
    #[test]
    fn links_shared_hangul_alias_and_glued_particle() {
        let l = linker(
            r#"{"version":1,"terms":[{"term":"llm","facet":"domain","aliases":[]},{"term":"opentelemetry","facet":"entity","aliases":[]},{"term":"distributed-tracing","facet":"technique","aliases":[]},{"term":"tracing","facet":"technique","aliases":[]}]}"#,
            r#"{"terms":{"distributed-tracing":{"ko":["분산 추적"],"en":[]},"tracing":{"ko":["분산 추적"]}}}"#,
        );
        assert_eq!(
            links(&l, "LLM의 분산 추적을 OpenTelemetry로 구현하기"),
            vec![lk("llm", 2, 1.0, "llm"), lk("opentelemetry", 0, 1.0, "opentelemetry"), lk("distributed-tracing", 1, 0.5, "분산추적"), lk("tracing", 1, 0.5, "분산추적")]
        );
    }

    #[test]
    fn links_plural_endings_and_inside_matches() {
        let l = linker(
            r#"{"version":1,"terms":[{"term":"caching","facet":"domain","aliases":["cache"]},{"term":"retry","facet":"technique","aliases":["retries"]},{"term":"mcp","facet":"entity","aliases":[]},{"term":"mcp-server","facet":"technique","aliases":["mcp-servers"]}]}"#,
            r#"{"terms":{}}"#,
        );
        assert_eq!(links(&l, "pods crashes: retries, and the caches."), vec![lk("caching", 2, 1.0, "cache"), lk("retry", 1, 1.0, "retries")]);
        assert_eq!(links(&l, "MCP servers and tools"), vec![lk("mcp-server", 1, 1.0, "mcpservers"), lk("mcp", 0, 0.5, "mcp")]);
        // a short key never matches inside a word; "cachex" does not end in s / es
        assert_eq!(links(&l, "nomcp cachex"), vec![]);
    }

    #[test]
    fn token_codes_are_base36() {
        assert_eq!(code(0), "z0");
        assert_eq!(code(35), "zz");
        assert_eq!(code(36), "z10");
        assert_eq!(code(u32::MAX), "z1z141z3");
    }
}
