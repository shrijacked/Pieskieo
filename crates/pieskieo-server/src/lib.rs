use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::Request,
    middleware::{self, Next},
    routing::{delete, get, post},
    Json, Router, Extension,
};
use sqlparser::{dialect::GenericDialect, parser::Parser};
use futures::future::join_all;
use pieskieo_core::{
    PieskieoDb, PieskieoError, SchemaDef, SchemaField, SqlResult, VectorParams as PieskieoVectorParams,
};
use serde::{Deserialize, Serialize};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use std::path::PathBuf;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    pool: Arc<DbPool>,
    auth: Arc<AuthConfig>,
    data_dir: PathBuf,
}

#[derive(Clone)]
struct AuthConfig {
    users: Vec<UserRec>,
    bearer: Option<String>,
    path: PathBuf,
}

#[derive(Clone)]
struct UserRec {
    user: String,
    password: String,
    role: Role,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[derive(Debug)]
enum Role {
    Admin,
    Write,
    Read,
}

impl AuthConfig {
    fn from_env(data_dir: &str) -> Self {
        // primary multi-user source: PIESKIEO_USERS as JSON array [{user,pass,role}]
        let mut users = Vec::new();
        let path = PathBuf::from(data_dir).join("auth_users.json");
        if let Ok(json) = std::env::var("PIESKIEO_USERS") {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&json) {
                if let Some(arr) = val.as_array() {
                    for item in arr {
                        if let (Some(u), Some(p)) =
                            (item.get("user").and_then(|v| v.as_str()), item.get("pass").and_then(|v| v.as_str()))
                        {
                            let role = item
                                .get("role")
                                .and_then(|v| v.as_str())
                                .map(Self::parse_role)
                                .unwrap_or(Role::Read);
                            users.push(UserRec {
                                user: u.to_string(),
                                password: p.to_string(),
                                role,
                            });
                        }
                    }
                }
            }
        }
        // fallback single user env
        if users.is_empty() {
            if let Ok(u) = std::env::var("PIESKIEO_AUTH_USER") {
                if let Ok(p) = std::env::var("PIESKIEO_AUTH_PASSWORD") {
                    users.push(UserRec {
                        user: u,
                        password: p,
                        role: Role::Admin,
                    });
                }
            }
        }
        // file-based persistent users
        if users.is_empty() && path.exists() {
            if let Ok(txt) = std::fs::read_to_string(&path) {
                if let Ok(arr) = serde_json::from_str::<Vec<UserDisk>>(&txt) {
                    for u in arr {
                        users.push(UserRec {
                            user: u.user,
                            password: u.pass,
                            role: Self::parse_role(&u.role),
                        });
                    }
                }
            }
        }
        let bearer = std::env::var("PIESKIEO_TOKEN").ok();
        // default admin if nothing configured
        if users.is_empty() && bearer.is_none() {
            users.push(UserRec {
                user: "Pieskieo".into(),
                password: "pieskieo".into(),
                role: Role::Admin,
            });
        }
        Self { users, bearer, path }
    }

    fn enabled(&self) -> bool {
        !self.users.is_empty() || self.bearer.is_some()
    }

    fn parse_role(s: &str) -> Role {
        match s.to_ascii_lowercase().as_str() {
            "admin" => Role::Admin,
            "write" | "writer" => Role::Write,
            _ => Role::Read,
        }
    }

    fn persist(&self) {
        let disk: Vec<UserDisk> = self
            .users
            .iter()
            .map(|u| UserDisk {
                user: u.user.clone(),
                pass: u.password.clone(),
                role: match u.role {
                    Role::Admin => "admin",
                    Role::Write => "write",
                    Role::Read => "read",
                }
                .to_string(),
            })
            .collect();
        if let Ok(txt) = serde_json::to_string_pretty(&disk) {
            let _ = std::fs::create_dir_all(self.path.parent().unwrap_or_else(|| std::path::Path::new(".")));
            let _ = std::fs::write(&self.path, txt);
        }
    }
}

