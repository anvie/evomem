//! Memory hygiene — store-level helpers for `consolidate`.
//!
//! `consolidate` (see [`crate::hygiene`]) folds near-duplicate docs into a
//! single newer survivor by setting `docs.superseded_by`. Superseded docs stay
//! on disk and in the database (history is never destroyed) but drop out of
//! retrieval, so a corpus that accretes near-identical captures stays clean
//! without the lexical/vector ranker fighting five spellings of one fact.

use rusqlite::params;

use crate::error::Result;

use super::Store;

/// A live doc plus its concatenated body text, the unit the consolidate pass
/// compares for near-duplication.
#[derive(Debug, Clone)]
pub struct DocText {
    pub id: i64,
    pub slug: String,
    pub doc_type: String,
    /// Top-level folder the doc lives under (memory/, entities/, …; empty for a
    /// root note). consolidate folds only within one source_dir so a private
    /// memory can never be folded into a knowledge note or vice versa.
    pub source_dir: String,
    pub updated_at: Option<String>,
    /// Title + every chunk's text, joined — the doc's full searchable surface.
    pub text: String,
}

impl Store {
    /// Every non-deleted doc with its full text (title + chunks), regardless of
    /// current supersession state — the consolidate pass clears and recomputes
    /// supersession from scratch, so it needs the complete live set each run.
    pub fn live_doc_texts(&self) -> Result<Vec<DocText>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.slug, p.doc_type, p.source_dir, p.updated_at,
                    p.title || ' ' || COALESCE(GROUP_CONCAT(c.text, ' '), '')
             FROM docs p
             LEFT JOIN chunks c ON c.doc_id = p.id
             WHERE p.deleted_at IS NULL
             GROUP BY p.id
             ORDER BY p.id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(DocText {
                    id: r.get(0)?,
                    slug: r.get(1)?,
                    doc_type: r.get(2)?,
                    source_dir: r.get(3)?,
                    updated_at: r.get(4)?,
                    text: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Clear every auto-supersession so a consolidate run starts from a clean
    /// slate (the pass is deterministic and fully recomputes the set). Returns
    /// how many rows were reset.
    pub fn clear_supersessions(&self) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE docs SET superseded_by = NULL WHERE superseded_by IS NOT NULL",
            [],
        )?)
    }

    /// Clear auto-supersessions only for docs under one `source_dir`, so a
    /// single-layer consolidate run recomputes its own scope without disturbing
    /// folds another layer set. Returns how many rows were reset.
    pub fn clear_supersessions_in(&self, source_dir: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE docs SET superseded_by = NULL
             WHERE superseded_by IS NOT NULL AND source_dir = ?1",
            params![source_dir],
        )?)
    }

    /// Mark `doc_id` as superseded by the newer `survivor_id`.
    pub fn set_superseded(&self, doc_id: i64, survivor_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE docs SET superseded_by = ?2 WHERE id = ?1",
            params![doc_id, survivor_id],
        )?;
        Ok(())
    }

    /// Count live docs currently hidden as superseded near-duplicates.
    pub fn superseded_count(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM docs WHERE deleted_at IS NULL AND superseded_by IS NOT NULL",
            [],
            |r| r.get(0),
        )?)
    }
}
