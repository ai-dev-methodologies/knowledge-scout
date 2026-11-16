//! The model file (fusion-model-r4.json, written by the search lab's algos/r4-unified-export.mjs): linear rankers per path
//! (off: lexical, on: with the embedding columns) and mode (query, terms), the out-of-fold rankers of the lab's dev view,
//! the analyzer and switch constants, and analyzer probes. Loading checks the file against this engine: the feature order,
//! every analyzer constant, the token probes, and that no ranker reads a column this engine does not compute.
use super::analyzer::{fnv1a32, stop_words, tokenize};
use super::build::{ASSOC_HEAD, ASSOC_KEEP_K, ASSOC_KEEP_V, ASSOC_MIN, ASSOC_MU, EVIDENCE, FIELDS, STAGE1_CAP, STAGE1_VT, STAGE1_W};
use super::features::{computed_columns, Rows, B, FEATURES, FP_A, FP_V, FP_X, K1, LEXICAL, PRF_DOCS, PRF_TERMS, PRF_TOKENS, W, WF};
use super::payload::{HEAD_MASK, IDF_SCALE, PRF_MASK, SCORE_MAX, SHORT_MASK, THEAD_MASK};
use super::plan::{BRIDGE, EXPAND_K, EXPAND_V, STAGE1_X, VC, VQ, VX};
use crate::{Error, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub const RECIPE: &str = "r4u-v1";
pub const CANDIDATES: usize = 400;
pub const DIM: usize = 1024;

/// A linear model: score = sum of a[c] * (x[col c] - b[c]), a = w / std (0 where std is 0), b = mean; stored as f32.
#[derive(Debug, Clone)]
pub struct Linear {
    pub cols: Vec<usize>,
    pub a: Vec<f64>,
    pub b: Vec<f64>,
}

impl Linear {
    pub fn score(&self, rows: &Rows) -> Vec<f32> {
        (0..rows.m)
            .map(|d| {
                let o = d * W;
                let mut s = 0f64;
                for c in 0..self.cols.len() {
                    s += self.a[c] * (rows.x[o + self.cols[c]] as f64 - self.b[c]);
                }
                s as f32
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenseText {
    /// the query text
    Q,
    /// query + "\n" + client terms joined by ", "
    Qt,
}

/// One ranker of one (path, mode): optional first model (its order picks the feedback cards of the m_ columns), the final
/// model, and for the embedding path the text of the query vector.
#[derive(Debug, Clone)]
pub struct Ranker {
    pub first: Option<Linear>,
    pub fin: Linear,
    pub dense: Option<DenseText>,
    /// reads an embedding or graph column
    pub uses_dense: bool,
    /// reads e_nn / e_nn_z
    pub uses_neighbours: bool,
}

/// rankers[path][mode]: path 0 = off, 1 = on; mode 0 = query, 1 = terms.
#[derive(Debug, Clone)]
pub struct Rankers {
    pub off: [Ranker; 2],
    pub on: [Ranker; 2],
}

#[derive(Debug, Clone)]
pub struct SwitchConfig {
    pub timeout_ms: u64,
    pub cooldown_ms: u64,
    pub on_timeout_ms: u64,
    pub min_vector_share: f64,
}

#[derive(Debug)]
pub struct Model {
    pub id: String,
    pub sha256: String,
    pub shipped: Rankers,
    pub folds: Vec<Rankers>,
    /// sha256(query)[0:16] -> fold
    pub fold_of: HashMap<String, usize>,
    pub switch: SwitchConfig,
    pub text: String,
}

fn bad(what: &str) -> Error {
    Error::msg(format!("fusion model: {what}"))
}

fn linear(v: &Value, col: &HashMap<&str, usize>, computed: &std::collections::HashSet<&str>) -> Result<Linear> {
    if v.get("kind").and_then(Value::as_str) != Some("linear") {
        return Err(bad("only linear models are supported"));
    }
    let arr = |k: &str| -> Result<Vec<f64>> { v.get(k).and_then(Value::as_array).ok_or_else(|| bad(&format!("model without {k}")))?.iter().map(|x| x.as_f64().ok_or_else(|| bad(&format!("{k}: not a number")))).collect() };
    let names: Vec<&str> = v.get("names").and_then(Value::as_array).ok_or_else(|| bad("model without names"))?.iter().map(|x| x.as_str().ok_or_else(|| bad("names: not a string"))).collect::<Result<_>>()?;
    let (mean, std, w) = (arr("mean")?, arr("std")?, arr("w")?);
    if mean.len() != names.len() || std.len() != names.len() || w.len() != names.len() {
        return Err(bad("names, mean, std and w differ in length"));
    }
    let mut cols = vec![];
    for n in &names {
        let c = *col.get(n).ok_or_else(|| bad(&format!("unknown feature {n}")))?;
        if !computed.contains(n) {
            return Err(bad(&format!("a ranker reads {n}, which this engine does not compute")));
        }
        cols.push(c);
    }
    let a = (0..names.len()).map(|c| if std[c] > 0.0 { w[c] / std[c] } else { 0.0 }).collect();
    Ok(Linear { cols, a, b: mean })
}

fn ranker(v: &Value, col: &HashMap<&str, usize>, computed: &std::collections::HashSet<&str>) -> Result<Ranker> {
    let first = match v.get("first") {
        None | Some(Value::Null) => None,
        Some(f) => Some(linear(f, col, computed)?),
    };
    let fin = linear(v.get("final").ok_or_else(|| bad("ranker without final"))?, col, computed)?;
    let dense = match v.get("dense") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s == "q" => Some(DenseText::Q),
        Some(Value::String(s)) if s == "qt" => Some(DenseText::Qt),
        Some(x) => return Err(bad(&format!("unknown dense text {x}"))),
    };
    let used: Vec<usize> = first.iter().chain(std::iter::once(&fin)).flat_map(|m| m.cols.iter().copied()).collect();
    let uses_dense = used.iter().any(|&c| c >= LEXICAL);
    let uses_neighbours = used.iter().any(|&c| FEATURES[c] == "e_nn" || FEATURES[c] == "e_nn_z");
    if uses_dense != dense.is_some() {
        return Err(bad("a ranker reads embedding columns without a dense text, or the other way round"));
    }
    Ok(Ranker { first, fin, dense, uses_dense, uses_neighbours })
}

fn rankers(v: &Value, col: &HashMap<&str, usize>, computed: &std::collections::HashSet<&str>) -> Result<Rankers> {
    let r = |path: &str, mode: &str| ranker(v.get(path).and_then(|p| p.get(mode)).ok_or_else(|| bad(&format!("no ranker {path}/{mode}")))?, col, computed);
    let rs = Rankers { off: [r("off", "query")?, r("off", "terms")?], on: [r("on", "query")?, r("on", "terms")?] };
    if rs.off.iter().any(|x| x.uses_dense) || rs.on.iter().any(|x| !x.uses_dense) {
        return Err(bad("the off rankers must be lexical and the on rankers must read the embedding"));
    }
    Ok(rs)
}

/// The analyzer constants this engine is compiled with, in the model file's shape. stage1.vp / tp (the lab's term
/// prediction) and near (its card-to-card columns) belong to blocks this engine does not run; they are compared so that
/// the section matches as a whole, key for key.
pub fn compiled_analyzer() -> Value {
    json!({
        "fields": FIELDS, "evidence": EVIDENCE, "idf_scale": IDF_SCALE,
        "stage1": {"w": STAGE1_W, "cap": STAGE1_CAP, "vt": STAGE1_VT, "k1": 1.2, "b": 0.75, "bridge": BRIDGE, "vq": VQ, "vc": VC, "x": STAGE1_X, "vx": VX, "vp": 0.75, "tp": 0.25},
        "prf": {"docs": PRF_DOCS, "tokens": PRF_TOKENS, "terms": PRF_TERMS},
        "assoc": {"head": ASSOC_HEAD, "mu": ASSOC_MU, "min": ASSOC_MIN, "keepK": ASSOC_KEEP_K, "keepV": ASSOC_KEEP_V, "scoreMax": SCORE_MAX},
        "expand": {"k": EXPAND_K, "v": EXPAND_V}, "near": {"docs": 8, "smooth": 30}, "candidates": CANDIDATES, "k1": K1, "b": B, "w": WF,
        "fp": {"a": FP_A, "v": FP_V, "x": FP_X}, "stop": stop_words(),
        "masks": {"short_mask": SHORT_MASK, "head_mask": HEAD_MASK, "prf_mask": PRF_MASK, "thead_mask": THEAD_MASK},
    })
}

fn num_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| num_eq(p, q)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| num_eq(v, w))),
        _ => a == b,
    }
}

