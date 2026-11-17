//! One feature row per candidate (r4-unified-core.mjs featureRows, feedbackOf, addModelFeedback), and the column names
//! (FEATURES_U of r4-unified-dense.mjs: the lexical columns, then the embedding and graph columns).
//! Values are stored as f32 exactly where the JS stores them in a Float32Array (every column, the first-pass score, the
//! feedback columns); everything in between is f64. Only the blocks the shipped rankers read are computed: the
//! prediction, concept, card-to-card and second feedback blocks stay 0 (`model.rs` refuses a ranker that reads them).
use super::payload::{Payload, AL, HEAD_MASK, IDF_SCALE, LONG, NF, PRF_MASK, SHORT_MASK, THEAD_MASK};
use super::plan::{own_of, Plan, Src};
use std::collections::{HashMap, HashSet};

pub const FEATURES: [&str; 142] = [
    "q_f", "q_f_rel", "q_title", "q_kt", "q_kw", "q_sum", "q_prob", "q_lim", "q_long", "q_al",
    "q_cov", "q_cov_short", "q_cov_head", "q_lat", "q_cov_lat", "q_cov_han", "q_tri", "q_pair", "t_f", "t_f_rel",
    "t_head", "t_sum", "t_prob", "t_long", "t_al", "t_cov", "a_f", "a_cov", "x_f", "x_cov",
    "xv_frac", "vc_frac", "vc_mass", "vu_frac", "vs_frac", "vs_mass", "vl_frac", "v_ent", "v_tech", "v_dom",
    "v_new", "v_cnt", "fp_rel", "fp_rr", "r_f", "r_v", "p_has_long", "p_long_len", "p_nterms", "p_nnew",
    "p_ev_tutorial", "p_ev_author_experience", "p_ev_opinion", "p_ev_official_doc", "p_ev_mixed", "p_ev_announcement", "p_ev_measurement", "q_en", "q_len", "q_nv",
    "en_q_f", "en_q_title", "en_q_long", "en_q_al", "en_a_f", "en_r_f", "en_vs_frac", "en_x_f", "en_xv_frac", "q_f_z",
    "q_long_z", "q_cov_z", "t_f_z", "a_f_z", "x_f_z", "r_f_z", "r_v_z", "fp_rel_z", "pe_frac", "ps_frac",
    "pf_frac", "ps_max", "pf_max", "pp_frac", "pp_mass", "pp_cnt", "pp_top", "pp_multi", "pp_ent", "pp_tech",
    "pp_dom", "pt_f", "pt_head", "pt_cov", "vsp_frac", "en_pp_frac", "en_ps_frac", "en_pt_f", "pp_frac_z", "ps_frac_z",
    "pf_frac_z", "pt_f_z", "r_f4", "r_v4", "r_f16", "r_v16", "r_head", "nn_v", "nn_h", "sm_v",
    "nn_v_z", "sm_v_z", "m_f", "m_v", "m_head", "m_rr", "m_s", "c_cov", "c_hit", "c_hit_s",
    "c_and", "c_miss", "c_and_z", "m_f4", "m_v4", "m_f16", "m_v16", "m_nn_v", "m_sm", "e_cos",
    "e_z", "e_rr", "e_rel", "e_prf", "en_e_cos", "en_e_z", "rd_f", "rd_v", "rd_f_z", "rd_v_z",
    "e_nn", "e_nn_z",
];
pub const Q_F: usize = 0;
pub const Q_F_REL: usize = 1;
pub const Q_TITLE: usize = 2;
pub const Q_KT: usize = 3;
pub const Q_KW: usize = 4;
pub const Q_SUM: usize = 5;
pub const Q_PROB: usize = 6;
pub const Q_LIM: usize = 7;
pub const Q_LONG: usize = 8;
pub const Q_AL: usize = 9;
pub const Q_COV: usize = 10;
pub const Q_COV_SHORT: usize = 11;
pub const Q_COV_HEAD: usize = 12;
pub const Q_LAT: usize = 13;
pub const Q_COV_LAT: usize = 14;
pub const Q_COV_HAN: usize = 15;
pub const Q_TRI: usize = 16;
pub const Q_PAIR: usize = 17;
pub const T_F: usize = 18;
pub const T_F_REL: usize = 19;
pub const T_HEAD: usize = 20;
pub const T_SUM: usize = 21;
pub const T_PROB: usize = 22;
pub const T_LONG: usize = 23;
pub const T_AL: usize = 24;
pub const T_COV: usize = 25;
pub const A_F: usize = 26;
pub const A_COV: usize = 27;
pub const X_F: usize = 28;
pub const X_COV: usize = 29;
pub const XV_FRAC: usize = 30;
pub const VC_FRAC: usize = 31;
pub const VC_MASS: usize = 32;
pub const VU_FRAC: usize = 33;
pub const VS_FRAC: usize = 34;
pub const VS_MASS: usize = 35;
pub const VL_FRAC: usize = 36;
pub const V_ENT: usize = 37;
pub const V_TECH: usize = 38;
pub const V_DOM: usize = 39;
pub const V_NEW: usize = 40;
pub const V_CNT: usize = 41;
pub const FP_REL: usize = 42;
pub const FP_RR: usize = 43;
pub const R_F: usize = 44;
pub const R_V: usize = 45;
pub const P_HAS_LONG: usize = 46;
pub const P_LONG_LEN: usize = 47;
pub const P_NTERMS: usize = 48;
pub const P_NNEW: usize = 49;
pub const P_EV_TUTORIAL: usize = 50;
pub const P_EV_AUTHOR_EXPERIENCE: usize = 51;
pub const P_EV_OPINION: usize = 52;
pub const P_EV_OFFICIAL_DOC: usize = 53;
pub const P_EV_MIXED: usize = 54;
pub const P_EV_ANNOUNCEMENT: usize = 55;
pub const P_EV_MEASUREMENT: usize = 56;
pub const Q_EN: usize = 57;
pub const Q_LEN: usize = 58;
pub const Q_NV: usize = 59;
pub const EN_Q_F: usize = 60;
pub const EN_Q_TITLE: usize = 61;
pub const EN_Q_LONG: usize = 62;
pub const EN_Q_AL: usize = 63;
pub const EN_A_F: usize = 64;
pub const EN_R_F: usize = 65;
pub const EN_VS_FRAC: usize = 66;
pub const EN_X_F: usize = 67;
pub const EN_XV_FRAC: usize = 68;
pub const Q_F_Z: usize = 69;
pub const Q_LONG_Z: usize = 70;
pub const Q_COV_Z: usize = 71;
pub const T_F_Z: usize = 72;
pub const A_F_Z: usize = 73;
pub const X_F_Z: usize = 74;
pub const R_F_Z: usize = 75;
pub const R_V_Z: usize = 76;
pub const FP_REL_Z: usize = 77;
pub const PE_FRAC: usize = 78;
pub const PS_FRAC: usize = 79;
pub const PF_FRAC: usize = 80;
pub const PS_MAX: usize = 81;
pub const PF_MAX: usize = 82;
pub const PP_FRAC: usize = 83;
pub const PP_MASS: usize = 84;
pub const PP_CNT: usize = 85;
pub const PP_TOP: usize = 86;
pub const PP_MULTI: usize = 87;
pub const PP_ENT: usize = 88;
pub const PP_TECH: usize = 89;
pub const PP_DOM: usize = 90;
pub const PT_F: usize = 91;
pub const PT_HEAD: usize = 92;
pub const PT_COV: usize = 93;
pub const VSP_FRAC: usize = 94;
pub const EN_PP_FRAC: usize = 95;
pub const EN_PS_FRAC: usize = 96;
pub const EN_PT_F: usize = 97;
pub const PP_FRAC_Z: usize = 98;
pub const PS_FRAC_Z: usize = 99;
pub const PF_FRAC_Z: usize = 100;
pub const PT_F_Z: usize = 101;
pub const R_F4: usize = 102;
pub const R_V4: usize = 103;
pub const R_F16: usize = 104;
pub const R_V16: usize = 105;
pub const R_HEAD: usize = 106;
pub const NN_V: usize = 107;
pub const NN_H: usize = 108;
pub const SM_V: usize = 109;
pub const NN_V_Z: usize = 110;
pub const SM_V_Z: usize = 111;
pub const M_F: usize = 112;
pub const M_V: usize = 113;
pub const M_HEAD: usize = 114;
pub const M_RR: usize = 115;
pub const M_S: usize = 116;
pub const C_COV: usize = 117;
pub const C_HIT: usize = 118;
pub const C_HIT_S: usize = 119;
pub const C_AND: usize = 120;
pub const C_MISS: usize = 121;
pub const C_AND_Z: usize = 122;
pub const M_F4: usize = 123;
pub const M_V4: usize = 124;
pub const M_F16: usize = 125;
pub const M_V16: usize = 126;
pub const M_NN_V: usize = 127;
pub const M_SM: usize = 128;
pub const E_COS: usize = 129;
pub const E_Z: usize = 130;
pub const E_RR: usize = 131;
pub const E_REL: usize = 132;
pub const E_PRF: usize = 133;
pub const EN_E_COS: usize = 134;
pub const EN_E_Z: usize = 135;
pub const RD_F: usize = 136;
pub const RD_V: usize = 137;
pub const RD_F_Z: usize = 138;
pub const RD_V_Z: usize = 139;
pub const E_NN: usize = 140;
pub const E_NN_Z: usize = 141;
/// Width of a feature row (lexical + embedding + graph columns).
pub const W: usize = FEATURES.len();
pub const LEXICAL: usize = 129;
pub const K1: f64 = 1.2;
pub const B: [f64; NF] = [0.3, 0.3, 0.3, 0.5, 0.5, 0.5, 0.75, 0.3];
pub const WF: [f64; NF] = [3.0, 3.0, 0.5, 2.0, 2.0, 0.5, 0.5, 1.0];
pub const FP_A: f64 = 0.3;
pub const FP_V: f64 = 1.5;
pub const FP_X: f64 = 1.0;
pub const PRF_DOCS: usize = 8;
pub const PRF_TOKENS: usize = 40;
pub const PRF_TERMS: usize = 8;

