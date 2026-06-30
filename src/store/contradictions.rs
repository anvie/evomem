//! Contradiction tracking — store layer.
//!
//! Retrieval will happily return two docs that flatly disagree ("works_at Acme"
//! vs "works_at Globex") with no signal that they conflict. This table records
//! such conflicts between two knowledge items (addressed by slug) so `think`
//! can warn when a cited fact is contested, and so a human (or the auto-detect
//! pass) can flag and later resolve them.
//!
//! Pairs are stored sorted (`item_a <= item_b`) and deduplicated on
//! `(item_a, item_b, edge_type)`, so flagging the same conflict twice is a
//! no-op and the human's resolution is never silently re-opened by a re-flag.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::Result;

use super::Store;

/// A flagged conflict between two knowledge items.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Contradiction {
    pub id: i64,
    pub item_a: String,
    pub item_b: String,
    /// Relation the conflict is about, or "" when unspecified.
    pub edge_type: String,
    pub description: String,
    /// `open` | `resolved`.
    pub status: String,
    pub resolution: Option<String>,
    pub resolved_by: Option<String>,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

fn row_to_contradiction(r: &rusqlite::Row) -> rusqlite::Result<Contradiction> {
    Ok(Contradiction {
        id: r.get("id")?,
        item_a: r.get("item_a")?,
        item_b: r.get("item_b")?,
        edge_type: r.get("edge_type")?,
        description: r.get("description")?,
        status: r.get("status")?,
        resolution: r.get("resolution")?,
        resolved_by: r.get("resolved_by")?,
        created_at: r.get("created_at")?,
        resolved_at: r.get("resolved_at")?,
    })
}

/// Order a pair so a conflict is stored once regardless of argument order.
fn ordered(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

impl Store {
    /// Flag a conflict between two items. Idempotent on `(item_a, item_b,
    /// edge_type)`: re-flagging an existing pair leaves it (and any resolution)
    /// untouched. Returns the row id (new or existing).
    pub fn flag_contradiction(
        &self,
        item_a: &str,
        item_b: &str,
        edge_type: Option<&str>,
        description: &str,
        now: &str,
    ) -> Result<i64> {
        let (a, b) = ordered(item_a, item_b);
        let edge = edge_type.unwrap_or("");
        self.conn.execute(
            "INSERT INTO contradictions (item_a, item_b, edge_type, description, status, created_at)
             VALUES (?1, ?2, ?3, ?4, 'open', ?5)
             ON CONFLICT(item_a, item_b, edge_type) DO NOTHING",
            params![a, b, edge, description, now],
        )?;
        Ok(self.conn.query_row(
            "SELECT id FROM contradictions WHERE item_a = ?1 AND item_b = ?2 AND edge_type = ?3",
            params![a, b, edge],
            |r| r.get(0),
        )?)
    }

    /// Mark a contradiction resolved. Returns false if the id doesn't exist.
    pub fn resolve_contradiction(
        &self,
        id: i64,
        resolution: Option<&str>,
        resolved_by: Option<&str>,
        now: &str,
    ) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE contradictions
             SET status = 'resolved', resolution = ?2, resolved_by = ?3, resolved_at = ?4
             WHERE id = ?1",
            params![id, resolution, resolved_by, now],
        )?;
        Ok(n > 0)
    }

    /// List contradictions, newest first; `open_only` filters to unresolved.
    pub fn list_contradictions(&self, open_only: bool) -> Result<Vec<Contradiction>> {
        let sql = if open_only {
            "SELECT * FROM contradictions WHERE status = 'open' ORDER BY id DESC"
        } else {
            "SELECT * FROM contradictions ORDER BY id DESC"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt
            .query_map([], row_to_contradiction)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Open contradictions where either item is in `slugs` — the `think` hook
    /// that warns when a cited fact is contested.
    pub fn open_contradictions_touching(&self, slugs: &[String]) -> Result<Vec<Contradiction>> {
        if slugs.is_empty() {
            return Ok(Vec::new());
        }
        let set: std::collections::HashSet<&str> = slugs.iter().map(|s| s.as_str()).collect();
        Ok(self
            .list_contradictions(true)?
            .into_iter()
            .filter(|c| set.contains(c.item_a.as_str()) || set.contains(c.item_b.as_str()))
            .collect())
    }

    /// Count of open (unresolved) contradictions — a stats signal.
    pub fn open_contradiction_count(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM contradictions WHERE status = 'open'",
            [],
            |r| r.get(0),
        )?)
    }

    /// Fetch one contradiction by id (used by tests / detail views).
    pub fn get_contradiction(&self, id: i64) -> Result<Option<Contradiction>> {
        Ok(self
            .conn
            .query_row(
                "SELECT * FROM contradictions WHERE id = ?1",
                [id],
                row_to_contradiction,
            )
            .optional()?)
    }

    /// Id of an existing contradiction for this (order-independent) pair +
    /// edge_type, or `None`. Lets the detector tell new flags from repeats.
    pub fn contradiction_id(
        &self,
        item_a: &str,
        item_b: &str,
        edge_type: Option<&str>,
    ) -> Result<Option<i64>> {
        let (a, b) = ordered(item_a, item_b);
        let edge = edge_type.unwrap_or("");
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM contradictions WHERE item_a = ?1 AND item_b = ?2 AND edge_type = ?3",
                params![a, b, edge],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Every resolved (non-dangling) typed edge as `(src_slug, edge_type,
    /// dst_slug)`, excluding plain `mentions` — the raw material the
    /// contradiction detector groups by subject + relation.
    pub fn resolved_typed_edges(&self) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT sd.slug, l.edge_type, dd.slug
             FROM links l
             JOIN docs sd ON sd.id = l.src_doc_id AND sd.deleted_at IS NULL
             JOIN docs dd ON dd.id = l.dst_doc_id AND dd.deleted_at IS NULL
             WHERE l.edge_type != 'mentions'
             ORDER BY sd.slug, l.edge_type, dd.slug",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}
