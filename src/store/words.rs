use crate::error::Result;

use super::Store;

/// One posting from the inverted index: an occurrence of an indexed word.
#[derive(Debug, Clone)]
pub struct Posting {
    pub chunk_id: i64,
    pub attr: i64,
    pub pos: i64,
}

impl Store {
    pub fn live_chunk_count(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM chunks c JOIN docs p ON p.id = c.doc_id
             WHERE p.deleted_at IS NULL",
            [],
            |r| r.get(0),
        )?)
    }

    /// Distinct vocabulary of live chunks. The lexical ranker scans this with
    /// bounded Levenshtein for typo-tolerant word resolution (the vocabulary
    /// is small relative to postings — same idea as Meilisearch's word FST).
    pub fn vocabulary(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT w.word FROM word_index w
             JOIN chunks c ON c.id = w.chunk_id
             JOIN docs p ON p.id = c.doc_id
             WHERE p.deleted_at IS NULL",
        )?;
        let words = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(words)
    }

    /// Number of distinct live chunks containing an indexed word — the
    /// document frequency used by the lexical df-gate and IDF tier.
    pub fn word_chunk_count(&self, word: &str, excluded_dirs: &[&str]) -> Result<i64> {
        let exclude_sql = Self::exclude_clause(excluded_dirs);
        let sql = format!(
            "SELECT COUNT(DISTINCT w.chunk_id) FROM word_index w
             JOIN chunks c ON c.id = w.chunk_id
             JOIN docs p ON p.id = c.doc_id
             WHERE w.word = ?1 AND p.deleted_at IS NULL {exclude_sql}"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        Ok(stmt.query_row([word], |r| r.get(0))?)
    }

    /// SQL fragment excluding hard-excluded source dirs. `excluded_dirs` is a
    /// compile-time constant list (config::SourceTiers), never user input, so
    /// inlining it into SQL is safe.
    fn exclude_clause(excluded_dirs: &[&str]) -> String {
        if excluded_dirs.is_empty() {
            String::new()
        } else {
            let quoted: Vec<String> = excluded_dirs
                .iter()
                .map(|d| format!("'{}'", d.replace('\'', "''")))
                .collect();
            format!("AND p.source_dir NOT IN ({})", quoted.join(","))
        }
    }

    /// All postings for an exact indexed word, restricted to live docs and
    /// excluding hard-excluded source dirs (filtered at the SQL level).
    pub fn postings(&self, word: &str, excluded_dirs: &[&str]) -> Result<Vec<Posting>> {
        let exclude_sql = Self::exclude_clause(excluded_dirs);
        let sql = format!(
            "SELECT w.chunk_id, w.attr, w.pos FROM word_index w
             JOIN chunks c ON c.id = w.chunk_id
             JOIN docs p ON p.id = c.doc_id
             WHERE w.word = ?1 AND p.deleted_at IS NULL {exclude_sql}"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt
            .query_map([word], |r| {
                Ok(Posting {
                    chunk_id: r.get(0)?,
                    attr: r.get(1)?,
                    pos: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}
