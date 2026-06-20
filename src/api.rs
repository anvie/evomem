//! Shared request/response DTOs used by the search pipeline, the REST server,
//! and the CLI remote client — one set of types so standalone CLI, server,
//! and CLI→server output are identical.

use serde::{Deserialize, Serialize};

use crate::model::{Evidence, Intent};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub rank: usize,
    pub slug: String,
    pub title: String,
    pub heading_path: String,
    pub snippet: String,
    pub score: f32,
    pub evidence: Evidence,
    pub page_type: String,
    pub source_dir: String,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub query: String,
    pub intent: Intent,
    pub mode: String,
    pub hits: Vec<SearchHit>,
    /// Set by the server when the response came from its query cache.
    #[serde(default)]
    pub cached: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkFact {
    pub slug: String,
    pub title: String,
    pub heading_path: String,
    pub lead: String,
    pub evidence: Evidence,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    StalePage,
    UnknownEntity,
    DanglingLink,
    LowConfidence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gap {
    pub kind: GapKind,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkResponse {
    pub query: String,
    pub intent: Intent,
    pub mode: String,
    pub facts: Vec<ThinkFact>,
    pub gaps: Vec<Gap>,
    #[serde(default)]
    pub cached: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdgeDto {
    pub src_slug: String,
    pub dst_slug: String,
    pub edge_type: String,
    pub hop: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphResponse {
    pub start: String,
    pub edges: Vec<GraphEdgeDto>,
    #[serde(default)]
    pub cached: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsResponse {
    pub pages: i64,
    pub deleted_pages: i64,
    pub chunks: i64,
    pub indexed_words: i64,
    pub links: i64,
    pub dangling_links: i64,
    pub links_by_type: Vec<(String, i64)>,
    pub pages_by_source: Vec<(String, i64)>,
    /// When the knowledge store was last synced from disk — staleness signal for
    /// clients of a long-running server.
    #[serde(default)]
    pub last_synced_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureRequest {
    pub text: String,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureResponse {
    pub slug: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageResponse {
    pub slug: String,
    pub title: String,
    pub page_type: String,
    pub tags: Vec<String>,
    pub updated_at: Option<String>,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}