impl Model {
    pub fn load(path: &std::path::Path) -> Result<Model> {
        let text = std::fs::read_to_string(path).map_err(|e| Error::msg(format!("{}: cannot read the fusion model ({e})", path.display())))?;
        Model::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Model> {
        let v: Value = serde_json::from_str(text).map_err(|e| bad(&format!("invalid JSON ({e})")))?;
        if v.get("schema").and_then(Value::as_str) != Some("ks-fusion-model") || v.get("version").and_then(Value::as_u64) != Some(1) {
            return Err(bad("expected schema ks-fusion-model, version 1"));
        }
        if v.get("recipe").and_then(Value::as_str) != Some(RECIPE) {
            return Err(bad(&format!("recipe must be {RECIPE}")));
        }
        let feats: Vec<&str> = v.get("features").and_then(Value::as_array).ok_or_else(|| bad("no features"))?.iter().map(|x| x.as_str().ok_or_else(|| bad("features: not a string"))).collect::<Result<_>>()?;
        if feats != FEATURES || v.get("lexical_features").and_then(Value::as_u64) != Some(LEXICAL as u64) {
            return Err(bad("the feature order differs from this engine's"));
        }
        let analyzer = v.get("analyzer").and_then(Value::as_object).ok_or_else(|| bad("no analyzer section"))?;
        let mine = compiled_analyzer();
        let mine = mine.as_object().unwrap();
        if let Some(k) = analyzer.keys().find(|k| !mine.contains_key(*k)) {
            return Err(bad(&format!("analyzer.{k} is not a constant of this engine")));
        }
        for (k, want) in mine {
            let got = analyzer.get(k).ok_or_else(|| bad(&format!("analyzer.{k} is missing")))?;
            if !num_eq(got, want) {
                return Err(bad(&format!("analyzer.{k} differs from this engine's ({got} vs {want})")));
            }
        }
        let probes = v.get("probes").and_then(Value::as_array).ok_or_else(|| bad("no probes"))?;
        if probes.is_empty() {
            return Err(bad("the probe list is empty: the analyzer cannot be checked"));
        }
        // every probe: non-empty text and all three expected lists present and well-typed; at least one probe must yield
        // tokens, so that the check checks something
        let mut checked_tokens = false;
        for p in probes {
            let text = p.get("text").and_then(Value::as_str).filter(|t| !t.is_empty()).ok_or_else(|| bad("a probe without text"))?;
            let strs = |k: &str| -> Result<Vec<String>> {
                p.get(k).and_then(Value::as_array).ok_or_else(|| bad(&format!("probe {text:?} has no {k}")))?.iter().map(|x| x.as_str().map(str::to_string).ok_or_else(|| bad(&format!("probe {text:?}: {k} holds a non-string")))).collect()
            };
            let hashes: Vec<u64> = p.get("hashes").and_then(Value::as_array).ok_or_else(|| bad(&format!("probe {text:?} has no hashes")))?.iter().map(|x| x.as_u64().ok_or_else(|| bad(&format!("probe {text:?}: hashes holds a non-integer")))).collect::<Result<_>>()?;
            let toks = tokenize(text, false, false);
            if toks != strs("tokens")? || tokenize(text, true, true) != strs("tokens_tri_pair")? || toks.iter().map(|t| fnv1a32(t) as u64).collect::<Vec<_>>() != hashes {
                return Err(bad(&format!("the analyzer disagrees with the model file's probe {text:?}")));
            }
            checked_tokens |= !toks.is_empty();
        }
        if !checked_tokens {
            return Err(bad("no probe yields a token: the analyzer cannot be checked"));
        }
        let col: HashMap<&str, usize> = FEATURES.iter().enumerate().map(|(i, n)| (*n, i)).collect();
        let computed = computed_columns();
        let shipped = rankers(v.get("models").ok_or_else(|| bad("no models"))?, &col, &computed)?;
        let folds_v = v.get("folds").ok_or_else(|| bad("no folds"))?;
        let fm = folds_v.get("models").ok_or_else(|| bad("no fold models"))?;
        let k = fm.get("off").and_then(|o| o.get("query")).and_then(Value::as_array).map_or(0, Vec::len);
        let mut folds = vec![];
        for i in 0..k {
            let pick = |path: &str| -> Value { json!({"query": fm[path]["query"][i], "terms": fm[path]["terms"][i]}) };
            folds.push(rankers(&json!({"off": pick("off"), "on": pick("on")}), &col, &computed)?);
        }
        let mut fold_of = HashMap::new();
        for (key, f) in folds_v.get("of").and_then(Value::as_object).ok_or_else(|| bad("no fold map"))? {
            let f = f.as_u64().ok_or_else(|| bad("fold map: not a number"))? as usize;
            if f >= folds.len() {
                return Err(bad("fold map names a fold without models"));
            }
            fold_of.insert(key.clone(), f);
        }
        let sw = v.get("switch").ok_or_else(|| bad("no switch section"))?;
        let ms = |k: &str| sw.get(k).and_then(Value::as_u64).ok_or_else(|| bad(&format!("switch.{k} missing")));
        let switch = SwitchConfig {
            timeout_ms: ms("timeout_ms")?,
            cooldown_ms: ms("cooldown_ms")?,
            on_timeout_ms: ms("on_timeout_ms")?,
            min_vector_share: sw.get("min_vector_share").and_then(Value::as_f64).ok_or_else(|| bad("switch.min_vector_share missing"))?,
        };
        if v.get("vectors").and_then(|x| x.get("dim")).and_then(Value::as_u64) != Some(DIM as u64) {
            return Err(bad(&format!("vectors.dim must be {DIM}")));
        }
        Ok(Model {
            id: v.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
            sha256: hex::encode(Sha256::digest(text.as_bytes())),
            shipped,
            folds,
            fold_of,
            switch,
            text: text.to_string(),
        })
    }

    /// The rankers of a query: the shipped ones, or in the out-of-fold view (`cv`) the fold rankers trained without the
    /// query's group when the query is one of the lab's dev queries.
    pub fn rankers_for(&self, query: &str, cv: bool) -> &Rankers {
        if cv {
            let key = &hex::encode(Sha256::digest(query.as_bytes()))[..16];
            if let Some(&f) = self.fold_of.get(key) {
                return &self.folds[f];
            }
        }
        &self.shipped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_analyzer_has_the_reference_values() {
        let a = compiled_analyzer();
        assert_eq!(a["stage1"]["w"], json!([3, 3, 1, 2, 2, 1, 1, 1]));
        assert_eq!(a["stop"].as_array().unwrap().len(), 144);
        assert_eq!(a["masks"]["prf_mask"], json!(31));
    }

    #[test]
    fn linear_scores_are_float32() {
        let col: HashMap<&str, usize> = FEATURES.iter().enumerate().map(|(i, n)| (*n, i)).collect();
        let m = linear(&json!({"kind": "linear", "names": ["q_f", "q_cov"], "mean": [0.5, 0.0], "std": [0.25, 0.0], "w": [1.0, 9.0]}), &col, &computed_columns()).unwrap();
        let mut rows = Rows { x: vec![0f32; 2 * W], fp: vec![0.0; 2], m: 2 };
        rows.set(0, col["q_f"], 1.0);
        rows.set(1, col["q_cov"], 1.0);
        assert_eq!(m.score(&rows), vec![2.0, -2.0]);
        let err = linear(&json!({"kind": "linear", "names": ["c_cov"], "mean": [0.0], "std": [1.0], "w": [1.0]}), &col, &computed_columns()).unwrap_err();
        assert!(err.to_string().contains("does not compute"), "{err}");
    }
    /// A small model file this engine accepts (rankers on q_f / e_cos, no folds, one probe).
    fn minimal() -> Value {
        let lin = |names: &[&str]| json!({"kind": "linear", "names": names, "mean": vec![0.0; names.len()], "std": vec![1.0; names.len()], "w": vec![1.0; names.len()]});
        let t = "Redis 캐시 tests";
        let toks = tokenize(t, false, false);
        json!({
            "schema": "ks-fusion-model", "version": 1, "id": "test", "recipe": RECIPE, "features": FEATURES.to_vec(), "lexical_features": LEXICAL,
            "models": {"off": {"query": {"first": null, "final": lin(&["q_f"]), "dense": null}, "terms": {"final": lin(&["t_f"]), "dense": null}},
                       "on": {"query": {"final": lin(&["q_f", "e_cos"]), "dense": "q"}, "terms": {"final": lin(&["e_cos"]), "dense": "qt"}}},
            "folds": {"models": {"off": {"query": [], "terms": []}, "on": {"query": [], "terms": []}}, "of": {}},
            "analyzer": compiled_analyzer(),
            "switch": {"timeout_ms": 700, "cooldown_ms": 5000, "on_timeout_ms": 10000, "min_vector_share": 0.8},
            "vectors": {"dim": DIM},
            "probes": [{"text": t, "tokens": toks, "tokens_tri_pair": tokenize(t, true, true), "hashes": toks.iter().map(|x| fnv1a32(x)).collect::<Vec<_>>()}],
        })
    }

    #[test]
    fn the_model_file_must_match_exactly() {
        assert!(Model::parse(&minimal().to_string()).is_ok());
        let mut extra = minimal();
        extra["analyzer"]["something_new"] = json!(1);
        assert!(Model::parse(&extra.to_string()).unwrap_err().to_string().contains("something_new"));
        let mut missing = minimal();
        missing["analyzer"].as_object_mut().unwrap().remove("near");
        assert!(Model::parse(&missing.to_string()).is_err());
        let mut nested = minimal();
        nested["analyzer"]["stage1"]["extra"] = json!(0);
        assert!(Model::parse(&nested.to_string()).is_err());
        let mut feats = minimal();
        feats["features"][3] = json!(7);
        assert!(Model::parse(&feats.to_string()).is_err());
        let mut probes = minimal();
        probes["probes"] = json!([]);
        assert!(Model::parse(&probes.to_string()).unwrap_err().to_string().contains("probe"));
        // a probe with empty text, or without its expected tokens / hashes, checks nothing
        let mut empty = minimal();
        empty["probes"] = json!([{"text": "", "tokens": [], "tokens_tri_pair": [], "hashes": []}]);
        assert!(Model::parse(&empty.to_string()).unwrap_err().to_string().contains("probe"));
        for k in ["tokens", "tokens_tri_pair", "hashes"] {
            let mut m = minimal();
            m["probes"][0].as_object_mut().unwrap().remove(k);
            assert!(Model::parse(&m.to_string()).unwrap_err().to_string().contains("probe"), "{k}");
        }
        // probes whose text yields no token (only stop words and punctuation) are not enough
        let mut silent = minimal();
        silent["probes"] = json!([{"text": "the of ...", "tokens": [], "tokens_tri_pair": [], "hashes": []}]);
        assert!(Model::parse(&silent.to_string()).unwrap_err().to_string().contains("probe"));
    }
}