/// The columns this engine computes; a ranker that reads any other column is refused when the model file is loaded.
pub fn computed_columns() -> HashSet<&'static str> {
    let lexical = FEATURES[..LEXICAL].iter().copied().filter(|n| {
        !(n.starts_with("pe_") || n.starts_with("ps_") || n.starts_with("pf_") || n.starts_with("pp_") || n.starts_with("pt_") || n.starts_with("vsp_") || n.starts_with("en_p"))
            && !["r_f4", "r_v4", "r_f16", "r_v16", "r_head", "nn_v", "nn_h", "sm_v", "nn_v_z", "sm_v_z"].contains(n)
            && !n.starts_with("c_")
            && !["m_f4", "m_v4", "m_f16", "m_v16", "m_nn_v", "m_sm"].contains(n)
    });
    lexical.chain(["e_cos", "e_z", "e_rr", "e_nn", "e_nn_z"]).collect()
}

/// Feature rows of the candidates: X (m x W, row-major) and the first-pass score per row.
pub struct Rows {
    pub x: Vec<f32>,
    pub fp: Vec<f32>,
    pub m: usize,
}

impl Rows {
    pub fn get(&self, d: usize, c: usize) -> f64 {
        self.x[d * W + c] as f64
    }
    pub fn set(&mut self, d: usize, c: usize, v: f64) {
        self.x[d * W + c] = v as f32;
    }
}

