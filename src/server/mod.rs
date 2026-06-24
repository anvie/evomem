//! Standalone server mode: REST API over the same engine the CLI uses, with
//! an in-memory LRU query cache for read endpoints (search/think/graph),
//! invalidated on every write (capture/sync).

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use lru::LruCache;
use serde::Deserialize;

use crate::api::{ApiError, CaptureRequest, PageResponse};
use crate::embed::Embedder;
use crate::error::{EvoError, Result};
use crate::model::Mode;
use crate::store::Store;
use crate::{capture, ingest, search, stats, think};

const CACHE_CAPACITY: usize = 256;

pub struct AppState {
    store: Mutex<Store>,
    embedder: Box<dyn Embedder>,
    cache: Mutex<LruCache<String, serde_json::Value>>,
}

type Shared = Arc<AppState>;

pub fn serve(store: Store, embedder: Box<dyn Embedder>, host: &str, port: u16) -> Result<()> {
    let state = Arc::new(AppState {
        store: Mutex::new(store),
        embedder,
        cache: Mutex::new(LruCache::new(NonZeroUsize::new(CACHE_CAPACITY).unwrap())),
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/api/search", get(api_search))
        .route("/api/think", get(api_think))
        .route("/api/graph", get(api_graph))
        .route("/api/stats", get(api_stats))
        .route("/api/pages/{*slug}", get(api_page))
        .route("/api/capture", post(api_capture))
        .route("/api/sync", post(api_sync))
        .with_state(state);

    let addr = format!("{host}:{port}");
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| EvoError::Server(format!("bind {addr}: {e}")))?;
        eprintln!("evomem server listening on http://{addr}");
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await
            .map_err(|e| EvoError::Server(e.to_string()))
    })
}

struct AppErr(EvoError);

impl IntoResponse for AppErr {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            EvoError::PageNotFound(_) => StatusCode::NOT_FOUND,
            EvoError::NotInitialized(_) => StatusCode::CONFLICT,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (
            status,
            Json(ApiError {
                error: self.0.to_string(),
            }),
        )
            .into_response()
    }
}

impl From<EvoError> for AppErr {
    fn from(e: EvoError) -> Self {
        AppErr(e)
    }
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

/// Run blocking engine work on the blocking pool.
async fn blocking<T: Send + 'static>(
    state: Shared,
    f: impl FnOnce(&AppState) -> Result<T> + Send + 'static,
) -> std::result::Result<T, AppErr> {
    tokio::task::spawn_blocking(move || f(&state))
        .await
        .map_err(|e| AppErr(EvoError::Server(format!("task join: {e}"))))?
        .map_err(AppErr)
}

/// Cache-through helper for read endpoints: returns the cached JSON (with
/// `cached: true`) or computes, caches, and returns it.
async fn cached_json(
    state: Shared,
    key: String,
    compute: impl FnOnce(&AppState) -> Result<serde_json::Value> + Send + 'static,
) -> std::result::Result<Json<serde_json::Value>, AppErr> {
    if let Some(mut hit) = lock(&state.cache).get(&key).cloned() {
        if let Some(obj) = hit.as_object_mut() {
            obj.insert("cached".into(), serde_json::Value::Bool(true));
        }
        return Ok(Json(hit));
    }
    let key2 = key.clone();
    let state2 = state.clone();
    let value = blocking(state2, compute).await?;
    lock(&state.cache).put(key2, value.clone());
    Ok(Json(value))
}

fn clear_cache(state: &AppState) {
    lock(&state.cache).clear();
}

fn parse_mode(s: Option<&str>) -> Mode {
    s.and_then(|m| m.parse().ok()).unwrap_or_default()
}

/// Lock a mutex, recovering from poisoning: SQLite's own transactionality
/// keeps the data consistent if a handler panicked mid-request, so the poison
/// flag would only convert one panic into permanent 500s.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Cache keys are JSON-encoded tuples: unambiguous regardless of what
/// delimiters appear inside query strings (keys are only ever compared).
fn cache_key<T: serde::Serialize>(parts: &T) -> String {
    serde_json::to_string(parts).expect("cache key serializable")
}

#[derive(Deserialize)]
struct SearchParams {
    q: String,
    mode: Option<String>,
    limit: Option<usize>,
}

async fn api_search(
    State(state): State<Shared>,
    Query(p): Query<SearchParams>,
) -> std::result::Result<Json<serde_json::Value>, AppErr> {
    let mode = parse_mode(p.mode.as_deref());
    let key = cache_key(&("search", format!("{mode:?}"), &p.q, p.limit));
    cached_json(state, key, move |app| {
        let store = lock(&app.store);
        let mut resp = search::search(&store, app.embedder.as_ref(), &p.q, mode)?;
        if let Some(limit) = p.limit {
            resp.hits.truncate(limit);
        }
        Ok(serde_json::to_value(&resp).expect("serializable"))
    })
    .await
}