#[derive(Serialize, Deserialize)]
struct UserDisk {
    user: String,
    pass: String,
    role: String,
}

#[derive(Deserialize)]
struct DocInput {
    id: Option<Uuid>,
    data: serde_json::Value,
    namespace: Option<String>,
    collection: Option<String>,
}

#[derive(Deserialize)]
struct QueryInput {
    filter: HashMap<String, serde_json::Value>,
    limit: Option<usize>,
    namespace: Option<String>,
    collection: Option<String>,
    table: Option<String>,
    offset: Option<usize>,
    sql: Option<String>,
}

#[derive(Deserialize)]
struct SqlInput {
    sql: String,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct SchemaInput {
    family: String, // "doc" or "row"
    namespace: Option<String>,
    name: String,
    fields: HashMap<String, SchemaField>,
}

#[derive(Deserialize)]
struct UserCreateInput {
    user: String,
    pass: String,
    role: Option<String>,
}

#[derive(Deserialize)]
struct VectorInput {
    id: Uuid,
    vector: Vec<f32>,
    meta: Option<HashMap<String, String>>,
    namespace: Option<String>,
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
    namespace: Option<String>,
    table: Option<String>,
}

#[derive(Deserialize)]
struct VectorSearchInput {
    query: Vec<f32>,
    k: Option<usize>,
    metric: Option<String>,
    filter_ids: Option<Vec<Uuid>>,
    ef_search: Option<usize>,
    filter_meta: Option<HashMap<String, String>>,
    namespace: Option<String>,
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

#[derive(Deserialize)]
struct NsParams {
    namespace: Option<String>,
    collection: Option<String>,
    table: Option<String>,
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

pub async fn serve() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let data_dir = std::env::var("PIESKIEO_DATA").unwrap_or_else(|_| "data".to_string());
    let auth = Arc::new(AuthConfig::from_env(&data_dir));
    let params = vector_params_from_env();
    let shards = params.shard_total.max(1);
    let pool = Arc::new(DbPool::new(&data_dir, params, shards)?);