fn norms(p: &Payload, avg: &[f64; NF]) -> [f64; NF] {
    let mut norm = [0f64; NF];
    for f in 0..NF {
        norm[f] = 1.0 / (1.0 - B[f] + B[f] * p.len[f] as f64 / avg[f]);
    }
    norm
}

fn merge_count(a: &[u32], b: &[u32]) -> usize {
    let (mut i, mut j, mut n) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        if a[i] < b[j] {
            i += 1;
        } else if a[i] > b[j] {
            j += 1;
        } else {
            n += 1;
            i += 1;
            j += 1;
        }
    }
    n
}

fn div(a: f64, b: f64) -> f64 {
    if b != 0.0 { a / b } else { 0.0 }
}

fn clamp_z(z: f64) -> f64 {
    (-4f64).max(8f64.min(z))
}

/// Pseudo-relevance feedback from the rows `top` (indices into p), each weighted by its share of their weight `w`.
/// Per row: f = BM25F of the feedback tokens, head = the same over title / Korean title / keywords, v = the share of the
/// feedback vocabulary terms the card carries.
pub struct Feedback {
    pub f: Vec<f32>,
    pub head: Vec<f32>,
    pub v: Vec<f32>,
}

struct Pick {
    h: u32,
    w: f64,
    idf: f64,
    g: f64,
}

