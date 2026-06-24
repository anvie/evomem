//! Graph augmentation: after hybrid search finds seed pages, walk typed edges
//! to pull in factually-connected pages that embeddings and keywords missed,
//! and boost hits that are graph-adjacent to other hits.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::api::GraphEdgeDto;
use crate::config::SourceTiers;
use crate::error::Result;
use crate::store::Store;

/// Score multiplier for candidates adjacent to another seed page.
const ADJACENCY_BOOST: f32 = 1.15;
/// Injected neighbor score = INJECT_ALPHA * max_seed_score * DECAY^hop.
const INJECT_ALPHA: f32 = 0.5;
const DECAY: f32 = 0.6;
/// Hub guard: expand at most this many edges per node during augmentation,
/// preferring typed edges over plain `mentions` when truncating. A daily-log
/// hub with thousands of inbound mentions must not flood BFS.
const MAX_NEIGHBORS_PER_NODE: usize = 64;
/// Hub guard: cap on pages injected into the candidate set per query.
const MAX_INJECTED: usize = 32;

/// Truncate a neighbor list for BFS expansion, keeping typed edges first.
fn cap_neighbors(
    mut edges: Vec<crate::store::links::EdgeRow>,
) -> Vec<crate::store::links::EdgeRow> {
    if edges.len() > MAX_NEIGHBORS_PER_NODE {
        edges.sort_by_key(|e| e.edge_type == "mentions"); // typed first (stable)
        edges.truncate(MAX_NEIGHBORS_PER_NODE);
    }
    edges
}

/// A page pulled in (or boosted) by traversal.
#[derive(Debug, Clone)]
pub struct GraphResult {
    pub page_id: i64,
    pub score: f32,
    pub injected: bool,
}

/// BFS from seed pages over typed edges up to `hops`. Returns adjusted scores
/// for seed pages (adjacency boost) plus injected neighbors with decayed
/// scores. `seed_scores` maps page_id -> current best fused score.
pub fn augment(
    store: &Store,
    seed_scores: &HashMap<i64, f32>,
    hops: usize,
) -> Result<Vec<GraphResult>> {
    let max_seed = seed_scores.values().fold(0.0f32, |a, &b| a.max(b));
    let seeds: HashSet<i64> = seed_scores.keys().copied().collect();
    let mut out: Vec<GraphResult> = Vec::new();

    // hop distance from any seed
    let mut dist: HashMap<i64, usize> = seeds.iter().map(|&s| (s, 0)).collect();
    let mut queue: VecDeque<i64> = seeds.iter().copied().collect();
    let mut adjacent_to_seed: HashSet<i64> = HashSet::new();

    while let Some(page) = queue.pop_front() {
        let d = dist[&page];
        if d >= hops {
            continue;
        }
        for edge in cap_neighbors(store.neighbors(page, None)?) {
            let other = if edge.src_page_id == page {
                match edge.dst_page_id {
                    Some(id) => id,
                    None => continue,
                }
            } else {
                edge.src_page_id
            };
            if seeds.contains(&other) && d == 0 {
                adjacent_to_seed.insert(other);
                adjacent_to_seed.insert(page);
            }
            if let std::collections::hash_map::Entry::Vacant(e) = dist.entry(other) {
                e.insert(d + 1);
                queue.push_back(other);
            }
        }
    }

    for (&page_id, &score) in seed_scores {
        let boosted = if adjacent_to_seed.contains(&page_id) {
            score * ADJACENCY_BOOST
        } else {
            score
        };
        out.push(GraphResult {
            page_id,
            score: boosted,
            injected: false,
        });
    }
    // Closest (then lowest-id) non-seed pages first; cap total injections.
    let mut injectable: Vec<(i64, usize)> = dist
        .iter()
        .filter(|(_, &d)| d > 0)
        .map(|(&id, &d)| (id, d))
        .collect();
    injectable.sort_by_key(|&(id, d)| (d, id));
    let mut injected = 0;
    for (page_id, d) in injectable {
        if injected >= MAX_INJECTED {
            break;
        }
        // Skip hard-excluded sources even when graph-connected.
        if let Some(p) = store.get_page_by_id(page_id)? {
            if SourceTiers::is_excluded(&p.source_dir) {
                continue;
            }
        } else {
            continue;
        }
        let score = INJECT_ALPHA * max_seed * DECAY.powi(d as i32 - 1);
        out.push(GraphResult {
            page_id,
            score,
            injected: true,
        });
        injected += 1;
    }
    Ok(out)
}

/// Multi-hop traversal for `graph-query`: BFS from a start page, optionally
/// filtered by edge type, returning the discovered edges with hop counts.
pub fn traverse(
    store: &Store,
    start_page_id: i64,
    edge_type: Option<&str>,
    hops: usize,
) -> Result<Vec<GraphEdgeDto>> {
    let mut dist: HashMap<i64, usize> = HashMap::from([(start_page_id, 0)]);
    let mut queue = VecDeque::from([start_page_id]);
    let mut edges = Vec::new();
    let mut seen_edges: HashSet<(i64, String, String)> = HashSet::new();

    while let Some(page) = queue.pop_front() {
        let d = dist[&page];
        if d >= hops {
            continue;
        }
        for edge in store.neighbors(page, edge_type)? {
            let key = (
                edge.src_page_id,
                edge.dst_slug.clone(),
                edge.edge_type.clone(),
            );
            if seen_edges.insert(key) {
                edges.push(GraphEdgeDto {
                    src_slug: edge.src_slug.clone(),
                    dst_slug: edge.dst_slug.clone(),
                    edge_type: edge.edge_type.clone(),
                    hop: d + 1,
                });
            }
            // Dangling destinations are reported above but never traversed.
            let other = if edge.src_page_id == page {
                match edge.dst_page_id {
                    Some(id) => id,
                    None => continue,
                }
            } else {
                edge.src_page_id
            };
            if let std::collections::hash_map::Entry::Vacant(e) = dist.entry(other) {
                e.insert(d + 1);
                queue.push_back(other);
            }
        }
    }
    Ok(edges)
}
