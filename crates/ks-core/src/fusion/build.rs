//! The fusion index in process (r4-unified-core.mjs buildIndex / cardDoc / assocRow): per card the typed tokens and the
//! payload, the df of every token, average field lengths, and the association rows of the token table.
use super::analyzer::{code, fnv1a32, is_set_token, opened, tokenize, Linker};
use super::payload::{encode_payload, js_round, pack_token_row, Doc, TokenRow, IDF_SCALE, NF};
use crate::card::Card;
use crate::normalize::js_len;
use crate::vocab::{Classified, Vocab};
use crate::Result;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

pub const FIELDS: [&str; NF] = ["title", "kt", "kw", "sum", "prob", "lim", "long", "al"];
pub const EVIDENCE: [&str; 7] = ["tutorial", "author_experience", "opinion", "official_doc", "mixed", "announcement", "measurement"];
/// First-stage field weights (RediSearch WEIGHT, integers >= 1), the vocabulary field's weight, per-field occurrence cap
/// (0 = none: the long summary counts as present / absent).
pub const STAGE1_W: [u32; NF] = [3, 3, 1, 2, 2, 1, 1, 1];
pub const STAGE1_CAP: [usize; NF] = [0, 0, 0, 0, 0, 0, 1, 0];
pub const STAGE1_VT: u32 = 3;
pub const ASSOC_HEAD: usize = 5;
pub const ASSOC_MU: f64 = 2.0;
pub const ASSOC_MIN: u32 = 2;
pub const ASSOC_KEEP_K: usize = 40;
pub const ASSOC_KEEP_V: usize = 12;

pub fn idf_of(n: usize, df: u32) -> f64 {
    (1.0 + (n as f64 - df as f64 + 0.5) / (df as f64 + 0.5)).ln()
}
pub fn idfq_of(n: usize, df: u32) -> u8 {
    js_round(idf_of(n, df) * IDF_SCALE).min(255.0) as u8
}

/// What the index reads of one card. `terms` = `vocab.classify_terms(tech_keywords + reusable element terms)` (what ks
/// indexes as the card's vocabulary terms); `long_summary` = the promoted long Korean summary, if any.
#[derive(Debug, Clone)]
pub struct CardInput {
    pub id: String,
    pub key: String,
    pub title: Option<String>,
    pub tech_keywords: Vec<String>,
    pub summary: Vec<String>,
    pub problems: Vec<String>,
    pub limits: String,
    pub evidence_type: Option<String>,
    pub terms: Classified,
    pub long_summary: Option<String>,
    /// private | team | public (stored with the card, returned with every hit; not used by the ranking)
    pub scope: String,
}

impl CardInput {
    pub fn from_card(c: &Card, vocab: &Vocab, long_summary: Option<String>) -> CardInput {
        let mut kw = c.tech_keywords.clone();
        kw.extend(c.reusable_elements.iter().map(|e| e.term.clone()));
        CardInput {
            id: c.id.clone(),
            key: c.card_key.clone(),
            title: c.title.clone(),
            tech_keywords: c.tech_keywords.clone(),
            summary: c.summary.clone(),
            problems: c.problems.clone(),
            limits: c.limits.clone(),
            evidence_type: c.evidence_type.clone(),
            terms: vocab.classify_terms(&kw),
            long_summary,
            scope: match c.scope {
                crate::card::Scope::Private => "private",
                crate::card::Scope::Team => "team",
                crate::card::Scope::Public => "public",
            }
            .to_string(),
        }
    }

    /// The text a card's vector is computed from (search-lab algos/dense.mjs docText, recipe card-v1).
    pub fn embed_text(&self) -> String {
        let mut parts: Vec<String> = vec![self.title.clone().unwrap_or_default()];
        parts.extend(self.summary.iter().cloned());
        parts.push(if self.problems.is_empty() { String::new() } else { format!("문제: {}", self.problems.join("; ")) });
        parts.push(if self.limits.is_empty() { String::new() } else { format!("한계: {}", self.limits) });
        parts.push(if self.tech_keywords.is_empty() { String::new() } else { format!("키워드: {}", self.tech_keywords.join(", ")) });
        parts.retain(|p| !p.is_empty());
        parts.join("\n")
    }
}

