pub mod chunker;
pub mod frontmatter;
pub mod linker;

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::embed::Embedder;
use crate::error::Result;
use crate::model::ChunkDraft;
use crate::store::pages::PageUpsert;
use crate::store::Store;

/// Files larger than this are skipped (and reported) instead of chunked and
/// embedded — an accidental export shouldn't take the sync down with it.
pub const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncIssue {
    pub path: String,
    pub message: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SyncReport {
    pub scanned: usize,
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub deleted: usize,
    /// Delete+add pairs with identical content recognized as renames; inbound
    /// links were repointed to the new slug.
    pub renamed: usize,
    pub links_resolved: usize,
    /// Per-file problems. Sync is total: a bad file is skipped and reported,
    /// never allowed to abort the rest of the run.
    #[serde(default)]
    pub errors: Vec<SyncIssue>,
}

/// Sync the knowledge repo (markdown files under `brain_root`) into the store:
/// scan → content-hash diff → parse/chunk/embed/link per changed file (one
/// transaction each) → soft-delete pages missing from disk → rename detection
/// → re-resolve dangling links. Markdown on disk is the source of truth.
///
/// Per-file failures (bad encoding, malformed frontmatter, oversized files)
/// are collected into `SyncReport.errors`; they never abort the sync, and the
/// failing file's existing page (if any) is left untouched rather than
/// soft-deleted.
pub fn sync_dir(store: &Store, embedder: &dyn Embedder) -> Result<SyncReport> {
    let root = store.brain_root.clone();
    let mut report = SyncReport::default();
    let mut live_slugs = Vec::new();
    // hash -> slugs added this run, for rename detection.
    let mut added_hashes: HashMap<String, Vec<String>> = HashMap::new();
    let now = chrono::Utc::now().to_rfc3339();

    for entry in WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !is_hidden(e.file_name().to_str().unwrap_or("")))
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file()
            || !entry
                .path()
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        {
            continue;
        }
        let slug = match slug_for(&root, entry.path()) {
            Some(s) => s,
            None => continue,
        };
        report.scanned += 1;
        // Registered as live before any fallible work: a file we fail to read
        // must not be soft-deleted as "missing".
        live_slugs.push(slug.clone());

        if entry.metadata().map(|m| m.len()).unwrap_or(0) > MAX_FILE_BYTES {
            report.errors.push(SyncIssue {
                path: entry.path().display().to_string(),
                message: format!("file exceeds {MAX_FILE_BYTES} bytes, skipped"),
            });
            continue;
        }
        let content = match std::fs::read_to_string(entry.path()) {
            Ok(c) => c,
            Err(e) => {
                report.errors.push(SyncIssue {
                    path: entry.path().display().to_string(),
                    message: format!("unreadable (not UTF-8?): {e}"),
                });
                continue;
            }
        };
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let is_new = match store.page_hash(&slug)? {
            Some(h) if h == hash => {
                report.unchanged += 1;
                continue;
            }
            Some(_) => false,
            None => true,
        };
        match sync_one(store, embedder, &slug, &content, &hash, &now, entry.path()) {
            Ok(()) => {
                if is_new {
                    report.added += 1;
                    added_hashes.entry(hash).or_default().push(slug.clone());
                } else {
                    report.updated += 1;
                }
            }
            Err(e) => report.errors.push(SyncIssue {
                path: entry.path().display().to_string(),
                message: e.to_string(),
            }),
        }
    }

    for warning in case_collisions(&live_slugs) {
        report.errors.push(SyncIssue {
            path: warning,
            message: "slugs differ only by case — they collide on case-insensitive filesystems"
                .to_string(),
        });
    }

    let deleted = store.soft_delete_missing(&live_slugs, &now)?;
    report.deleted = deleted.len();

    // Rename detection: a soft-deleted page whose content hash matches exactly
    // one page added this run is a move — repoint inbound links to the new
    // slug. Ambiguous matches (duplicate content) are left dangling: never guess.
    let mut deleted_by_hash: HashMap<&str, Vec<&str>> = HashMap::new();
    for (slug, hash) in &deleted {
        deleted_by_hash
            .entry(hash.as_str())
            .or_default()
            .push(slug.as_str());
    }
    for (hash, old_slugs) in deleted_by_hash {
        if let ([old_slug], Some([new_slug])) = (
            old_slugs.as_slice(),
            added_hashes.get(hash).map(|v| v.as_slice()),
        ) {
            store.rewrite_link_targets(old_slug, new_slug)?;
            report.renamed += 1;
        }
    }

    report.links_resolved = store.resolve_dangling_links()?;
    store.set_meta("last_synced_at", &now)?;
    Ok(report)
}

