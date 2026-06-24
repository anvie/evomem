use crate::embed::{cosine, decode_embedding, Embedder};
use crate::error::Result;
use crate::store::Store;

/// Brute-force cosine scan over all live chunk embeddings, streamed from the
/// database. Returns the top-k (chunk_id, similarity), best first. Fine to
/// ~50K chunks; swap for an HNSW index behind this same signature if needed.
pub fn search(
    store: &Store,
    embedder: &dyn Embedder,
    query: &str,
    top_k: usize,
) -> Result<Vec<(i64, f32)>> {
    let q = embedder.embed(query)?;
    let mut hits: Vec<(i64, f32)> = Vec::new();
    store.for_each_embedding(embedder.dim(), |chunk_id, blob| {
        let v = decode_embedding(blob);
        let sim = cosine(&q, &v);
        if sim <= 0.0 {
            return;
        }
        if hits.len() < top_k {
            hits.push((chunk_id, sim));
            if hits.len() == top_k {
                hits.sort_by(|a, b| b.1.total_cmp(&a.1));
            }
        } else if sim > hits.last().unwrap().1 {
            hits.pop();
            let at = hits.partition_point(|h| h.1 >= sim);
            hits.insert(at, (chunk_id, sim));
        }
    })?;
    hits.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    Ok(hits)
}
