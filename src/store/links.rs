use rusqlite::params;

use crate::error::Result;
use crate::model::LinkDraft;

use super::Store;

/// A typed edge hydrated for graph traversal / display.
#[derive(Debug, Clone)]
pub struct EdgeRow {
    pub src_doc_id: i64,
    pub src_slug: String,
    pub dst_slug: String,
    pub dst_doc_id: Option<i64>,
    pub edge_type: String,
    pub anchor_text: Option<String>,
}

impl Store {
    /// Resolve a wiki-link target slug to a destination doc id, Obsidian-style.
    ///
    /// Tries, in order, returning the first live match:
    ///   1. exact slug
    ///   2. `{slug}/index` (folder/workspace index docs)
    ///   3. title == basename (case-insensitive)
    ///   4. alias == basename (case-insensitive)
    ///   5. another doc whose slug basename == basename
    ///
    /// `basename` is the last `/`-segment of the target. Steps 3–5 require a
    /// **unique** match — if more than one doc matches, the link is left
    /// dangling rather than guessing (mirrors the rename "never guess" rule).
    pub fn resolve_link_target(&self, dst_slug: &str) -> Result<Option<i64>> {
        // 1. exact slug (slug is UNIQUE → at most one)
        if let Some(id) = self.unique_id(
            "SELECT id FROM docs WHERE slug = ?1 AND deleted_at IS NULL LIMIT 2",
            dst_slug,
        )? {
            return Ok(Some(id));
        }
        // 2. {slug}/index
        let index_slug = format!("{dst_slug}/index");
        if let Some(id) = self.unique_id(
            "SELECT id FROM docs WHERE slug = ?1 AND deleted_at IS NULL LIMIT 2",
            &index_slug,
        )? {
            return Ok(Some(id));
        }
        let base = dst_slug.rsplit('/').next().unwrap_or(dst_slug);
        // 3. unique title (NOCASE)
        if let Some(id) = self.unique_id(
            "SELECT id FROM docs WHERE title = ?1 COLLATE NOCASE AND deleted_at IS NULL LIMIT 2",
            base,
        )? {
            return Ok(Some(id));
        }
        // 4. unique alias (NOCASE; alias column is COLLATE NOCASE)
        if let Some(id) = self.unique_id(
            "SELECT a.doc_id FROM doc_aliases a JOIN docs d ON d.id = a.doc_id
             WHERE a.alias = ?1 AND d.deleted_at IS NULL LIMIT 2",
            base,
        )? {
            return Ok(Some(id));
        }
        // 5. unique slug basename (e.g. `trips/jakarta` resolves a bare `jakarta`)
        if let Some(id) = self.unique_id(
            "SELECT id FROM docs WHERE (slug = ?1 OR slug LIKE '%/' || ?1)
             AND deleted_at IS NULL LIMIT 2",
            base,
        )? {
            return Ok(Some(id));
        }
        Ok(None)
    }

