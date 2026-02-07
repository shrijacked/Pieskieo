use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    extract::{Path, State},
    routing::{delete, get, post},
    Json, Router,
};
use kaededb_core::{KaedeDb, KaedeDbError, VectorParams as KaedeDbVectorParams};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    db: Arc<KaedeDb>,
}

#[derive(Deserialize)]
struct DocInput {
    id: Option<Uuid>,
    data: serde_json::Value,
}

#[derive(Deserialize)]
struct VectorInput {
    id: Uuid,
    vector: Vec<f32>,
    meta: Option<HashMap<String, String>>,
}

#[derive(Deserialize)]
struct VectorBulk {
    items: Vec<VectorInput>,
}

#[derive(Deserialize)]
struct VectorMetaInput {
    meta: HashMap<String, String>,
}

#[derive(Deserialize)]
struct VectorMetaDeleteInput {
    keys: Vec<String>,
}

#[derive(Deserialize)]
struct RowInput {
    id: Option<Uuid>,
    data: serde_json::Value,
}

#[derive(Deserialize)]
struct VectorSearchInput {
    query: Vec<f32>,
    k: Option<usize>,
    metric: Option<String>,
    filter_ids: Option<Vec<Uuid>>,
    ef_search: Option<usize>,
    filter_meta: Option<HashMap<String, String>>,
}

#[derive(Deserialize)]
struct VectorConfigInput {
    ef_search: Option<usize>,
    ef_construction: Option<usize>,
    link_top_k: Option<usize>,
}

#[derive(Deserialize)]
struct EdgeInput {
    src: Uuid,
    dst: Uuid,
    weight: Option<f32>,
}

#[derive(Serialize)]
struct ApiResponse<T> {
    ok: bool,
    data: T,
}

#[derive(Serialize)]
struct VectorOutput {
    id: Uuid,
    vector: Vec<f32>,
    meta: Option<HashMap<String, String>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let data_dir = std::env::var("KAEDEDB_DATA").unwrap_or_else(|_| "data".to_string());
    let params = vector_params_from_env();
    let db = Arc::new(KaedeDb::open_with_params(&data_dir, params)?);

    let state = AppState { db };

    // background WAL flusher (group commit) for better latency.
    let flush_ms = std::env::var("KAEDEDB_WAL_FLUSH_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(50);
    {
        let db = state.db.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(flush_ms));
            loop {
                interval.tick().await;
                if let Err(e) = db.flush_wal() {
                    tracing::warn!("wal flush failed: {e}");
                }
            }
        });
    }

    if let Ok(secs) = std::env::var("KAEDEDB_SNAPSHOT_INTERVAL_SECS") {
        if let Ok(secs) = secs.parse::<u64>() {
            let db = state.db.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(secs));
                loop {
                    interval.tick().await;
                    if let Err(e) = db.save_vector_snapshot() {
                        tracing::warn!("snapshot save failed: {e}");
                    }
                }
            });
        }
    }

    if let Ok(secs) = std::env::var("KAEDEDB_REBUILD_INTERVAL_SECS") {
        if let Ok(secs) = secs.parse::<u64>() {
            let db = state.db.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(secs));
                loop {
                    interval.tick().await;
                    if let Err(e) = db.rebuild_vectors() {
                        tracing::warn!("vector rebuild failed: {e}");
                    }
                }
            });
        }
    }

    let app = Router::new()
        .route("/healthz", get(health))
        .route("/v1/doc", post(put_doc))
        .route("/v1/doc/:id", get(get_doc))
        .route("/v1/doc/:id", delete(delete_doc))
        .route("/v1/row", post(put_row))
        .route("/v1/row/:id", get(get_row))
        .route("/v1/row/:id", delete(delete_row))
        .route("/v1/vector", post(put_vector))
        .route("/v1/vector/:id/meta", post(update_vector_meta))
        .route("/v1/vector/config", post(update_vector_config))
        .route("/v1/vector/:id/meta/delete", post(delete_vector_meta_keys))
        .route("/v1/vector/:id", get(get_vector))
        .route("/v1/vector/vacuum", post(vacuum_vectors))
        .route("/v1/vector/search", post(search_vector))
        .route("/v1/vector/rebuild", post(rebuild_vectors))
        .route("/v1/vector/snapshot/save", post(save_snapshot))
        .route("/v1/vector/bulk", post(put_vector_bulk))
        .route("/v1/vector/:id", delete(delete_vector))
        .route("/metrics", get(metrics))
        .route("/v1/graph/edge", post(add_edge))
        .route("/v1/graph/:id", get(list_neighbors))
        .with_state(state);

    let addr: SocketAddr = std::env::var("KAEDEDB_LISTEN")
        .unwrap_or_else(|_| "0.0.0.0:8000".into())
        .parse()?;

    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "listening");
    axum::serve(listener, app).await?;
    Ok(())
}