fn pick(order: Vec<u32>, map: &HashMap<u32, (f64, f64)>, k: usize) -> Vec<Pick> {
    let mut all: Vec<Pick> = order.into_iter().map(|h| Pick { h, w: map[&h].0, idf: map[&h].1, g: 0.0 }).collect();
    all.sort_by(|a, b| super::desc(a.w * a.idf, b.w * b.idf).then(a.h.cmp(&b.h)));
    all.truncate(k);
    let mut mx = all.iter().fold(0f64, |a, e| a.max(e.w));
    if mx == 0.0 {
        mx = 1.0;
    }
    for e in all.iter_mut() {
        e.g = (e.w / mx) * e.idf;
    }
    all
}

pub fn feedback_of(p: &[Payload], w: &[f32], top: &[usize], own: &HashSet<u32>, avg: &[f64; NF]) -> Feedback {
    let m = p.len();
    let (mut f, mut head, mut v) = (vec![0f32; m], vec![0f32; m], vec![0f32; m]);
    let mass = top.iter().fold(0f64, |a, &d| a + w[d] as f64);
    let (mut rt, mut rv): (HashMap<u32, (f64, f64)>, HashMap<u32, (f64, f64)>) = (HashMap::new(), HashMap::new());
    let (mut rt_order, mut rv_order) = (vec![], vec![]);
    if mass > 0.0 {
        for &d in top {
            let q = &p[d];
            let share = w[d] as f64 / mass;
            for j in 0..q.hash.len() {
                if q.mask[j] & PRF_MASK != 0 && !own.contains(&q.hash[j]) {
                    match rt.get_mut(&q.hash[j]) {
                        Some(e) => e.0 += share,
                        None => {
                            rt.insert(q.hash[j], (share, q.idfq[j] as f64 / IDF_SCALE));
                            rt_order.push(q.hash[j]);
                        }
                    }
                }
            }
            for j in 0..q.vhash.len() {
                match rv.get_mut(&q.vhash[j]) {
                    Some(e) => e.0 += share,
                    None => {
                        rv.insert(q.vhash[j], (share, q.vidfq[j] as f64 / IDF_SCALE));
                        rv_order.push(q.vhash[j]);
                    }
                }
            }
        }
    }
    let mut rtok = pick(rt_order, &rt, PRF_TOKENS);
    rtok.sort_by_key(|e| e.h);
    let rvt = pick(rv_order, &rv, PRF_TERMS);
    let ir = rtok.iter().fold(0f64, |a, e| a + e.g);
    let irv = rvt.iter().fold(0f64, |a, e| a + e.g);
    for d in 0..m {
        let q = &p[d];
        let (mut fr, mut fh, mut frv) = (0f64, 0f64, 0f64);
        if ir > 0.0 {
            let norm = norms(q, avg);
            let (mut i, mut j) = (0, 0);
            while i < rtok.len() && j < q.hash.len() {
                let (a, b) = (rtok[i].h, q.hash[j]);
                if a < b {
                    i += 1;
                    continue;
                }
                if a > b {
                    j += 1;
                    continue;
                }
                let mask = q.mask[j];
                let mut k = q.off[j];
                let (mut tf_f, mut t_h) = (0f64, 0f64);
                for x in 0..NF {
                    if (mask >> x) & 1 != 0 {
                        let t = WF[x] * q.buf[k] as f64 * norm[x];
                        k += 1;
                        tf_f += t;
                        if x < 3 {
                            t_h += t;
                        }
                    }
                }
                fr += rtok[i].g * tf_f / (tf_f + K1);
                if t_h != 0.0 {
                    fh += rtok[i].g * t_h / (t_h + K1);
                }
                i += 1;
                j += 1;
            }
        }
        for e in &rvt {
            if q.vhash.contains(&e.h) {
                frv += e.g;
            }
        }
        f[d] = div(fr, ir) as f32;
        head[d] = div(fh, ir) as f32;
        v[d] = div(frv, irv) as f32;
    }
    Feedback { f, head, v }
}