/// Sync one file: parse frontmatter, chunk, embed, extract links — all
/// persisted atomically per file.
pub fn sync_one(
    store: &Store,
    embedder: &dyn Embedder,
    slug: &str,
    content: &str,
    hash: &str,
    now: &str,
    path: &Path,
) -> Result<()> {
    let (fm, body) = frontmatter::parse(&path.display().to_string(), content)?;
    let title = fm.title.clone().unwrap_or_else(|| title_from_slug(slug));
    let source_dir = slug.split('/').next().unwrap_or("").to_string();
    let updated_at = fm.updated.clone().or_else(|| file_mtime_iso(path));

    let mut drafts = chunker::chunk(&body);
    if drafts.is_empty() {
        // Frontmatter-only page: index the title as its single chunk so the
        // page is still reachable through lexical and vector search.
        drafts.push(ChunkDraft {
            heading_path: String::new(),
            text: title.clone(),
        });
    }
    let texts: Vec<&str> = drafts.iter().map(|d| d.text.as_str()).collect();
    let embeddings = embedder.embed_batch(&texts)?;

    let page_dir = slug.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let links = linker::extract_links(&body, page_dir);

    store.begin()?;
    let result = (|| -> Result<()> {
        let page_id = store.upsert_page(
            &PageUpsert {
                slug,
                title: &title,
                page_type: fm.page_type.as_deref().unwrap_or("note"),
                source_dir: &source_dir,
                tags: &fm.tags,
                content_hash: hash,
                created_at: fm.created.as_deref(),
                updated_at: updated_at.as_deref(),
                aliases: &fm.aliases,
            },
            now,
        )?;
        store.replace_chunks_for_page(page_id, &title, &drafts, &embeddings)?;
        store.replace_links_for_page(page_id, &links)?;
        Ok(())
    })();
    match result {
        Ok(()) => store.commit(),
        Err(e) => {
            let _ = store.rollback();
            Err(e)
        }
    }
}

/// Slugs that differ only by case — fine on Linux, a collision on APFS/NTFS.
fn case_collisions(slugs: &[String]) -> Vec<String> {
    let mut by_lower: HashMap<String, Vec<&str>> = HashMap::new();
    for s in slugs {
        by_lower.entry(s.to_lowercase()).or_default().push(s);
    }
    let mut out: Vec<String> = by_lower
        .into_values()
        .filter(|group| group.len() > 1)
        .map(|group| group.join(" / "))
        .collect();
    out.sort();
    out
}

pub(crate) fn is_hidden(name: &str) -> bool {
    name.starts_with('.') && name != "." && name != ".."
}

pub(crate) fn slug_for(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let s = rel.to_string_lossy().replace('\\', "/");
    let stem = s
        .strip_suffix(".md")
        .or_else(|| s.strip_suffix(".MD"))
        .or_else(|| s.strip_suffix(".Md"))
        .or_else(|| s.strip_suffix(".mD"))?;
    Some(stem.to_string())
}

fn title_from_slug(slug: &str) -> String {
    let name = slug.rsplit('/').next().unwrap_or(slug);
    let spaced = name.replace(['-', '_'], " ");
    // Title Case each word.
    spaced
        .split_whitespace()
        .map(|w| {
            let mut cs = w.chars();
            match cs.next() {
                Some(f) => f.to_uppercase().collect::<String>() + cs.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn file_mtime_iso(path: &Path) -> Option<String> {
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    let dt: chrono::DateTime<chrono::Utc> = mtime.into();
    Some(dt.to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_from_slug_works() {
        assert_eq!(title_from_slug("people/garry-tan"), "Garry Tan");
        assert_eq!(title_from_slug("note_one"), "Note One");
    }

    #[test]
    fn case_collision_detection() {
        let slugs = vec![
            "people/Alice".to_string(),
            "people/alice".to_string(),
            "companies/acme".to_string(),
        ];
        let warnings = case_collisions(&slugs);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("people/Alice") && warnings[0].contains("people/alice"));
        assert!(case_collisions(&["a/b".to_string(), "a/c".to_string()]).is_empty());
    }

    #[test]
    fn uppercase_md_extension_is_synced() {
        let root = Path::new("/brain");
        assert_eq!(
            slug_for(root, Path::new("/brain/notes/x.MD")),
            Some("notes/x".into())
        );
        assert_eq!(
            slug_for(root, Path::new("/brain/notes/x.md")),
            Some("notes/x".into())
        );
        assert_eq!(slug_for(root, Path::new("/brain/notes/x.txt")), None);
    }
}