    let state = AppState { pool, auth, data_dir: data_dir.clone().into() };

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
        .route("/v1/doc/query", post(query_docs))
        .route("/v1/row", post(put_row))
        .route("/v1/row/:id", get(get_row))
        .route("/v1/row/:id", delete(delete_row))
        .route("/v1/row/query", post(query_rows))
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
        .route("/v1/schema", post(set_schema))
        .route("/v1/sql", post(query_sql))
        .route("/metrics", get(metrics))
        .route("/v1/graph/edge", post(add_edge))
        .route("/v1/graph/:id", get(list_neighbors))
        .route("/v1/graph/:id/bfs", get(list_bfs))
        .route("/v1/graph/:id/dfs", get(list_dfs))
        .route("/v1/auth/users", get(list_users))
        .route("/v1/auth/users", post(create_user))
        .layer(middleware::from_fn_with_state(
            state.auth.clone(),
            auth_middleware,
        ))
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
        .put_doc_ns(
            input.namespace.as_deref(),
            input.collection.as_deref(),
            id,
            input.data,
        )
        .map_err(ApiError::from)?;
    Ok(Json(ApiResponse { ok: true, data: id }))
}

async fn get_doc(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(ns): Query<NsParams>,
) -> Result<Json<ApiResponse<serde_json::Value>>, ApiError> {
    let doc = state
        .pool
        .shard_for(&id)
        .get_doc_ns(ns.namespace.as_deref(), ns.collection.as_deref(), &id)
        .ok_or(ApiError::NotFound)?;
    Ok(Json(ApiResponse {
        ok: true,
        data: doc,
    }))
}

async fn delete_doc(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(ns): Query<NsParams>,
) -> Result<Json<ApiResponse<&'static str>>, ApiError> {
    state
        .pool
        .shard_for(&id)
        .delete_doc_ns(ns.namespace.as_deref(), ns.collection.as_deref(), &id)
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
        .put_row_ns(
            input.namespace.as_deref(),
            input.table.as_deref(),
            id,
            &input.data,
        )
        .map_err(ApiError::from)?;
    Ok(Json(ApiResponse { ok: true, data: id }))
}

async fn query_docs(
    State(state): State<AppState>,
    Json(input): Json<QueryInput>,
) -> Result<Json<ApiResponse<Vec<(Uuid, serde_json::Value)>>>, ApiError> {
    let mut hits = Vec::new();
    if let Some(sql) = input.sql {
        for shard in state.pool.each() {
            match shard.query_sql(&sql)? {
                SqlResult::Select(mut rows) => {
                    hits.append(&mut rows);
                }
                _ => return Err(ApiError::BadRequest("SQL must be SELECT".into())),
            }
        }
        if let Some(limit) = input.limit {
            hits.truncate(limit);
        }
    } else {
        for shard in state.pool.each() {
            hits.extend(shard.query_docs_ns(
                input.namespace.as_deref(),
                input.collection.as_deref(),
                &input.filter,
                input.limit.unwrap_or(100),
                input.offset.unwrap_or(0),
            ));
        }
    }
    Ok(Json(ApiResponse {
        ok: true,
        data: hits,
    }))
}

async fn query_sql(
    State(state): State<AppState>,
    Extension(role): Extension<Role>,
    Json(input): Json<SqlInput>,
) -> Result<Json<ApiResponse<serde_json::Value>>, ApiError> {
    if !authorize(role, "/v1/sql", "POST") {
        return Err(ApiError::Forbidden);
    }
    let dialect = GenericDialect {};
    let ast = Parser::parse_sql(&dialect, &input.sql)
        .map_err(|e: sqlparser::parser::ParserError| ApiError::BadRequest(e.to_string()))?;
    if ast.is_empty() {
        return Err(ApiError::BadRequest("empty SQL".into()));
    }
    let first = &ast[0];
    let is_select = matches!(first, sqlparser::ast::Statement::Query(_));
    if is_select {
        let mut rows = Vec::new();
        for shard in state.pool.each() {
            match shard.query_sql(&input.sql)? {
                SqlResult::Select(mut r) => rows.append(&mut r),
                _ => {}
            }
        }
        if let Some(limit) = input.limit {
            rows.truncate(limit);
        }
        return Ok(Json(ApiResponse {
            ok: true,
            data: serde_json::json!({ "kind": "select", "rows": rows }),
        }));
    }

    // non-select: route to first shard (or broadcast for update/delete)
    match first {
        sqlparser::ast::Statement::Update { .. } | sqlparser::ast::Statement::Delete { .. } => {
            let mut affected = 0usize;
            for shard in state.pool.each() {
            match shard.query_sql(&input.sql)? {
                SqlResult::Update { affected: a } | SqlResult::Delete { affected: a } => {
                    affected += a;
                }
                _ => {}
            }
        }
            Ok(Json(ApiResponse {
                ok: true,
                data: serde_json::json!({ "kind": "write", "affected": affected }),
            }))
        }
        sqlparser::ast::Statement::Insert { .. } => {
            // choose shard 0 for now
            let shard = state.pool.shards[0].clone();
            match shard.query_sql(&input.sql)? {
                SqlResult::Insert { ids } => Ok(Json(ApiResponse {
                    ok: true,
                    data: serde_json::json!({ "kind": "insert", "ids": ids }),
                })),
                _ => Err(ApiError::Internal(anyhow::anyhow!("unexpected result"))),
            }
        }
        _ => Err(ApiError::BadRequest(
            "statement type not supported in /v1/sql".into(),
        )),
    }
}

async fn set_schema(
    State(state): State<AppState>,
    Extension(role): Extension<Role>,
    Json(input): Json<SchemaInput>,
) -> Result<Json<ApiResponse<&'static str>>, ApiError> {
    if !authorize(role, "/v1/schema", "POST") {
        return Err(ApiError::Forbidden);
    }
    let def = SchemaDef {
        fields: input.fields,
    };
    match input.family.as_str() {
        "doc" | "docs" | "collection" | "collections" => {
            for shard in state.pool.each() {
                shard.set_doc_schema(input.namespace.as_deref(), Some(&input.name), def.clone())?;
            }
        }
        "row" | "rows" | "table" | "tables" => {
            for shard in state.pool.each() {
                shard.set_row_schema(input.namespace.as_deref(), Some(&input.name), def.clone())?;
            }
        }
        _ => return Err(ApiError::BadRequest("family must be doc or row".into())),
    }
    Ok(Json(ApiResponse {
        ok: true,
        data: "schema set",
    }))
}

