use rusqlite::Connection;

use crate::error::Result;

pub const SCHEMA_VERSION: i64 = 1;

const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS pages (
  id           INTEGER PRIMARY KEY,
  slug         TEXT NOT NULL UNIQUE,
  title        TEXT NOT NULL,
  page_type    TEXT NOT NULL DEFAULT 'note',
  source_dir   TEXT NOT NULL DEFAULT '',
  tags         TEXT NOT NULL DEFAULT '[]',
  content_hash TEXT NOT NULL,
  created_at   TEXT,
  updated_at   TEXT,
  synced_at    TEXT NOT NULL,
  deleted_at   TEXT
);
CREATE INDEX IF NOT EXISTS idx_pages_live ON pages(deleted_at) WHERE deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS page_aliases (
  page_id INTEGER NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
  alias   TEXT NOT NULL COLLATE NOCASE,
  PRIMARY KEY (alias, page_id)
);

CREATE TABLE IF NOT EXISTS chunks (
  id           INTEGER PRIMARY KEY,
  page_id      INTEGER NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
  chunk_index  INTEGER NOT NULL,
  heading_path TEXT NOT NULL DEFAULT '',
  text         TEXT NOT NULL,
  embedding    BLOB NOT NULL,
  UNIQUE(page_id, chunk_index)
);

CREATE TABLE IF NOT EXISTS links (
  src_page_id INTEGER NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
  dst_slug    TEXT NOT NULL,
  dst_page_id INTEGER REFERENCES pages(id),
  edge_type   TEXT NOT NULL DEFAULT 'mentions',
  anchor_text TEXT,
  PRIMARY KEY (src_page_id, dst_slug, edge_type)
);
CREATE INDEX IF NOT EXISTS idx_links_dst ON links(dst_page_id);

-- Inverted index for Meilisearch-style lexical ranking (no FTS5/BM25).
-- attr: 0 = title, 1 = heading_path, 2 = body text.
CREATE TABLE IF NOT EXISTS word_index (
  word     TEXT NOT NULL,
  chunk_id INTEGER NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
  attr     INTEGER NOT NULL,
  pos      INTEGER NOT NULL,
  PRIMARY KEY (word, chunk_id, attr, pos)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS idx_word_index_chunk ON word_index(chunk_id);
"#;

pub fn migrate(conn: &Connection) -> Result<()> {
    // busy_timeout: a CLI sync against a DB the server holds open must wait
    // for the writer, not fail instantly with SQLITE_BUSY.
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;",
    )?;
    conn.execute_batch(DDL)?;
    conn.execute(
        "INSERT INTO meta(key, value) VALUES ('schema_version', ?1)
         ON CONFLICT(key) DO NOTHING",
        [SCHEMA_VERSION.to_string()],
    )?;
    Ok(())
}
