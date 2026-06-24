pub mod hash_embedder;

pub use hash_embedder::HashEmbedder;

use crate::error::Result;

/// Pluggable embedding provider. The default is the offline deterministic
/// [`HashEmbedder`]; an HTTP provider (Ollama/OpenAI/...) can implement this
/// trait without touching any call site. The `id()` is recorded in the
/// database `meta` so vectors from different embedders are never mixed.
pub trait Embedder: Send + Sync {
    fn id(&self) -> &str;
    fn dim(&self) -> usize;
    fn embed(&self, text: &str) -> Result<Vec<f32>>;

    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        texts.iter().map(|t| self.embed(t)).collect()
    }
}

/// Cosine similarity of two equal-length vectors. Inputs from embedders are
/// already L2-normalized, but normalize defensively against zero vectors.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// Vec<f32> -> little-endian bytes for BLOB storage.
pub fn encode_embedding(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// BLOB bytes -> Vec<f32>; trailing partial floats are ignored.
pub fn decode_embedding(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_roundtrip() {
        let v = vec![0.5f32, -1.25, 3.0, 0.0];
        assert_eq!(decode_embedding(&encode_embedding(&v)), v);
    }

    #[test]
    fn cosine_basics() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
    }
}