async fn api_think(
    State(state): State<Shared>,
    Query(p): Query<SearchParams>,
) -> std::result::Result<Json<serde_json::Value>, AppErr> {
    let mode = parse_mode(p.mode.as_deref());
    let key = cache_key(&("think", format!("{mode:?}"), &p.q));
    cached_json(state, key, move |app| {
        let store = lock(&app.store);
        let resp = think::think(
            &store,
            app.embedder.as_ref(),
            &p.q,
            mode,
            chrono::Utc::now(),
        )?;
        Ok(serde_json::to_value(&resp).expect("serializable"))
    })
    .await
}

#[derive(Deserialize)]
struct GraphParams {
    start: String,
    edge: Option<String>,
    hops: Option<usize>,
}

async fn api_graph(
    State(state): State<Shared>,
    Query(p): Query<GraphParams>,
) -> std::result::Result<Json<serde_json::Value>, AppErr> {
    let key = cache_key(&("graph", &p.start, &p.edge, p.hops));
    cached_json(state, key, move |app| {
        let store = lock(&app.store);
        let page = store
            .resolve_page(&p.start)?
            .ok_or_else(|| EvoError::PageNotFound(p.start.clone()))?;
        let edges =
            search::graph::traverse(&store, page.id, p.edge.as_deref(), p.hops.unwrap_or(2))?;
        let resp = crate::api::GraphResponse {
            start: page.slug,
            edges,
            cached: false,
        };
        Ok(serde_json::to_value(&resp).expect("serializable"))
    })
    .await
}

async fn api_stats(
    State(state): State<Shared>,
) -> std::result::Result<Json<serde_json::Value>, AppErr> {
    let resp = blocking(state, |app| {
        let store = lock(&app.store);
        stats::stats(&store)
    })
    .await?;
    Ok(Json(serde_json::to_value(&resp).expect("serializable")))
}

async fn api_page(
    State(state): State<Shared>,
    AxPath(slug): AxPath<String>,
) -> std::result::Result<Json<PageResponse>, AppErr> {
    let resp = blocking(state, move |app| {
        let store = lock(&app.store);
        let page = store
            .resolve_page(&slug)?
            .ok_or_else(|| EvoError::PageNotFound(slug.clone()))?;
        let content = std::fs::read_to_string(store.brain_root.join(format!("{}.md", page.slug)))
            .unwrap_or_default();
        Ok(PageResponse {
            slug: page.slug,
            title: page.title,
            page_type: page.page_type,
            tags: page.tags,
            updated_at: page.updated_at,
            content,
        })
    })
    .await?;
    Ok(Json(resp))
}

async fn api_capture(
    State(state): State<Shared>,
    Json(req): Json<CaptureRequest>,
) -> std::result::Result<Json<serde_json::Value>, AppErr> {
    let state2 = state.clone();
    let resp = blocking(state2, move |app| {
        let store = lock(&app.store);
        capture::capture(&store, app.embedder.as_ref(), &req, chrono::Utc::now())
    })
    .await?;
    clear_cache(&state);
    Ok(Json(serde_json::to_value(&resp).expect("serializable")))
}

async fn api_sync(
    State(state): State<Shared>,
) -> std::result::Result<Json<serde_json::Value>, AppErr> {
    let state2 = state.clone();
    let report = blocking(state2, |app| {
        let store = lock(&app.store);
        ingest::sync_dir(&store, app.embedder.as_ref())
    })
    .await?;
    clear_cache(&state);
    Ok(Json(serde_json::to_value(&report).expect("serializable")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_keys_cannot_collide_across_fields() {
        // Delimiter characters inside the query must not blur field
        // boundaries the way string concatenation could.
        let a = cache_key(&("search", "balanced", "a|None", None::<usize>));
        let b = cache_key(&("search", "balanced", "a", None::<usize>));
        let c = cache_key(&("search", "balanced", "a\",\"b", Some(2usize)));
        let d = cache_key(&("search", "balanced", "a", Some(2usize)));
        let keys = [a, b, c, d];
        for (i, x) in keys.iter().enumerate() {
            for y in keys.iter().skip(i + 1) {
                assert_ne!(x, y);
            }
        }
        // Endpoint is part of the key.
        assert_ne!(
            cache_key(&("search", "balanced", "q")),
            cache_key(&("think", "balanced", "q"))
        );
    }

    #[test]
    fn lock_recovers_from_poisoning() {
        let m = std::sync::Arc::new(Mutex::new(7));
        let m2 = m.clone();
        let _ = std::thread::spawn(move || {
            let _guard = m2.lock().unwrap();
            panic!("poison it");
        })
        .join();
        assert!(m.lock().is_err(), "mutex should be poisoned");
        assert_eq!(*lock(&m), 7, "lock() must recover the value");
    }
}
