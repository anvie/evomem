use crate::model::{Intent, Mode};

/// Embedding dimension of the built-in hash embedder.
pub const EMBED_DIM: usize = 512;
/// Identifier recorded in `meta`; mixing embedders is a hard error.
pub const EMBEDDER_ID: &str = "hash-v1-d512";
/// Reciprocal-rank-fusion constant.
pub const RRF_K: f32 = 60.0;
/// Candidates taken from each retrieval strategy before fusion.
pub const STRATEGY_TOP_K: usize = 40;
/// Candidates kept after fusion, fed to graph augmentation.
pub const FUSED_TOP_K: usize = 30;
/// Cosine similarity threshold for the `high_vector_match` evidence tag.
pub const HIGH_VECTOR_THRESHOLD: f32 = 0.45;
/// Pages with no update for this many days are flagged stale by `think`.
pub const STALE_DAYS: i64 = 45;
/// Trust floor: a cited doc whose provenance confidence is below this is
/// down-weighted in ranking and flagged `low_trust` by `think`.
pub const CONFIDENCE_FLOOR: f64 = 0.4;
/// Edge types treated as single-valued for contradiction auto-detection: a doc
/// asserting one of these to two different targets is a candidate conflict.
/// Deliberately conservative — most relations are legitimately many-valued.
pub const FUNCTIONAL_EDGES: &[&str] = &["works_at"];
/// Database file name inside the knowledge root.
pub const DB_FILE: &str = ".evomem.db";

// EvoRank query-time priors (deterministic, zero LLM):
/// Weight of the graph-authority prior: score *= 1 + W * ln(1 + in_degree).
pub const AUTHORITY_WEIGHT: f32 = 0.08;
/// Recency boost amplitude for temporal-intent queries.
pub const RECENCY_TEMPORAL: f32 = 0.25;
/// Tiny freshness tiebreak for all other intents.
pub const RECENCY_BASE: f32 = 0.02;
/// Recency decay half-life-ish constants (days).
pub const RECENCY_TEMPORAL_TAU: f32 = 30.0;
pub const RECENCY_BASE_TAU: f32 = 90.0;

/// Per-mode knob bundle (gbrain's "mode binds cost/quality knobs").
#[derive(Debug, Clone, Copy)]
pub struct ModeKnobs {
    /// Approximate token budget for returned result text.
    pub token_budget: usize,
    /// Graph traversal depth from seed docs.
    pub graph_hops: usize,
    /// Maximum results returned.
    pub max_results: usize,
}

pub fn knobs(mode: Mode, intent: Intent) -> ModeKnobs {
    let mut k = match mode {
        Mode::Conservative => ModeKnobs {
            token_budget: 2_000,
            graph_hops: 1,
            max_results: 5,
        },
        Mode::Balanced => ModeKnobs {
            token_budget: 6_000,
            graph_hops: 1,
            max_results: 10,
        },
        Mode::Tokenmax => ModeKnobs {
            token_budget: 16_000,
            graph_hops: 2,
            max_results: 20,
        },
    };
    // Entity queries lean on the graph: factual neighbors matter more.
    if intent == Intent::Entity {
        k.graph_hops = k.graph_hops.max(2);
    }
    k
}

/// Source-tier ranking: curated content boosted, bulk content demoted,
/// scratch content hard-excluded at the SQL level.
pub struct SourceTiers;

impl SourceTiers {
    pub const HARD_EXCLUDE: [&'static str; 3] = ["test", "attachments", ".raw"];
    const BOOST: [&'static str; 3] = ["originals", "concepts", "writing"];
    const DEMOTE: [(&'static str, f32); 4] = [
        ("chat", 0.6),
        ("daily", 0.6),
        ("media", 0.7),
        ("archive", 0.5),
    ];

    /// Multiplier applied to a fused score based on the doc's top-level dir.
    /// Temporal queries skip the chat/daily demote (recency is the point).
    pub fn factor(source_dir: &str, intent: Intent) -> f32 {
        if Self::BOOST.contains(&source_dir) {
            return 1.3;
        }
        if intent == Intent::Temporal && (source_dir == "chat" || source_dir == "daily") {
            return 1.0;
        }
        for (dir, f) in Self::DEMOTE {
            if dir == source_dir {
                return f;
            }
        }
        1.0
    }

    pub fn is_excluded(source_dir: &str) -> bool {
        Self::HARD_EXCLUDE.contains(&source_dir)
    }
}

/// Crude token estimator (chars/4), isolated so a real tokenizer can replace it.
pub fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}
