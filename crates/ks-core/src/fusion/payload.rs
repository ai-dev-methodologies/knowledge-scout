//! What the fusion index stores per card and per token, byte for byte as r4-unified-core.mjs / r4-unified-redis.mjs write it.
//!
//! Card payload (little-endian): version u8 (1), evidence u8, nVocab u8, 0, longLen u16, 8 x field length u16, nTok u16,
//! nTri u16; nTok x (hash u32, idfq u8, field mask u8, one tf u8 per set bit) sorted by hash; nTri x hash u32 sorted (the
//! trigram and pair tokens); nVocab x (hash u32, idfq u8, facet u8).
//! Token row: df u32, nK u8, nV u8, 0, 0; (nK + nV) x (hash u32, score u16 = score / 8 * 65535, idf u8 = idf * 24).
//! Card vector: 1,024 IEEE half floats, little-endian (the hash field `e`).
use super::analyzer::{fnv1a32, is_set_token};
use crate::{Error, Result};
use std::collections::{HashMap, HashSet};

pub const NF: usize = 8;
pub const LONG: usize = 6;
pub const AL: usize = 7;
pub const SHORT_MASK: u8 = 0b0011_1111;
pub const HEAD_MASK: u8 = 0b0000_0011;
pub const PRF_MASK: u8 = 0b0001_1111;
pub const THEAD_MASK: u8 = 0b0000_0111;
pub const IDF_SCALE: f64 = 24.0;
pub const SCORE_MAX: f64 = 8.0;
const HEADER: usize = 26;

/// One card as the engine indexes it: typed tokens per field (FIELDS order), vocabulary terms with their facet (0..3),
/// evidence index (7 = none of EVIDENCE), characters (UTF-16 units) of the long summary.
#[derive(Debug, Clone, PartialEq)]
pub struct Doc {
    pub fields: Vec<Vec<String>>,
    pub vterms: Vec<(String, u8)>,
    pub evidence: u8,
    pub long_len: usize,
}

/// Errors when the token or set table has more than 65,535 entries (the count is a u16; the JS writer throws too).
pub fn encode_payload(doc: &Doc, idfq: impl Fn(u32) -> u8) -> Result<Vec<u8>> {
    let mut tf: HashMap<u32, [u8; NF]> = HashMap::new();
    let mut tri: HashSet<u32> = HashSet::new();
    let mut len = [0usize; NF];
    for (f, toks) in doc.fields.iter().enumerate() {
        for t in toks {
            let h = fnv1a32(t);
            if is_set_token(t) {
                tri.insert(h);
                continue;
            }
            len[f] += 1;
            let row = tf.entry(h).or_insert([0; NF]);
            if row[f] < 255 {
                row[f] += 1;
            }
        }
    }
    let mut toks: Vec<(u32, [u8; NF])> = tf.into_iter().collect();
    toks.sort_by_key(|x| x.0);
    let mut tris: Vec<u32> = tri.into_iter().collect();
    tris.sort();
    if toks.len() > 65535 || tris.len() > 65535 {
        return Err(Error::msg(format!("fusion: a card has {} distinct tokens and {} trigram / pair tokens; the payload holds at most 65535 of each", toks.len(), tris.len())));
    }
    // the JS keeps the first 255 vocabulary terms of a card (one-byte count); so does this writer
    let vts = &doc.vterms[..doc.vterms.len().min(255)];
    let mut buf = Vec::with_capacity(HEADER + toks.len() * 8 + tris.len() * 4 + vts.len() * 6);
    buf.extend([1u8, doc.evidence, vts.len() as u8, 0]);
    buf.extend((doc.long_len.min(65535) as u16).to_le_bytes());
    for l in len {
        buf.extend((l.min(65535) as u16).to_le_bytes());
    }
    buf.extend((toks.len() as u16).to_le_bytes());
    buf.extend((tris.len() as u16).to_le_bytes());
    for (h, row) in &toks {
        buf.extend(h.to_le_bytes());
        buf.push(idfq(*h));
        let mut mask = 0u8;
        for (f, &c) in row.iter().enumerate() {
            if c > 0 {
                mask |= 1 << f;
            }
        }
        buf.push(mask);
        buf.extend(row.iter().copied().filter(|&c| c > 0));
    }
    for h in &tris {
        buf.extend(h.to_le_bytes());
    }
    for (term, facet) in vts {
        let h = fnv1a32(&format!("v:{term}"));
        buf.extend(h.to_le_bytes());
        buf.push(idfq(h));
        buf.push(*facet);
    }
    Ok(buf)
}

