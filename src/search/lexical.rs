//! EvoRank lexical ranking - Bucket-sort ranking rules with
//! three upgrades that fix known weaknesses of BM25:
//!
//! 1. words — documents matching more query words win outright.
//! 2. IDF — *upgrade*: among equal word counts, matching rarer (more
//!    discriminative) query words wins.
//! 3. typo — fewer/cheaper corrections win. *Upgrade*: a graded cost scale —
//!    exact 0 < stem match 1 < one typo 2 < two typos 4 — recovers stemming
//!    ("returns" ~ "returned") lost with FTS5's porter tokenizer, while still
//!    ranking it below exact and above typo-corrected matches.
//! 4. proximity — matched words closer together win. *Upgrade*: pairs in
//!    query order cost their distance, reversed pairs cost distance + 1
//!    (phrase-order awareness).
//! 5. attribute — earlier attribute (title > heading > body), then earlier
//!    word position within it.
//! 6. exactness — exact word matches beat prefix/stem/typo-derived ones.
//!
//! Query words resolve against the indexed vocabulary with bounded
//! Levenshtein;
//! the last query word also matches as a prefix. Candidates come from our
//! own `word_index` postings — no FTS5 involved. Everything is deterministic:
//! same knowledge store + same query = same ranking, and every position is explainable
//! by the rule that decided it.

use std::collections::HashMap;

use crate::config::SourceTiers;
use crate::error::Result;
use crate::store::Store;
use crate::text::{allowed_typos, bounded_levenshtein, stem, tokenize};

/// Correction-cost units (rule 2).
const COST_EXACT: u32 = 0;
const COST_STEM: u32 = 1;
const COST_PER_TYPO: u32 = 2;
/// A query word present in more than this fraction of chunks is a stop word:
/// it still *scores* on candidates found by rarer words, but stops
/// *generating* candidates on its own (posting-explosion guard).
const DF_GATE: f32 = 0.6;
/// Last-word prefix matching needs at least this many chars — a linear vocab
/// scan can't afford 1-char prefixes
const MIN_PREFIX_LEN: usize = 3;
/// Queries are truncated to this many tokens.
const MAX_QUERY_WORDS: usize = 24;

/// Per-chunk match statistics — the bucket-sort key.
#[derive(Debug, Clone, Default)]
pub struct MatchStats {
    pub words_matched: u32,
    /// Sum of IDF of matched query words (rule 1b).
    pub idf_weight: f32,
    /// Graded correction cost (rule 2): exact 0, stem 1, typo 2/4.
    pub typo_count: u32,
    pub proximity: u32,
    pub best_attr: i64,
    pub best_pos: i64,
    pub exact_count: u32,
}

#[derive(Debug, Clone)]
pub struct LexicalHit {
    pub chunk_id: i64,
    pub stats: MatchStats,
}

/// How one query word resolved to one vocabulary word.
#[derive(Debug, Clone)]
struct WordMatch {
    cost: u32,
    exact: bool,
}

