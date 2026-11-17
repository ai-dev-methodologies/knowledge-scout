//! The last step of a search: feature rows, the rankers of the path, the order (r4-unified-rank.mjs rankUnified).
use super::dense::{dense_rows, neighbour_index, neighbour_rows};
use super::features::{add_model_feedback, feature_rows};
use super::model::Ranker;
use super::payload::{Payload, NF};
use super::plan::Plan;
use crate::normalize::js_cmp;
use crate::{Error, Result};

/// Row indices by score, best first; ties by card key (JS string order).
pub fn by_score(s: &[f32], keys: &[String]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..s.len()).collect();
    idx.sort_by(|&a, &b| super::desc(s[a] as f64, s[b] as f64).then_with(|| js_cmp(&keys[a], &keys[b])));
    idx
}

/// The order of the candidates under ranker `r` (at most `limit` row indices) and the final score of every row.
/// `q`: the query vector (required by an embedding ranker); `vecs`: the candidates' stored vectors.
pub fn rank_unified(r: &Ranker, avg: &[f64; NF], plan: &Plan, payloads: &[Payload], keys: &[String], limit: usize, q: Option<&[f32]>, vecs: &[Option<Vec<f32>>]) -> Result<(Vec<usize>, Vec<f32>)> {
    let mut rows = feature_rows(avg, plan, payloads);
    if r.uses_dense {
        let q = q.ok_or_else(|| Error::msg("fusion: a ranker with embedding columns needs the query vector"))?;
        if vecs.len() != payloads.len() {
            return Err(Error::msg("fusion: one stored vector (or none) per candidate is required"));
        }
        dense_rows(q, vecs, &mut rows);
        if r.uses_neighbours {
            neighbour_rows(vecs, &mut rows, &neighbour_index(vecs));
        }
    }
    if let Some(first) = &r.first {
        let s = first.score(&rows);
        add_model_feedback(avg, plan, payloads, &mut rows, &by_score(&s, keys), &s);
    }
    let s = r.fin.score(&rows);
    let mut order = by_score(&s, keys);
    order.truncate(limit);
    Ok((order, s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scores_with_nan_order_without_panicking_and_zero_signs_tie() {
        let keys: Vec<String> = ["c", "b", "a", "d"].iter().map(|s| s.to_string()).collect();
        let o = by_score(&[1.0, f32::NAN, 2.0, 1.0], &keys);
        assert_eq!(o.len(), 4);
        assert_eq!(o[0], 2);
        // -0 and +0 are equal (the JS difference is 0): the key decides
        assert_eq!(by_score(&[0.0, -0.0], &["b".to_string(), "a".to_string()]), vec![1, 0]);
    }
}
