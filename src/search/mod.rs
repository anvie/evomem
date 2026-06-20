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
use crate::model::{Evidence, Intent, Mode, Page};
use crate::store::chunks::ChunkRow;
use crate::store::Store;
use crate::text::tokenize;

/// One candidate flowing through the late pipeline stages.
struct Candidate {
    chunk: ChunkRow,
    page: Page,
    score: f32,
    vector_sim: f32,
    lexical: Option<lexical::MatchStats>,
    graph_injected: bool,
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

    // Hydrate candidates; keep best fused score per page for graph seeding.
    let chunk_ids: Vec<i64> = fused.iter().map(|(id, _)| *id).collect();
    let rows = store.get_chunks(&chunk_ids)?;
    let row_by_id: HashMap<i64, ChunkRow> = rows.into_iter().map(|r| (r.id, r)).collect();

    let mut candidates: Vec<Candidate> = Vec::new();
    let mut page_best: HashMap<i64, f32> = HashMap::new();
    for (chunk_id, score) in &fused {
        let Some(row) = row_by_id.get(chunk_id) else {
            continue;
        };
        let Some(page) = store.get_page_by_id(row.page_id)? else {
            continue;
        };
        if SourceTiers::is_excluded(&page.source_dir) {
            continue;
        }
        let best = page_best.entry(row.page_id).or_insert(0.0);
        *best = best.max(*score);
        candidates.push(Candidate {
            chunk: row.clone(),
            page,
            score: *score,
            vector_sim: vec_sims.get(chunk_id).copied().unwrap_or(0.0),
            lexical: lex_stats.get(chunk_id).cloned(),
            graph_injected: false,
        });
    }

    // Graph augmentation: boost adjacency, inject factually-connected pages.
    let graph_results = graph::augment(store, &page_best, k.graph_hops)?;
    for gr in graph_results {
        if gr.injected {
            if let (Some(chunk), Some(page)) = (
                store.first_chunk_for_page(gr.page_id)?,
                store.get_page_by_id(gr.page_id)?,
            ) {
                candidates.push(Candidate {
                    chunk,
                    page,
                    score: gr.score,
                    vector_sim: 0.0,
                    lexical: None,
                    graph_injected: true,
                });
            }
        } else {
            for c in candidates.iter_mut().filter(|c| c.page.id == gr.page_id) {
                // augment() returns the page-level boosted score; preserve the
                // chunk's own share by applying the same multiplier.
                if page_best[&gr.page_id] > 0.0 {
                    c.score *= gr.score / page_best[&gr.page_id];
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
        c.score *= SourceTiers::factor(&c.page.source_dir, intent);
        // Graph-authority prior: pages the knowledge graph points at are
        // salient (typed edges count full, plain mentions half).
        let (typed_in, mention_in) = store.in_degree(c.page.id)?;
        let in_weight = typed_in as f32 + 0.5 * mention_in as f32;
        c.score *= 1.0 + config::AUTHORITY_WEIGHT * (1.0 + in_weight).ln();
        // Recency prior: strong for temporal queries, a whisper otherwise.
        if let Some(age_days) = page_age_days(&c.page, now) {
            let (amp, tau) = if intent == Intent::Temporal {
                (config::RECENCY_TEMPORAL, config::RECENCY_TEMPORAL_TAU)
            } else {
                (config::RECENCY_BASE, config::RECENCY_BASE_TAU)
            };
            c.score *= 1.0 + amp * (-age_days / tau).exp();
        }
        let aliases = page_aliases(store, c.page.id)?;
        let alias_hit = aliases.iter().any(|a| a.to_lowercase() == q_norm);
        let title_hit = c.page.title.to_lowercase() == q_norm
            || (!q_norm.is_empty() && c.page.title.to_lowercase().contains(&q_norm));
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

    // Dedup by slug (page max-pool: one page = its best chunk), stable order.
    hits.sort_by(|a, b| {
        b.0.score
            .total_cmp(&a.0.score)
            .then(a.0.page.slug.cmp(&b.0.page.slug))
    });
    let mut seen_slugs = std::collections::HashSet::new();
    hits.retain(|(c, _)| seen_slugs.insert(c.page.slug.clone()));

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
            slug: c.page.slug,
            title: c.page.title,
            heading_path: c.chunk.heading_path,
            snippet,
            score: c.score,
            evidence,
            page_type: c.page.page_type,
            source_dir: c.page.source_dir,
            updated_at: c.page.updated_at,
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
            .resolve_page(span)
            .map(|p| p.is_some())
            .unwrap_or(false)
    })
}

fn page_aliases(store: &Store, page_id: i64) -> Result<Vec<String>> {
    let mut stmt = store
        .conn
        .prepare_cached("SELECT alias FROM page_aliases WHERE page_id = ?1")?;
    let rows = stmt
        .query_map([page_id], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(rows)
}

fn page_age_days(page: &Page, now: chrono::DateTime<chrono::Utc>) -> Option<f32> {
    let s = page.updated_at.as_deref()?;
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





