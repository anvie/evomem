use rusqlite::params;

use crate::embed::encode_embedding;
use crate::error::Result;
use crate::model::ChunkDraft;
use crate::text::tokenize;

use super::Store;

/// Attribute ranks for the Meilisearch-style `attribute` ranking rule.
pub const ATTR_TITLE: i64 = 0;
pub const ATTR_HEADING: i64 = 1;
pub const ATTR_BODY: i64 = 2;

/// A chunk row hydrated for ranking.
#[derive(Debug, Clone)]
pub struct ChunkRow {
    pub id: i64,
    pub doc_id: i64,
    pub heading_path: String,
    pub text: String,
}

impl Store {
    /// Replace all chunks (embeddings + inverted word index) for a doc.
    /// Caller wraps the whole per-file sync in a transaction.
    pub fn replace_chunks_for_page(
        &self,
        doc_id: i64,
        title: &str,
        drafts: &[ChunkDraft],
        embeddings: &[Vec<f32>],
    ) -> Result<()> {
        self.delete_chunks_for_page(doc_id)?;
        let mut insert_word = self.conn.prepare_cached(
            "INSERT OR IGNORE INTO word_index (word, chunk_id, attr, pos) VALUES (?1, ?2, ?3, ?4)",
        )?;
        for (i, (draft, emb)) in drafts.iter().zip(embeddings).enumerate() {
            self.conn.execute(
                "INSERT INTO chunks (doc_id, chunk_index, heading_path, text, embedding)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    doc_id,
                    i as i64,
                    draft.heading_path,
                    draft.text,
                    encode_embedding(emb)
                ],
            )?;
            let chunk_id = self.conn.last_insert_rowid();
            for (attr, source) in [
                (ATTR_TITLE, title),
                (ATTR_HEADING, draft.heading_path.as_str()),
                (ATTR_BODY, draft.text.as_str()),
            ] {
                for (pos, word) in tokenize(source).into_iter().enumerate() {
                    insert_word.execute(params![word, chunk_id, attr, pos as i64])?;
                }
            }
        }
        Ok(())
    }

    /// Delete a doc's chunks; word_index rows go with them via FK cascade.
    pub fn delete_chunks_for_page(&self, doc_id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM chunks WHERE doc_id = ?1", [doc_id])?;
        Ok(())
    }

    /// Stream every live chunk embedding through `f`, avoiding loading all
    /// blobs at once. `f` receives (chunk_id, embedding bytes). A blob whose
    /// size doesn't match `dim` is corruption or embedder drift — fail loudly
    /// instead of silently computing wrong similarities.
    pub fn for_each_embedding(&self, dim: usize, mut f: impl FnMut(i64, &[u8])) -> Result<()> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.embedding FROM chunks c
             JOIN docs p ON p.id = c.doc_id
             WHERE p.deleted_at IS NULL",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let blob = row.get_ref(1)?.as_blob().map_err(rusqlite::Error::from)?;
            if blob.len() != dim * 4 {
                return Err(crate::error::EvoError::Other(format!(
                    "chunk {id}: embedding blob is {} bytes, expected {} ({dim} dims) — \
                     database corruption or embedder drift; reinitialize the knowledge store",
                    blob.len(),
                    dim * 4
                )));
            }
            f(id, blob);
        }
        Ok(())
    }

    pub fn get_chunks(&self, ids: &[i64]) -> Result<Vec<ChunkRow>> {
        let mut out = Vec::with_capacity(ids.len());
        let mut stmt = self
            .conn
            .prepare("SELECT id, doc_id, heading_path, text FROM chunks WHERE id = ?1")?;
        for id in ids {
            if let Some(row) = stmt
                .query_map([id], |r| {
                    Ok(ChunkRow {
                        id: r.get(0)?,
                        doc_id: r.get(1)?,
                        heading_path: r.get(2)?,
                        text: r.get(3)?,
                    })
                })?
                .next()
            {
                out.push(row?);
            }
        }
        Ok(out)
    }

    /// Best chunk (lowest index) for a doc — used when the graph stage
    /// injects factually-connected docs that hybrid search didn't surface.
    pub fn first_chunk_for_page(&self, doc_id: i64) -> Result<Option<ChunkRow>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT id, doc_id, heading_path, text FROM chunks
                 WHERE doc_id = ?1 ORDER BY chunk_index LIMIT 1",
                [doc_id],
                |r| {
                    Ok(ChunkRow {
                        id: r.get(0)?,
                        doc_id: r.get(1)?,
                        heading_path: r.get(2)?,
                        text: r.get(3)?,
                    })
                },
            )
            .optional()?)
    }
}