async fn query_rows(
    State(state): State<AppState>,
    Json(input): Json<QueryInput>,
) -> Result<Json<ApiResponse<Vec<(Uuid, serde_json::Value)>>>, ApiError> {
    let mut hits = Vec::new();
    if let Some(sql) = input.sql {
        for shard in state.pool.each() {
            match shard.query_sql(&sql)? {
                SqlResult::Select(mut rows) => {
                    hits.append(&mut rows);
                }
                _ => return Err(ApiError::BadRequest("SQL must be SELECT".into())),
            }
        }
        if let Some(limit) = input.limit {
            hits.truncate(limit);
        }
    } else {
        for shard in state.pool.each() {
            hits.extend(shard.query_rows_ns(
                input.namespace.as_deref(),
                input.table.as_deref(),
                &input.filter,
                input.limit.unwrap_or(100),
                input.offset.unwrap_or(0),
            ));
        }
    }
    Ok(Json(ApiResponse {
        ok: true,
        data: hits,
    }))
}

async fn get_row(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(ns): Query<NsParams>,
) -> Result<Json<ApiResponse<serde_json::Value>>, ApiError> {
    let row = state
        .pool
        .shard_for(&id)
        .get_row_ns(ns.namespace.as_deref(), ns.table.as_deref(), &id)
        .ok_or(ApiError::NotFound)?;
    Ok(Json(ApiResponse {
        ok: true,
        data: row,
    }))
}

async fn delete_row(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(ns): Query<NsParams>,
) -> Result<Json<ApiResponse<&'static str>>, ApiError> {
    state
        .pool
        .shard_for(&id)
        .delete_row_ns(ns.namespace.as_deref(), ns.table.as_deref(), &id)
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
        .put_vector_with_meta_ns(
            input.namespace.as_deref(),
            input.id,
            input.vector,
            input.meta,
        )
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
            .put_vector_with_meta_ns(
                item.namespace.as_deref(),
                item.id,
                item.vector.clone(),
                item.meta.clone(),
            )
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
            let ns = input.namespace.clone();
            tokio::task::spawn_blocking(move || match ns {
                Some(ref ns) => {
                    shard.search_vector_metric_ns(Some(ns.as_str()), &q, k, metric, filter)
                }
                None => shard.search_vector_metric(&q, k, metric, filter),
            })
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
    Ok(Json(ApiResponse {
        ok: true,
        data: edges,
    }))
}

