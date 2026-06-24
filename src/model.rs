use serde::{Deserialize, Serialize};

/// A page in the knowledge store: one markdown file, identified by its slug
/// (path relative to the knowledge root, '/'-separated, no `.md` extension).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub id: i64,
    pub slug: String,
    pub title: String,
    pub page_type: String,
    pub source_dir: String,
    pub tags: Vec<String>,
    pub content_hash: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub synced_at: String,
    pub deleted_at: Option<String>,
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
    pub page_type: Option<String>,
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

/// A chunk of a page produced by the chunker, before persistence.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkDraft {
    pub heading_path: String,
    pub text: String,
}

/// A typed edge extracted from a page body, before slug resolution.
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





