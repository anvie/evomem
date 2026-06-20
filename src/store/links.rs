use rusqlite::params;

use crate::error::Result;
use crate::model::LinkDraft;

use super::Store;

/// A typed edge hydrated for graph traversal / display.
#[derive(Debug, Clone)]
pub struct EdgeRow {
    pub src_page_id: i64,
    pub src_slug: String,
    pub dst_slug: String,
    pub dst_page_id: Option<i64>,
    pub edge_type: String,
    pub anchor_text: Option<String>,
}

impl Store {
    /// Replace all outgoing links for a page, resolving destinations that
    /// already exist; unresolved ones stay dangling (dst_page_id NULL) and are
    /// re-resolved at the end of every sync.
    pub fn replace_links_for_page(&self, src_page_id: i64, drafts: &[LinkDraft]) -> Result<()> {
        self.conn
            .execute("DELETE FROM links WHERE src_page_id = ?1", [src_page_id])?;
        let mut stmt = self.conn.prepare_cached(
            "INSERT INTO links (src_page_id, dst_slug, dst_page_id, edge_type, anchor_text)
             VALUES (?1, ?2,
                     (SELECT id FROM pages WHERE slug = ?2 AND deleted_at IS NULL),
                     ?3, ?4)
             ON CONFLICT(src_page_id, dst_slug, edge_type) DO NOTHING",
        )?;
        for d in drafts {
            stmt.execute(params![
                src_page_id,
                d.dst_slug,
                d.edge_type.as_str(),
                d.anchor_text
            ])?;
        }
        Ok(())
    }

    /// Re-resolve dangling links after a sync (targets may have appeared).
    pub fn resolve_dangling_links(&self) -> Result<usize> {
        let n = self.conn.execute(
            "UPDATE links SET dst_page_id =
               (SELECT id FROM pages WHERE slug = links.dst_slug AND deleted_at IS NULL)
             WHERE dst_page_id IS NULL
               AND EXISTS (SELECT 1 FROM pages WHERE slug = links.dst_slug AND deleted_at IS NULL)",
            [],
        )?;
        Ok(n)
    }

    /// Outgoing + incoming edges for a page (direction-agnostic traversal for
    /// MVP), optionally filtered by edge type. Outgoing edges may be dangling
    /// (dst_page_id NULL) — callers display them but don't traverse them.
    pub fn neighbors(&self, page_id: i64, edge_type: Option<&str>) -> Result<Vec<EdgeRow>> {
        let mut sql = String::from(
            "SELECT l.src_page_id, ps.slug, l.dst_slug, l.dst_page_id, l.edge_type, l.anchor_text
             FROM links l
             JOIN pages ps ON ps.id = l.src_page_id AND ps.deleted_at IS NULL
             WHERE (l.src_page_id = ?1 OR l.dst_page_id = ?1)",
        );
        if edge_type.is_some() {
            sql.push_str(" AND l.edge_type = ?2");
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let map = |r: &rusqlite::Row| -> rusqlite::Result<EdgeRow> {
            Ok(EdgeRow {
                src_page_id: r.get(0)?,
                src_slug: r.get(1)?,
                dst_slug: r.get(2)?,
                dst_page_id: r.get(3)?,
                edge_type: r.get(4)?,
                anchor_text: r.get(5)?,
            })
        };
        let rows = match edge_type {
            Some(t) => stmt
                .query_map(params![page_id, t], map)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            None => stmt
                .query_map([page_id], map)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        };
        Ok(rows)
    }

    /// Repoint every link aimed at `old_slug` to `new_slug` (rename support).
    /// OR IGNORE: if a source already links to the new slug with the same
    /// edge type, the duplicate row is dropped instead of conflicting.
    pub fn rewrite_link_targets(&self, old_slug: &str, new_slug: &str) -> Result<usize> {
        let n = self.conn.execute(
            "UPDATE OR IGNORE links SET
               dst_slug = ?2,
               dst_page_id = (SELECT id FROM pages WHERE slug = ?2 AND deleted_at IS NULL)
             WHERE dst_slug = ?1",
            [old_slug, new_slug],
        )?;
        Ok(n)
    }

    /// Inbound link counts for a page: (typed edges, plain mentions). Used as
    /// a query-time authority prior — pages the graph points at are salient.
    pub fn in_degree(&self, page_id: i64) -> Result<(i64, i64)> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(CASE WHEN edge_type != 'mentions' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN edge_type = 'mentions' THEN 1 ELSE 0 END), 0)
             FROM links WHERE dst_page_id = ?1 AND src_page_id != ?1",
            [page_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    /// Dangling destinations referenced by the given pages (for gap analysis).
    pub fn dangling_from_pages(&self, page_ids: &[i64]) -> Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        let mut stmt = self.conn.prepare(
            "SELECT p.slug, l.dst_slug FROM links l
             JOIN pages p ON p.id = l.src_page_id
             WHERE l.src_page_id = ?1 AND l.dst_page_id IS NULL",
        )?;
        for id in page_ids {
            let rows = stmt
                .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<(String, String)>>>()?;
            out.extend(rows);
        }
        Ok(out)
    }
}