/// A decoded payload. `off[i]` is the byte offset of token i's first tf byte in `buf`.
#[derive(Debug, Clone)]
pub struct Payload {
    pub buf: Vec<u8>,
    pub hash: Vec<u32>,
    pub idfq: Vec<u8>,
    pub mask: Vec<u8>,
    pub off: Vec<usize>,
    pub len: [u16; NF],
    pub tri: Vec<u32>,
    pub vhash: Vec<u32>,
    pub vidfq: Vec<u8>,
    pub vfacet: Vec<u8>,
    pub evidence: u8,
    pub long_len: u16,
}

fn u16_at(b: &[u8], o: usize) -> Result<u16> {
    b.get(o..o + 2).map(|x| u16::from_le_bytes([x[0], x[1]])).ok_or_else(|| Error::msg("fusion: truncated payload"))
}
fn u32_at(b: &[u8], o: usize) -> Result<u32> {
    b.get(o..o + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]])).ok_or_else(|| Error::msg("fusion: truncated payload"))
}
fn u8_at(b: &[u8], o: usize) -> Result<u8> {
    b.get(o).copied().ok_or_else(|| Error::msg("fusion: truncated payload"))
}

pub fn decode_payload(buf: Vec<u8>) -> Result<Payload> {
    if buf.first() != Some(&1) {
        return Err(Error::msg(format!("fusion: unknown payload version {:?}", buf.first())));
    }
    let n_tok = u16_at(&buf, 22)? as usize;
    let n_tri = u16_at(&buf, 24)? as usize;
    let n_v = u8_at(&buf, 2)? as usize;
    let mut len = [0u16; NF];
    for (f, l) in len.iter_mut().enumerate() {
        *l = u16_at(&buf, 6 + 2 * f)?;
    }
    let (mut hash, mut idfq, mut mask, mut off) = (Vec::with_capacity(n_tok), Vec::with_capacity(n_tok), Vec::with_capacity(n_tok), Vec::with_capacity(n_tok));
    let mut o = HEADER;
    for _ in 0..n_tok {
        hash.push(u32_at(&buf, o)?);
        idfq.push(u8_at(&buf, o + 4)?);
        let m = u8_at(&buf, o + 5)?;
        mask.push(m);
        off.push(o + 6);
        o += 6 + m.count_ones() as usize;
    }
    let mut tri = Vec::with_capacity(n_tri);
    for _ in 0..n_tri {
        tri.push(u32_at(&buf, o)?);
        o += 4;
    }
    let (mut vhash, mut vidfq, mut vfacet) = (Vec::with_capacity(n_v), Vec::with_capacity(n_v), Vec::with_capacity(n_v));
    for _ in 0..n_v {
        vhash.push(u32_at(&buf, o)?);
        vidfq.push(u8_at(&buf, o + 4)?);
        vfacet.push(u8_at(&buf, o + 5)?);
        o += 6;
    }
    if o > buf.len() {
        return Err(Error::msg("fusion: truncated payload"));
    }
    let evidence = buf[1];
    let long_len = u16_at(&buf, 4)?;
    Ok(Payload { buf, hash, idfq, mask, off, len, tri, vhash, vidfq, vfacet, evidence, long_len })
}

// ---------------------------------------------------------------- token rows

/// An association row: (target hash, score, idf) per Korean bigram (`k`) and per vocabulary term (`v`), best first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TokenRow {
    pub k: Vec<(u32, f64, f64)>,
    pub v: Vec<(u32, f64, f64)>,
}