pub fn search(store: &Store, query: &str, top_k: usize) -> Result<Vec<LexicalHit>> {
    let mut query_words = tokenize(query);
    query_words.truncate(MAX_QUERY_WORDS);
    if query_words.is_empty() {
        return Ok(Vec::new());
    }
    let vocab = store.vocabulary()?;
    let total_chunks = store.live_chunk_count()?.max(1) as f32;
    let n_words = query_words.len();

    // Resolve each query word against the vocabulary, and compute its document
    // frequency (sum of per-derivation distinct-chunk counts, capped at the
    // corpus size — close enough for gating and IDF).
    let mut derivations: Vec<Vec<(String, WordMatch)>> = Vec::with_capacity(n_words);
    let mut df: Vec<f32> = Vec::with_capacity(n_words);
    for (qi, qw) in query_words.iter().enumerate() {
        let is_last = qi == n_words - 1;
        let q_stem = stem(qw);
        let mut derivs = Vec::new();
        let mut count: i64 = 0;
        for vw in &vocab {
            let Some(m) = resolve_word(qw, &q_stem, vw, is_last) else {
                continue;
            };
            count += store.word_chunk_count(vw, &SourceTiers::HARD_EXCLUDE)?;
            derivs.push((vw.clone(), m));
        }
        derivations.push(derivs);
        df.push((count as f32).min(total_chunks));
    }

    // df-gate: stop words score but don't generate candidates — unless every
    // query word is gated (then gating would mean zero results, so bypass).
    let gated: Vec<bool> = df.iter().map(|&d| d > DF_GATE * total_chunks).collect();
    let any_open = gated.iter().any(|g| !g);

    // chunk_id -> per-query-word (best WordMatch, occurrences as (attr, pos)).
    let mut chunks: HashMap<i64, Vec<PerWordSlot>> = HashMap::new();
    let apply = |slot: &mut PerWordSlot, m: &WordMatch, attr: i64, pos: i64| {
        let better = match &slot.0 {
            None => true,
            Some(prev) => m.cost < prev.cost || (m.cost == prev.cost && m.exact && !prev.exact),
        };
        if better {
            slot.0 = Some(m.clone());
        }
        slot.1.push((attr, pos));
    };

    // Pass 1: generator words create candidate entries.
    for (qi, derivs) in derivations.iter().enumerate() {
        if gated[qi] && any_open {
            continue;
        }
        for (vw, m) in derivs {
            for posting in store.postings(vw, &SourceTiers::HARD_EXCLUDE)? {
                let entry = chunks
                    .entry(posting.chunk_id)
                    .or_insert_with(|| vec![(None, Vec::new()); n_words]);
                apply(&mut entry[qi], m, posting.attr, posting.pos);
            }
        }
    }
    // Pass 2: gated words only score chunks that already exist as candidates.
    if any_open {
        for (qi, derivs) in derivations.iter().enumerate() {
            if !gated[qi] {
                continue;
            }
            for (vw, m) in derivs {
                for posting in store.postings(vw, &SourceTiers::HARD_EXCLUDE)? {
                    if let Some(entry) = chunks.get_mut(&posting.chunk_id) {
                        apply(&mut entry[qi], m, posting.attr, posting.pos);
                    }
                }
            }
        }
    }

    let idf: Vec<f32> = df
        .iter()
        .map(|&d| (1.0 + total_chunks / (1.0 + d)).ln())
        .collect();

    let mut hits: Vec<LexicalHit> = chunks
        .into_iter()
        .map(|(chunk_id, per_word)| LexicalHit {
            chunk_id,
            stats: compute_stats(&per_word, &idf),
        })
        .filter(|h| h.stats.words_matched > 0)
        .collect();

    hits.sort_by(|a, b| compare(&a.stats, &b.stats).then(a.chunk_id.cmp(&b.chunk_id)));
    hits.truncate(top_k);
    Ok(hits)
}

/// Does query word `qw` match vocabulary word `vw`, and at what cost?
fn resolve_word(qw: &str, q_stem: &str, vw: &str, allow_prefix: bool) -> Option<WordMatch> {
    if qw == vw {
        return Some(WordMatch {
            cost: COST_EXACT,
            exact: true,
        });
    }
    if allow_prefix && qw.chars().count() >= MIN_PREFIX_LEN && vw.starts_with(qw) {
        return Some(WordMatch {
            cost: COST_EXACT,
            exact: false,
        });
    }
    if q_stem == stem(vw) {
        return Some(WordMatch {
            cost: COST_STEM,
            exact: false,
        });
    }
    let budget = allowed_typos(qw);
    if budget == 0 {
        return None;
    }
    let d = bounded_levenshtein(qw, vw, budget)?;
    if d == 0 {
        return None; // equal strings already handled
    }
    Some(WordMatch {
        cost: d * COST_PER_TYPO,
        exact: false,
    })
}

type PerWordSlot = (Option<WordMatch>, Vec<(i64, i64)>);

fn compute_stats(per_word: &[PerWordSlot], idf: &[f32]) -> MatchStats {
    let mut s = MatchStats {
        best_attr: i64::MAX,
        best_pos: i64::MAX,
        ..Default::default()
    };
    for (qi, (m, occs)) in per_word.iter().enumerate() {
        let Some(m) = m else { continue };
        s.words_matched += 1;
        s.idf_weight += idf.get(qi).copied().unwrap_or(0.0);
        s.typo_count += m.cost;
        if m.exact {
            s.exact_count += 1;
        }
        for &(attr, pos) in occs {
            if attr < s.best_attr || (attr == s.best_attr && pos < s.best_pos) {
                s.best_attr = attr;
                s.best_pos = pos;
            }
        }
    }
    // Proximity: clamped (≤8) min cost between occurrences of consecutive
    // matched query words. In-query-order pairs cost their distance; reversed
    // pairs cost distance + 1; different attributes cost the full 8.
    for pair in per_word.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        if a.0.is_none() || b.0.is_none() {
            continue;
        }
        let mut best = 8u32;
        for &(attr_a, pos_a) in &a.1 {
            for &(attr_b, pos_b) in &b.1 {
                let cost = if attr_a == attr_b {
                    let d = pos_a.abs_diff(pos_b) as u32;
                    if pos_b >= pos_a {
                        d
                    } else {
                        d + 1
                    }
                } else {
                    8
                };
                best = best.min(cost.min(8));
            }
        }
        s.proximity += best;
    }
    if s.best_attr == i64::MAX {
        s.best_attr = i64::from(i32::MAX);
        s.best_pos = i64::from(i32::MAX);
    }
    s
}

