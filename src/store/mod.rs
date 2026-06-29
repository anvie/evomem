pub mod chunks;
pub mod docs;
pub mod hygiene;
pub mod links;
pub mod schema;
pub mod words;

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::error::{EvoError, Result};

/// Handle to the embedded SQLite database living inside the knowledge root.
pub struct Store {
    pub(crate) conn: Connection,
    pub brain_root: PathBuf,
}

impl Store {
    /// Create or open the database at `<brain_root>/.evomem.db` and run migrations.
    pub fn init(brain_root: &Path, embedder_id: &str, embed_dim: usize) -> Result<Store> {
        std::fs::create_dir_all(brain_root)?;
        let conn = Connection::open(brain_root.join(crate::config::DB_FILE))?;
        schema::migrate(&conn)?;
        let store = Store {
            conn,
            brain_root: brain_root.to_path_buf(),
        };
        store.set_meta("brain_root", &brain_root.to_string_lossy())?;
        store.set_meta("embedder_id", embedder_id)?;
        store.set_meta("embed_dim", &embed_dim.to_string())?;
        Ok(store)
    }

    /// Open an existing knowledge store; errors if it was never initialized or the
    /// embedder doesn't match the one the database was built with.
    pub fn open(brain_root: &Path, embedder_id: &str) -> Result<Store> {
        let db = brain_root.join(crate::config::DB_FILE);
        if !db.exists() {
            return Err(EvoError::NotInitialized(brain_root.display().to_string()));
        }
        let conn = Connection::open(&db)?;
        schema::migrate(&conn)?;
        let store = Store {
            conn,
            brain_root: brain_root.to_path_buf(),
        };
        if let Some(stored) = store.get_meta("embedder_id")? {
            if stored != embedder_id {
                return Err(EvoError::EmbedderMismatch {
                    stored,
                    current: embedder_id.to_string(),
                });
            }
        }
        Ok(store)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    /// The knowledge root is meant to live in git; the database and its WAL
    /// siblings don't. Append the ignore line once, creating the file if
    /// needed. Returns true if the line was added.
    pub fn ensure_gitignore(brain_root: &Path) -> Result<bool> {
        const LINE: &str = ".evomem.db*";
        let path = brain_root.join(".gitignore");
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if existing.lines().any(|l| l.trim() == LINE) {
            return Ok(false);
        }
        let mut content = existing;
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(LINE);
        content.push('\n');
        std::fs::write(&path, content)?;
        Ok(true)
    }

    pub fn begin(&self) -> Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        Ok(())
    }

    pub fn commit(&self) -> Result<()> {
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    pub fn rollback(&self) -> Result<()> {
        self.conn.execute_batch("ROLLBACK")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ChunkDraft;

    fn test_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::init(dir.path(), "test-embedder", 4).unwrap();
        (dir, store)
    }

    #[test]
    fn word_index_is_populated_and_cascades_on_chunk_delete() {
        let (_dir, store) = test_store();
        let doc_id = store
            .upsert_page(
                &crate::store::docs::DocUpsert {
                    slug: "people/alice",
                    title: "Alice Chen",
                    doc_type: "person",
                    source_dir: "people",
                    tags: &[],
                    content_hash: "h1",
                    created_at: None,
                    updated_at: None,
                    aliases: &[],
                },
                "2026-06-13T00:00:00Z",
            )
            .unwrap();
        let drafts = vec![ChunkDraft {
            heading_path: "Career".into(),
            text: "Alice works at Acme Corp on retrieval systems.".into(),
        }];
        store
            .replace_chunks_for_page(doc_id, "Alice Chen", &drafts, &[vec![0.0; 4]])
            .unwrap();

        let vocab = store.vocabulary().unwrap();
        assert!(vocab.contains(&"alice".to_string()));
        assert!(vocab.contains(&"retrieval".to_string()));
        assert!(
            vocab.contains(&"career".to_string()),
            "heading words indexed"
        );

        let postings = store.postings("alice", &[]).unwrap();
        // "alice" appears in title (attr 0) and body (attr 2).
        assert!(postings.iter().any(|p| p.attr == 0));
        assert!(postings.iter().any(|p| p.attr == 2));

        store.delete_chunks_for_page(doc_id).unwrap();
        assert!(
            store.postings("alice", &[]).unwrap().is_empty(),
            "FK cascade cleared index"
        );
    }

    #[test]
    fn gitignore_gets_db_line_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Store::ensure_gitignore(dir.path()).unwrap());
        assert!(
            !Store::ensure_gitignore(dir.path()).unwrap(),
            "no duplicate line"
        );
        let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(content.matches(".evomem.db*").count(), 1);

        // Appends to an existing .gitignore without clobbering it.
        let dir2 = tempfile::tempdir().unwrap();
        std::fs::write(dir2.path().join(".gitignore"), "target/").unwrap();
        assert!(Store::ensure_gitignore(dir2.path()).unwrap());
        let content = std::fs::read_to_string(dir2.path().join(".gitignore")).unwrap();
        assert!(content.contains("target/") && content.contains(".evomem.db*"));
    }

    #[test]
    fn busy_timeout_is_configured() {
        let (_dir, store) = test_store();
        let timeout: i64 = store
            .conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert_eq!(timeout, 5000);
    }

    #[test]
    fn truncated_embedding_blob_is_a_loud_error() {
        let (_dir, store) = test_store();
        let doc_id = store
            .upsert_page(
                &crate::store::docs::DocUpsert {
                    slug: "n/x",
                    title: "X",
                    doc_type: "note",
                    source_dir: "n",
                    tags: &[],
                    content_hash: "h",
                    created_at: None,
                    updated_at: None,
                    aliases: &[],
                },
                "2026-06-13T00:00:00Z",
            )
            .unwrap();
        store
            .replace_chunks_for_page(
                doc_id,
                "X",
                &[ChunkDraft {
                    heading_path: String::new(),
                    text: "hello".into(),
                }],
                &[vec![0.0; 4]],
            )
            .unwrap();
        // Corrupt the blob: wrong byte length for dim 4.
        store
            .conn
            .execute("UPDATE chunks SET embedding = X'0000'", [])
            .unwrap();
        let err = store.for_each_embedding(4, |_, _| {}).unwrap_err();
        assert!(err.to_string().contains("expected 16"), "{err}");
        // Correct-length blobs stream fine.
        store
            .conn
            .execute(
                "UPDATE chunks SET embedding = X'00000000000000000000000000000000'",
                [],
            )
            .unwrap();
        let mut n = 0;
        store.for_each_embedding(4, |_, _| n += 1).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn embedder_mismatch_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        Store::init(dir.path(), "embedder-a", 8).unwrap();
        match Store::open(dir.path(), "embedder-b") {
            Err(EvoError::EmbedderMismatch { .. }) => {}
            Err(e) => panic!("wrong error: {e}"),
            Ok(_) => panic!("mismatch was accepted"),
        }
    }
}