/// `Math.round` for the non-negative values it is used on here (half away from zero equals half up there).
pub(crate) fn js_round(x: f64) -> f64 {
    x.round()
}

pub fn pack_token_row(df: u32, row: &TokenRow) -> Vec<u8> {
    let mut buf = Vec::with_capacity(8 + (row.k.len() + row.v.len()) * 7);
    buf.extend(df.to_le_bytes());
    buf.extend([row.k.len() as u8, row.v.len() as u8, 0, 0]);
    for (h, s, idf) in row.k.iter().chain(row.v.iter()) {
        buf.extend(h.to_le_bytes());
        buf.extend((js_round(s / SCORE_MAX * 65535.0).min(65535.0) as u16).to_le_bytes());
        buf.push(js_round(idf * IDF_SCALE).min(255.0) as u8);
    }
    buf
}

pub fn unpack_token_row(buf: &[u8]) -> Result<(u32, TokenRow)> {
    let df = u32_at(buf, 0)?;
    let (nk, nv) = (u8_at(buf, 4)? as usize, u8_at(buf, 5)? as usize);
    let read = |o: usize| -> Result<(u32, f64, f64)> { Ok((u32_at(buf, o)?, u16_at(buf, o + 4)? as f64 / 65535.0 * SCORE_MAX, u8_at(buf, o + 6)? as f64 / IDF_SCALE)) };
    let mut row = TokenRow::default();
    let mut o = 8;
    for _ in 0..nk {
        row.k.push(read(o)?);
        o += 7;
    }
    for _ in 0..nv {
        row.v.push(read(o)?);
        o += 7;
    }
    Ok((df, row))
}

// ---------------------------------------------------------------- half floats

/// IEEE binary16 -> binary32 (exact).
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h & 0x8000) as u32) << 16;
    let exp = ((h >> 10) & 0x1f) as u32;
    let man = (h & 0x3ff) as u32;
    if exp == 0 {
        let v = man as f32 * (1.0 / 16_777_216.0);
        return if sign != 0 { -v } else { v };
    }
    if exp == 0x1f {
        return f32::from_bits(sign | 0x7f80_0000 | (man << 13));
    }
    f32::from_bits(sign | ((exp + 112) << 23) | (man << 13))
}

/// IEEE binary32 -> binary16, round to nearest, ties to even (what `Float16Array.from` does to a float32 value).
pub fn f32_to_f16(value: f32) -> u16 {
    let x = value.to_bits();
    let sign = x & 0x8000_0000;
    let exp = x & 0x7f80_0000;
    let man = x & 0x007f_ffff;
    let half_sign = sign >> 16;
    if exp == 0x7f80_0000 {
        let nan_bit = if man == 0 { 0 } else { 0x0200 };
        return (half_sign | 0x7c00 | nan_bit | (man >> 13)) as u16;
    }
    let half_exp = ((exp >> 23) as i32) - 127 + 15;
    if half_exp >= 0x1f {
        return (half_sign | 0x7c00) as u16;
    }
    if half_exp <= 0 {
        if 14 - half_exp > 24 {
            return half_sign as u16;
        }
        let man = man | 0x0080_0000;
        let mut half_man = man >> (14 - half_exp);
        let round_bit = 1u32 << (13 - half_exp);
        if (man & round_bit) != 0 && (man & (3 * round_bit - 1)) != 0 {
            half_man += 1;
        }
        return (half_sign | half_man) as u16;
    }
    let half = half_sign | ((half_exp as u32) << 10) | (man >> 13);
    let round_bit = 0x0000_1000;
    if (man & round_bit) != 0 && (man & (3 * round_bit - 1)) != 0 {
        (half + 1) as u16
    } else {
        half as u16
    }
}

pub fn to_f16_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|&x| f32_to_f16(x).to_le_bytes()).collect()
}