const Z_COLS: [(usize, usize); 16] = [
    (Q_F, Q_F_Z),
    (Q_LONG, Q_LONG_Z),
    (Q_COV, Q_COV_Z),
    (T_F, T_F_Z),
    (A_F, A_F_Z),
    (X_F, X_F_Z),
    (R_F, R_F_Z),
    (R_V, R_V_Z),
    (FP_REL, FP_REL_Z),
    (PP_FRAC, PP_FRAC_Z),
    (PS_FRAC, PS_FRAC_Z),
    (PF_FRAC, PF_FRAC_Z),
    (PT_F, PT_F_Z),
    (NN_V, NN_V_Z),
    (SM_V, SM_V_Z),
    (C_AND, C_AND_Z),
];

/// z-score of column j inside the candidate set, clamped to [-4, 8], written to column jz.
pub(crate) fn z_column(rows: &mut Rows, j: usize, jz: usize) {
    let m = rows.m;
    let mut mu = 0f64;
    for d in 0..m {
        mu += rows.get(d, j);
    }
    mu /= if m == 0 { 1.0 } else { m as f64 };
    let mut v = 0f64;
    for d in 0..m {
        let x = rows.get(d, j) - mu;
        v += x * x;
    }
    let sd = (v / if m == 0 { 1.0 } else { m as f64 }).sqrt();
    for d in 0..m {
        let z = if sd > 1e-9 { clamp_z((rows.get(d, j) - mu) / sd) } else { 0.0 };
        rows.set(d, jz, z);
    }
}

