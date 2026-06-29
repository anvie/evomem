//! Trust layer — per-doc provenance & freshness.
//!
//! Retrieval answers "does this match?". Provenance answers "should I act on
//! it?": where a fact came from, how much to trust it, when it was last
//! verified, and how long that verification stays good. The fields are written
//! by the author in frontmatter and projected into a `provenance` row on every
//! sync (1:1 with the doc), so disk stays the source of truth.

use rusqlite::{params, OptionalExtension};

use crate::error::Result;

use super::Store;

/// Trust + freshness metadata for one doc. Every field is optional: a doc with
/// no trust frontmatter simply has no provenance row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Provenance {
    /// Free-form origin hint (user_stated | inferred | external | agent_reported).
    pub source: Option<String>,
    /// Trust level, 0.0–1.0.
    pub confidence: Option<f64>,
    /// When last verified (date or RFC3339).
    pub verified_at: Option<String>,
    /// Re-verify after this many days; `0`/None means it never expires on its own.
    pub stale_after_days: Option<i64>,
}

impl Provenance {
    /// True when no trust field is set — such a doc gets no provenance row.
    pub fn is_empty(&self) -> bool {
        self.source.is_none()
            && self.confidence.is_none()
            && self.verified_at.is_none()
            && self.stale_after_days.is_none()
    }
}

fn row_to_provenance(r: &rusqlite::Row) -> rusqlite::Result<Provenance> {
    Ok(Provenance {
        source: r.get("source")?,
        confidence: r.get("confidence")?,
        verified_at: r.get("verified_at")?,
        stale_after_days: r.get("stale_after_days")?,
    })
}

impl Store {
    /// Upsert (or, when empty, clear) the provenance row for a doc. Called for
    /// every doc on sync so the row always reflects current frontmatter.
    pub fn set_provenance(&self, doc_id: i64, p: &Provenance) -> Result<()> {
        if p.is_empty() {
            self.conn
                .execute("DELETE FROM provenance WHERE doc_id = ?1", [doc_id])?;
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO provenance (doc_id, source, confidence, verified_at, stale_after_days)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(doc_id) DO UPDATE SET
               source = excluded.source, confidence = excluded.confidence,
               verified_at = excluded.verified_at, stale_after_days = excluded.stale_after_days",
            params![
                doc_id,
                p.source,
                p.confidence,
                p.verified_at,
                p.stale_after_days
            ],
        )?;
        Ok(())
    }

    /// Provenance for a doc id, or `None` when the doc has no trust metadata.
    pub fn get_provenance(&self, doc_id: i64) -> Result<Option<Provenance>> {
        Ok(self
            .conn
            .query_row(
                "SELECT source, confidence, verified_at, stale_after_days
                 FROM provenance WHERE doc_id = ?1",
                [doc_id],
                row_to_provenance,
            )
            .optional()?)
    }

    /// Provenance for a live doc addressed by slug (used by `think`).
    pub fn get_provenance_by_slug(&self, slug: &str) -> Result<Option<Provenance>> {
        Ok(self
            .conn
            .query_row(
                "SELECT pr.source, pr.confidence, pr.verified_at, pr.stale_after_days
                 FROM provenance pr JOIN docs d ON d.id = pr.doc_id
                 WHERE d.slug = ?1 AND d.deleted_at IS NULL",
                [slug],
                row_to_provenance,
            )
            .optional()?)
    }
}
