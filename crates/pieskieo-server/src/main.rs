use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    extract::{Path, State},
    routing::{delete, get, post},
    Json, Router,
};
use futures::future::join_all;
use pieskieo_core::{PieskieoDb, PieskieoError, VectorParams as PieskieoVectorParams};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    pool: Arc<DbPool>,
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

struct DbPool {
    shards: Vec<Arc<PieskieoDb>>,
}

impl DbPool {
    fn new(base_dir: &str, params: PieskieoVectorParams, shards: usize) -> anyhow::Result<Self> {
        let mut v = Vec::with_capacity(shards.max(1));
        for i in 0..shards.max(1) {
            let mut p = params.clone();
            p.shard_id = i;
            p.shard_total = shards.max(1);
            let dir = if shards > 1 {
                format!("{base_dir}/shard{i}")
            } else {
                base_dir.to_string()
            };
            std::fs::create_dir_all(&dir)?;
            v.push(Arc::new(PieskieoDb::open_with_params(&dir, p)?));
        }
        Ok(Self { shards: v })
    }

    fn shard_for(&self, id: &Uuid) -> Arc<PieskieoDb> {
        if self.shards.len() == 1 {
            return self.shards[0].clone();
        }
        let mut arr = [0u8; 8];
        arr.copy_from_slice(&id.as_bytes()[..8]);
        let idx = (u64::from_le_bytes(arr) as usize) % self.shards.len();
        self.shards[idx].clone()
    }