/// A stored vector back to float32 at unit length (r4-unified-dense.mjs fromF16: the norm summed in f64, each value divided
/// in f64 and stored as f32). None when it does not have `dim` values or holds an infinity or NaN (the card then counts as
/// a card without a vector).
pub fn from_f16_bytes(b: &[u8], dim: usize) -> Option<Vec<f32>> {
    if b.len() != dim * 2 {
        return None;
    }
    let mut v: Vec<f32> = b.chunks_exact(2).map(|c| f16_to_f32(u16::from_le_bytes([c[0], c[1]]))).collect();
    if !v.iter().all(|x| x.is_finite()) {
        return None;
    }
    unit_in_place(&mut v, false);
    vector_ok(&v, dim).then_some(v)
}

/// `dim` values, every one finite.
pub fn vector_ok(v: &[f32], dim: usize) -> bool {
    v.len() == dim && v.iter().all(|x| x.is_finite())
}

/// Bring a vector to unit length the JS way. `or_one`: a zero vector is divided by 1 (lib/embed.mjs unit, the switch's
/// client); otherwise it is left as is (fromF16).
pub fn unit_in_place(v: &mut [f32], or_one: bool) {
    let mut n = 0f64;
    for &x in v.iter() {
        n += x as f64 * x as f64;
    }
    let mut n = n.sqrt();
    if n == 0.0 {
        if !or_one {
            return;
        }
        n = 1.0;
    }
    for x in v.iter_mut() {
        *x = (*x as f64 / n) as f32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats_round_to_nearest_even() {
        assert_eq!(f32_to_f16(1.0), 0x3c00);
        assert_eq!(f32_to_f16(-2.0), 0xc000);
        assert_eq!(f32_to_f16(0.1), 0x2e66);
        assert_eq!(f32_to_f16(65504.0), 0x7bff);
        assert_eq!(f32_to_f16(70000.0), 0x7c00);
        assert_eq!(f32_to_f16(1e-8), 0);
        assert_eq!(f32_to_f16(6.0e-8), 0x0001); // smallest subnormal, rounded up
        // 1 + 2^-11 is exactly between 1 and the next half (1 + 2^-10): ties to even -> 1
        assert_eq!(f32_to_f16(1.0 + 1.0 / 2048.0), 0x3c00);
        // 1 + 3 * 2^-11 is between 1 + 2^-10 (odd) and 1 + 2^-9 (even): ties to even -> 1 + 2^-9
        assert_eq!(f32_to_f16(1.0 + 3.0 / 2048.0), 0x3c02);
        for h in [0x0001u16, 0x03ff, 0x0400, 0x3555, 0x3c00, 0x7bff, 0x8001, 0xbc00] {
            assert_eq!(f32_to_f16(f16_to_f32(h)), h, "{h:#06x}");
        }
        assert_eq!(f16_to_f32(0x0001), 5.960_464_5e-8);
    }

    #[test]
    fn stored_vectors_come_back_at_unit_length() {
        let v = [0.6f32, 0.8, 0.0, 0.0];
        let b = to_f16_bytes(&v);
        assert_eq!(b.len(), 8);
        let back = from_f16_bytes(&b, 4).unwrap();
        let n: f64 = back.iter().map(|&x| x as f64 * x as f64).sum();
        assert!((n - 1.0).abs() < 1e-6, "{n}");
        assert!((back[0] - 0.6).abs() < 1e-3 && (back[1] - 0.8).abs() < 1e-3);
        assert_eq!(from_f16_bytes(&to_f16_bytes(&[0.0, 0.0]), 2), Some(vec![0.0, 0.0]));
    }

    #[test]
    fn payload_layout_round_trips() {
        let doc = Doc {
            fields: vec![
                vec!["w:redis".into(), "w:redis".into(), "t:abc".into()],
                vec![],
                vec!["w:cache".into()],
                vec![],
                vec![],
                vec![],
                vec!["k:캐시".into(), "k:캐시".into()],
                vec![],
            ],
            vterms: vec![("redis".into(), 0), ("caching".into(), 2)],
            evidence: 3,
            long_len: 1234,
        };
        let buf = encode_payload(&doc, |h| (h % 200) as u8).unwrap();
        let p = decode_payload(buf.clone()).unwrap();
        assert_eq!((p.evidence, p.long_len), (3, 1234));
        assert_eq!(p.len, [2, 0, 1, 0, 0, 0, 2, 0]);
        assert_eq!(p.tri, vec![fnv1a32("t:abc")]);
        let mut want: Vec<u32> = ["w:redis", "w:cache", "k:캐시"].iter().map(|t| fnv1a32(t)).collect();
        want.sort();
        assert_eq!(p.hash, want);
        let i = p.hash.iter().position(|&h| h == fnv1a32("w:redis")).unwrap();
        assert_eq!((p.mask[i], p.buf[p.off[i]], p.idfq[i]), (0b1, 2, (fnv1a32("w:redis") % 200) as u8));
        let j = p.hash.iter().position(|&h| h == fnv1a32("k:캐시")).unwrap();
        assert_eq!((p.mask[j], p.buf[p.off[j]]), (1 << LONG, 2));
        assert_eq!(p.vhash, vec![fnv1a32("v:redis"), fnv1a32("v:caching")]);
        assert_eq!(p.vfacet, vec![0, 2]);
        // header: version, evidence, nVocab, 0, longLen, 8 field lengths, nTok, nTri
        assert_eq!(&buf[..6], &[1, 3, 2, 0, 0xd2, 0x04]);
        assert_eq!(buf.len(), 26 + 3 * 7 + 4 + 2 * 6);
        assert!(decode_payload(buf[..buf.len() - 1].to_vec()).is_err());
        assert!(decode_payload(vec![2, 0, 0]).is_err());
    }

    #[test]
    fn token_rows_round_trip_with_quantized_scores() {
        let row = TokenRow { k: vec![(7, 4.0, 2.5)], v: vec![(9, 8.0, 0.0), (11, 0.001, 10.625)] };
        let buf = pack_token_row(42, &row);
        assert_eq!(buf.len(), 8 + 3 * 7);
        let (df, back) = unpack_token_row(&buf).unwrap();
        assert_eq!(df, 42);
        assert_eq!(back.k, vec![(7, 32768.0 / 65535.0 * 8.0, 60.0 / 24.0)]);
        assert_eq!(back.v[0], (9, 8.0, 0.0));
        assert_eq!(back.v[1], (11, 8.0 / 65535.0 * 8.0, 255.0 / 24.0));
    }

    #[test]
    fn too_many_tokens_is_an_error_not_a_truncation() {
        let mut fields = vec![vec![]; NF];
        fields[0] = (0..70_000).map(|i| format!("w:t{i}")).collect();
        let doc = Doc { fields, vterms: vec![], evidence: 7, long_len: 0 };
        let e = encode_payload(&doc, |_| 0).unwrap_err().to_string();
        assert!(e.contains("65535"), "{e}");
    }

    #[test]
    fn non_finite_or_mis_sized_vectors_are_refused() {
        assert!(from_f16_bytes(&to_f16_bytes(&[0.6, 0.8]), 2).is_some());
        assert!(from_f16_bytes(&to_f16_bytes(&[1.0, 0.0]), 3).is_none(), "wrong dimension");
        assert!(from_f16_bytes(&[0u8; 3], 2).is_none(), "odd byte count");
        for bad in [0x7c00u16, 0xfc00, 0x7e00] {
            let mut b = to_f16_bytes(&[0.5, 0.5]);
            b[2..4].copy_from_slice(&bad.to_le_bytes());
            assert!(from_f16_bytes(&b, 2).is_none(), "{bad:#06x}");
        }
        assert!(vector_ok(&[0.6, 0.8], 2));
        assert!(!vector_ok(&[0.6, f32::NAN], 2) && !vector_ok(&[0.6], 2) && !vector_ok(&[f32::INFINITY, 0.0], 2));
    }
}