/// The eight field texts of a card (FIELDS order).
fn field_texts(c: &CardInput, l: &Linker) -> [String; NF] {
    let mut long = c.long_summary.clone().unwrap_or_default();
    let mut kt = String::new();
    if long.starts_with("# ") {
        match long.find('\n') {
            Some(i) => {
                kt = long[2..i].to_string();
                long = long[i + 1..].to_string();
            }
            None => {
                kt = long[2..].to_string();
                long = String::new();
            }
        }
    }
    let mine = c.terms.entities.iter().chain(&c.terms.techniques).chain(&c.terms.domains);
    // the term itself is in the keywords already: only its other surface forms
    let al: Vec<&str> = mine.flat_map(|t| l.texts.get(t).map(|s| s[1..].iter().map(String::as_str).collect::<Vec<_>>()).unwrap_or_default()).collect();
    [
        c.title.clone().unwrap_or_default(),
        kt,
        c.tech_keywords.iter().map(|k| opened(k)).collect::<Vec<_>>().join("\n"),
        c.summary.join("\n"),
        c.problems.join("\n"),
        c.limits.clone(),
        long,
        al.join("\n"),
    ]
}

pub fn card_doc(c: &CardInput, l: &Linker) -> Doc {
    let texts = field_texts(c, l);
    let mut vterms = vec![];
    for (facet, list) in [&c.terms.entities, &c.terms.techniques, &c.terms.domains, &c.terms.new_terms].into_iter().enumerate() {
        let mut seen = HashSet::new();
        for t in list {
            if seen.insert(t) {
                vterms.push((t.clone(), facet as u8));
            }
        }
    }
    let evidence = c.evidence_type.as_deref().and_then(|e| EVIDENCE.iter().position(|x| *x == e)).unwrap_or(7) as u8;
    let fields = texts.iter().enumerate().map(|(f, t)| tokenize(t, f < 6, f < 6)).collect();
    Doc { fields, vterms, evidence, long_len: js_len(c.long_summary.as_deref().unwrap_or("")) }
}

/// The text a first-stage field holds: its token codes, a capped field keeping each token at most `cap` times.
pub fn field_codes(toks: &[String], cap: usize) -> String {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    let mut out: Vec<String> = vec![];
    for t in toks {
        if is_set_token(t) {
            continue;
        }
        if cap > 0 {
            let c = seen.entry(t.as_str()).or_insert(0);
            if *c >= cap {
                continue;
            }
            *c += 1;
        }
        out.push(code(fnv1a32(t)));
    }
    out.join(" ")
}

pub fn vocab_codes(doc: &Doc) -> String {
    doc.vterms.iter().map(|(t, _)| code(fnv1a32(&format!("v:{t}")))).collect::<Vec<_>>().join(" ")
}

pub struct Assoc {
    latin: HashSet<u32>,
    src: HashMap<u32, Vec<u32>>,
    kdf: HashMap<u32, u32>,
    k_rows: Vec<Vec<u32>>,
    v_rows: Vec<Vec<u32>>,
}

pub struct BuiltIndex {
    pub n: usize,
    pub cards: Vec<CardInput>,
    pub docs: Vec<Doc>,
    pub payloads: Vec<Vec<u8>>,
    /// token hash -> number of cards carrying it (any field or as a vocabulary term), in first-seen order
    pub df: HashMap<u32, u32>,
    pub df_order: Vec<u32>,
    pub avg: [f64; NF],
    assoc: Assoc,
}

