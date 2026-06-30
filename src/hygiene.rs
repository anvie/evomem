//! Memory hygiene — the `consolidate` pass.
//!
//! `capture` and hand-written notes accrete near-duplicates: the same fact
//! recorded five times with slightly different wording. Left alone they crowd
//! the top-k (especially under the hash embedder, where near-identical text
//! yields near-identical vectors) and make the corpus noisier over time.
//!
//! `consolidate` is a deterministic post-sync maintenance pass: it groups live
//! docs of the same `type` whose token sets overlap at or above a Jaccard
//! threshold, keeps the **newest** doc of each group as the survivor, and marks
//! the older ones `superseded_by` it. Superseded docs remain on disk and in the
//! database (history is preserved) but are filtered out of retrieval.
//!
//! The pass is idempotent: it clears prior supersessions and recomputes from
//! the full live set each run, so the result depends only on disk content —
//! the same corpus always consolidates the same way.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::store::Store;
use crate::text::tokenize;

/// One older doc folded into a newer survivor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MergePair {
    /// Slug of the newer doc kept in retrieval.
    pub survivor: String,
    /// Slug of the older near-duplicate now hidden.
    pub duplicate: String,
    /// Jaccard token overlap that triggered the merge (threshold..=1.0).
    pub score: f64,
}

/// Outcome of a consolidate run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConsolidateReport {
    /// Live docs examined.
    pub scanned: usize,
    /// Jaccard threshold used.
    pub threshold: f64,
    /// Whether this was a preview (no database writes).
    pub dry_run: bool,
    /// Each (survivor, duplicate) folding, newest-survivor first.
    pub merged: Vec<MergePair>,
}

/// Fold same-type near-duplicate docs into their newest member.
///
/// `threshold` is the minimum Jaccard token overlap (clamped to `0.0..=1.0`)
/// for two docs to be considered duplicates. With `dry_run`, the merges are
/// computed and returned but no `superseded_by` is written — a safe preview.
pub fn consolidate(
    store: &Store,
    threshold: f64,
    dry_run: bool,
    source_dir: Option<&str>,
) -> Result<ConsolidateReport> {
    let threshold = threshold.clamp(0.0, 1.0);
    let mut docs = store.live_doc_texts()?;
    // Optional single-layer scope: an automated caller folds only volatile
    // captures (e.g. `memory`) and never touches hand-authored notes/entities.
    if let Some(dir) = source_dir {
        docs.retain(|d| d.source_dir == dir);
    }
    // Newest first: the survivor of each duplicate group is the freshest doc.
    // Ties (equal/absent timestamps) break by higher id — also "newer".
    docs.sort_by(|a, b| {
        ts(b.updated_at.as_deref())
            .cmp(&ts(a.updated_at.as_deref()))
            .then(b.id.cmp(&a.id))
    });

    let token_sets: Vec<HashSet<String>> = docs
        .iter()
        .map(|d| tokenize(&d.text).into_iter().collect())
        .collect();

    let mut consumed = vec![false; docs.len()];
    let mut merged = Vec::new();
    for i in 0..docs.len() {
        if consumed[i] || token_sets[i].is_empty() {
            continue;
        }
        for j in (i + 1)..docs.len() {
            // Fold only within the same type AND the same source_dir: a private
            // memory/ doc must never be folded into a knowledge note (both are
            // type `note`) or vice versa — they are separate layers.
            if consumed[j]
                || token_sets[j].is_empty()
                || docs[j].doc_type != docs[i].doc_type
                || docs[j].source_dir != docs[i].source_dir
            {
                continue;
            }
            let score = jaccard(&token_sets[i], &token_sets[j]);
            if score >= threshold {
                consumed[j] = true; // older doc folded into the newer survivor i
                merged.push((
                    docs[i].id,
                    docs[j].id,
                    MergePair {
                        survivor: docs[i].slug.clone(),
                        duplicate: docs[j].slug.clone(),
                        score,
                    },
                ));
            }
        }
    }

    if !dry_run {
        // Reset only the scope this run owns: a single-layer run recomputes its
        // own source_dir and leaves another layer's folds intact.
        match source_dir {
            Some(dir) => store.clear_supersessions_in(dir)?,
            None => store.clear_supersessions()?,
        };
        for (survivor_id, duplicate_id, _) in &merged {
            store.set_superseded(*duplicate_id, *survivor_id)?;
        }
    }

    Ok(ConsolidateReport {
        scanned: docs.len(),
        threshold,
        dry_run,
        merged: merged.into_iter().map(|(_, _, p)| p).collect(),
    })
}

/// Jaccard similarity of two token sets: |A ∩ B| / |A ∪ B|, in `0.0..=1.0`.
/// Two empty sets are treated as dissimilar (0.0) — callers skip empties.
fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count();
    let union = a.len() + b.len() - inter;
    if union == 0 {
        0.0
    } else {
        inter as f64 / union as f64
    }
}