fn vector_params_from_env() -> KaedeDbVectorParams {
    let metric = match std::env::var("KAEDEDB_VECTOR_METRIC")
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "cosine" => kaededb_core::vector::VectorMetric::Cosine,
        "dot" => kaededb_core::vector::VectorMetric::Dot,
        _ => kaededb_core::vector::VectorMetric::L2,
    };
    let ef_c = std::env::var("KAEDEDB_EF_CONSTRUCTION")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(200);
    let ef_s = std::env::var("KAEDEDB_EF_SEARCH")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(50);
    let max_el = std::env::var("KAEDEDB_VEC_MAX_ELEMENTS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(100_000);
    let link_top_k = std::env::var("KAEDEDB_LINK_K")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(4);

    KaedeDbVectorParams {
        metric,
        ef_construction: ef_c,
        ef_search: ef_s,
        max_elements: max_el,
        link_top_k,
    }
}
async fn health() -> &'static str {
    "ok"
}

async fn put_doc(
    State(state): State<AppState>,
    Json(input): Json<DocInput>,
) -> Result<Json<ApiResponse<Uuid>>, ApiError> {
    let id = input.id.unwrap_or_else(Uuid::new_v4);
    state.db.put_doc(id, input.data).map_err(ApiError::from)?;
    Ok(Json(ApiResponse { ok: true, data: id }))
}

async fn get_doc(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<serde_json::Value>>, ApiError> {
    let doc = state.db.get_doc(&id).ok_or(ApiError::NotFound)?;
    Ok(Json(ApiResponse {
        ok: true,
        data: doc,
    }))
}

async fn delete_doc(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<&'static str>>, ApiError> {
    state.db.delete_doc(&id).map_err(ApiError::from)?;
    Ok(Json(ApiResponse {
        ok: true,
        data: "deleted",
    }))
}

async fn put_row(
    State(state): State<AppState>,
    Json(input): Json<RowInput>,
) -> Result<Json<ApiResponse<Uuid>>, ApiError> {
    let id = input.id.unwrap_or_else(Uuid::new_v4);
    state.db.put_row(id, &input.data).map_err(ApiError::from)?;
    Ok(Json(ApiResponse { ok: true, data: id }))
}

async fn get_row(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<serde_json::Value>>, ApiError> {
    let row = state.db.get_row(&id).ok_or(ApiError::NotFound)?;
    Ok(Json(ApiResponse {
        ok: true,
        data: row,
    }))
}

async fn delete_row(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<&'static str>>, ApiError> {
    state.db.delete_row(&id).map_err(ApiError::from)?;
    Ok(Json(ApiResponse {
        ok: true,
        data: "deleted",
    }))
}

async fn put_vector(
    State(state): State<AppState>,
    Json(input): Json<VectorInput>,
) -> Result<Json<ApiResponse<&'static str>>, ApiError> {
    state
        .db
        .put_vector_with_meta(input.id, input.vector, input.meta)
        .map_err(ApiError::from)?;
    Ok(Json(ApiResponse {
        ok: true,
        data: "stored",
    }))
}

async fn put_vector_bulk(
    State(state): State<AppState>,
    Json(input): Json<VectorBulk>,
) -> Result<Json<ApiResponse<usize>>, ApiError> {
    let mut stored = 0usize;
    for item in input.items {
        state
            .db
            .put_vector_with_meta(item.id, item.vector.clone(), item.meta.clone())
            .map_err(ApiError::from)?;
        stored += 1;
    }
    Ok(Json(ApiResponse {
        ok: true,
        data: stored,
    }))
}

async fn delete_vector(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<&'static str>>, ApiError> {
    state.db.delete_vector(&id).map_err(ApiError::from)?;
    Ok(Json(ApiResponse {
        ok: true,
        data: "deleted",
    }))
}

async fn get_vector(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<VectorOutput>>, ApiError> {
    let (vector, meta) = state.db.get_vector(&id).ok_or(ApiError::NotFound)?;
    Ok(Json(ApiResponse {
        ok: true,
        data: VectorOutput { id, vector, meta },
    }))
}

async fn update_vector_meta(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(input): Json<VectorMetaInput>,
) -> Result<Json<ApiResponse<&'static str>>, ApiError> {
    state
        .db
        .update_vector_meta(id, input.meta)
        .map_err(ApiError::from)?;
    Ok(Json(ApiResponse {
        ok: true,
        data: "meta-updated",
    }))
}

async fn delete_vector_meta_keys(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(input): Json<VectorMetaDeleteInput>,
) -> Result<Json<ApiResponse<&'static str>>, ApiError> {
    state
        .db
        .remove_vector_meta_keys(id, &input.keys)
        .map_err(ApiError::from)?;
    Ok(Json(ApiResponse {
        ok: true,
        data: "meta-keys-removed",
    }))
}

