//! Contradiction detection — the `contradiction detect` pass.
//!
//! evomem's self-wiring graph already extracts typed edges from prose. This
//! pass mines those edges for conflicts: a doc that asserts the same
//! single-valued relation (see [`crate::config::FUNCTIONAL_EDGES`]) to two
//! different targets — "Alice works_at Acme" *and* "Alice works_at Globex" —
//! is a candidate contradiction. Each distinct target pair is flagged once
//! (idempotently), so re-running never duplicates and never re-opens a conflict
//! a human already resolved.
//!
//! It is deliberately conservative: most relations are legitimately
//! many-valued, so only edges in the functional set are mined. Everything else
//! goes through the manual `contradiction flag` command.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::config::FUNCTIONAL_EDGES;
use crate::error::Result;
use crate::store::Store;

/// One conflicting target pair surfaced by detection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DetectedConflict {
    /// The doc asserting the conflicting relation.
    pub subject: String,
    /// The single-valued relation in conflict.
    pub edge_type: String,
    pub item_a: String,
    pub item_b: String,
    /// True if this run created the flag (false = it already existed).
    pub new: bool,
}

/// Outcome of a detect run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DetectReport {
    /// Newly flagged conflicts (existing ones are not re-counted).
    pub flagged: usize,
    /// Every conflict the pass saw this run (new and pre-existing).
    pub conflicts: Vec<DetectedConflict>,
}

/// Scan functional typed edges for same-subject/same-relation/different-target
/// conflicts and flag each pair. Idempotent; returns what it found.
pub fn detect_contradictions(store: &Store, now: &str) -> Result<DetectReport> {
    // Group functional edges by (subject, relation) → set of distinct targets.
    let mut groups: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for (src, edge_type, dst) in store.resolved_typed_edges()? {
        if FUNCTIONAL_EDGES.contains(&edge_type.as_str()) {
            groups.entry((src, edge_type)).or_default().insert(dst);
        }
    }

    let mut report = DetectReport::default();
    for ((subject, edge_type), targets) in groups {
        if targets.len() < 2 {
            continue; // a single target is not a conflict
        }
        let targets: Vec<String> = targets.into_iter().collect();
        for i in 0..targets.len() {
            for j in (i + 1)..targets.len() {
                let (a, b) = (&targets[i], &targets[j]);
                let existed = store.contradiction_id(a, b, Some(&edge_type))?.is_some();
                let description = format!("{subject} asserts {edge_type} to both {a} and {b}");
                store.flag_contradiction(a, b, Some(&edge_type), &description, now)?;
                if !existed {
                    report.flagged += 1;
                }
                report.conflicts.push(DetectedConflict {
                    subject: subject.clone(),
                    edge_type: edge_type.clone(),
                    item_a: a.clone(),
                    item_b: b.clone(),
                    new: !existed,
                });
            }
        }
    }
    Ok(report)
}
