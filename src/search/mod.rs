pub mod fusion;
pub mod graph;
pub mod intent;
pub mod lexical;
pub mod vector;

use std::collections::HashMap;

use crate::api::{SearchHit, SearchResponse};
use crate::config::{
    self, knobs, SourceTiers, FUSED_TOP_K, HIGH_VECTOR_THRESHOLD, RRF_K, STRATEGY_TOP_K,
};
use crate::embed::Embedder;
use crate::error::Result;
use crate::model::{Doc, Evidence, Intent, Mode};
use crate::store::chunks::ChunkRow;
use crate::store::Store;
use crate::text::tokenize;

/// One candidate flowing through the late pipeline stages.
struct Candidate {
    chunk: ChunkRow,
    doc: Doc,
    score: f32,
    vector_sim: f32,
    lexical: Option<lexical::MatchStats>,
    graph_injected: bool,
    /// Trust level from provenance (0.0–1.0), filled during ranking.
    confidence: Option<f32>,
}

/// The full retrieval pipeline:
/// intent → (vector ∥ lexical) → RRF → graph augment → source-tier ranking →
/// evidence tagging → dedup by slug → token budget. Orchestration is pure;
/// latency lives in the index scans.
pub fn search(
    store: &Store,
    embedder: &dyn Embedder,
    query: &str,
    mode: Mode,
) -> Result<SearchResponse> {
    let intent = classify_intent(store, query);
    let k = knobs(mode, intent);

    // Strategy 1+2: lexical (Meilisearch-style bucket sort) and vector.
    let lex_hits = lexical::search(store, query, STRATEGY_TOP_K)?;
    let vec_hits = vector::search(store, embedder, query, STRATEGY_TOP_K)?;

    let lex_stats: HashMap<i64, lexical::MatchStats> = lex_hits
        .iter()
        .map(|h| (h.chunk_id, h.stats.clone()))
        .collect();
    let vec_sims: HashMap<i64, f32> = vec_hits.iter().copied().collect();

    // Intent tilts strategy weights: entity/event queries lean lexical+graph,
    // general leans slightly vector.
    let (w_lex, w_vec) = match intent {
        Intent::Entity | Intent::Event => (1.2, 1.0),
        Intent::Temporal => (1.0, 1.0),
        Intent::General => (1.0, 1.1),
    };
    let fused = fusion::rrf(
        &[
            (w_lex, lex_hits.iter().map(|h| h.chunk_id).collect()),
            (w_vec, vec_hits.iter().map(|(id, _)| *id).collect()),
        ],
        RRF_K,
    );
    let fused: Vec<(i64, f32)> = fused.into_iter().take(FUSED_TOP_K).collect();

    // Hydrate candidates; keep best fused score per doc for graph seeding.
    let chunk_ids: Vec<i64> = fused.iter().map(|(id, _)| *id).collect();
    let rows = store.get_chunks(&chunk_ids)?;
    let row_by_id: HashMap<i64, ChunkRow> = rows.into_iter().map(|r| (r.id, r)).collect();

    let mut candidates: Vec<Candidate> = Vec::new();
    let mut page_best: HashMap<i64, f32> = HashMap::new();
    for (chunk_id, score) in &fused {
        let Some(row) = row_by_id.get(chunk_id) else {
            continue;
        };
        let Some(doc) = store.get_doc_by_id(row.doc_id)? else {
            continue;
        };
        // Hygiene: a near-duplicate folded into a newer survivor stays out of
        // retrieval (it's kept only for history).
        if doc.superseded_by.is_some() {
            continue;
        }
        if SourceTiers::is_excluded(&doc.source_dir) {
            continue;
        }
        let best = page_best.entry(row.doc_id).or_insert(0.0);
        *best = best.max(*score);
        candidates.push(Candidate {
            chunk: row.clone(),
            doc,
            score: *score,
            vector_sim: vec_sims.get(chunk_id).copied().unwrap_or(0.0),
            lexical: lex_stats.get(chunk_id).cloned(),
            graph_injected: false,
            confidence: None,
        });
    }

    // Graph augmentation: boost adjacency, inject factually-connected docs.
    let graph_results = graph::augment(store, &page_best, k.graph_hops)?;
    for gr in graph_results {
        if gr.injected {
            if let (Some(chunk), Some(doc)) = (
                store.first_chunk_for_page(gr.doc_id)?,
                store.get_doc_by_id(gr.doc_id)?,
            ) {
                if doc.superseded_by.is_some() {
                    continue;
                }
                candidates.push(Candidate {
                    chunk,
                    doc,
                    score: gr.score,
                    vector_sim: 0.0,
                    lexical: None,
                    graph_injected: true,
                    confidence: None,
                });
            }
        } else {
            for c in candidates.iter_mut().filter(|c| c.doc.id == gr.doc_id) {
                // augment() returns the doc-level boosted score; preserve the
                // chunk's own share by applying the same multiplier.
                if page_best[&gr.doc_id] > 0.0 {
                    c.score *= gr.score / page_best[&gr.doc_id];
                }
            }
        }
    }

    // Source-aware ranking + EvoRank priors + title/alias boost + evidence.
    let q_norm = query.trim().to_lowercase();
    let q_words = tokenize(query);
    let now = chrono::Utc::now();
    let mut hits: Vec<(Candidate, Evidence)> = Vec::new();
    for mut c in candidates {
        c.score *= SourceTiers::factor(&c.doc.source_dir, intent);
        // Graph-authority prior: docs the knowledge graph points at are
        // salient (typed edges count full, plain mentions half).
        let (typed_in, mention_in) = store.in_degree(c.doc.id)?;
        let in_weight = typed_in as f32 + 0.5 * mention_in as f32;
        c.score *= 1.0 + config::AUTHORITY_WEIGHT * (1.0 + in_weight).ln();
        // Recency prior: strong for temporal queries, a whisper otherwise.
        if let Some(age_days) = doc_age_days(&c.doc, now) {
            let (amp, tau) = if intent == Intent::Temporal {
                (config::RECENCY_TEMPORAL, config::RECENCY_TEMPORAL_TAU)
            } else {
                (config::RECENCY_BASE, config::RECENCY_BASE_TAU)
            };
            c.score *= 1.0 + amp * (-age_days / tau).exp();
        }
        // Trust prior: a doc you don't trust shouldn't crowd out one you do.
        // Confidence 1.0 => neutral (×1.0), 0.0 => ×0.6; absent => neutral.
        let confidence = store.get_provenance(c.doc.id)?.and_then(|p| p.confidence);
        if let Some(conf) = confidence {
            c.score *= 0.6 + 0.4 * (conf.clamp(0.0, 1.0) as f32);
        }
        c.confidence = confidence.map(|x| x as f32);
        let aliases = doc_aliases(store, c.doc.id)?;
        let alias_hit = aliases.iter().any(|a| a.to_lowercase() == q_norm);
        let title_hit = c.doc.title.to_lowercase() == q_norm
            || (!q_norm.is_empty() && c.doc.title.to_lowercase().contains(&q_norm));
        if alias_hit {
            c.score *= 1.6;
        } else if title_hit {
            c.score *= 1.4;
        }
        let evidence = if alias_hit {
            Evidence::AliasHit
        } else if title_hit {
            Evidence::ExactTitleMatch
        } else if c
            .lexical
            .as_ref()
            .is_some_and(|s| s.words_matched as usize == q_words.len() && s.typo_count == 0)
        {
            Evidence::KeywordExact
        } else if c.vector_sim >= HIGH_VECTOR_THRESHOLD {
            Evidence::HighVectorMatch
        } else if c.graph_injected {
            Evidence::GraphAdjacent
        } else {
            Evidence::WeakSemantic
        };
        hits.push((c, evidence));
    }

    // Dedup by slug (doc max-pool: one doc = its best chunk), stable order.
    hits.sort_by(|a, b| {
        b.0.score
            .total_cmp(&a.0.score)
            .then(a.0.doc.slug.cmp(&b.0.doc.slug))
    });
    let mut seen_slugs = std::collections::HashSet::new();
    hits.retain(|(c, _)| seen_slugs.insert(c.doc.slug.clone()));

    // Token budget + result cap.
    let mut out = Vec::new();
    let mut budget = k.token_budget;
    for (i, (c, evidence)) in hits.into_iter().enumerate() {
        if out.len() >= k.max_results {
            break;
        }
        let snippet = snippet_of(&c.chunk.text);
        let cost = config::estimate_tokens(&snippet);
        if cost > budget && !out.is_empty() {
            break;
        }
        budget = budget.saturating_sub(cost);
        out.push(SearchHit {
            rank: i + 1,
            slug: c.doc.slug,
            title: c.doc.title,
            heading_path: c.chunk.heading_path,
            snippet,
            score: c.score,
            evidence,
            doc_type: c.doc.doc_type,
            source_dir: c.doc.source_dir,
            updated_at: c.doc.updated_at,
            confidence: c.confidence,
        });
    }

    Ok(SearchResponse {
        query: query.to_string(),
        intent,
        mode: format!("{mode:?}").to_lowercase(),
        hits: out,
        cached: false,
    })
}

pub fn classify_intent(store: &Store, query: &str) -> Intent {
    intent::classify(query, |span| {
        store
            .resolve_doc(span)
            .map(|p| p.is_some())
            .unwrap_or(false)
    })
}

fn doc_aliases(store: &Store, doc_id: i64) -> Result<Vec<String>> {
    let mut stmt = store
        .conn
        .prepare_cached("SELECT alias FROM doc_aliases WHERE doc_id = ?1")?;
    let rows = stmt
        .query_map([doc_id], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(rows)
}

fn doc_age_days(doc: &Doc, now: chrono::DateTime<chrono::Utc>) -> Option<f32> {
    let s = doc.updated_at.as_deref()?;
    let when = chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .ok()
        .or_else(|| {
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|ndt| chrono::DateTime::from_naive_utc_and_offset(ndt, chrono::Utc))
        })?;
    Some(((now - when).num_hours() as f32 / 24.0).max(0.0))
}

fn snippet_of(text: &str) -> String {
    const MAX: usize = 320;
    let trimmed = text.trim();
    if trimmed.len() <= MAX {
        return trimmed.to_string();
    }
    let mut end = MAX;
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &trimmed[..end])
}
