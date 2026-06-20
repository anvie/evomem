use rusqlite::{params, OptionalExtension};

use crate::error::Result;
use crate::model::Page;

use super::Store;

/// Everything needed to upsert one page (metadata only; chunks/links separate).
pub struct PageUpsert<'a> {
    pub slug: &'a str,
    pub title: &'a str,
    pub page_type: &'a str,
    pub source_dir: &'a str,
    pub tags: &'a [String],
    pub content_hash: &'a str,
    pub created_at: Option<&'a str>,
    pub updated_at: Option<&'a str>,
    pub aliases: &'a [String],
}

fn row_to_page(r: &rusqlite::Row) -> rusqlite::Result<Page> {
    let tags_json: String = r.get("tags")?;
    Ok(Page {
        id: r.get("id")?,
        slug: r.get("slug")?,
        title: r.get("title")?,
        page_type: r.get("page_type")?,
        source_dir: r.get("source_dir")?,
        tags: serde_json::from_str(&tags_json).unwrap_or_default(),
        content_hash: r.get("content_hash")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
        synced_at: r.get("synced_at")?,
        deleted_at: r.get("deleted_at")?,
    })
}

impl Store {
    /// Insert or update a page row (revives soft-deleted pages) and reproject
    /// its aliases. Returns the page id.
    pub fn upsert_page(&self, p: &PageUpsert, now: &str) -> Result<i64> {
        let tags = serde_json::to_string(p.tags).unwrap_or_else(|_| "[]".into());
        self.conn.execute(
            "INSERT INTO pages (slug, title, page_type, source_dir, tags, content_hash,
                                created_at, updated_at, synced_at, deleted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL)
             ON CONFLICT(slug) DO UPDATE SET
               title = excluded.title, page_type = excluded.page_type,
               source_dir = excluded.source_dir, tags = excluded.tags,
               content_hash = excluded.content_hash, created_at = excluded.created_at,
               updated_at = excluded.updated_at, synced_at = excluded.synced_at,
               deleted_at = NULL",
            params![
                p.slug,
                p.title,
                p.page_type,
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
                .query_row("SELECT id FROM pages WHERE slug = ?1", [p.slug], |r| {
                    r.get(0)
                })?;
        self.conn
            .execute("DELETE FROM page_aliases WHERE page_id = ?1", [id])?;
        for alias in p.aliases {
            self.conn.execute(
                "INSERT OR IGNORE INTO page_aliases (page_id, alias) VALUES (?1, ?2)",
                params![id, alias],
            )?;
        }
        Ok(id)
    }

    pub fn page_hash(&self, slug: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT content_hash FROM pages WHERE slug = ?1 AND deleted_at IS NULL",
                [slug],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn get_page_by_slug(&self, slug: &str) -> Result<Option<Page>> {
        Ok(self
            .conn
            .query_row(
                "SELECT * FROM pages WHERE slug = ?1 AND deleted_at IS NULL",
                [slug],
                row_to_page,
            )
            .optional()?)
    }

    pub fn get_page_by_id(&self, id: i64) -> Result<Option<Page>> {
        Ok(self
            .conn
            .query_row(
                "SELECT * FROM pages WHERE id = ?1 AND deleted_at IS NULL",
                [id],
                row_to_page,
            )
            .optional()?)
    }

    /// Resolve a name to a page via exact slug, then case-insensitive title,
    /// then alias (gbrain's "alias hop").
    pub fn resolve_page(&self, name: &str) -> Result<Option<Page>> {
        if let Some(p) = self.get_page_by_slug(name)? {
            return Ok(Some(p));
        }
        let by_title = self
            .conn
            .query_row(
                "SELECT * FROM pages WHERE title = ?1 COLLATE NOCASE AND deleted_at IS NULL",
                [name],
                row_to_page,
            )
            .optional()?;
        if by_title.is_some() {
            return Ok(by_title);
        }
        Ok(self
            .conn
            .query_row(
                "SELECT p.* FROM pages p JOIN page_aliases a ON a.page_id = p.id
                 WHERE a.alias = ?1 AND p.deleted_at IS NULL",
                [name],
                row_to_page,
            )
            .optional()?)
    }

    /// Soft-delete every live page whose slug is NOT in `live_slugs`, purging
    /// its chunks (and index rows) so deleted content can't surface in search.
    /// Returns (slug, content_hash) of each soft-deleted page — the hash feeds
    /// rename detection in the sync layer.
    pub fn soft_delete_missing(
        &self,
        live_slugs: &[String],
        now: &str,
    ) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, slug, content_hash FROM pages WHERE deleted_at IS NULL")?;
        let existing: Vec<(i64, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);

        let live: std::collections::HashSet<&str> = live_slugs.iter().map(|s| s.as_str()).collect();
        let mut deleted = Vec::new();
        for (id, slug, hash) in existing {
            if !live.contains(slug.as_str()) {
                self.conn.execute(
                    "UPDATE pages SET deleted_at = ?1 WHERE id = ?2",
                    params![now, id],
                )?;
                self.delete_chunks_for_page(id)?;
                self.conn
                    .execute("DELETE FROM links WHERE src_page_id = ?1", [id])?;
                deleted.push((slug, hash));
            }
        }
        Ok(deleted)
    }
}