/// Parse an `updated_at` to a comparable epoch-seconds value for recency
/// ordering. Unparseable/absent timestamps sort oldest (`i64::MIN`).
fn ts(s: Option<&str>) -> i64 {
    let Some(s) = s else {
        return i64::MIN;
    };
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return dt.timestamp();
    }
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|ndt| ndt.and_utc().timestamp())
        .unwrap_or(i64::MIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jaccard_basic() {
        let a: HashSet<String> = ["alice", "works", "acme"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let b: HashSet<String> = ["alice", "works", "acme"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(jaccard(&a, &b), 1.0);
        let c: HashSet<String> = ["bob", "plays", "chess"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(jaccard(&a, &c), 0.0);
        let d: HashSet<String> = ["alice", "works", "globex"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // intersection {alice, works} = 2, union {alice,works,acme,globex} = 4
        assert_eq!(jaccard(&a, &d), 0.5);
        assert_eq!(jaccard(&a, &HashSet::new()), 0.0);
    }

    #[test]
    fn ts_orders_dates_and_rfc3339() {
        assert!(ts(Some("2026-06-13")) < ts(Some("2026-06-14")));
        assert!(ts(Some("2026-06-13T10:00:00Z")) < ts(Some("2026-06-13T11:00:00Z")));
        assert_eq!(ts(None), i64::MIN);
        assert_eq!(ts(Some("not a date")), i64::MIN);
    }

    // A near-duplicate that lives in a different source_dir must never be folded:
    // a private `memory/` doc and a root knowledge note are both type `note`, so
    // only the source_dir guard keeps consolidate from crossing the two layers.
    #[test]
    fn consolidate_never_folds_across_source_dirs() {
        use crate::model::ChunkDraft;
        use crate::store::docs::DocUpsert;
        use crate::store::Store;

        let dir = tempfile::tempdir().unwrap();
        let store = Store::init(dir.path(), "test-embedder", 4).unwrap();

        let put = |slug: &str, source_dir: &str, updated: &str| {
            let id = store
                .upsert_page(
                    &DocUpsert {
                        slug,
                        title: "Shared",
                        doc_type: "note",
                        source_dir,
                        tags: &[],
                        content_hash: slug,
                        created_at: None,
                        updated_at: Some(updated),
                        aliases: &[],
                    },
                    updated,
                )
                .unwrap();
            store
                .replace_chunks_for_page(
                    id,
                    "Shared",
                    &[ChunkDraft {
                        heading_path: String::new(),
                        text: "fact alpha beta gamma delta".into(),
                    }],
                    &[vec![0.0; 4]],
                )
                .unwrap();
        };

        // Two identical memory docs (newer survives) + one identical root note.
        put("memory/m1", "memory", "2026-06-13T00:00:00Z");
        put("memory/m2", "memory", "2026-06-14T00:00:00Z");
        put("rootnote", "", "2026-06-15T00:00:00Z");

        let report = consolidate(&store, 0.85, false, None).unwrap();
        // Exactly one fold — both ends inside `memory/`; the root note is untouched
        // even though its text is identical, because its source_dir differs.
        assert_eq!(report.merged.len(), 1, "only the same-source_dir pair folds");
        let p = &report.merged[0];
        assert_eq!(p.survivor, "memory/m2");
        assert_eq!(p.duplicate, "memory/m1");
        assert!(
            !report
                .merged
                .iter()
                .any(|m| m.survivor == "rootnote" || m.duplicate == "rootnote"),
            "a root knowledge note must never fold with a memory doc"
        );
    }

    // A source_dir-scoped run folds ONLY that layer: pointed at `memory`, it folds
    // the memory near-dupes and never looks at entities — the safety the automated
    // post-turn hook relies on so hand-authored entities are never auto-hidden.
    #[test]
    fn consolidate_scoped_to_source_dir_ignores_other_layers() {
        use crate::model::ChunkDraft;
        use crate::store::docs::DocUpsert;
        use crate::store::Store;

        let dir = tempfile::tempdir().unwrap();
        let store = Store::init(dir.path(), "test-embedder", 4).unwrap();

        let put = |slug: &str, source_dir: &str, doc_type: &str, updated: &str, body: &str| {
            let id = store
                .upsert_page(
                    &DocUpsert {
                        slug,
                        title: "T",
                        doc_type,
                        source_dir,
                        tags: &[],
                        content_hash: slug,
                        created_at: None,
                        updated_at: Some(updated),
                        aliases: &[],
                    },
                    updated,
                )
                .unwrap();
            store
                .replace_chunks_for_page(
                    id,
                    "T",
                    &[ChunkDraft {
                        heading_path: String::new(),
                        text: body.into(),
                    }],
                    &[vec![0.0; 4]],
                )
                .unwrap();
        };

        // Two near-duplicate entities (would fold at 0.85 if scanned) + two
        // near-duplicate memory docs.
        put("entities/e1", "entities", "concept", "2026-06-13T00:00:00Z", "alpha beta gamma");
        put("entities/e2", "entities", "concept", "2026-06-14T00:00:00Z", "alpha beta gamma");
        put("memory/m1", "memory", "note", "2026-06-13T00:00:00Z", "delta epsilon zeta");
        put("memory/m2", "memory", "note", "2026-06-14T00:00:00Z", "delta epsilon zeta");

        let report = consolidate(&store, 0.85, false, Some("memory")).unwrap();
        assert_eq!(report.scanned, 2, "only memory/ docs were scanned");
        assert_eq!(report.merged.len(), 1, "only the memory pair folds");
        assert_eq!(report.merged[0].survivor, "memory/m2");
        assert_eq!(report.merged[0].duplicate, "memory/m1");
        assert!(
            !report
                .merged
                .iter()
                .any(|m| m.survivor.starts_with("entities/") || m.duplicate.starts_with("entities/")),
            "a memory-scoped run must never fold an entity"
        );
    }
}