    /// Run a query that selects up to 2 doc ids; return `Some(id)` only when
    /// exactly one row matches (uniqueness guard for ambiguous resolution).
    fn unique_id(&self, sql: &str, param: &str) -> Result<Option<i64>> {
        let mut stmt = self.conn.prepare_cached(sql)?;
        let ids: Vec<i64> = stmt
            .query_map([param], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(if ids.len() == 1 { Some(ids[0]) } else { None })
    }

    /// Replace all outgoing links for a doc. Destinations are inserted as
    /// dangling (dst_doc_id NULL) and resolved in a single pass at the end of
    /// sync via [`Store::resolve_dangling_links`]. Deferring resolution makes it
    /// order-independent: a wiki-link is resolved against the *complete* doc set,
    /// so an ambiguous title (two docs share it) reliably stays dangling no
    /// matter which doc the sync walked first.
    pub fn replace_links_for_doc(&self, src_doc_id: i64, drafts: &[LinkDraft]) -> Result<()> {
        self.conn
            .execute("DELETE FROM links WHERE src_doc_id = ?1", [src_doc_id])?;
        let mut stmt = self.conn.prepare_cached(
            "INSERT INTO links (src_doc_id, dst_slug, dst_doc_id, edge_type, anchor_text)
             VALUES (?1, ?2, NULL, ?3, ?4)
             ON CONFLICT(src_doc_id, dst_slug, edge_type) DO NOTHING",
        )?;
        for d in drafts {
            stmt.execute(params![
                src_doc_id,
                d.dst_slug,
                d.edge_type.as_str(),
                d.anchor_text
            ])?;
        }
        Ok(())
    }

    /// Re-resolve dangling links after a sync (targets may have appeared).
    /// Resolves each distinct dangling slug via [`Store::resolve_link_target`].
    pub fn resolve_dangling_links(&self) -> Result<usize> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT dst_slug FROM links WHERE dst_doc_id IS NULL")?;
        let slugs: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        let mut n = 0;
        for slug in slugs {
            if let Some(id) = self.resolve_link_target(&slug)? {
                n += self.conn.execute(
                    "UPDATE links SET dst_doc_id = ?1 WHERE dst_slug = ?2 AND dst_doc_id IS NULL",
                    params![id, slug],
                )?;
            }
        }
        Ok(n)
    }

    /// Outgoing + incoming edges for a doc (direction-agnostic traversal for
    /// MVP), optionally filtered by edge type. Outgoing edges may be dangling
    /// (dst_doc_id NULL) — callers display them but don't traverse them.
    pub fn neighbors(&self, doc_id: i64, edge_type: Option<&str>) -> Result<Vec<EdgeRow>> {
        let mut sql = String::from(
            "SELECT l.src_doc_id, ps.slug, l.dst_slug, l.dst_doc_id, l.edge_type, l.anchor_text
             FROM links l
             JOIN docs ps ON ps.id = l.src_doc_id AND ps.deleted_at IS NULL
             WHERE (l.src_doc_id = ?1 OR l.dst_doc_id = ?1)",
        );
        if edge_type.is_some() {
            sql.push_str(" AND l.edge_type = ?2");
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let map = |r: &rusqlite::Row| -> rusqlite::Result<EdgeRow> {
            Ok(EdgeRow {
                src_doc_id: r.get(0)?,
                src_slug: r.get(1)?,
                dst_slug: r.get(2)?,
                dst_doc_id: r.get(3)?,
                edge_type: r.get(4)?,
                anchor_text: r.get(5)?,
            })
        };
        let rows = match edge_type {
            Some(t) => stmt
                .query_map(params![doc_id, t], map)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            None => stmt
                .query_map([doc_id], map)?
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
               dst_doc_id = (SELECT id FROM docs WHERE slug = ?2 AND deleted_at IS NULL)
             WHERE dst_slug = ?1",
            [old_slug, new_slug],
        )?;
        Ok(n)
    }

    /// Inbound link counts for a doc: (typed edges, plain mentions). Used as
    /// a query-time authority prior — docs the graph points at are salient.
    pub fn in_degree(&self, doc_id: i64) -> Result<(i64, i64)> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(CASE WHEN edge_type != 'mentions' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN edge_type = 'mentions' THEN 1 ELSE 0 END), 0)
             FROM links WHERE dst_doc_id = ?1 AND src_doc_id != ?1",
            [doc_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    /// Dangling destinations referenced by the given docs (for gap analysis).
    pub fn dangling_from_pages(&self, doc_ids: &[i64]) -> Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        let mut stmt = self.conn.prepare(
            "SELECT p.slug, l.dst_slug FROM links l
             JOIN docs p ON p.id = l.src_doc_id
             WHERE l.src_doc_id = ?1 AND l.dst_doc_id IS NULL",
        )?;
        for id in doc_ids {
            let rows = stmt
                .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<(String, String)>>>()?;
            out.extend(rows);
        }
        Ok(out)
    }
}
