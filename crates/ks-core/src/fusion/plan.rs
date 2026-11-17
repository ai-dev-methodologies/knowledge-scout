//! A request as weighted tokens and vocabulary terms (r4-unified-core.mjs planQuery / fillIdf / expandPlan / stage1Query
//! and r4-unified-redis.mjs stage1Search). Token groups: q (query text), t (client terms as text), a (the surface forms of
//! every linked term: the bridge between Korean and English), x (the translation of Latin tokens into Korean bigrams).
//! Vocabulary terms by source: C client term resolved, U client term not in the vocabulary, L linked inside a client term,
//! S linked in the query, X voted for by the request's tokens.
use super::analyzer::{code, fnv1a32, is_hangul, opened, tokenize, Linker};
use super::build::idf_of;
use super::payload::TokenRow;
use crate::normalize::js_trim;
use crate::vocab::{normalize_term, Resolved, Vocab};
use std::collections::HashMap;

pub const BRIDGE: f64 = 0.3;
pub const STAGE1_X: f64 = 1.0;
pub const VQ: f64 = 1.0;
pub const VC: f64 = 1.5;
pub const VX: f64 = 0.3;
pub const EXPAND_K: usize = 60;
pub const EXPAND_V: usize = 6;
pub const MAX_TERMS: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Src {
    C,
    U,
    S,
    L,
    X,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tok {
    pub h: u32,
    pub wq: f64,
    pub wt: f64,
    pub wa: f64,
    pub wx: f64,
    pub lat: bool,
    pub idf: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VTerm {
    pub term: Option<String>,
    pub h: u32,
    pub src: Src,
    pub w: f64,
    pub idf: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// sorted by hash
    pub toks: Vec<Tok>,
    pub vterms: Vec<VTerm>,
    pub tri: Vec<u32>,
    pub pair: Vec<u32>,
    /// Latin share of the query's letters
    pub en: f64,
    /// query tokens
    pub q_len: usize,
    pub has_terms: bool,
    /// (Latin share of the request's own token weight)^2: how much the query needs the translation group
    pub xs: f64,
}

/// `terms?.some((t) => t.trim())`: terms mode needs at least one non-blank client term.
pub fn has_terms(terms: Option<&[String]>) -> bool {
    terms.is_some_and(|ts| ts.iter().any(|t| !js_trim(t).is_empty()))
}

fn resolve_key(key: &str, vocab: &Vocab) -> Option<Resolved> {
    let mut hit = vocab.lookup(key).cloned();
    if hit.is_none() && key.ends_with("ies") {
        hit = vocab.lookup(&format!("{}y", &key[..key.len() - 3])).cloned();
    }
    if hit.is_none() && key.ends_with("es") {
        hit = vocab.lookup(&key[..key.len() - 2]).cloned();
    }
    if hit.is_none() && key.ends_with('s') {
        hit = vocab.lookup(&key[..key.len() - 1]).cloned();
    }
    hit
}

#[derive(Default)]
struct Toks {
    list: Vec<Tok>,
    at: HashMap<String, usize>,
}

impl Toks {
    fn get(&mut self, t: &str) -> &mut Tok {
        let i = match self.at.get(t) {
            Some(&i) => i,
            None => {
                self.list.push(Tok { h: fnv1a32(t), wq: 0.0, wt: 0.0, wa: 0.0, wx: 0.0, lat: t.starts_with('w'), idf: 0.0 });
                self.at.insert(t.to_string(), self.list.len() - 1);
                self.list.len() - 1
            }
        };
        &mut self.list[i]
    }
}

/// The plan of a request (idf not filled yet: `fill_idf` with the df values read from the token table).
pub fn plan_query(l: &Linker, vocab: &Vocab, query: &str, terms: Option<&[String]>) -> Plan {
    let mut toks = Toks::default();
    let (mut tri, mut pair): (Vec<u32>, Vec<u32>) = (vec![], vec![]);
    let mut vt: Vec<VTerm> = vec![];
    let mut vt_at: HashMap<(String, u8), usize> = HashMap::new();
    let mut text = |toks: &mut Toks, s: &str, key: u8| {
        for t in tokenize(s, true, true) {
            if t.starts_with('t') {
                tri.push(fnv1a32(&t));
            } else if t.starts_with('p') {
                pair.push(fnv1a32(&t));
            } else {
                let x = toks.get(&t);
                if key == 0 { x.wq = 1.0 } else { x.wt = 1.0 }
            }
        }
    };
    let mut vterm = |vt: &mut Vec<VTerm>, term: &str, src: Src, w: f64| {
        let k = (term.to_string(), src as u8);
        match vt_at.get(&k) {
            None => {
                vt.push(VTerm { term: Some(term.to_string()), h: fnv1a32(&format!("v:{term}")), src, w, idf: 0.0 });
                vt_at.insert(k, vt.len() - 1);
            }
            Some(&i) => {
                if w > vt[i].w {
                    vt[i].w = w;
                }
            }
        }
    };
    text(&mut toks, query, 0);
    for m in l.link_text(query) {
        vterm(&mut vt, &m.term, Src::S, m.w);
    }
    let mut has = false;
    for raw in terms.unwrap_or(&[]) {
        if !js_trim(raw).is_empty() {
            has = true;
        }
        let norm = normalize_term(raw);
        if norm.is_empty() {
            continue;
        }
        match resolve_key(&norm, vocab) {
            Some(hit) => vterm(&mut vt, &hit.term, Src::C, 1.0),
            None => {
                vterm(&mut vt, &norm, Src::U, 1.0);
                for m in l.link_text(&opened(raw)) {
                    vterm(&mut vt, &m.term, Src::L, m.w);
                }
            }
        }
        text(&mut toks, &opened(raw), 1);
    }
    // bridge: the surface forms of every linked vocabulary term, at the best weight it was linked with
    let mut linked: Vec<(String, f64)> = vec![];
    for v in vt.iter().filter(|v| v.src != Src::U) {
        let term = v.term.as_deref().unwrap();
        match linked.iter_mut().find(|(t, _)| t == term) {
            Some(e) => {
                if v.w > e.1 {
                    e.1 = v.w;
                }
            }
            None => linked.push((term.to_string(), v.w)),
        }
    }
    for (term, w) in &linked {
        for s in l.texts.get(term).map(|v| v.as_slice()).unwrap_or(&[]) {
            for t in tokenize(s, false, false) {
                let x = toks.get(&t);
                if *w > x.wa {
                    x.wa = *w;
                }
            }
        }
    }
    let han = query.chars().filter(|&c| is_hangul(c)).count() as f64;
    let lat = query.chars().filter(|c| c.is_ascii_alphabetic()).count() as f64;
    tri.sort();
    tri.dedup();
    pair.sort();
    pair.dedup();
    let q_len = toks.list.iter().filter(|x| x.wq != 0.0).count();
    let mut list = toks.list;
    list.sort_by_key(|x| x.h);
    Plan { toks: list, vterms: vt, tri, pair, en: if han + lat > 0.0 { lat / (han + lat) } else { 0.0 }, q_len, has_terms: has, xs: 0.0 }
}

/// df_of(hash) -> document frequency (0 = in no card).
pub fn fill_idf(plan: &mut Plan, df_of: impl Fn(u32) -> u32, n: usize) {
    for x in plan.toks.iter_mut() {
        let df = df_of(x.h);
        x.idf = if df > 0 { idf_of(n, df) } else { 0.0 };
    }
    for v in plan.vterms.iter_mut() {
        let df = df_of(v.h);
        v.idf = if df > 0 { idf_of(n, df) } else { 0.0 };
    }
}

/// Expansion of a plan whose own tokens carry their idf: the Latin tokens vote for Korean bigrams (group x, the best 60),
/// every own token votes for vocabulary terms (source X, the best 6).
pub fn expand_plan(plan: &mut Plan, row_of: impl Fn(u32) -> Option<TokenRow>) {
    let mut acc_k: HashMap<u32, (f64, f64)> = HashMap::new();
    let mut acc_v: HashMap<u32, (f64, f64)> = HashMap::new();
    let (mut lat, mut han) = (0f64, 0f64);
    for x in &plan.toks {
        let w = x.wq.max(x.wt);
        if !(w > 0.0) {
            continue;
        }
        if x.lat { lat += w } else { han += w }
        let g = w * x.idf;
        if !(g > 0.0) {
            continue;
        }
        let Some(row) = row_of(x.h) else { continue };
        for &(t, sc, idf) in &row.k {
            acc_k.entry(t).and_modify(|e| e.0 += g * sc).or_insert((g * sc, idf));
        }
        for &(t, sc, idf) in &row.v {
            acc_v.entry(t).and_modify(|e| e.0 += g * sc).or_insert((g * sc, idf));
        }
    }
    plan.xs = if lat + han > 0.0 {
        let r = lat / (lat + han);
        r * r
    } else {
        0.0
    };
    let best = |acc: HashMap<u32, (f64, f64)>, k: usize| -> Vec<(u32, f64, f64)> {
        let mut r: Vec<(u32, (f64, f64))> = acc.into_iter().collect();
        r.sort_by(|x, y| super::desc(x.1 .0, y.1 .0).then(x.0.cmp(&y.0)));
        r.truncate(k);
        // the best vote; when it is 0 (every association score quantized to 0) the JS divides 0 by 0 and its NaN weights
        // drop the query's own tokens from the first stage. Here those weights are 0 (documented deviation).
        let mx = r.first().map_or(1.0, |e| e.1 .0);
        r.into_iter().map(|(h, (a, idf))| (h, if mx > 0.0 { a / mx } else { 0.0 }, idf)).collect()
    };
    let mut by: HashMap<u32, usize> = HashMap::new();
    for (i, x) in plan.toks.iter().enumerate() {
        by.insert(x.h, i);
    }
    for (h, w, idf) in best(acc_k, EXPAND_K) {
        let i = match by.get(&h) {
            Some(&i) => i,
            None => {
                plan.toks.push(Tok { h, wq: 0.0, wt: 0.0, wa: 0.0, wx: 0.0, lat: false, idf });
                by.insert(h, plan.toks.len() - 1);
                plan.toks.len() - 1
            }
        };
        plan.toks[i].wx = w;
    }
    for (h, w, idf) in best(acc_v, EXPAND_V) {
        plan.vterms.push(VTerm { term: None, h, src: Src::X, w, idf });
    }
    plan.toks.sort_by_key(|x| x.h);
}

/// The first-stage query: (hash, weight) over every token group and the vocabulary terms, in first-seen order.
pub fn stage1_query(plan: &Plan) -> Vec<(u32, f64)> {
    let mut out: Vec<(u32, f64)> = vec![];
    let mut at: HashMap<u32, usize> = HashMap::new();
    for x in &plan.toks {
        let w = x.wq.max(x.wt).max(BRIDGE * x.wa).max(STAGE1_X * plan.xs * x.wx).max(0.0);
        if w > 0.0 {
            match at.get(&x.h) {
                Some(&i) => out[i].1 = w,
                None => {
                    at.insert(x.h, out.len());
                    out.push((x.h, w));
                }
            }
        }
    }
    for v in &plan.vterms {
        let w = (match v.src {
            Src::X => VX,
            Src::S | Src::L => VQ,
            _ => VC,
        }) * v.w;
        match at.get(&v.h) {
            Some(&i) => {
                if w > out[i].1 {
                    out[i].1 = w;
                }
            }
            None => {
                at.insert(v.h, out.len());
                out.push((v.h, w));
            }
        }
    }
    out
}

/// `String(Number(w.toFixed(4)))`: JS decimal rounding (ties away from zero on the exact binary value), shortest form.
pub fn js_num4(w: f64) -> String {
    let exact = format!("{:.80}", w.abs());
    let (int, frac) = exact.split_once('.').unwrap();
    let (keep, rest) = frac.split_at(4);
    let mut digits: Vec<u8> = format!("{int}{keep}").into_bytes();
    // toFixed picks the larger n when two are equally close, so a 5 followed by anything rounds up
    let up = rest.as_bytes()[0] >= b'5';
    if up {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, b'1');
                break;
            }
            i -= 1;
            if digits[i] == b'9' {
                digits[i] = b'0';
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    let s = String::from_utf8(digits).unwrap();
    let (i, f) = s.split_at(s.len() - 4);
    let v: f64 = format!("{}{i}.{f}", if w < 0.0 { "-" } else { "" }).parse().unwrap();
    if v == 0.0 {
        return "0".to_string();
    }
    format!("{v}")
}

/// The FT.SEARCH query string of a first-stage query (heaviest terms first, at most MAX_TERMS), or None when nothing can match.
pub fn stage1_string(q: &[(u32, f64)]) -> Option<String> {
    let mut terms: Vec<(u32, f64)> = q.iter().copied().filter(|x| x.1 > 0.0).collect();
    terms.sort_by(|a, b| super::desc(a.1, b.1).then(a.0.cmp(&b.0)));
    terms.truncate(MAX_TERMS);
    if terms.is_empty() {
        return None;
    }
    Some(terms.iter().map(|(h, w)| format!("({})=>{{$weight:{}}}", code(*h), js_num4(*w))).collect::<Vec<_>>().join(" | "))
}

/// The tokens the request carries itself (query, client terms, bridge, translation): feedback leaves them out.
pub fn own_of(plan: &Plan) -> std::collections::HashSet<u32> {
    plan.toks.iter().filter(|x| x.wq != 0.0 || x.wt != 0.0 || x.wa != 0.0 || x.wx != 0.0).map(|x| x.h).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_format_like_js_to_fixed_4() {
        assert_eq!(js_num4(1.0), "1");
        assert_eq!(js_num4(1.5), "1.5");
        assert_eq!(js_num4(0.3), "0.3");
        assert_eq!(js_num4(0.15), "0.15");
        assert_eq!(js_num4(0.188_649_3), "0.1886");
        assert_eq!(js_num4(0.000_049), "0");
        assert_eq!(js_num4(0.000_05), "0.0001"); // 0.00005 is stored slightly above 5e-5
        assert_eq!(js_num4(0.031_25), "0.0313"); // an exact tie: toFixed takes the larger
        assert_eq!(js_num4(9.999_96), "10");
    }

    #[test]
    fn stage1_string_orders_by_weight_then_hash() {
        assert_eq!(stage1_string(&[(36, 0.3), (35, 1.0), (1, 1.0), (2, 0.0)]).unwrap(), "(z1)=>{$weight:1} | (zz)=>{$weight:1} | (z10)=>{$weight:0.3}");
        assert_eq!(stage1_string(&[(2, 0.0)]), None);
    }

    #[test]
    fn zero_association_scores_give_zero_weights_not_nan() {
        // the JS divides by the best vote (0 here) and gets NaN, which then drops the query's own token from the first
        // stage; this engine gives weight 0 instead and keeps the own token's clause
        let mut plan = Plan { toks: vec![Tok { h: 10, wq: 1.0, wt: 0.0, wa: 0.0, wx: 0.0, lat: true, idf: 2.0 }], vterms: vec![], tri: vec![], pair: vec![], en: 1.0, q_len: 1, has_terms: false, xs: 0.0 };
        expand_plan(&mut plan, |h| if h == 10 { Some(TokenRow { k: vec![(10, 0.0, 1.5), (20, 0.0, 1.2)], v: vec![(30, 0.0, 2.0)] }) } else { None });
        assert!(plan.toks.iter().all(|t| t.wx == 0.0), "{:?}", plan.toks);
        assert!(plan.vterms.iter().all(|v| v.w == 0.0));
        assert_eq!(stage1_query(&plan), vec![(10, 1.0), (30, 0.0)]);
        assert_eq!(stage1_string(&stage1_query(&plan)).unwrap(), "(za)=>{$weight:1}");
    }
}