/// The cards ks search would search: of cards sharing an id the last one wins (index.rs rebuild), and only eligible
/// cards (`skip_reason` empty, ks's `@eligible:{1}`) are kept, in the order of the kept occurrences.
/// Returns (kept, skipped as not eligible, dropped as duplicates).
pub fn eligible_last_wins<T>(items: Vec<T>, id: impl Fn(&T) -> &str, eligible: impl Fn(&T) -> bool) -> (Vec<T>, usize, usize) {
    let mut last: HashMap<String, usize> = HashMap::new();
    for (i, x) in items.iter().enumerate() {
        last.insert(id(x).to_string(), i);
    }
    let (mut kept, mut skipped, mut dups) = (vec![], 0, 0);
    for (i, x) in items.into_iter().enumerate() {
        if last[id(&x)] != i {
            dups += 1;
        } else if !eligible(&x) {
            skipped += 1;
        } else {
            kept.push(x);
        }
    }
    (kept, skipped, dups)
}

pub fn build_index(cards: Vec<CardInput>, l: &Linker) -> Result<BuiltIndex> {
    let n = cards.len();
    let docs: Vec<Doc> = cards.iter().map(|c| card_doc(c, l)).collect();
    let mut df: HashMap<u32, u32> = HashMap::new();
    let mut df_order: Vec<u32> = vec![];
    let mut avg = [0f64; NF];
    let mut assoc = Assoc { latin: HashSet::new(), src: HashMap::new(), kdf: HashMap::new(), k_rows: vec![], v_rows: vec![] };
    for (d, doc) in docs.iter().enumerate() {
        let mut tfw: Vec<u32> = vec![]; // distinct hashes of the card (first-stage weights are Redis's business)
        let mut tfw_seen: HashSet<u32> = HashSet::new();
        let mut capped: HashMap<(usize, u32), usize> = HashMap::new();
        let (mut head, mut head_seen) = (vec![], HashSet::new());
        let (mut kt, mut kt_seen) = (vec![], HashSet::new());
        for (f, toks) in doc.fields.iter().enumerate() {
            for t in toks {
                if is_set_token(t) {
                    continue;
                }
                let h = fnv1a32(t);
                let lat = t.starts_with('w');
                avg[f] += 1.0;
                if f < ASSOC_HEAD && head_seen.insert(h) {
                    head.push(h);
                }
                if !lat && (f == 1 || f == 3 || f == 4) && kt_seen.insert(h) {
                    kt.push(h);
                }
                if lat {
                    assoc.latin.insert(h);
                }
                if STAGE1_CAP[f] > 0 {
                    let c = capped.entry((f, h)).or_insert(0);
                    if *c >= STAGE1_CAP[f] {
                        continue;
                    }
                    *c += 1;
                }
                if tfw_seen.insert(h) {
                    tfw.push(h);
                }
            }
        }
        for &h in &head {
            assoc.src.entry(h).or_default().push(d as u32);
        }
        for &h in &kt {
            *assoc.kdf.entry(h).or_insert(0) += 1;
        }
        assoc.k_rows.push(kt);
        assoc.v_rows.push(doc.vterms.iter().map(|(t, _)| fnv1a32(&format!("v:{t}"))).collect());
        for (t, _) in &doc.vterms {
            let h = fnv1a32(&format!("v:{t}"));
            if tfw_seen.insert(h) {
                tfw.push(h);
            }
        }
        for h in tfw {
            let e = df.entry(h).or_insert_with(|| {
                df_order.push(h);
                0
            });
            *e += 1;
        }
    }
    for a in avg.iter_mut() {
        *a /= if n == 0 { 1.0 } else { n as f64 };
        if *a == 0.0 || a.is_nan() {
            *a = 1.0;
        }
    }
    let payloads = docs
        .iter()
        .zip(&cards)
        .map(|(doc, c)| encode_payload(doc, |h| idfq_of(n, df.get(&h).copied().unwrap_or(0))).map_err(|e| crate::Error::msg(format!("card {}: {e}", c.id))))
        .collect::<Result<Vec<_>>>()?;
    Ok(BuiltIndex { n, cards, docs, payloads, df, df_order, avg, assoc })
}

