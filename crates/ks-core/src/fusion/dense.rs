//! The embedding columns of a feature row (r4-unified-dense.mjs denseRows / graphRows, the columns the shipped rankers
//! read): e_cos (query-card cosine), e_z (its z-score among the candidates), e_rr (1 / (9 + its rank)), e_nn / e_nn_z (the
//! fused first-pass level of the card's 5 nearest candidates by card-to-card cosine, and its z-score).
//! A candidate without a stored vector is neutral: the candidates' mean cosine, z 0, the rank of the mean, the mean
//! neighbour level. Statistics are taken over the candidates that have a vector.
use super::features::{Rows, E_COS, E_NN, E_NN_Z, E_RR, E_Z};

pub const NN: usize = 5;

/// Four running sums in f64 over f32 inputs, summed s0 + s1 + s2 + s3 (the JS order, so the bits agree). Vectors of
/// unequal length (refused upstream) are read only up to the shorter one.
pub fn dot(a: &[f32], b: &[f32]) -> f64 {
    let len = a.len().min(b.len());
    let (a, b) = (&a[..len], &b[..len]);
    let (mut s0, mut s1, mut s2, mut s3) = (0f64, 0f64, 0f64, 0f64);
    let mut i = 0;
    let n = a.len().saturating_sub(3);
    while i < n {
        s0 += a[i] as f64 * b[i] as f64;
        s1 += a[i + 1] as f64 * b[i + 1] as f64;
        s2 += a[i + 2] as f64 * b[i + 2] as f64;
        s3 += a[i + 3] as f64 * b[i + 3] as f64;
        i += 4;
    }
    while i < a.len() {
        s0 += a[i] as f64 * b[i] as f64;
        i += 1;
    }
    s0 + s1 + s2 + s3
}

fn clamp_z(z: f64) -> f64 {
    (-4f64).max(8f64.min(z))
}

/// e_cos, e_z, e_rr of every row. Returns false (columns left 0) when no candidate has a vector.
pub fn dense_rows(q: &[f32], vecs: &[Option<Vec<f32>>], rows: &mut Rows) -> bool {
    let m = vecs.len();
    let mut cos = vec![0f64; m];
    let mut have = vec![];
    for d in 0..m {
        if let Some(v) = &vecs[d] {
            cos[d] = dot(q, v);
            have.push(d);
        }
    }
    if have.is_empty() {
        return false;
    }
    let (mut mu, mut best) = (0f64, f64::NEG_INFINITY);
    for &d in &have {
        mu += cos[d];
        if cos[d] > best {
            best = cos[d];
        }
    }
    mu /= have.len() as f64;
    let mut v = 0f64;
    for &d in &have {
        let x = cos[d] - mu;
        v += x * x;
    }
    let sd = (v / have.len() as f64).sqrt();
    let mut order = have.clone();
    order.sort_by(|&a, &b| super::desc(cos[a], cos[b]).then(a.cmp(&b)));
    let sorted: Vec<f64> = order.iter().map(|&d| cos[d]).collect();
    let mut rank = vec![0usize; m];
    for (r, &d) in order.iter().enumerate() {
        rank[d] = r + 1;
    }
    let rank_of = |c: f64| sorted.iter().take_while(|&&s| s > c).count() + 1;
    for &d in &have {
        let z = if sd > 1e-9 { clamp_z((cos[d] - mu) / sd) } else { 0.0 };
        rows.set(d, E_COS, cos[d]);
        rows.set(d, E_Z, z);
        rows.set(d, E_RR, 1.0 / (9.0 + rank[d] as f64));
    }
    let mu_rank = rank_of(mu);
    for d in 0..m {
        if vecs[d].is_none() {
            rows.set(d, E_COS, mu);
            rows.set(d, E_Z, 0.0);
            rows.set(d, E_RR, 1.0 / (9.0 + mu_rank as f64));
        }
    }
    true
}

/// The NN nearest candidates of every row by vector cosine (the row itself left out): m * NN slots, -1 = none.
/// Depends on the stored vectors only, so it can be computed while the query embedding is still running.
pub fn neighbour_index(vecs: &[Option<Vec<f32>>]) -> Vec<i32> {
    let m = vecs.len();
    let mut bc = vec![f64::NEG_INFINITY; m * NN];
    let mut bi = vec![-1i32; m * NN];
    let mut offer = |d: usize, c: f64, e: usize| {
        let o = d * NN;
        let mut lo = o;
        for k in o + 1..o + NN {
            if bc[k] < bc[lo] {
                lo = k;
            }
        }
        if c > bc[lo] {
            bc[lo] = c;
            bi[lo] = e as i32;
        }
    };
    for d in 0..m {
        let Some(vd) = &vecs[d] else { continue };
        for e in d + 1..m {
            if let Some(ve) = &vecs[e] {
                let c = dot(vd, ve);
                offer(d, c, e);
                offer(e, c, d);
            }
        }
    }
    bi
}

fn z_of(xs: &[f64]) -> Vec<f64> {
    let n = xs.len();
    let mut mu = 0f64;
    for &x in xs {
        mu += x;
    }
    mu /= if n == 0 { 1.0 } else { n as f64 };
    let mut v = 0f64;
    for &x in xs {
        v += (x - mu) * (x - mu);
    }
    let sd = (v / if n == 0 { 1.0 } else { n as f64 }).sqrt();
    xs.iter().map(|&x| if sd > 1e-9 { (x - mu) / sd } else { 0.0 }).collect()
}