async fn search_vector(
    State(state): State<AppState>,
    Json(input): Json<VectorSearchInput>,
) -> Result<Json<ApiResponse<Vec<kaededb_core::VectorSearchResult>>>, ApiError> {
    let k = input.k.unwrap_or(10);
    let metric = match input.metric.as_deref() {
        Some("cosine") => kaededb_core::vector::VectorMetric::Cosine,
        Some("dot") => kaededb_core::vector::VectorMetric::Dot,
        Some("l2") => kaededb_core::vector::VectorMetric::L2,
        _ => kaededb_core::vector::VectorMetric::L2,
    };

    if let Some(ef) = input.ef_search {
        state.db.set_ef_search(ef);
    }

    // For now metric selection is per-query; in future persist per-index config.
    let mut hits =
        state
            .db
            .search_vector_metric(&input.query, k, metric, input.filter_meta.clone())?;
    if let Some(filter_ids) = input.filter_ids {
        let allow: std::collections::HashSet<Uuid> = filter_ids.into_iter().collect();
        hits.retain(|h| allow.contains(&h.id));
    }
    Ok(Json(ApiResponse {
        ok: true,
        data: hits,
    }))
}

async fn update_vector_config(
    State(state): State<AppState>,
    Json(input): Json<VectorConfigInput>,
) -> Result<Json<ApiResponse<&'static str>>, ApiError> {
    if let Some(ef) = input.ef_search {
        state.db.set_ef_search(ef);
    }
    if let Some(efc) = input.ef_construction {
        state.db.set_ef_construction(efc);
    }
    if let Some(k) = input.link_top_k {
        // Arc<KaedeDb> is shared; clone then try to get mutable ref if no other owners.
        let mut db_arc = state.db.clone();
        if let Some(db_mut) = Arc::get_mut(&mut db_arc) {
            db_mut.set_link_top_k(k);
        } else {
            // Fallback: log and skip if currently in use by other handles.
            tracing::warn!("link_top_k update skipped (db state is shared)");
        }
    }
    Ok(Json(ApiResponse {
        ok: true,
        data: "updated",
    }))
}

async fn rebuild_vectors(
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<&'static str>>, ApiError> {
    state.db.rebuild_vectors().map_err(ApiError::from)?;
    Ok(Json(ApiResponse {
        ok: true,
        data: "rebuilt",
    }))
}

async fn vacuum_vectors(
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<&'static str>>, ApiError> {
    state.db.vacuum().map_err(ApiError::from)?;
    Ok(Json(ApiResponse {
        ok: true,
        data: "vacuumed",
    }))
}

async fn save_snapshot(
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<&'static str>>, ApiError> {
    state.db.save_vector_snapshot().map_err(ApiError::from)?;
    Ok(Json(ApiResponse {
        ok: true,
        data: "saved",
    }))
}

async fn add_edge(
    State(state): State<AppState>,
    Json(input): Json<EdgeInput>,
) -> Result<Json<ApiResponse<&'static str>>, ApiError> {
    let weight = input.weight.unwrap_or(1.0);
    state
        .db
        .add_edge(input.src, input.dst, weight)
        .map_err(ApiError::from)?;
    Ok(Json(ApiResponse {
        ok: true,
        data: "stored",
    }))
}

async fn list_neighbors(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<Vec<kaededb_core::Edge>>>, ApiError> {
    let edges = state.db.neighbors(id, 100);
    Ok(Json(ApiResponse {
        ok: true,
        data: edges,
    }))
}

async fn metrics(
    State(state): State<AppState>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    let m = state.db.metrics();
    let body = format!(
        "kaededb_docs {}\nkaededb_rows {}\nkaededb_vectors {}\nkaededb_vector_tombstones {}\nkaededb_hnsw_ready {}\nkaededb_ef_search {}\nkaededb_ef_construction {}\nkaededb_link_top_k {}\n",
        m.docs,
        m.rows,
        m.vectors,
        m.vector_tombstones,
        m.hnsw_ready as u8,
        m.ef_search,
        m.ef_construction,
        m.link_top_k,
    );
    let resp = (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        body,
    );
    Ok(resp)
}

#[derive(Debug)]
enum ApiError {
    NotFound,
    Internal(anyhow::Error),
}

impl From<KaedeDbError> for ApiError {
    fn from(value: KaedeDbError) -> Self {
        match value {
            KaedeDbError::NotFound => ApiError::NotFound,
            other => ApiError::Internal(anyhow::Error::new(other)),
        }
    }
}

impl axum::response::IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        use axum::http::StatusCode;
        match self {
            ApiError::NotFound => StatusCode::NOT_FOUND.into_response(),
            ApiError::Internal(err) => {
                tracing::error!("api_error" = %err);
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}
