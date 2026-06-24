//! `think` — the knowledge synthesis layer. Runs the same retrieval as `search`,
//! then composes a deterministic synthesis: key facts with citations plus gap
//! analysis (what is NOT known). No LLM calls.

use chrono::{DateTime, Utc};

use crate::api::{Gap, GapKind, ThinkFact, ThinkResponse};
use crate::config::STALE_DAYS;
use crate::embed::Embedder;
use crate::error::Result;
use crate::model::{Evidence, Mode};
use crate::search;
use crate::search::intent::titlecase_spans;
use crate::store::Store;

pub fn think(
    store: &Store,
    embedder: &dyn Embedder,
    query: &str,
    mode: Mode,
    now: DateTime<Utc>,
) -> Result<ThinkResponse> {
    let response = search::search(store, embedder, query, mode)?;

    let mut facts = Vec::new();
    let mut page_ids = Vec::new();
    for hit in &response.hits {
        if let Some(page) = store.get_page_by_slug(&hit.slug)? {
            page_ids.push(page.id);
        }
        facts.push(ThinkFact {
            slug: hit.slug.clone(),
            title: hit.title.clone(),
            heading_path: hit.heading_path.clone(),
            lead: lead_sentences(&hit.snippet, 2),
            evidence: hit.evidence,
            updated_at: hit.updated_at.clone(),
        });
    }

    let mut gaps = Vec::new();

    // 1. Stale pages: cited but not updated in STALE_DAYS. Deduped by slug so
    // a page cited through several facts is reported once.
    let mut stale_seen = std::collections::HashSet::new();
    for fact in &facts {
        if let Some(updated) = fact.updated_at.as_deref().and_then(parse_when) {
            let age_days = (now - updated).num_days();
            if age_days > STALE_DAYS && stale_seen.insert(fact.slug.clone()) {
                gaps.push(Gap {
                    kind: GapKind::StalePage,
                    message: format!(
                        "No updates on \"{}\" ({}) since {} ({} weeks ago) — verify before relying on it.",
                        fact.title,
                        fact.slug,
                        updated.format("%Y-%m-%d"),
                        age_days / 7
                    ),
                });
            }
        }
    }

    // 2. Unknown entities: TitleCase spans in the query with no page/alias.
    // Spans opening with a sentence-leader ("Should We Acquire …") are
    // question grammar, not entity names — skip them. Capped to keep one
    // noisy query from drowning the report.
    let mut unknown_count = 0;
    for span in titlecase_spans(query) {
        if span.split_whitespace().count() < 2 {
            continue; // single capitalized words are too noisy
        }
        if starts_with_sentence_leader(&span) {
            continue;
        }
        if unknown_count >= 3 {
            break;
        }
        if store.resolve_page(&span)?.is_none() {
            unknown_count += 1;
            gaps.push(Gap {
                kind: GapKind::UnknownEntity,
                message: format!("No page exists for \"{span}\"."),
            });
        }
    }

    // 3. Dangling links: cited pages referencing pages that don't exist yet.
    for (src, dst) in store.dangling_from_pages(&page_ids)? {
        gaps.push(Gap {
            kind: GapKind::DanglingLink,
            message: format!("\"{src}\" references missing page \"{dst}\" — a hole to fill."),
        });
    }

    // 4. Low confidence: nothing strong matched.
    let weak = facts.is_empty()
        || facts
            .iter()
            .all(|f| matches!(f.evidence, Evidence::WeakSemantic | Evidence::GraphAdjacent));
    if weak {
        gaps.push(Gap {
            kind: GapKind::LowConfidence,
            message: "Little direct information is available about this query.".to_string(),
        });
    }

    Ok(ThinkResponse {
        query: response.query,
        intent: response.intent,
        mode: response.mode,
        facts,
        gaps,
        cached: false,
    })
}

const SENTENCE_LEADERS: [&str; 18] = [
    "The", "A", "An", "Should", "What", "When", "Who", "Whom", "How", "Why", "Is", "Are", "Will",
    "Can", "Could", "Does", "Do", "Did",
];

fn starts_with_sentence_leader(span: &str) -> bool {
    span.split_whitespace()
        .next()
        .is_some_and(|w| SENTENCE_LEADERS.contains(&w))
}

fn lead_sentences(text: &str, n: usize) -> String {
    let mut out = String::new();
    let mut count = 0;
    for ch in text.chars() {
        out.push(ch);
        if matches!(ch, '.' | '!' | '?') {
            count += 1;
            if count >= n {
                break;
            }
        }
    }
    out.trim().to_string()
}

/// Parse frontmatter/file dates that may be `2026-06-13` or full RFC3339.
fn parse_when(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|ndt| DateTime::from_naive_utc_and_offset(ndt, Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lead_sentence_extraction() {
        assert_eq!(lead_sentences("One. Two. Three.", 2), "One. Two.");
        assert_eq!(lead_sentences("No periods here", 2), "No periods here");
    }

    #[test]
    fn date_parsing_both_forms() {
        assert!(parse_when("2026-06-13").is_some());
        assert!(parse_when("2026-06-13T10:00:00+00:00").is_some());
        assert!(parse_when("not a date").is_none());
    }

    #[test]
    fn sentence_leaders_are_not_entities() {
        assert!(starts_with_sentence_leader("Should We Acquire Acme"));
        assert!(starts_with_sentence_leader("The Big Meeting"));
        assert!(!starts_with_sentence_leader("Zara Quinn"));
        assert!(!starts_with_sentence_leader("Acme Corp"));
    }
}