    fn each(&self) -> impl Iterator<Item = Arc<PieskieoDb>> + '_ {
        self.shards.iter().cloned()
    }

    fn aggregate_metrics(&self) -> pieskieo_core::engine::MetricsSnapshot {
        let mut agg = pieskieo_core::engine::MetricsSnapshot {
            docs: 0,
            rows: 0,
            vectors: 0,
            vector_tombstones: 0,
            hnsw_ready: true,
            ef_search: 0,
            ef_construction: 0,
            wal_path: std::path::PathBuf::new(),
            wal_bytes: 0,
            snapshot_mtime: None,
            link_top_k: 0,
            shard_id: 0,
            shard_total: self.shards.len(),
        };
        for shard in &self.shards {
            let m = shard.metrics();
            agg.docs += m.docs;
            agg.rows += m.rows;
            agg.vectors += m.vectors;
            agg.vector_tombstones += m.vector_tombstones;
            agg.hnsw_ready &= m.hnsw_ready;
            agg.ef_search = m.ef_search;
            agg.ef_construction = m.ef_construction;
            agg.link_top_k = m.link_top_k;
            agg.wal_bytes += m.wal_bytes;
            agg.snapshot_mtime = agg.snapshot_mtime.or(m.snapshot_mtime);
        }
        agg
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let data_dir = std::env::var("PIESKIEO_DATA").unwrap_or_else(|_| "data".to_string());
    let params = vector_params_from_env();
    let shards = params.shard_total.max(1);
    let pool = Arc::new(DbPool::new(&data_dir, params, shards)?);

    let state = AppState { pool };

    // background WAL flusher (group commit) for better latency.
    let flush_ms = std::env::var("PIESKIEO_WAL_FLUSH_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(50);
    {
        let pool = state.pool.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(flush_ms));
            loop {
                interval.tick().await;
                for shard in pool.each() {
                    if let Err(e) = shard.flush_wal() {
                        tracing::warn!("wal flush failed: {e}");
                    }
                }
            }
        });
    }

    if let Ok(secs) = std::env::var("PIESKIEO_SNAPSHOT_INTERVAL_SECS") {
        if let Ok(secs) = secs.parse::<u64>() {
            let pool = state.pool.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(secs));
                loop {
                    interval.tick().await;
                    for shard in pool.each() {
                        if let Err(e) = shard.save_vector_snapshot() {
                            tracing::warn!("snapshot save failed: {e}");
                        }
                    }
                }
            });
        }
    }

    if let Ok(secs) = std::env::var("PIESKIEO_REBUILD_INTERVAL_SECS") {
        if let Ok(secs) = secs.parse::<u64>() {
            let pool = state.pool.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(secs));
                loop {
                    interval.tick().await;
                    for shard in pool.each() {
                        if let Err(e) = shard.rebuild_vectors() {
                            tracing::warn!("vector rebuild failed: {e}");
                        }
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
        .route("/v1/shard/which/:id", get(which_shard))
        .route("/v1/vector/search", post(search_vector))
        .route("/v1/vector/rebuild", post(rebuild_vectors))
        .route("/v1/vector/snapshot/save", post(save_snapshot))
        .route("/v1/vector/bulk", post(put_vector_bulk))
        .route("/v1/vector/:id", delete(delete_vector))
        .route("/metrics", get(metrics))
        .route("/v1/graph/edge", post(add_edge))
        .route("/v1/graph/:id", get(list_neighbors))
        .route("/v1/graph/:id/bfs", get(list_bfs))
        .route("/v1/graph/:id/dfs", get(list_dfs))
        .with_state(state);

    let addr: SocketAddr = std::env::var("PIESKIEO_LISTEN")
        .unwrap_or_else(|_| "0.0.0.0:8000".into())
        .parse()?;

    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "listening");
    axum::serve(listener, app).await?;
    Ok(())
}

fn vector_params_from_env() -> PieskieoVectorParams {
    let metric = match std::env::var("PIESKIEO_VECTOR_METRIC")
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "cosine" => pieskieo_core::vector::VectorMetric::Cosine,
        "dot" => pieskieo_core::vector::VectorMetric::Dot,
        _ => pieskieo_core::vector::VectorMetric::L2,
    };
    let ef_c = std::env::var("PIESKIEO_EF_CONSTRUCTION")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(200);
    let ef_s = std::env::var("PIESKIEO_EF_SEARCH")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(50);
    let max_el = std::env::var("PIESKIEO_VEC_MAX_ELEMENTS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(100_000);
    let link_top_k = std::env::var("PIESKIEO_LINK_K")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(4);
    let shard_total = std::env::var("PIESKIEO_SHARD_TOTAL")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(1);
    let shard_id = std::env::var("PIESKIEO_SHARD_ID")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);

    PieskieoVectorParams {
        metric,
        ef_construction: ef_c,
        ef_search: ef_s,
        max_elements: max_el,
        link_top_k,
        shard_id,
        shard_total,
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
    state
        .pool
        .shard_for(&id)
        .put_doc(id, input.data)
        .map_err(ApiError::from)?;
    Ok(Json(ApiResponse { ok: true, data: id }))
}

async fn get_doc(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<serde_json::Value>>, ApiError> {
    let doc = state
        .pool
        .shard_for(&id)
        .get_doc(&id)
        .ok_or(ApiError::NotFound)?;
    Ok(Json(ApiResponse {
        ok: true,
        data: doc,
    }))
}

async fn delete_doc(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<&'static str>>, ApiError> {
    state
        .pool
        .shard_for(&id)
        .delete_doc(&id)
        .map_err(ApiError::from)?;
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
    state
        .pool
        .shard_for(&id)
        .put_row(id, &input.data)
        .map_err(ApiError::from)?;
    Ok(Json(ApiResponse { ok: true, data: id }))
}

async fn get_row(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<serde_json::Value>>, ApiError> {
    let row = state
        .pool
        .shard_for(&id)
        .get_row(&id)
        .ok_or(ApiError::NotFound)?;
    Ok(Json(ApiResponse {
        ok: true,
        data: row,
    }))
}

async fn delete_row(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<&'static str>>, ApiError> {
    state
        .pool
        .shard_for(&id)
        .delete_row(&id)
        .map_err(ApiError::from)?;
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
        .pool
        .shard_for(&input.id)
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
            .pool
            .shard_for(&item.id)
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
    state
        .pool
        .shard_for(&id)
        .delete_vector(&id)
        .map_err(ApiError::from)?;
    Ok(Json(ApiResponse {
        ok: true,
        data: "deleted",
    }))
}

async fn get_vector(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<VectorOutput>>, ApiError> {
    let (vector, meta) = state
        .pool
        .shard_for(&id)
        .get_vector(&id)
        .ok_or(ApiError::NotFound)?;
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
        .pool
        .shard_for(&id)
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
        .pool
        .shard_for(&id)
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
) -> Result<Json<ApiResponse<Vec<pieskieo_core::VectorSearchResult>>>, ApiError> {
    let k = input.k.unwrap_or(10);
    let metric = match input.metric.as_deref() {
        Some("cosine") => pieskieo_core::vector::VectorMetric::Cosine,
        Some("dot") => pieskieo_core::vector::VectorMetric::Dot,
        Some("l2") => pieskieo_core::vector::VectorMetric::L2,
        _ => pieskieo_core::vector::VectorMetric::L2,
    };

    if let Some(ef) = input.ef_search {
        for shard in state.pool.each() {
            shard.set_ef_search(ef);
        }
    }

    // For now metric selection is per-query; in future persist per-index config.
    let futures = state
        .pool
        .each()
        .map(|shard| {
            let q = input.query.clone();
            let filter = input.filter_meta.clone();
            tokio::task::spawn_blocking(move || shard.search_vector_metric(&q, k, metric, filter))
        })
        .collect::<Vec<_>>();
    let mut all_hits = Vec::new();
    for res in join_all(futures).await {
        if let Ok(Ok(mut h)) = res {
            all_hits.append(&mut h);
        }
    }
    all_hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    all_hits.truncate(k);
    let mut hits = all_hits;
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
        for shard in state.pool.each() {
            shard.set_ef_search(ef);
        }
    }
    if let Some(efc) = input.ef_construction {
        for shard in state.pool.each() {
            shard.set_ef_construction(efc);
        }
    }
    if let Some(k) = input.link_top_k {
        for shard in state.pool.each() {
            if let Some(db_mut) = Arc::get_mut(&mut shard.clone()) {
                db_mut.set_link_top_k(k);
            } else {
                tracing::warn!("link_top_k update skipped (db state is shared)");
            }
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
    for shard in state.pool.each() {
        shard.rebuild_vectors().map_err(ApiError::from)?;
    }
    Ok(Json(ApiResponse {
        ok: true,
        data: "rebuilt",
    }))
}

async fn vacuum_vectors(
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<&'static str>>, ApiError> {
    for shard in state.pool.each() {
        shard.vacuum().map_err(ApiError::from)?;
    }
    Ok(Json(ApiResponse {
        ok: true,
        data: "vacuumed",
    }))
}

async fn save_snapshot(
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<&'static str>>, ApiError> {
    for shard in state.pool.each() {
        shard.save_vector_snapshot().map_err(ApiError::from)?;
    }
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
        .pool
        .shard_for(&input.src)
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
) -> Result<Json<ApiResponse<Vec<pieskieo_core::Edge>>>, ApiError> {
    let edges = state.pool.shard_for(&id).neighbors(id, 100);
    Ok(Json(ApiResponse {
        ok: true,
        data: edges,
    }))
}

async fn list_bfs(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<Vec<pieskieo_core::Edge>>>, ApiError> {
    let edges = state.pool.shard_for(&id).bfs(id, 100);
    Ok(Json(ApiResponse { ok: true, data: edges }))
}

async fn list_dfs(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<Vec<pieskieo_core::Edge>>>, ApiError> {
    let edges = state.pool.shard_for(&id).dfs(id, 100);
    Ok(Json(ApiResponse { ok: true, data: edges }))
}

async fn which_shard(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<HashMap<&'static str, usize>>>, ApiError> {
    let shard_total = state.pool.shards.len();
    let mut arr = [0u8; 8];
    arr.copy_from_slice(&id.as_bytes()[..8]);
    let shard_id = (u64::from_le_bytes(arr) as usize) % shard_total;
    let mut map = HashMap::new();
    map.insert("shard_id", shard_id);
    map.insert("shard_total", shard_total);
    Ok(Json(ApiResponse {
        ok: true,
        data: map,
    }))
}

async fn metrics(
    State(state): State<AppState>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    let m = state.pool.aggregate_metrics();
    let mut body = format!(
        "pieskieo_docs {}\npieskieo_rows {}\npieskieo_vectors {}\npieskieo_vector_tombstones {}\npieskieo_hnsw_ready {}\npieskieo_ef_search {}\npieskieo_ef_construction {}\npieskieo_link_top_k {}\npieskieo_shard_total {}\n",
        m.docs,
        m.rows,
        m.vectors,
        m.vector_tombstones,
        m.hnsw_ready as u8,
        m.ef_search,
        m.ef_construction,
        m.link_top_k,
        m.shard_total,
    );
    for (idx, shard) in state.pool.shards.iter().enumerate() {
        let s = shard.metrics();
        body.push_str(&format!(
            "pieskieo_shard_vectors{{shard=\"{}\"}} {}\npieskieo_shard_docs{{shard=\"{}\"}} {}\npieskieo_shard_rows{{shard=\"{}\"}} {}\n",
            idx, s.vectors, idx, s.docs, idx, s.rows
        ));
    }
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
    WrongShard,
    Internal(anyhow::Error),
}

impl From<PieskieoError> for ApiError {
    fn from(value: PieskieoError) -> Self {
        match value {
            PieskieoError::NotFound => ApiError::NotFound,
            PieskieoError::WrongShard => ApiError::WrongShard,
            other => ApiError::Internal(anyhow::Error::new(other)),
        }
    }
}

impl axum::response::IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        use axum::http::StatusCode;
        match self {
            ApiError::NotFound => StatusCode::NOT_FOUND.into_response(),
            ApiError::WrongShard => StatusCode::CONFLICT.into_response(),
            ApiError::Internal(err) => {
                tracing::error!("api_error" = %err);
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}
