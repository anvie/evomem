//! CLI → server mode: a blocking HTTP client mirroring the local engine
//! operations, returning the same DTOs so output rendering is shared.

use crate::api::{
    CaptureRequest, CaptureResponse, GraphResponse, DocResponse, SearchResponse, StatsResponse,
    ThinkResponse,
};
use crate::error::{EvoError, Result};
use crate::ingest::SyncReport;
use crate::model::Mode;

pub struct RemoteClient {
    base: String,
    http: reqwest::blocking::Client,
}

impl RemoteClient {
    pub fn new(base: &str) -> RemoteClient {
        RemoteClient {
            base: base.trim_end_matches('/').to_string(),
            http: reqwest::blocking::Client::new(),
        }
    }

    fn get<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        let url = format!("{}{}", self.base, path);
        let resp = self
            .http
            .get(&url)
            .query(query)
            .send()
            .map_err(|e| EvoError::Server(format!("GET {url}: {e}")))?;
        Self::decode(resp, &url)
    }

    fn post<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let url = format!("{}{}", self.base, path);
        let resp = self
            .http
            .post(&url)
            .json(body)
            .send()
            .map_err(|e| EvoError::Server(format!("POST {url}: {e}")))?;
        Self::decode(resp, &url)
    }

    fn decode<T: serde::de::DeserializeOwned>(
        resp: reqwest::blocking::Response,
        url: &str,
    ) -> Result<T> {
        let status = resp.status();
        let text = resp.text().map_err(|e| EvoError::Server(e.to_string()))?;
        if !status.is_success() {
            let msg = serde_json::from_str::<crate::api::ApiError>(&text)
                .map(|e| e.error)
                .unwrap_or(text);
            return Err(EvoError::Server(format!("{url} -> {status}: {msg}")));
        }
        serde_json::from_str(&text)
            .map_err(|e| EvoError::Server(format!("bad response from {url}: {e}")))
    }

    pub fn search(&self, q: &str, mode: Mode, limit: Option<usize>) -> Result<SearchResponse> {
        let mut params = vec![
            ("q", q.to_string()),
            ("mode", format!("{mode:?}").to_lowercase()),
        ];
        if let Some(l) = limit {
            params.push(("limit", l.to_string()));
        }
        self.get("/api/search", &params)
    }

    pub fn think(&self, q: &str, mode: Mode) -> Result<ThinkResponse> {
        self.get(
            "/api/think",
            &[
                ("q", q.to_string()),
                ("mode", format!("{mode:?}").to_lowercase()),
            ],
        )
    }

    pub fn graph(&self, start: &str, edge: Option<&str>, hops: usize) -> Result<GraphResponse> {
        let mut params = vec![("start", start.to_string()), ("hops", hops.to_string())];
        if let Some(e) = edge {
            params.push(("edge", e.to_string()));
        }
        self.get("/api/graph", &params)
    }

    pub fn stats(&self) -> Result<StatsResponse> {
        self.get("/api/stats", &[])
    }

    pub fn doc(&self, slug: &str) -> Result<DocResponse> {
        self.get(&format!("/api/docs/{slug}"), &[])
    }

    pub fn capture(&self, req: &CaptureRequest) -> Result<CaptureResponse> {
        self.post("/api/capture", req)
    }

    pub fn sync(&self) -> Result<SyncReport> {
        self.post("/api/sync", &serde_json::json!({}))
    }
}
