use serde::{Deserialize, Serialize};

/// A doc in the knowledge store: one markdown file, identified by its slug
/// (path relative to the knowledge root, '/'-separated, no `.md` extension).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Doc {
    pub id: i64,
    pub slug: String,
    pub title: String,
    pub doc_type: String,
    pub source_dir: String,
    pub tags: Vec<String>,
    pub content_hash: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub synced_at: String,
    pub deleted_at: Option<String>,
    /// Hygiene: id of the newer doc this one was folded into by `consolidate`.
    /// `Some` => a near-duplicate hidden from retrieval but kept for history.
    #[serde(default)]
    pub superseded_by: Option<i64>,
    /// Recall tracking: how many times this doc was surfaced into an agent's
    /// recall context (bumped via `recall-bump`, never reset by `sync`).
    #[serde(default)]
    pub recall_count: i64,
    /// When this doc was last recalled (RFC3339), or `None` if never.
    #[serde(default)]
    pub last_recalled_at: Option<String>,
}

/// Parsed YAML frontmatter. Every field is optional and parsed leniently:
/// scalars where lists are expected (`aliases: Ali`), numbers or dates where
/// strings are expected (`title: 123`, `created: 2026-01-05`) all work —
/// users hand-write this and a stray scalar must not reject the whole file.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Frontmatter {
    #[serde(default, deserialize_with = "lenient_string")]
    pub title: Option<String>,
    #[serde(rename = "type", default, deserialize_with = "lenient_string")]
    pub doc_type: Option<String>,
    #[serde(default, deserialize_with = "lenient_string")]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "string_or_seq")]
    pub aliases: Vec<String>,
    #[serde(default, deserialize_with = "string_or_seq")]
    pub tags: Vec<String>,
    #[serde(default, deserialize_with = "lenient_string")]
    pub created: Option<String>,
    #[serde(default, deserialize_with = "lenient_string")]
    pub updated: Option<String>,
    // ── Trust layer (provenance) ──────────────────────────────────────────
    /// Where this fact came from: user_stated | inferred | external |
    /// agent_reported (free-form; only a hint for the reader).
    #[serde(default, deserialize_with = "lenient_string")]
    pub source: Option<String>,
    /// How much to trust this doc, 0.0–1.0. Below the confidence floor it is
    /// down-ranked and flagged by `think`.
    #[serde(default, deserialize_with = "lenient_f64")]
    pub confidence: Option<f64>,
    /// When the doc was last verified (date or RFC3339); staleness is measured
    /// from here when present, instead of the global update-age fallback.
    #[serde(default, deserialize_with = "lenient_string")]
    pub verified: Option<String>,
    /// Re-verify after this many days. `0`/absent => never goes stale on its own.
    #[serde(default, deserialize_with = "lenient_i64")]
    pub stale_after: Option<i64>,
}

fn yaml_scalar_to_string(v: serde_yaml::Value) -> Option<String> {
    match v {
        serde_yaml::Value::String(s) => Some(s),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn lenient_string<'de, D>(d: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v: Option<serde_yaml::Value> = serde::Deserialize::deserialize(d)?;
    Ok(v.and_then(yaml_scalar_to_string))
}

/// Parse an optional float that may arrive as a YAML number or a quoted string
/// (`confidence: 0.9` or `confidence: "0.9"`); anything else yields `None`.
fn lenient_f64<'de, D>(d: D) -> std::result::Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v: Option<serde_yaml::Value> = serde::Deserialize::deserialize(d)?;
    Ok(v.and_then(|v| match v {
        serde_yaml::Value::Number(n) => n.as_f64(),
        serde_yaml::Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }))
}

/// Parse an optional integer that may arrive as a YAML number (truncating a
/// float) or a quoted string; anything else yields `None`.
fn lenient_i64<'de, D>(d: D) -> std::result::Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v: Option<serde_yaml::Value> = serde::Deserialize::deserialize(d)?;
    Ok(v.and_then(|v| match v {
        serde_yaml::Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        serde_yaml::Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }))
}

fn string_or_seq<'de, D>(d: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v: Option<serde_yaml::Value> = serde::Deserialize::deserialize(d)?;
    Ok(match v {
        Some(serde_yaml::Value::Sequence(seq)) => {
            seq.into_iter().filter_map(yaml_scalar_to_string).collect()
        }
        Some(scalar) => yaml_scalar_to_string(scalar)
            .map(|s| vec![s])
            .unwrap_or_default(),
        None => Vec::new(),
    })
}

/// A chunk of a doc produced by the chunker, before persistence.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkDraft {
    pub heading_path: String,
    pub text: String,
}

/// A typed edge extracted from a doc body, before slug resolution.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkDraft {
    pub dst_slug: String,
    pub edge_type: EdgeType,
    pub anchor_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EdgeType {
    Founded,
    InvestedIn,
    WorksAt,
    Advises,
    Attended,
    Mentions,
    /// User-defined type from a typed-link blockquote.
    Custom(String),
}

impl EdgeType {
    pub fn as_str(&self) -> &str {
        match self {
            EdgeType::Founded => "founded",
            EdgeType::InvestedIn => "invested_in",
            EdgeType::WorksAt => "works_at",
            EdgeType::Advises => "advises",
            EdgeType::Attended => "attended",
            EdgeType::Mentions => "mentions",
            EdgeType::Custom(s) => s,
        }
    }

    pub fn parse(s: &str) -> EdgeType {
        match s {
            "founded" => EdgeType::Founded,
            "invested_in" => EdgeType::InvestedIn,
            "works_at" => EdgeType::WorksAt,
            "advises" => EdgeType::Advises,
            "attended" => EdgeType::Attended,
            "mentions" => EdgeType::Mentions,
            other => EdgeType::Custom(other.to_string()),
        }
    }
}

/// Deterministic query intent classification (no LLM).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Intent {
    Entity,
    Temporal,
    Event,
    General,
}

/// Why a result matched — the "evidence contract" each hit carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    AliasHit,
    ExactTitleMatch,
    KeywordExact,
    HighVectorMatch,
    GraphAdjacent,
    WeakSemantic,
}

/// Retrieval mode bundles cost/quality knobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Conservative,
    #[default]
    Balanced,
    Tokenmax,
}

impl std::str::FromStr for Mode {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "conservative" => Ok(Mode::Conservative),
            "balanced" => Ok(Mode::Balanced),
            "tokenmax" => Ok(Mode::Tokenmax),
            other => Err(format!(
                "unknown mode `{other}` (conservative|balanced|tokenmax)"
            )),
        }
    }
}
