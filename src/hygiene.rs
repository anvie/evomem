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
pub fn consolidate(store: &Store, threshold: f64, dry_run: bool) -> Result<ConsolidateReport> {
    let threshold = threshold.clamp(0.0, 1.0);
    let mut docs = store.live_doc_texts()?;
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
            if consumed[j] || token_sets[j].is_empty() || docs[j].doc_type != docs[i].doc_type {
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
        store.clear_supersessions()?;
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
}
