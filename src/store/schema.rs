use rusqlite::Connection;

use crate::error::Result;

pub const SCHEMA_VERSION: i64 = 5;

const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS docs (
  id           INTEGER PRIMARY KEY,
  slug         TEXT NOT NULL UNIQUE,
  title        TEXT NOT NULL,
  doc_type     TEXT NOT NULL DEFAULT 'note',
  source_dir   TEXT NOT NULL DEFAULT '',
  tags         TEXT NOT NULL DEFAULT '[]',
  content_hash TEXT NOT NULL,
  created_at   TEXT,
  updated_at   TEXT,
  synced_at    TEXT NOT NULL,
  deleted_at   TEXT,
  superseded_by INTEGER REFERENCES docs(id)
);
CREATE INDEX IF NOT EXISTS idx_docs_live ON docs(deleted_at) WHERE deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS doc_aliases (
  doc_id INTEGER NOT NULL REFERENCES docs(id) ON DELETE CASCADE,
  alias   TEXT NOT NULL COLLATE NOCASE,
  PRIMARY KEY (alias, doc_id)
);

CREATE TABLE IF NOT EXISTS chunks (
  id           INTEGER PRIMARY KEY,
  doc_id      INTEGER NOT NULL REFERENCES docs(id) ON DELETE CASCADE,
  chunk_index  INTEGER NOT NULL,
  heading_path TEXT NOT NULL DEFAULT '',
  text         TEXT NOT NULL,
  embedding    BLOB NOT NULL,
  UNIQUE(doc_id, chunk_index)
);

CREATE TABLE IF NOT EXISTS links (
  src_doc_id INTEGER NOT NULL REFERENCES docs(id) ON DELETE CASCADE,
  dst_slug    TEXT NOT NULL,
  dst_doc_id INTEGER REFERENCES docs(id),
  edge_type   TEXT NOT NULL DEFAULT 'mentions',
  anchor_text TEXT,
  PRIMARY KEY (src_doc_id, dst_slug, edge_type)
);
CREATE INDEX IF NOT EXISTS idx_links_dst ON links(dst_doc_id);

-- Trust layer: per-doc provenance & freshness, projected from frontmatter on
-- every sync (1:1 with docs, dropped with the doc via FK cascade).
CREATE TABLE IF NOT EXISTS provenance (
  doc_id           INTEGER PRIMARY KEY REFERENCES docs(id) ON DELETE CASCADE,
  source           TEXT,
  confidence       REAL,
  verified_at      TEXT,
  stale_after_days INTEGER
);

-- Contradiction tracking: flagged conflicts between two knowledge items
-- (addressed by slug), resolvable by id. Items are stored sorted (a <= b) so
-- a pair is recorded once regardless of order; edge_type '' = unspecified.
CREATE TABLE IF NOT EXISTS contradictions (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  item_a      TEXT NOT NULL,
  item_b      TEXT NOT NULL,
  edge_type   TEXT NOT NULL DEFAULT '',
  description TEXT NOT NULL DEFAULT '',
  status      TEXT NOT NULL DEFAULT 'open',
  resolution  TEXT,
  resolved_by TEXT,
  created_at  TEXT NOT NULL,
  resolved_at TEXT,
  UNIQUE(item_a, item_b, edge_type)
);
CREATE INDEX IF NOT EXISTS idx_contradictions_open ON contradictions(status);

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
    // Additive upgrades for databases created before a column existed. New
    // tables come from the CREATE-IF-NOT-EXISTS DDL above; only added columns
    // need a guarded ALTER (SQLite has no `ADD COLUMN IF NOT EXISTS`).
    add_column_if_missing(conn, "docs", "superseded_by", "INTEGER REFERENCES docs(id)")?;
    // Recall tracking (agent-memory-layers): how many times a doc was actually
    // surfaced into an agent's recall context, and when last. Runtime state —
    // NOT projected from frontmatter, so `sync`'s upsert (which only SETs the
    // frontmatter-derived columns) never resets these. Feeds the Auto Dream
    // prune phase ("stale + never recalled" → drop candidate).
    add_column_if_missing(conn, "docs", "recall_count", "INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(conn, "docs", "last_recalled_at", "TEXT")?;
    conn.execute(
        "INSERT INTO meta(key, value) VALUES ('schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [SCHEMA_VERSION.to_string()],
    )?;
    Ok(())
}

/// Add `<table>.<column> <decl>` only when the column is absent — an
/// idempotent stand-in for the `ADD COLUMN IF NOT EXISTS` SQLite lacks, so
/// reopening an older database upgrades it in place without data loss.
fn add_column_if_missing(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let exists = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .filter_map(|c| c.ok())
        .any(|c| c == column);
    drop(stmt);
    if !exists {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl};"))?;
    }
    Ok(())
}