/// e_nn / e_nn_z of every row (graphRows with neighbours, neutral rows). Needs e_cos (dense_rows) and the first-pass scores.
pub fn neighbour_rows(vecs: &[Option<Vec<f32>>], rows: &mut Rows, nbr: &[i32]) {
    let m = vecs.len();
    if m == 0 {
        return;
    }
    // the first pass fused with the embedding: z of the lexical first-pass score + z of the cosine
    let fp: Vec<f64> = rows.fp.iter().map(|&x| x as f64).collect();
    let cs: Vec<f64> = (0..m).map(|d| rows.get(d, E_COS)).collect();
    let (a, b) = (z_of(&fp), z_of(&cs));
    let s: Vec<f64> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
    let mut nnv = vec![f32::NAN; m];
    for d in 0..m {
        let (mut sum, mut k) = (0f64, 0usize);
        for &j in &nbr[d * NN..d * NN + NN] {
            if j >= 0 {
                sum += s[j as usize];
                k += 1;
            }
        }
        if vecs[d].is_some() {
            nnv[d] = if k > 0 { (sum / k as f64) as f32 } else { 0.0 };
        }
    }
    let (mut mu, mut k) = (0f64, 0usize);
    for &x in &nnv {
        if !x.is_nan() {
            mu += x as f64;
            k += 1;
        }
    }
    mu /= if k == 0 { 1.0 } else { k as f64 };
    let mut v = 0f64;
    for &x in &nnv {
        if !x.is_nan() {
            v += (x as f64 - mu) * (x as f64 - mu);
        }
    }
    let sd = (v / if k == 0 { 1.0 } else { k as f64 }).sqrt();
    for d in 0..m {
        let x = if nnv[d].is_nan() { mu } else { nnv[d] as f64 };
        rows.set(d, E_NN, x);
        rows.set(d, E_NN_Z, if sd > 1e-9 { clamp_z((x - mu) / sd) } else { 0.0 });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fusion::features::W;

    fn rows(m: usize, fp: &[f32]) -> Rows {
        Rows { x: vec![0f32; m * W], fp: fp.to_vec(), m }
    }

    #[test]
    fn dot_uses_four_sums() {
        let a = [1.0f32, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(dot(&a, &a), 55.0);
        assert_eq!(dot(&[1.0, 2.0, 3.0], &[1.0, 2.0]), 5.0);
        assert_eq!(dot(&[1.0; 9], &[1.0; 5]), 5.0);
    }

    #[test]
    fn a_card_without_a_vector_is_neutral() {
        let q = vec![1.0f32, 0.0];
        let vecs = vec![Some(vec![1.0f32, 0.0]), None, Some(vec![0.0f32, 1.0]), Some(vec![0.6f32, 0.8])];
        let mut r = rows(4, &[3.0, 2.0, 1.0, 0.5]);
        assert!(dense_rows(&q, &vecs, &mut r));
        let mu = (1.0 + 0.0 + 0.6f32 as f64) / 3.0;
        assert_eq!(r.get(0, E_COS), 1.0);
        assert_eq!(r.get(1, E_COS), mu as f32 as f64);
        assert_eq!(r.get(1, E_Z), 0.0);
        // ranks: 1.0 (1), 0.6 (2), 0.0 (3); the mean 0.533 would rank 3rd
        assert_eq!(r.get(3, E_RR), (1.0f64 / 11.0) as f32 as f64);
        assert_eq!(r.get(1, E_RR), (1.0f64 / 12.0) as f32 as f64);
        assert!(r.get(0, E_Z) > 0.0 && r.get(2, E_Z) < 0.0);
        // no vector at all: nothing is written, the caller ranks lexically
        let mut r2 = rows(2, &[1.0, 1.0]);
        assert!(!dense_rows(&q, &[None, None], &mut r2));
        assert!(r2.x.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn neighbours_skip_cards_without_a_vector_and_keep_the_best_five() {
        let vecs: Vec<Option<Vec<f32>>> = (0..8).map(|i| if i == 3 { None } else { Some(vec![1.0f32, i as f32 / 10.0]) }).collect();
        let nbr = neighbour_index(&vecs);
        assert_eq!(nbr.len(), 8 * NN);
        assert!(nbr[3 * NN..4 * NN].iter().all(|&j| j == -1));
        assert!(!nbr.contains(&3));
        let mut r = rows(8, &[8.0, 7.0, 6.0, 5.0, 4.0, 3.0, 2.0, 1.0]);
        dense_rows(&[1.0, 0.0], &vecs, &mut r);
        neighbour_rows(&vecs, &mut r, &nbr);
        let others: Vec<f64> = (0..8).filter(|&d| d != 3).map(|d| r.get(d, E_NN)).collect();
        let mean = others.iter().sum::<f64>() / 7.0;
        assert!((r.get(3, E_NN) - mean).abs() < 1e-6);
        assert!(r.get(3, E_NN_Z).abs() < 1e-6);
    }
    #[test]
    fn a_nan_cosine_does_not_panic() {
        let vecs = vec![Some(vec![1.0f32, 0.0]), Some(vec![0.0f32, 1.0]), Some(vec![0.6f32, 0.8])];
        let mut r = rows(3, &[3.0, 2.0, 1.0]);
        dense_rows(&[f32::NAN, 0.0], &vecs, &mut r);
        neighbour_rows(&vecs, &mut r, &neighbour_index(&vecs));
    }
}