async fn list_dfs(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<Vec<pieskieo_core::Edge>>>, ApiError> {
    let edges = state.pool.shard_for(&id).dfs(id, 100);
    Ok(Json(ApiResponse {
        ok: true,
        data: edges,
    }))
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

async fn auth_middleware(
    State(auth): State<Arc<AuthConfig>>,
    req: Request<Body>,
    next: Next,
) -> Result<axum::response::Response, ApiError> {
    if !auth.enabled() {
        return Ok(next.run(req).await);
    }
    if let Some(header) = req.headers().get(axum::http::header::AUTHORIZATION) {
        if let Ok(val) = header.to_str() {
            if let Some(tok) = val.strip_prefix("Bearer ") {
                if let Some(expected) = &auth.bearer {
                    if tok == expected {
                        return Ok(next.run(req).await);
                    }
                }
            }
            if let Some(basic) = val.strip_prefix("Basic ") {
                if let Ok(decoded) = B64.decode(basic) {
                    if let Ok(s) = String::from_utf8(decoded) {
                        if let Some((u, p)) = s.split_once(':') {
                            if let Some(user) =
                                auth.users.iter().find(|usr| usr.user == u && usr.password == p)
                            {
                                if authorize(user.role, req.uri().path(), req.method().as_str()) {
                                    let mut req = req;
                                    req.extensions_mut().insert(user.role);
                                    return Ok(next.run(req).await);
                                } else {
                                    return Err(ApiError::Forbidden);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Err(ApiError::Unauthorized)
}

fn authorize(role: Role, path: &str, method: &str) -> bool {
    use Role::*;
    if matches!(role, Admin) {
        return true;
    }
    let is_read = is_read_path(path, method);
    match role {
        Read => is_read,
        Write => is_read || is_write_path(path, method),
        Admin => true,
    }
}

fn is_read_path(path: &str, method: &str) -> bool {
    if path == "/healthz" || path == "/metrics" {
        return true;
    }
    let m = method.to_uppercase();
    if m == "GET" {
        return true;
    }
    // vector search is POST but read
    if path.contains("/vector/search") && m == "POST" {
        return true;
    }
    if path.contains("/graph") && m == "GET" {
        return true;
    }
    false
}

fn is_write_path(path: &str, method: &str) -> bool {
    let m = method.to_uppercase();
    if m == "POST" || m == "DELETE" || m == "PUT" || m == "PATCH" {
        return true;
    }
    // SQL endpoint treated as write by default
    if path.contains("/v1/sql") {
        return true;
    }
    false
}

#[derive(Debug)]
enum ApiError {
    NotFound,
    WrongShard,
    BadRequest(String),
    Conflict(String),
    Unauthorized,
    Forbidden,
    Internal(anyhow::Error),
}

impl From<PieskieoError> for ApiError {
    fn from(value: PieskieoError) -> Self {
        match value {
            PieskieoError::NotFound => ApiError::NotFound,
            PieskieoError::WrongShard => ApiError::WrongShard,
            PieskieoError::Validation(msg) => ApiError::BadRequest(msg),
            PieskieoError::UniqueViolation(field) => {
                ApiError::Conflict(format!("unique constraint on field {field}"))
            }
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
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
            ApiError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
            ApiError::Unauthorized => StatusCode::UNAUTHORIZED.into_response(),
            ApiError::Forbidden => StatusCode::FORBIDDEN.into_response(),
            ApiError::Internal(err) => {
                tracing::error!("api_error" = %err);
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}
async fn list_users(
    State(state): State<AppState>,
    Extension(role): Extension<Role>,
) -> Result<Json<ApiResponse<Vec<String>>>, ApiError> {
    if !matches!(role, Role::Admin) {
        return Err(ApiError::Forbidden);
    }
    let users = state
        .auth
        .users
        .iter()
        .map(|u| format!("{} ({:?})", u.user, u.role))
        .collect();
    Ok(Json(ApiResponse { ok: true, data: users }))
}

async fn create_user(
    State(state): State<AppState>,
    Extension(role): Extension<Role>,
    Json(input): Json<UserCreateInput>,
) -> Result<Json<ApiResponse<&'static str>>, ApiError> {
    if !matches!(role, Role::Admin) {
        return Err(ApiError::Forbidden);
    }
    let mut auth = state.auth.as_ref().clone();
    let role = input
        .role
        .as_deref()
        .map(AuthConfig::parse_role)
        .unwrap_or(Role::Read);
    auth.users.push(UserRec {
        user: input.user,
        password: input.pass,
        role,
    });
    auth.persist();
    // replace shared auth
    *Arc::get_mut(&mut Arc::clone(&state.auth)).unwrap() = auth;
    Ok(Json(ApiResponse { ok: true, data: "created" }))
}