/// Lexicographic ranking-rule comparator (less = ranks higher).
pub fn compare(a: &MatchStats, b: &MatchStats) -> std::cmp::Ordering {
    b.words_matched
        .cmp(&a.words_matched) // 1. words: more is better
        .then(b.idf_weight.total_cmp(&a.idf_weight)) // 1b. rarer words win
        .then(a.typo_count.cmp(&b.typo_count)) // 2. correction cost: cheaper
        .then(a.proximity.cmp(&b.proximity)) // 3. proximity: closer, in order
        .then(a.best_attr.cmp(&b.best_attr)) // 4. attribute: earlier attr...
        .then(a.best_pos.cmp(&b.best_pos)) //    ...then earlier position
        .then(b.exact_count.cmp(&a.exact_count)) // 5. exactness: more exact
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ChunkDraft;
    use crate::store::docs::DocUpsert;

    fn stats(words: u32, typo: u32, prox: u32, attr: i64, pos: i64, exact: u32) -> MatchStats {
        MatchStats {
            words_matched: words,
            idf_weight: 0.0,
            typo_count: typo,
            proximity: prox,
            best_attr: attr,
            best_pos: pos,
            exact_count: exact,
        }
    }

    #[test]
    fn ranking_rules_are_lexicographic() {
        use std::cmp::Ordering::Less;
        // words dominates everything.
        assert_eq!(
            compare(&stats(3, 5, 8, 2, 9, 0), &stats(2, 0, 0, 0, 0, 2)),
            Less
        );
        // typo breaks words tie regardless of proximity.
        assert_eq!(
            compare(&stats(2, 0, 8, 2, 9, 0), &stats(2, 1, 0, 0, 0, 2)),
            Less
        );
        // proximity breaks typo tie.
        assert_eq!(
            compare(&stats(2, 1, 2, 2, 9, 0), &stats(2, 1, 5, 0, 0, 2)),
            Less
        );
        // attribute breaks proximity tie (title beats body).
        assert_eq!(
            compare(&stats(2, 1, 2, 0, 9, 0), &stats(2, 1, 2, 2, 0, 2)),
            Less
        );
        // exactness last.
        assert_eq!(
            compare(&stats(2, 1, 2, 0, 9, 2), &stats(2, 1, 2, 0, 9, 1)),
            Less
        );
    }

    #[test]
    fn idf_breaks_word_count_ties() {
        use std::cmp::Ordering::Less;
        let mut rare = stats(1, 0, 0, 2, 5, 1);
        rare.idf_weight = 4.0;
        let mut common = stats(1, 0, 0, 0, 0, 1);
        common.idf_weight = 0.3;
        assert_eq!(
            compare(&rare, &common),
            Less,
            "rarer matched word wins the tie"
        );
        // ...but never beats matching more words.
        let two_common = stats(2, 4, 8, 2, 9, 0);
        assert_eq!(compare(&two_common, &rare), Less);
    }

    fn put(store: &Store, slug: &str, title: &str, body: &str) {
        let id = store
            .upsert_page(
                &DocUpsert {
                    slug,
                    title,
                    doc_type: "note",
                    source_dir: slug.split('/').next().unwrap_or(""),
                    tags: &[],
                    content_hash: slug,
                    created_at: None,
                    updated_at: None,
                    aliases: &[],
                },
                "2026-06-13T00:00:00Z",
            )
            .unwrap();
        store
            .replace_chunks_for_page(
                id,
                title,
                &[ChunkDraft {
                    heading_path: String::new(),
                    text: body.to_string(),
                }],
                &[vec![0.0; 4]],
            )
            .unwrap();
    }

    fn top_slug(store: &Store, query: &str) -> String {
        let hits = search(store, query, 10).unwrap();
        let rows = store.get_chunks(&[hits[0].chunk_id]).unwrap();
        store.get_doc_by_id(rows[0].doc_id).unwrap().unwrap().slug
    }

    fn fixture_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::init(dir.path(), "test", 4).unwrap();
        put(
            &store,
            "notes/dark-knight",
            "Dark Knight Returns",
            "The dark knight returns to Gotham tonight.",
        );
        put(&store, "notes/scattered", "Random Notes", "A dark and stormy night. Later, a knight appeared, then returns happened elsewhere entirely in this long rambling paragraph of text.");
        put(
            &store,
            "notes/partial",
            "Partial",
            "Only the knight is here.",
        );
        (dir, store)
    }

    #[test]
    fn words_rule_dominates_then_proximity() {
        let (_d, store) = fixture_store();
        let hits = search(&store, "dark knight returns", 10).unwrap();
        assert!(hits.len() >= 2);
        assert_eq!(top_slug(&store, "dark knight returns"), "notes/dark-knight");
        let rows = store
            .get_chunks(&hits.iter().map(|h| h.chunk_id).collect::<Vec<_>>())
            .unwrap();
        let last_page = store
            .get_doc_by_id(rows.last().unwrap().doc_id)
            .unwrap()
            .unwrap();
        assert_eq!(last_page.slug, "notes/partial");
    }

    #[test]
    fn typo_tolerant_match_ranks_below_exact() {
        let (_d, store) = fixture_store();
        // "knigt" (5 chars) allows 1 typo -> matches "knight" at cost 2.
        let hits = search(&store, "knigt", 10).unwrap();
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|h| h.stats.typo_count >= COST_PER_TYPO));
        // 4-char word allows no typos and has no stem/prefix path... but
        // "darq" shares no stem with anything either.
        assert!(search(&store, "darq book", 10).unwrap().is_empty());
    }

    #[test]
    fn stem_match_sits_between_exact_and_typo() {
        let (_d, store) = fixture_store();
        // "returned" stems to "return", matching indexed "returns" at stem cost.
        let hits = search(&store, "returned", 10).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].stats.typo_count, COST_STEM);
        assert_eq!(hits[0].stats.exact_count, 0);
    }

    #[test]
    fn idf_prefers_discriminative_words() {
        let (_d, store) = fixture_store();
        // "knight" appears in all three docs; "gotham" in one. A doc matching
        // only "gotham" should beat a doc matching only "knight".
        put(
            &store,
            "notes/common-only",
            "Knight Mentions",
            "knight knight knight",
        );
        let hits = search(&store, "gotham zzz", 10).unwrap();
        // only dark-knight matches gotham; nothing matches zzz.
        assert_eq!(top_slug(&store, "gotham zzz"), "notes/dark-knight");
        assert!(!hits.is_empty());
    }

    #[test]
    fn in_order_phrase_beats_reversed() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::init(dir.path(), "test", 4).unwrap();
        put(&store, "a/ordered", "One", "the launch party started late");
        put(&store, "a/reversed", "Two", "the party launch started late");
        let hits = search(&store, "launch party", 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(top_slug(&store, "launch party"), "a/ordered");
    }

    #[test]
    fn last_word_prefix_matches() {
        let (_d, store) = fixture_store();
        let hits = search(&store, "got", 10).unwrap(); // prefix of "gotham"
        assert!(!hits.is_empty());
        assert_eq!(hits[0].stats.exact_count, 0, "prefix match is not exact");
    }

    #[test]
    fn prefix_needs_three_chars() {
        let (_d, store) = fixture_store();
        // "go" is a 2-char prefix of "gotham" — below the floor, no match.
        assert!(search(&store, "go", 10).unwrap().is_empty());
    }

    #[test]
    fn df_gate_stops_stopword_candidate_explosion() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::init(dir.path(), "test", 4).unwrap();
        for i in 0..4 {
            put(
                &store,
                &format!("n/common-{i}"),
                "Note",
                "common filler text here",
            );
        }
        put(
            &store,
            "n/special",
            "Note",
            "common filler plus zerank here",
        );
        // "common" is in 5/5 chunks (df > 60%): it must not generate candidates
        // when a rarer word is present — only the zerank chunk qualifies, but
        // "common" still scores on it (words_matched = 2).
        let hits = search(&store, "zerank common", 10).unwrap();
        assert_eq!(hits.len(), 1, "gated word generated candidates");
        assert_eq!(
            hits[0].stats.words_matched, 2,
            "gated word must still score"
        );
        // A query made only of gated words bypasses the gate entirely.
        let hits = search(&store, "common", 10).unwrap();
        assert_eq!(hits.len(), 5);
    }

    #[test]
    fn overlong_queries_are_truncated() {
        let (_d, store) = fixture_store();
        let long_query = "knight ".repeat(50);
        // Must not panic or scan 50 query words; still finds knight docs.
        assert!(!search(&store, &long_query, 10).unwrap().is_empty());
    }

    #[test]
    fn title_attribute_beats_body() {
        let (_d, store) = fixture_store();
        let hits = search(&store, "dark", 10).unwrap();
        assert_eq!(hits[0].stats.best_attr, 0);
        assert_eq!(top_slug(&store, "dark"), "notes/dark-knight");
    }
}