/// The lexical feature rows of the candidates (featureRows of the JS, without extra rows).
pub fn feature_rows(avg: &[f64; NF], plan: &Plan, p: &[Payload]) -> Rows {
    let m = p.len();
    let mut rows = Rows { x: vec![0f32; m * W], fp: vec![0f32; m], m };
    let toks = &plan.toks;
    let sum = |key: fn(&super::plan::Tok) -> f64, keep: fn(&super::plan::Tok) -> bool| toks.iter().fold(0f64, |a, x| a + if keep(x) { key(x) * x.idf } else { 0.0 });
    let ix = sum(|x| x.wx, |_| true);
    let ixv = plan.vterms.iter().fold(0f64, |a, v| a + if v.src == Src::X { v.w * v.idf } else { 0.0 });
    let xs = plan.xs;
    let iq = sum(|x| x.wq, |_| true);
    let it = sum(|x| x.wt, |_| true);
    let ia = sum(|x| x.wa, |_| true);
    let iql = sum(|x| x.wq, |x| x.lat);
    let iqh = sum(|x| x.wq, |x| !x.lat);
    let den = |s: Src| plan.vterms.iter().fold(0f64, |a, v| a + if v.src == s { v.w * v.idf } else { 0.0 });
    let (den_c, den_u, den_s, den_l) = (den(Src::C), den(Src::U), den(Src::S), den(Src::L));
    let n_s = plan.vterms.iter().filter(|v| v.src == Src::S).count();
    let (mut raw_q, mut raw_t) = (vec![0f32; m], vec![0f32; m]);
    for d in 0..m {
        let q = &p[d];
        let norm = norms(q, avg);
        let mut qf = [0f64; NF];
        let mut x = [0f64; NF];
        let (mut fq, mut ft, mut fa, mut cq, mut cqs, mut cqh, mut fl, mut cl, mut ch) = (0f64, 0f64, 0f64, 0f64, 0f64, 0f64, 0f64, 0f64, 0f64);
        let (mut th, mut tl, mut ta, mut ct, mut ca, mut fx, mut cx, mut ts, mut tp) = (0f64, 0f64, 0f64, 0f64, 0f64, 0f64, 0f64, 0f64, 0f64);
        let (mut i, mut j) = (0, 0);
        while i < toks.len() && j < q.hash.len() {
            let (a, b) = (toks[i].h, q.hash[j]);
            if a < b {
                i += 1;
                continue;
            }
            if a > b {
                j += 1;
                continue;
            }
            let t = &toks[i];
            let mask = q.mask[j];
            let mut k = q.off[j];
            let (mut tf_f, mut t_head) = (0f64, 0f64);
            for f in 0..NF {
                if (mask >> f) & 1 != 0 {
                    x[f] = q.buf[k] as f64 * norm[f];
                    k += 1;
                    tf_f += WF[f] * x[f];
                    if f < 3 {
                        t_head += WF[f] * x[f];
                    }
                } else {
                    x[f] = 0.0;
                }
            }
            let sat = tf_f / (tf_f + K1);
            let g = t.idf;
            if t.wq != 0.0 {
                let gq = t.wq * g;
                fq += gq * sat;
                cq += gq;
                for f in 0..NF {
                    if x[f] != 0.0 {
                        qf[f] += gq * x[f] / (x[f] + K1);
                    }
                }
                if mask & SHORT_MASK != 0 {
                    cqs += gq;
                }
                if mask & HEAD_MASK != 0 {
                    cqh += gq;
                }
                if t.lat {
                    fl += gq * sat;
                    cl += gq;
                } else {
                    ch += gq;
                }
            }
            if t.wt != 0.0 {
                let gt = t.wt * g;
                ft += gt * sat;
                ct += gt;
                if mask & THEAD_MASK != 0 {
                    th += gt * t_head / (t_head + K1);
                }
                if x[3] != 0.0 {
                    ts += gt * x[3] / (x[3] + K1);
                }
                if x[4] != 0.0 {
                    tp += gt * x[4] / (x[4] + K1);
                }
                if x[LONG] != 0.0 {
                    tl += gt * x[LONG] / (x[LONG] + K1);
                }
                if x[AL] != 0.0 {
                    ta += gt * x[AL] / (x[AL] + K1);
                }
            }
            if t.wa != 0.0 {
                let ga = t.wa * g;
                fa += ga * sat;
                ca += ga;
            }
            if t.wx != 0.0 {
                let gx = t.wx * g;
                fx += gx * sat;
                cx += gx;
            }
            i += 1;
            j += 1;
        }
        let tri_hit = merge_count(&plan.tri, &q.tri);
        let pair_hit = merge_count(&plan.pair, &q.tri);
        // vocabulary terms: by source, and once per term (its best weight) by facet
        let (mut num_c, mut num_u, mut num_s, mut num_l) = (0f64, 0f64, 0f64, 0f64);
        let mut facet = [0f64; 4];
        let (mut vfp, mut vcnt, mut num_x) = (0f64, 0usize, 0f64);
        let mut seen: HashMap<u32, f64> = HashMap::new();
        for v in &plan.vterms {
            let Some(at) = q.vhash.iter().position(|&h| h == v.h) else { continue };
            if !(v.idf > 0.0) {
                continue;
            }
            if v.src == Src::X {
                num_x += v.w * v.idf;
                continue;
            }
            match v.src {
                Src::C => num_c += v.w * v.idf,
                Src::U => num_u += v.w * v.idf,
                Src::S => num_s += v.w * v.idf,
                _ => num_l += v.w * v.idf,
            }
            let prev = seen.get(&v.h).copied().unwrap_or(0.0);
            if v.w > prev {
                if let Some(slot) = facet.get_mut(q.vfacet[at] as usize) {
                    *slot += (v.w - prev) * v.idf;
                }
                vfp += (v.w - prev) * v.idf.sqrt();
                if prev == 0.0 {
                    vcnt += 1;
                }
                seen.insert(v.h, v.w);
            }
        }
        rows.set(d, Q_F, div(fq, iq));
        for f in 0..NF {
            rows.set(d, Q_TITLE + f, div(qf[f], iq));
        }
        rows.set(d, Q_COV, div(cq, iq));
        rows.set(d, Q_COV_SHORT, div(cqs, iq));
        rows.set(d, Q_COV_HEAD, div(cqh, iq));
        rows.set(d, Q_LAT, div(fl, iql));
        rows.set(d, Q_COV_LAT, div(cl, iql));
        rows.set(d, Q_COV_HAN, div(ch, iqh));
        rows.set(d, Q_TRI, if plan.tri.is_empty() { 0.0 } else { tri_hit as f64 / plan.tri.len() as f64 });
        rows.set(d, Q_PAIR, if plan.pair.is_empty() { 0.0 } else { pair_hit as f64 / plan.pair.len() as f64 });
        rows.set(d, X_F, div(fx, ix));
        rows.set(d, X_COV, div(cx, ix));
        rows.set(d, XV_FRAC, div(num_x, ixv));
        rows.set(d, T_SUM, div(ts, it));
        rows.set(d, T_PROB, div(tp, it));
        rows.set(d, T_F, div(ft, it));
        rows.set(d, T_HEAD, div(th, it));
        rows.set(d, T_LONG, div(tl, it));
        rows.set(d, T_AL, div(ta, it));
        rows.set(d, T_COV, div(ct, it));
        rows.set(d, A_F, div(fa, ia));
        rows.set(d, A_COV, div(ca, ia));
        rows.set(d, VC_FRAC, div(num_c, den_c));
        rows.set(d, VC_MASS, num_c / 10.0);
        rows.set(d, VU_FRAC, div(num_u, den_u));
        rows.set(d, VS_FRAC, div(num_s, den_s));
        rows.set(d, VS_MASS, num_s / 10.0);
        rows.set(d, VL_FRAC, div(num_l, den_l));
        rows.set(d, V_ENT, facet[0] / 10.0);
        rows.set(d, V_TECH, facet[1] / 10.0);
        rows.set(d, V_DOM, facet[2] / 10.0);
        rows.set(d, V_NEW, facet[3] / 10.0);
        rows.set(d, V_CNT, vcnt as f64 / 5.0);
        let nnew = q.vfacet.iter().filter(|&&f| f == 3).count();
        rows.set(d, P_HAS_LONG, if q.long_len > 0 { 1.0 } else { 0.0 });
        rows.set(d, P_LONG_LEN, (q.long_len as f64).ln_1p() / 10.0);
        rows.set(d, P_NTERMS, (q.vfacet.len() - nnew) as f64 / 10.0);
        rows.set(d, P_NNEW, nnew as f64 / 10.0);
        if (q.evidence as usize) < 7 {
            rows.set(d, P_EV_TUTORIAL + q.evidence as usize, 1.0);
        }
        rows.set(d, Q_EN, plan.en);
        rows.set(d, Q_LEN, (plan.q_len as f64 / 60.0).min(1.0));
        rows.set(d, Q_NV, (n_s as f64 / 5.0).min(1.0));
        raw_q[d] = fq as f32;
        raw_t[d] = ft as f32;
        rows.fp[d] = (fq + ft + FP_A * fa + FP_V * vfp + FP_X * xs * fx) as f32;
    }
    // relative to the best candidate, and the rank among the candidates
    let (mut max_q, mut max_t, mut max_fp) = (0f64, 0f64, 0f64);
    for d in 0..m {
        max_q = if (raw_q[d] as f64) > max_q { raw_q[d] as f64 } else { max_q };
        max_t = if (raw_t[d] as f64) > max_t { raw_t[d] as f64 } else { max_t };
        max_fp = if (rows.fp[d] as f64) > max_fp { rows.fp[d] as f64 } else { max_fp };
    }
    let mut order: Vec<usize> = (0..m).collect();
    order.sort_by(|&a, &b| super::desc(rows.fp[a] as f64, rows.fp[b] as f64).then(a.cmp(&b)));
    let mut rank = vec![0usize; m];
    for (r, &d) in order.iter().enumerate() {
        rank[d] = r + 1;
    }
    let own = own_of(plan);
    let fb = feedback_of(p, &rows.fp, &order[..order.len().min(PRF_DOCS)], &own, avg);
    let en = plan.en;
    for d in 0..m {
        rows.set(d, Q_F_REL, div(raw_q[d] as f64, max_q));
        rows.set(d, T_F_REL, div(raw_t[d] as f64, max_t));
        rows.set(d, FP_REL, div(rows.fp[d] as f64, max_fp));
        rows.set(d, FP_RR, 1.0 / (9.0 + rank[d] as f64));
        rows.set(d, R_F, fb.f[d] as f64);
        rows.set(d, R_V, fb.v[d] as f64);
        rows.set(d, R_HEAD, fb.head[d] as f64);
        rows.set(d, EN_Q_F, en * rows.get(d, Q_F));
        rows.set(d, EN_Q_TITLE, en * rows.get(d, Q_TITLE));
        rows.set(d, EN_Q_LONG, en * rows.get(d, Q_LONG));
        rows.set(d, EN_Q_AL, en * rows.get(d, Q_AL));
        rows.set(d, EN_A_F, en * rows.get(d, A_F));
        rows.set(d, EN_R_F, en * rows.get(d, R_F));
        rows.set(d, EN_VS_FRAC, en * rows.get(d, VS_FRAC));
        rows.set(d, EN_X_F, en * rows.get(d, X_F));
        rows.set(d, EN_XV_FRAC, en * rows.get(d, XV_FRAC));
    }
    for (j, jz) in Z_COLS {
        z_column(&mut rows, j, jz);
    }
    rows
}

