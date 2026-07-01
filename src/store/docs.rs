use rusqlite::{params, OptionalExtension};

use crate::error::Result;
use crate::model::Doc;

use super::Store;

/// Everything needed to upsert one doc (metadata only; chunks/links separate).
pub struct DocUpsert<'a> {
    pub slug: &'a str,
    pub title: &'a str,
    pub doc_type: &'a str,
    pub source_dir: &'a str,
    pub tags: &'a [String],
    pub content_hash: &'a str,
    pub created_at: Option<&'a str>,
    pub updated_at: Option<&'a str>,
    pub aliases: &'a [String],
}

fn row_to_doc(r: &rusqlite::Row) -> rusqlite::Result<Doc> {
    let tags_json: String = r.get("tags")?;
    Ok(Doc {
        id: r.get("id")?,
        slug: r.get("slug")?,
        title: r.get("title")?,
        doc_type: r.get("doc_type")?,
        source_dir: r.get("source_dir")?,
        tags: serde_json::from_str(&tags_json).unwrap_or_default(),
        content_hash: r.get("content_hash")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
        synced_at: r.get("synced_at")?,
        deleted_at: r.get("deleted_at")?,
        superseded_by: r.get("superseded_by")?,
        recall_count: r.get("recall_count")?,
        last_recalled_at: r.get("last_recalled_at")?,
    })
}

impl Store {
    /// Insert or update a doc row (revives soft-deleted docs) and reproject
    /// its aliases. Returns the doc id.
    pub fn upsert_page(&self, p: &DocUpsert, now: &str) -> Result<i64> {
        let tags = serde_json::to_string(p.tags).unwrap_or_else(|_| "[]".into());
        self.conn.execute(
            "INSERT INTO docs (slug, title, doc_type, source_dir, tags, content_hash,
                                created_at, updated_at, synced_at, deleted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL)
             ON CONFLICT(slug) DO UPDATE SET
               title = excluded.title, doc_type = excluded.doc_type,
               source_dir = excluded.source_dir, tags = excluded.tags,
               content_hash = excluded.content_hash, created_at = excluded.created_at,
               updated_at = excluded.updated_at, synced_at = excluded.synced_at,
               deleted_at = NULL",
            params![
                p.slug,
                p.title,
                p.doc_type,
                p.source_dir,
                tags,
                p.content_hash,
                p.created_at,
                p.updated_at,
                now
            ],
        )?;
        let id: i64 =
            self.conn
                .query_row("SELECT id FROM docs WHERE slug = ?1", [p.slug], |r| {
                    r.get(0)
                })?;
        self.conn
            .execute("DELETE FROM doc_aliases WHERE doc_id = ?1", [id])?;
        for alias in p.aliases {
            self.conn.execute(
                "INSERT OR IGNORE INTO doc_aliases (doc_id, alias) VALUES (?1, ?2)",
                params![id, alias],
            )?;
        }
        Ok(id)
    }

    pub fn doc_hash(&self, slug: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT content_hash FROM docs WHERE slug = ?1 AND deleted_at IS NULL",
                [slug],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn get_doc_by_slug(&self, slug: &str) -> Result<Option<Doc>> {
        Ok(self
            .conn
            .query_row(
                "SELECT * FROM docs WHERE slug = ?1 AND deleted_at IS NULL",
                [slug],
                row_to_doc,
            )
            .optional()?)
    }

    pub fn get_doc_by_id(&self, id: i64) -> Result<Option<Doc>> {
        Ok(self
            .conn
            .query_row(
                "SELECT * FROM docs WHERE id = ?1 AND deleted_at IS NULL",
                [id],
                row_to_doc,
            )
            .optional()?)
    }

    /// Bump the recall counter for each live doc addressed by slug: increments
    /// `recall_count` and sets `last_recalled_at = now`. Runtime state, so it is
    /// deliberately NOT touched by `sync` (upsert only sets frontmatter-derived
    /// columns). Unknown/deleted slugs are silently skipped. Returns the number
    /// of rows updated.
    pub fn bump_recall(&self, slugs: &[String], now: &str) -> Result<usize> {
        let mut updated = 0usize;
        for slug in slugs {
            updated += self.conn.execute(
                "UPDATE docs
                 SET recall_count = recall_count + 1, last_recalled_at = ?2
                 WHERE slug = ?1 AND deleted_at IS NULL",
                params![slug, now],
            )?;
        }
        Ok(updated)
    }

    /// Resolve a name to a doc via exact slug, then case-insensitive title,
    /// then alias (gbrain's "alias hop").
    pub fn resolve_doc(&self, name: &str) -> Result<Option<Doc>> {
        if let Some(p) = self.get_doc_by_slug(name)? {
            return Ok(Some(p));
        }
        let by_title = self
            .conn
            .query_row(
                "SELECT * FROM docs WHERE title = ?1 COLLATE NOCASE AND deleted_at IS NULL",
                [name],
                row_to_doc,
            )
            .optional()?;
        if by_title.is_some() {
            return Ok(by_title);
        }
        Ok(self
            .conn
            .query_row(
                "SELECT p.* FROM docs p JOIN doc_aliases a ON a.doc_id = p.id
                 WHERE a.alias = ?1 AND p.deleted_at IS NULL",
                [name],
                row_to_doc,
            )
            .optional()?)
    }

    /// Soft-delete every live doc whose slug is NOT in `live_slugs`, purging
    /// its chunks (and index rows) so deleted content can't surface in search.
    /// Returns (slug, content_hash) of each soft-deleted doc — the hash feeds
    /// rename detection in the sync layer.
    pub fn soft_delete_missing(
        &self,
        live_slugs: &[String],
        now: &str,
    ) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, slug, content_hash FROM docs WHERE deleted_at IS NULL")?;
        let existing: Vec<(i64, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);

        let live: std::collections::HashSet<&str> = live_slugs.iter().map(|s| s.as_str()).collect();
        let mut deleted = Vec::new();
        for (id, slug, hash) in existing {
            if !live.contains(slug.as_str()) {
                self.conn.execute(
                    "UPDATE docs SET deleted_at = ?1 WHERE id = ?2",
                    params![now, id],
                )?;
                self.delete_chunks_for_page(id)?;
                self.conn
                    .execute("DELETE FROM links WHERE src_doc_id = ?1", [id])?;
                deleted.push((slug, hash));
            }
        }
        Ok(deleted)
    }
}
