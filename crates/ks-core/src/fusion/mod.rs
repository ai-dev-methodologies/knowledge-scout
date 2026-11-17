//! Fusion ranker (r4-unified of the search lab, ported): a Redis + CPU ranker with a switchable CPU embedding.
//!
//! * index   one Redis index under its own key prefix (default `ksf:`, never `ks:`): per card one HASH with eight TEXT
//!           fields of token codes, the vocabulary terms, one binary payload, the card id / key / title, and OPTIONALLY the
//!           card's bge-m3 vector (FLOAT16, not indexed); one token table (df + association row per token); one meta hash.
//! * query   `HMGET` token table, one weighted-union `FT.SEARCH` top 400 with payloads, one lexical feature row per
//!           candidate, linear rankers (path off / on x mode query / terms) with model-ordered feedback.
//! * switch  `auto | on | off`: the query is embedded by a local llama-server beside the Redis round trips; `auto` falls
//!           back to the lexical ranker for that query when the call fails or is late (700 ms), behind a circuit breaker.
//!
//! Every number is computed the way the JS reference computes it (f32 where it stores f32), so the Rust ranking equals
//! the JS ranking on the same index; the fixture check lives in the research repo (algos/ks-fusion-parity.mjs).
pub mod analyzer;
pub mod build;
pub mod dense;
pub mod embed;
pub mod features;
pub mod model;
pub mod payload;
pub mod plan;
pub mod rank;
pub mod redis;
pub mod switch;

use std::cmp::Ordering;

/// Descending order of two scores that is total (NaN never panics a sort; a NaN ranks last) and treats -0 and +0 as
/// equal, as the JS comparator `b - a` does; the callers add the reference's tie-break.
pub(crate) fn desc(x: f64, y: f64) -> Ordering {
    match (x.is_nan(), y.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        _ if x == y => Ordering::Equal,
        _ => y.total_cmp(&x),
    }
}