/// Feedback from the order of a first model instead of the hand-set first pass (addModelFeedback): m_f / m_head / m_v
/// = feedback from the best 8 rows of `order` (weighted by their first-pass score), m_rr = 1 / (9 + rank in that order),
/// m_s = the first model's score as a z-score over the candidates.
pub fn add_model_feedback(avg: &[f64; NF], plan: &Plan, p: &[Payload], rows: &mut Rows, order: &[usize], scores: &[f32]) {
    let m = rows.m;
    let own = own_of(plan);
    let fb = feedback_of(p, &rows.fp, &order[..order.len().min(PRF_DOCS)], &own, avg);
    let mut mu = 0f64;
    for &s in scores {
        mu += s as f64;
    }
    mu /= if m == 0 { 1.0 } else { m as f64 };
    let mut v = 0f64;
    for &s in scores {
        let x = s as f64 - mu;
        v += x * x;
    }
    let sd = (v / if m == 0 { 1.0 } else { m as f64 }).sqrt();
    for d in 0..m {
        rows.set(d, M_F, fb.f[d] as f64);
        rows.set(d, M_HEAD, fb.head[d] as f64);
        rows.set(d, M_V, fb.v[d] as f64);
        rows.set(d, M_RR, 0.0);
        rows.set(d, M_S, if sd > 1e-9 { clamp_z((scores[d] as f64 - mu) / sd) } else { 0.0 });
    }
    for (r, &d) in order.iter().enumerate() {
        rows.set(d, M_RR, 1.0 / (9.0 + r as f64 + 1.0));
    }
}
