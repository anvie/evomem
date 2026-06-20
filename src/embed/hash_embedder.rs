use crate::config::{EMBEDDER_ID, EMBED_DIM};
use crate::error::Result;
use crate::text::tokenize;

use super::Embedder;

/// Offline, deterministic feature-hashing embedder. Features are word tokens
/// plus boundary-marked char 3/4-grams, hashed with FNV-1a into a fixed-dim
/// vector using the hashing-trick sign bit, sublinear tf, then L2-normalized.
///
/// This is lexical smearing, not semantics — it catches word-overlap and
/// morphological similarity. Precision comes from the lexical ranker and the
/// knowledge graph; swap in a real model via the [`Embedder`] trait when
/// semantic recall matters.
pub struct HashEmbedder;

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h = FNV_OFFSET;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

impl HashEmbedder {
    fn features(text: &str) -> std::collections::BTreeMap<u64, u32> {
        let mut counts = std::collections::BTreeMap::new();
        for word in tokenize(text) {
            *counts.entry(fnv1a64(word.as_bytes())).or_insert(0) += 1;
            let marked: Vec<char> = format!("^{word}$").chars().collect();
            for n in [3usize, 4] {
                if marked.len() < n {
                    continue;
                }
                for gram in marked.windows(n) {
                    let s: String = gram.iter().collect();
                    *counts.entry(fnv1a64(s.as_bytes())).or_insert(0) += 1;
                }
            }
        }
        counts
    }
}

impl Embedder for HashEmbedder {
    fn id(&self) -> &str {
        EMBEDDER_ID
    }

    fn dim(&self) -> usize {
        EMBED_DIM
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let mut v = vec![0f32; EMBED_DIM];
        for (hash, count) in Self::features(text) {
            let idx = ((hash >> 1) % EMBED_DIM as u64) as usize;
            let sign = if hash & 1 == 0 { 1.0 } else { -1.0 };
            let tf = 1.0 + (count as f32).ln();
            v[idx] += sign * tf;
        }
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::cosine;

    #[test]
    fn deterministic_across_calls() {
        let e = HashEmbedder;
        let a = e.embed("Alice works at Acme on retrieval").unwrap();
        let b = e.embed("Alice works at Acme on retrieval").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn self_similarity_is_one_and_overlap_orders_correctly() {
        let e = HashEmbedder;
        let q = e.embed("retrieval pipeline ranking quality").unwrap();
        let related = e
            .embed("the retrieval pipeline improves ranking quality a lot")
            .unwrap();
        let unrelated = e
            .embed("pasta carbonara recipe with eggs and pecorino")
            .unwrap();
        assert!((cosine(&q, &q) - 1.0).abs() < 1e-5);
        // Ordering assertion only — never absolute thresholds for this embedder.
        assert!(cosine(&q, &related) > cosine(&q, &unrelated));
    }

    #[test]
    fn output_is_normalized_and_fixed_dim() {
        let e = HashEmbedder;
        let v = e.embed("hello world").unwrap();
        assert_eq!(v.len(), EMBED_DIM);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
        assert_eq!(e.embed("").unwrap(), vec![0f32; EMBED_DIM]);
    }
}