impl BuiltIndex {
    /// The association row of one source token: the Korean bigrams (Latin sources only: a translation table) and the
    /// vocabulary terms over-represented on the cards whose head fields carry it. Score p * ln(p / q) as stored (u16).
    pub fn assoc_row(&self, h: u32) -> TokenRow {
        let a = &self.assoc;
        let mut row = TokenRow::default();
        let Some(docs) = a.src.get(&h) else { return row };
        let count = |rows: &Vec<Vec<u32>>| -> HashMap<u32, u32> {
            let mut c = HashMap::new();
            for &d in docs {
                for &t in &rows[d as usize] {
                    *c.entry(t).or_insert(0) += 1;
                }
            }
            c
        };
        let n = self.n as f64;
        let rank = |c: HashMap<u32, u32>, df_of: &dyn Fn(u32) -> u32, keep: usize| -> Vec<(u32, f64, f64)> {
            let mut out = vec![];
            for (t, k) in c {
                if k < ASSOC_MIN || t == h {
                    continue;
                }
                let pk = k as f64 / (docs.len() as f64 + ASSOC_MU);
                let pb = df_of(t) as f64 / n;
                if pk > pb {
                    let s = js_round(pk * (pk / pb).ln() / 8.0 * 65535.0).min(65535.0) / 65535.0 * 8.0;
                    out.push((t, s, idfq_of(self.n, self.df.get(&t).copied().unwrap_or(0)) as f64 / IDF_SCALE));
                }
            }
            out.sort_by(|x, y| super::desc(x.1, y.1).then(x.0.cmp(&y.0)));
            out.truncate(keep);
            out
        };
        if a.latin.contains(&h) {
            row.k = rank(count(&a.k_rows), &|t| a.kdf.get(&t).copied().unwrap_or(0), ASSOC_KEEP_K);
        }
        row.v = rank(count(&a.v_rows), &|t| self.df.get(&t).copied().unwrap_or(0), ASSOC_KEEP_V);
        row
    }

    /// The hash fields of card `i` as the first stage indexes them: (field name, token codes), empty fields left out.
    pub fn text_fields(&self, i: usize) -> Vec<(String, String)> {
        let doc = &self.docs[i];
        let mut out = vec![];
        for (f, toks) in doc.fields.iter().enumerate() {
            let s = field_codes(toks, STAGE1_CAP[f]);
            if !s.is_empty() {
                out.push((format!("f{f}"), s));
            }
        }
        if !doc.vterms.is_empty() {
            out.push(("v".to_string(), vocab_codes(doc)));
        }
        out
    }

    /// Digest of what is written (compared with the JS export's index digest): per card the sha256 of the payload and of
    /// the text fields, the sha256 of the token table (code \t row hex, sorted by code), the average field lengths.
    pub fn digest(&self) -> serde_json::Value {
        let cards: Vec<serde_json::Value> = (0..self.n)
            .map(|i| {
                let fields: String = self.text_fields(i).iter().map(|(k, v)| format!("{k}={v}\n")).collect();
                serde_json::json!({"id": self.cards[i].id, "payload_sha256": hex::encode(Sha256::digest(&self.payloads[i])), "fields_sha256": hex::encode(Sha256::digest(fields.as_bytes()))})
            })
            .collect();
        let mut rows: Vec<(String, String)> = self.df_order.iter().map(|&h| (code(h), hex::encode(pack_token_row(self.df[&h], &self.assoc_row(h))))).collect();
        rows.sort();
        let table: String = rows.iter().map(|(c, r)| format!("{c}\t{r}\n")).collect();
        serde_json::json!({"cards": self.n, "avg": self.avg.to_vec(), "tokens": rows.len(), "token_table_sha256": hex::encode(Sha256::digest(table.as_bytes())), "card_digests": cards})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_eligible_cards_are_kept_and_the_last_duplicate_wins() {
        let items = vec![("a", true, 1), ("b", true, 2), ("a", true, 3), ("c", false, 4), ("d", true, 5), ("c", true, 6), ("b", false, 7)];
        let (kept, skipped, dups) = eligible_last_wins(items, |x| x.0, |x| x.1);
        // last occurrences: a@3, d@5, c@6, b@7 (b's last is not eligible, so b is gone; ks counts it as a card, not eligible)
        assert_eq!(kept, vec![("a", true, 3), ("d", true, 5), ("c", true, 6)]);
        assert_eq!((skipped, dups), (1, 3));
    }
}
