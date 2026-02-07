use crate::error::Result;
use crate::vector::{VectorIndex, VectorMetric};
use crate::wal::{DataFamily, RecordKind, Wal};
use crate::{error::PieskieoError, graph::GraphStore};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlparser::ast::{
    BinaryOperator, Expr, Function, FunctionArg, FunctionArgExpr, JoinConstraint, JoinOperator,
    OrderByExpr, Select, SelectItem, SetExpr, Statement, TableFactor,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Default)]
pub(crate) struct Collections {
    // namespace -> collection/table -> (id -> payload)
    rows: HashMap<String, HashMap<String, BTreeMap<Uuid, Value>>>,
    docs: HashMap<String, HashMap<String, BTreeMap<Uuid, Value>>>,
    // simple equality secondary index: ns -> collection -> field -> value_json -> ids
    row_index: HashMap<String, HashMap<String, HashMap<String, HashMap<String, Vec<Uuid>>>>>,
    doc_index: HashMap<String, HashMap<String, HashMap<String, HashMap<String, Vec<Uuid>>>>>,
    // schemas
    row_schema: HashMap<String, HashMap<String, SchemaDef>>,
    doc_schema: HashMap<String, HashMap<String, SchemaDef>>,
}

pub struct PieskieoDb {
    path: PathBuf,
    pub(crate) wal: RwLock<Wal>,
    pub(crate) data: Arc<RwLock<Collections>>,
    stats: Arc<RwLock<Stats>>,
    // namespace -> vector index
    pub(crate) vectors: Arc<RwLock<HashMap<String, Arc<VectorIndex>>>>,
    // vector id -> namespace (for auto-link + delete convenience)
    pub(crate) vector_ns: Arc<RwLock<HashMap<Uuid, String>>>,
    pub(crate) graph: GraphStore,
    link_top_k: usize,
    shard_id: usize,
    shard_total: usize,
    default_params: VectorParams,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SchemaField {
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub unique: bool,
    #[serde(default)]
    pub r#type: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SchemaDef {
    pub fields: HashMap<String, SchemaField>,
}

#[derive(Default, Clone, Serialize, Deserialize, Debug)]
pub struct Stats {
    docs: HashMap<String, HashMap<String, usize>>,
    rows: HashMap<String, HashMap<String, usize>>,
}

#[derive(Clone, Debug, Serialize)]
pub enum SqlResult {
    Select(Vec<(Uuid, Value)>),
    Insert { ids: Vec<Uuid> },
    Update { affected: usize },
    Delete { affected: usize },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpsertDoc {
    pub id: Uuid,
    pub json: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VecWalRecord {
    #[serde(default)]
    namespace: Option<String>,
    vector: Vec<f32>,
    meta: Option<HashMap<String, String>>,
}

impl PieskieoDb {
    fn ns(ns: Option<&str>) -> String {
        ns.unwrap_or("default").to_string()
    }

    fn col(col: Option<&str>) -> String {
        col.unwrap_or("default").to_string()
    }

    fn default_ns() -> String {
        "default".to_string()
    }

    fn enforce_doc_schema(
        &self,
        ns: Option<&str>,
        collection: Option<&str>,
        id: &Uuid,
        json: &Value,
    ) -> Result<()> {
        let ns_key = Self::ns(ns);
        let col_key = Self::col(collection);
        let guard = self.data.read();
        if let Some(schema) = guard.doc_schema.get(&ns_key).and_then(|m| m.get(&col_key)) {
            Self::validate_object(json)?;
            Self::check_schema(
                id,
                json,
                schema,
                guard.doc_index.get(&ns_key).and_then(|m| m.get(&col_key)),
            )?;
        }
        Ok(())
    }

    fn bump_doc_stats(&self, ns: &str, coll: &str, delta: i64) {
        let mut stats = self.stats.write();
        let entry = stats
            .docs
            .entry(ns.to_string())
            .or_default()
            .entry(coll.to_string())
            .or_default();
        if delta.is_negative() {
            *entry = entry.saturating_sub(delta.unsigned_abs() as usize);
        } else {
            *entry += delta as usize;
        }
    }

    fn bump_row_stats(&self, ns: &str, tbl: &str, delta: i64) {
        let mut stats = self.stats.write();
        let entry = stats
            .rows
            .entry(ns.to_string())
            .or_default()
            .entry(tbl.to_string())
            .or_default();
        if delta.is_negative() {
            *entry = entry.saturating_sub(delta.unsigned_abs() as usize);
        } else {
            *entry += delta as usize;
        }
    }

    fn enforce_row_schema(
        &self,
        ns: Option<&str>,
        table: Option<&str>,
        id: &Uuid,
        json: &Value,
    ) -> Result<()> {
        let ns_key = Self::ns(ns);
        let tbl_key = Self::col(table);
        let guard = self.data.read();
        if let Some(schema) = guard.row_schema.get(&ns_key).and_then(|m| m.get(&tbl_key)) {
            Self::validate_object(json)?;
            Self::check_schema(
                id,
                json,
                schema,
                guard.row_index.get(&ns_key).and_then(|m| m.get(&tbl_key)),
            )?;
        }
        Ok(())
    }

    fn validate_object(json: &Value) -> Result<()> {
        if !json.is_object() {
            return Err(PieskieoError::Validation("value must be object".into()));
        }
        Ok(())
    }

    fn check_schema(
        id: &Uuid,
        json: &Value,
        schema: &SchemaDef,
        index: Option<&HashMap<String, HashMap<String, Vec<Uuid>>>>,
    ) -> Result<()> {
        let obj = json
            .as_object()
            .ok_or_else(|| PieskieoError::Validation("value must be object".into()))?;
        for (field, spec) in &schema.fields {
            if spec.required && !obj.contains_key(field) {
                return Err(PieskieoError::Validation(format!(
                    "field '{field}' is required"
                )));
            }
            if spec.unique {
                if let Some(val) = obj.get(field) {
                    if let Some(key) = Self::index_key(val) {
                        if let Some(idx_field) = index.and_then(|m| m.get(field)) {
                            if let Some(ids) = idx_field.get(&key) {
                                if ids.iter().any(|existing| existing != id) {
                                    return Err(PieskieoError::UniqueViolation(field.clone()));
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn exec_insert(&self, stmt: &Statement) -> Result<SqlResult> {
        let insert = match stmt {
            Statement::Insert {
                table_name,
                columns,
                source,
                ..
            } => (table_name, columns, source),
            _ => return Err(PieskieoError::Internal("not insert".into())),
        };
        let (family, ns, coll) = self.split_name(insert.0)?;
        let target_rows = Self::target_is_rows(family.as_deref(), &coll);
        let columns: Vec<String> = insert.1.iter().map(|c| c.value.clone()).collect();
        if columns.is_empty() {
            return Err(PieskieoError::Internal(
                "column list required for INSERT".into(),
            ));
        }
        let query = insert
            .2
            .as_ref()
            .ok_or_else(|| PieskieoError::Internal("INSERT source required".into()))?;
        let values = match query.body.as_ref() {
            SetExpr::Values(v) => {
                if v.rows.len() != 1 {
                    return Err(PieskieoError::Internal(
                        "only single VALUES row supported".into(),
                    ));
                }
                v.rows[0].clone()
            }
            _ => {
                return Err(PieskieoError::Internal(
                    "only VALUES insert supported".into(),
                ))
            }
        };
        if columns.len() != values.len() {
            return Err(PieskieoError::Internal(
                "column count does not match values".into(),
            ));
        }
        let mut obj = serde_json::Map::new();
        for (idx, expr) in values.iter().enumerate() {
            let key = if columns.is_empty() {
                format!("col{idx}")
            } else {
                columns[idx].clone()
            };
            let val = Self::literal_to_value(expr)
                .ok_or_else(|| PieskieoError::Internal("unsupported literal in insert".into()))?;
            obj.insert(key, val);
        }
        // determine id
        let mut id = None;
        for k in &["id", "_id"] {
            if let Some(v) = obj.get(*k) {
                if let Some(s) = v.as_str() {
                    if let Ok(uuid) = Uuid::parse_str(s) {
                        id = Some(uuid);
                    }
                }
            }
        }
        let uid = id.unwrap_or_else(Uuid::new_v4);
        if target_rows {
            self.put_row_ns(Some(&ns), Some(&coll), uid, &Value::Object(obj))?;
        } else {
            self.put_doc_ns(Some(&ns), Some(&coll), uid, Value::Object(obj))?;
        }
        Ok(SqlResult::Insert { ids: vec![uid] })
    }

    fn exec_update(&self, stmt: &Statement) -> Result<SqlResult> {
        let (table, assignments, selection) = match stmt {
            Statement::Update {
                table,
                assignments,
                selection,
                ..
            } => (table, assignments, selection),
            _ => return Err(PieskieoError::Internal("not update".into())),
        };
        let name = self.extract_name_from_table_factor(&table.relation)?;
        let (family, ns, coll) = self.split_name(name)?;
        let target_rows = Self::target_is_rows(family.as_deref(), &coll);
        let conds = if let Some(expr) = selection {
            let mut c = Vec::new();
            self.walk_expr(expr, &mut c)?;
            c
        } else {
            Vec::new()
        };
        // collect matches
        let matches: Vec<(Uuid, Value)> = {
            let guard = self.data.read();
            if target_rows {
                guard
                    .rows
                    .get(&ns)
                    .and_then(|m| m.get(&coll))
                    .map(|map| self.filter_conditions(map, &conds, usize::MAX, 0))
                    .unwrap_or_default()
            } else {
                guard
                    .docs
                    .get(&ns)
                    .and_then(|m| m.get(&coll))
                    .map(|map| self.filter_conditions(map, &conds, usize::MAX, 0))
                    .unwrap_or_default()
            }
        };
        let mut affected = 0usize;
        for (id, mut val) in matches {
            if let Some(obj) = val.as_object_mut() {
                for assign in assignments {
                    let key = assign
                        .id
                        .first()
                        .ok_or_else(|| PieskieoError::Internal("assignment missing column".into()))?
                        .value
                        .clone();
                    let rhs = Self::literal_to_value(&assign.value).ok_or_else(|| {
                        PieskieoError::Internal("assignment literal not supported".into())
                    })?;
                    obj.insert(key, rhs);
                }
            }
            if target_rows {
                self.put_row_ns(Some(&ns), Some(&coll), id, &val)?;
            } else {
                self.put_doc_ns(Some(&ns), Some(&coll), id, val)?;
            }
            affected += 1;
        }
        Ok(SqlResult::Update { affected })
    }

    fn exec_delete(&self, stmt: &Statement) -> Result<SqlResult> {
        let (tables, selection) = match stmt {
            Statement::Delete {
                tables, selection, ..
            } => (tables, selection),
            _ => return Err(PieskieoError::Internal("not delete".into())),
        };
        let table = tables
            .first()
            .ok_or_else(|| PieskieoError::Internal("table required".into()))?;
        let (family, ns, coll) = self.split_name(table)?;
        let target_rows = Self::target_is_rows(family.as_deref(), &coll);
        let conds = if let Some(expr) = selection {
            let mut c = Vec::new();
            self.walk_expr(expr, &mut c)?;
            c
        } else {
            Vec::new()
        };
        let matches: Vec<Uuid> = {
            let guard = self.data.read();
            if target_rows {
                guard
                    .rows
                    .get(&ns)
                    .and_then(|m| m.get(&coll))
                    .map(|map| self.filter_conditions(map, &conds, usize::MAX, 0))
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect()
            } else {
                guard
                    .docs
                    .get(&ns)
                    .and_then(|m| m.get(&coll))
                    .map(|map| self.filter_conditions(map, &conds, usize::MAX, 0))
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect()
            }
        };
        for id in &matches {
            if target_rows {
                self.delete_row_ns(Some(&ns), Some(&coll), id)?;
            } else {
                self.delete_doc_ns(Some(&ns), Some(&coll), id)?;
            }
        }
        Ok(SqlResult::Delete {
            affected: matches.len(),
        })
    }

    /// Fetch existing index for namespace or create one with default params.
    fn vector_index(&self, ns: &str) -> Arc<VectorIndex> {
        let mut guard = self.vectors.write();
        guard
            .entry(ns.to_string())
            .or_insert_with(|| {
                Arc::new(VectorIndex::with_params(
                    self.default_params.metric,
                    self.default_params.ef_construction,
                    self.default_params.ef_search,
                    self.default_params.max_elements,
                ))
            })
            .clone()
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_params(path, VectorParams::default())
    }

    pub fn open_with_params(path: impl AsRef<Path>, params: VectorParams) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let wal = Wal::open(&path)?;
        let data = Arc::new(RwLock::new(Collections::default()));
        let stats = Arc::new(RwLock::new(Stats::default()));
        let mut vecs = HashMap::new();
        vecs.insert(
            Self::default_ns(),
            Arc::new(VectorIndex::with_params(
                params.metric,
                params.ef_construction,
                params.ef_search,
                params.max_elements,
            )),
        );
        let vectors = Arc::new(RwLock::new(vecs));
        let vector_ns = Arc::new(RwLock::new(HashMap::new()));
        let graph = GraphStore::new();

        for rec in wal.replay()? {
            match rec {
                RecordKind::Put {
                    family,
                    key,
                    payload,
                    namespace,
                    collection,
                    table,
                } => match family {
                    DataFamily::Doc => {
                        let ns = namespace.unwrap_or_else(Self::default_ns);
                        let col = collection.unwrap_or_else(Self::default_ns);
                        let v: Value = serde_json::from_slice(&payload)?;
                        let mut guard = data.write();
                        guard
                            .docs
                            .entry(ns.clone())
                            .or_default()
                            .entry(col.clone())
                            .or_default()
                            .insert(key, v.clone());
                        Self::index_upsert_doc(&mut guard, ns, col, key, &v);
                    }
                    DataFamily::Row => {
                        let ns = namespace.unwrap_or_else(Self::default_ns);
                        let table = table.unwrap_or_else(Self::default_ns);
                        let v: Value = serde_json::from_slice(&payload)?;
                        let mut guard = data.write();
                        guard
                            .rows
                            .entry(ns.clone())
                            .or_default()
                            .entry(table.clone())
                            .or_default()
                            .insert(key, v.clone());
                        Self::index_upsert_row(&mut guard, ns, table, key, &v);
                    }
                    DataFamily::Vec => match bincode::deserialize::<VecWalRecord>(&payload) {
                        Ok(rec) => {
                            let ns = rec.namespace.unwrap_or_else(Self::default_ns);
                            let mut guard = vectors.write();
                            let entry = guard.entry(ns.clone()).or_insert_with(|| {
                                Arc::new(VectorIndex::with_params(
                                    params.metric,
                                    params.ef_construction,
                                    params.ef_search,
                                    params.max_elements,
                                ))
                            });
                            let _ = entry.insert(key, rec.vector, rec.meta);
                            vector_ns.write().insert(key, ns);
                        }
                        Err(_) => {
                            let vec: Vec<f32> = bincode::deserialize(&payload)?;
                            let guard = vectors.write();
                            if let Some(idx) = guard.get(Self::default_ns().as_str()) {
                                let _ = idx.insert(key, vec, None);
                                vector_ns.write().insert(key, Self::default_ns());
                            }
                        }
                    },
                    DataFamily::Graph => {
                        let edge: crate::graph::Edge = bincode::deserialize(&payload)?;
                        graph.add_edge(edge.src, edge.dst, edge.weight);
                    }
                },
                RecordKind::Delete {
                    family,
                    key,
                    namespace,
                    collection,
                    table,
                } => match family {
                    DataFamily::Doc => {
                        let ns = namespace.unwrap_or_else(Self::default_ns);
                        let col = collection.unwrap_or_else(Self::default_ns);
                        let mut guard = data.write();
                        if let Some(map) = guard.docs.get_mut(&ns) {
                            if let Some(c) = map.get_mut(&col) {
                                if let Some(old) = c.remove(&key) {
                                    Self::index_remove_doc(&mut guard, ns, col, &key, &old);
                                }
                            }
                        }
                    }
                    DataFamily::Row => {
                        let ns = namespace.unwrap_or_else(Self::default_ns);
                        let tbl = table.unwrap_or_else(Self::default_ns);
                        let mut guard = data.write();
                        if let Some(map) = guard.rows.get_mut(&ns) {
                            if let Some(t) = map.get_mut(&tbl) {
                                if let Some(old) = t.remove(&key) {
                                    Self::index_remove_row(&mut guard, ns, tbl, &key, &old);
                                }
                            }
                        }
                    }
                    DataFamily::Vec => {
                        let ns = namespace.unwrap_or_else(Self::default_ns);
                        if let Some(idx) = vectors.write().get(&ns) {
                            idx.delete(&key);
                            vector_ns.write().remove(&key);
                        }
                    }
                    DataFamily::Graph => {}
                },
                RecordKind::Schema {
                    family,
                    namespace,
                    collection,
                    table,
                    schema,
                } => {
                    let def: SchemaDef = serde_json::from_slice(&schema)?;
                    let mut guard = data.write();
                    match family {
                        DataFamily::Doc => {
                            let ns = namespace.unwrap_or_else(Self::default_ns);
                            let col = collection.unwrap_or_else(Self::default_ns);
                            guard.doc_schema.entry(ns).or_default().insert(col, def);
                        }
                        DataFamily::Row => {
                            let ns = namespace.unwrap_or_else(Self::default_ns);
                            let tbl = table.unwrap_or_else(Self::default_ns);
                            guard.row_schema.entry(ns).or_default().insert(tbl, def);
                        }
                        _ => {}
                    }
                }
                RecordKind::AddEdge { src, dst, weight } => {
                    graph.add_edge(src, dst, weight);
                }
            }
        }

        // Optional fast reload of vectors from per-namespace snapshots.
        let snap_dir = path.join("vectors");
        if snap_dir.exists() && snap_dir.is_dir() {
            for entry in std::fs::read_dir(&snap_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("snapshot") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        let ns = stem.to_string();
                        let idx = {
                            let mut guard = vectors.write();
                            guard
                                .entry(ns.clone())
                                .or_insert_with(|| {
                                    Arc::new(VectorIndex::with_params(
                                        params.metric,
                                        params.ef_construction,
                                        params.ef_search,
                                        params.max_elements,
                                    ))
                                })
                                .clone()
                        };
                        let _ = idx.load_snapshot(&path);
                        let hnsw = snap_dir.join(format!("{ns}.hnsw"));
                        let _ = idx.load_hnsw(&hnsw);
                        let _ = idx.rebuild_hnsw();
                        for id in idx.inner.read().keys() {
                            vector_ns.write().insert(*id, ns.clone());
                        }
                    }
                }
            }
        } else {
            // backwards compatibility: single-snapshot file
            let snapshot = path.join("vectors.snapshot");
            if snapshot.exists() {
                if let Some(idx) = vectors.write().get(&Self::default_ns()).cloned() {
                    let _ = idx.load_snapshot(&snapshot);
                    let _ = idx.rebuild_hnsw();
                    for id in idx.inner.read().keys() {
                        vector_ns.write().insert(*id, Self::default_ns());
                    }
                }
            }
        }

        Ok(Self {
            path,
            wal: RwLock::new(wal),
            data,
            stats,
            vectors,
            vector_ns,
            graph,
            link_top_k: params.link_top_k,
            shard_id: params.shard_id,
            shard_total: params.shard_total.max(1),
            default_params: params,
        })
    }

    pub fn put_doc_ns(
        &self,
        ns: Option<&str>,
        collection: Option<&str>,
        id: Uuid,
        json: Value,
    ) -> Result<()> {
        if !self.owns(&id) {
            return Err(PieskieoError::WrongShard);
        }
        self.enforce_doc_schema(ns, collection, &id, &json)?;
        let payload = serde_json::to_vec(&json)?;
        self.append_record(&RecordKind::Put {
            family: DataFamily::Doc,
            key: id,
            payload,
            namespace: Some(Self::ns(ns)),
            collection: Some(Self::col(collection)),
            table: None,
        })?;
        {
            let mut guard = self.data.write();
            let ns_key = Self::ns(ns);
            let col_key = Self::col(collection);
            guard
                .docs
                .entry(ns_key.clone())
                .or_default()
                .entry(col_key.clone())
                .or_default()
                .insert(id, json.clone());
            Self::index_upsert_doc(&mut guard, ns_key.clone(), col_key.clone(), id, &json);
            self.bump_doc_stats(&ns_key, &col_key, 1);
        }
        Ok(())
    }

    pub fn put_doc(&self, id: Uuid, json: Value) -> Result<()> {
        self.put_doc_ns(None, None, id, json)
    }

    pub fn delete_doc_ns(
        &self,
        ns: Option<&str>,
        collection: Option<&str>,
        id: &Uuid,
    ) -> Result<()> {
        if !self.owns(id) {
            return Err(PieskieoError::WrongShard);
        }
        self.append_record(&RecordKind::Delete {
            family: DataFamily::Doc,
            key: *id,
            namespace: Some(Self::ns(ns)),
            collection: Some(Self::col(collection)),
            table: None,
        })?;
        {
            let mut guard = self.data.write();
            let ns_key = Self::ns(ns);
            let col_key = Self::col(collection);
            if let Some(ns_map) = guard.docs.get_mut(&ns_key) {
                if let Some(col_map) = ns_map.get_mut(&col_key) {
                    if let Some(old) = col_map.remove(id) {
                        Self::index_remove_doc(
                            &mut guard,
                            ns_key.clone(),
                            col_key.clone(),
                            id,
                            &old,
                        );
                        self.bump_doc_stats(&ns_key, &col_key, -1);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn delete_doc(&self, id: &Uuid) -> Result<()> {
        self.delete_doc_ns(None, None, id)
    }

    pub fn update_doc(&self, id: Uuid, json: Value) -> Result<()> {
        self.put_doc(id, json)
    }

    pub fn put_row_ns<T: Serialize>(
        &self,
        ns: Option<&str>,
        table: Option<&str>,
        id: Uuid,
        row: &T,
    ) -> Result<()> {
        if !self.owns(&id) {
            return Err(PieskieoError::WrongShard);
        }
        let json = serde_json::to_value(row)?;
        self.enforce_row_schema(ns, table, &id, &json)?;
        let payload = serde_json::to_vec(&json)?;
        self.append_record(&RecordKind::Put {
            family: DataFamily::Row,
            key: id,
            payload,
            namespace: Some(Self::ns(ns)),
            table: Some(Self::col(table)),
            collection: None,
        })?;
        {
            let mut guard = self.data.write();
            let ns_key = Self::ns(ns);
            let tbl_key = Self::col(table);
            guard
                .rows
                .entry(ns_key.clone())
                .or_default()
                .entry(tbl_key.clone())
                .or_default()
                .insert(id, json.clone());
            Self::index_upsert_row(&mut guard, ns_key.clone(), tbl_key.clone(), id, &json);
            self.bump_row_stats(&ns_key, &tbl_key, 1);
        }
        Ok(())
    }

    pub fn set_doc_schema(
        &self,
        ns: Option<&str>,
        collection: Option<&str>,
        schema: SchemaDef,
    ) -> Result<()> {
        let ns_key = Self::ns(ns);
        let col_key = Self::col(collection);
        let payload = serde_json::to_vec(&schema)?;
        self.append_record(&RecordKind::Schema {
            family: DataFamily::Doc,
            namespace: Some(ns_key.clone()),
            collection: Some(col_key.clone()),
            table: None,
            schema: payload,
        })?;
        let mut guard = self.data.write();
        guard
            .doc_schema
            .entry(ns_key)
            .or_default()
            .insert(col_key, schema);
        Ok(())
    }

    pub fn set_row_schema(
        &self,
        ns: Option<&str>,
        table: Option<&str>,
        schema: SchemaDef,
    ) -> Result<()> {
        let ns_key = Self::ns(ns);
        let tbl_key = Self::col(table);
        let payload = serde_json::to_vec(&schema)?;
        self.append_record(&RecordKind::Schema {
            family: DataFamily::Row,
            namespace: Some(ns_key.clone()),
            collection: None,
            table: Some(tbl_key.clone()),
            schema: payload,
        })?;
        let mut guard = self.data.write();
        guard
            .row_schema
            .entry(ns_key)
            .or_default()
            .insert(tbl_key, schema);
        Ok(())
    }

    pub fn put_row<T: Serialize>(&self, id: Uuid, row: &T) -> Result<()> {
        self.put_row_ns(None, None, id, row)
    }

    pub fn delete_row_ns(&self, ns: Option<&str>, table: Option<&str>, id: &Uuid) -> Result<()> {
        if !self.owns(id) {
            return Err(PieskieoError::WrongShard);
        }
        self.append_record(&RecordKind::Delete {
            family: DataFamily::Row,
            key: *id,
            namespace: Some(Self::ns(ns)),
            table: Some(Self::col(table)),
            collection: None,
        })?;
        {
            let mut guard = self.data.write();
            let ns_key = Self::ns(ns);
            let tbl_key = Self::col(table);
            if let Some(ns_map) = guard.rows.get_mut(&ns_key) {
                if let Some(tbl_map) = ns_map.get_mut(&tbl_key) {
                    if let Some(old) = tbl_map.remove(id) {
                        Self::index_remove_row(
                            &mut guard,
                            ns_key.clone(),
                            tbl_key.clone(),
                            id,
                            &old,
                        );
                        self.bump_row_stats(&ns_key, &tbl_key, -1);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn delete_row(&self, id: &Uuid) -> Result<()> {
        self.delete_row_ns(None, None, id)
    }

    pub fn update_row<T: Serialize>(&self, id: Uuid, row: &T) -> Result<()> {
        self.put_row(id, row)
    }

    pub fn get_doc_ns(
        &self,
        ns: Option<&str>,
        collection: Option<&str>,
        id: &Uuid,
    ) -> Option<Value> {
        if !self.owns(id) {
            return None;
        }
        self.data
            .read()
            .docs
            .get(&Self::ns(ns))
            .and_then(|m| m.get(&Self::col(collection)))
            .and_then(|m| m.get(id).cloned())
    }

    pub fn get_doc(&self, id: &Uuid) -> Option<Value> {
        self.get_doc_ns(None, None, id)
    }

    pub fn get_row_ns(&self, ns: Option<&str>, table: Option<&str>, id: &Uuid) -> Option<Value> {
        if !self.owns(id) {
            return None;
        }
        self.data
            .read()
            .rows
            .get(&Self::ns(ns))
            .and_then(|m| m.get(&Self::col(table)))
            .and_then(|m| m.get(id).cloned())
    }

    pub fn get_row(&self, id: &Uuid) -> Option<Value> {
        self.get_row_ns(None, None, id)
    }

    pub fn query_docs(
        &self,
        filter: &HashMap<String, Value>,
        limit: usize,
        offset: usize,
    ) -> Vec<(Uuid, Value)> {
        self.query_docs_ns(None, None, filter, limit, offset)
    }

    pub fn query_docs_ns(
        &self,
        ns: Option<&str>,
        collection: Option<&str>,
        filter: &HashMap<String, Value>,
        limit: usize,
        offset: usize,
    ) -> Vec<(Uuid, Value)> {
        let guard = self.data.read();
        self.filter_map_with_index(
            &guard.docs,
            &guard.doc_index,
            ns,
            collection,
            filter,
            limit,
            offset,
        )
    }

    pub fn query_rows(
        &self,
        filter: &HashMap<String, Value>,
        limit: usize,
        offset: usize,
    ) -> Vec<(Uuid, Value)> {
        self.query_rows_ns(None, None, filter, limit, offset)
    }

    pub fn query_rows_ns(
        &self,
        ns: Option<&str>,
        table: Option<&str>,
        filter: &HashMap<String, Value>,
        limit: usize,
        offset: usize,
    ) -> Vec<(Uuid, Value)> {
        let guard = self.data.read();
        self.filter_map_with_index(
            &guard.rows,
            &guard.row_index,
            ns,
            table,
            filter,
            limit,
            offset,
        )
    }

    /// SQL-ish over docs/rows. Supports SELECT/INSERT/UPDATE/DELETE (single statement).
    pub fn query_sql(&self, sql: &str) -> Result<SqlResult> {
        let dialect = GenericDialect {};
        let ast = Parser::parse_sql(&dialect, sql)
            .map_err(|e| PieskieoError::Internal(format!("sql parse error: {e}").into()))?;
        if ast.len() != 1 {
            return Err(PieskieoError::Internal("one statement expected".into()));
        }
        let stmt = &ast[0];
        match stmt {
            Statement::Query(_) => self.exec_select(stmt),
            Statement::Insert { .. } => self.exec_insert(stmt),
            Statement::Update { .. } => self.exec_update(stmt),
            Statement::Delete { .. } => self.exec_delete(stmt),
            _ => Err(PieskieoError::Internal("statement not supported".into())),
        }
    }

    pub fn put_vector(&self, id: Uuid, vector: Vec<f32>) -> Result<()> {
        self.put_vector_with_meta_ns(None, id, vector, None)
    }

    pub fn put_vector_ns(&self, ns: Option<&str>, id: Uuid, vector: Vec<f32>) -> Result<()> {
        self.put_vector_with_meta_ns(ns, id, vector, None)
    }

    pub fn put_vector_with_meta(
        &self,
        id: Uuid,
        vector: Vec<f32>,
        meta: Option<HashMap<String, String>>,
    ) -> Result<()> {
        self.put_vector_with_meta_ns(None, id, vector, meta)
    }

    pub fn put_vector_with_meta_ns(
        &self,
        ns: Option<&str>,
        id: Uuid,
        vector: Vec<f32>,
        meta: Option<HashMap<String, String>>,
    ) -> Result<()> {
        if !self.owns(&id) {
            return Err(PieskieoError::WrongShard);
        }
        let namespace = Self::ns(ns);
        let payload = bincode::serialize(&VecWalRecord {
            namespace: Some(namespace.clone()),
            vector: vector.clone(),
            meta: meta.clone(),
        })?;
        self.append_record(&RecordKind::Put {
            family: DataFamily::Vec,
            key: id,
            payload,
            namespace: Some(namespace.clone()),
            collection: None,
            table: None,
        })?;
        let idx = self.vector_index(&namespace);
        idx.insert(id, vector, meta)?;
        self.vector_ns.write().insert(id, namespace.clone());
        self.auto_link_neighbors(id, &namespace);
        Ok(())
    }

    /// Merge or set metadata for an existing vector without changing the embedding.
    pub fn update_vector_meta(&self, id: Uuid, meta_patch: HashMap<String, String>) -> Result<()> {
        let ns = {
            let map = self.vector_ns.read();
            map.get(&id).cloned().unwrap_or_else(Self::default_ns)
        };
        let idx = self.vector_index(&ns);
        let (vector, new_meta) = {
            let data = idx.inner.read();
            let meta = idx.meta.read();
            let Some(vec) = data.get(&id).cloned() else {
                return Err(PieskieoError::NotFound);
            };
            let merged = if let Some(existing) = meta.get(&id) {
                let mut m = existing.clone();
                for (k, v) in meta_patch {
                    m.insert(k, v);
                }
                m
            } else {
                meta_patch
            };
            (vec, merged)
        };
        let payload = bincode::serialize(&VecWalRecord {
            namespace: Some(ns.clone()),
            vector: vector.clone(),
            meta: Some(new_meta.clone()),
        })?;
        self.append_record(&RecordKind::Put {
            family: DataFamily::Vec,
            key: id,
            payload,
            namespace: Some(ns.clone()),
            collection: None,
            table: None,
        })?;
        idx.insert(id, vector, Some(new_meta))?;
        Ok(())
    }

    pub fn update_vector(&self, id: Uuid, vector: Vec<f32>) -> Result<()> {
        self.put_vector(id, vector)
    }

    pub fn delete_vector(&self, id: &Uuid) -> Result<()> {
        if !self.owns(id) {
            return Err(PieskieoError::WrongShard);
        }
        let ns = {
            let map = self.vector_ns.read();
            map.get(id).cloned().unwrap_or_else(Self::default_ns)
        };
        self.append_record(&RecordKind::Delete {
            family: DataFamily::Vec,
            key: *id,
            namespace: Some(ns.clone()),
            collection: None,
            table: None,
        })?;
        if let Some(idx) = self.vectors.read().get(&ns) {
            idx.delete(id);
        }
        self.vector_ns.write().remove(id);
        Ok(())
    }

    fn auto_link_neighbors(&self, id: Uuid, ns: &str) {
        if self.link_top_k == 0 {
            return;
        }
        let vector = self
            .vectors
            .read()
            .get(ns)
            .and_then(|idx| idx.inner.read().get(&id).cloned());
        let Some(vector) = vector else {
            return;
        };
        let mut hits = match self.search_vector_metric_ns(
            Some(ns),
            &vector,
            self.link_top_k + 1,
            self.default_params.metric,
            None,
        ) {
            Ok(h) => h,
            Err(_) => return,
        };
        hits.retain(|h| h.id != id);
        for h in hits.into_iter().take(self.link_top_k) {
            let weight = 1.0 / (1.0 + h.score.abs());
            let _ = self.add_edge(id, h.id, weight);
            let _ = self.add_edge(h.id, id, weight);
        }
    }

    pub fn search_vector(
        &self,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<crate::vector::VectorSearchResult>> {
        self.search_vector_metric(query, k, self.default_params.metric, None)
    }

    pub fn search_vector_metric(
        &self,
        query: &[f32],
        k: usize,
        metric: crate::vector::VectorMetric,
        filter_meta: Option<HashMap<String, String>>,
    ) -> Result<Vec<crate::vector::VectorSearchResult>> {
        // search across all namespaces and merge top-k
        let mut all = Vec::new();
        for (_ns, idx) in self.vectors.read().iter() {
            let local = crate::vector::VectorIndex::from_shared(
                idx.inner.clone(),
                idx.dim.clone(),
                metric,
                idx.hnsw.clone(),
                idx.owned_store.clone(),
                idx.id_map.clone(),
                idx.rev_map.clone(),
                idx.next_id.clone(),
                idx.tombstones.clone(),
                std::sync::atomic::AtomicUsize::new(
                    idx.ef_construction
                        .load(std::sync::atomic::Ordering::SeqCst),
                ),
                std::sync::atomic::AtomicUsize::new(
                    idx.ef_search.load(std::sync::atomic::Ordering::SeqCst),
                ),
                idx.max_elements,
                idx.meta.clone(),
            );
            let hits = local.search_ann_filtered(query, k, filter_meta.clone())?;
            for h in hits {
                all.push(h);
            }
        }
        all.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
        all.truncate(k);
        Ok(all)
    }

    pub fn search_vector_metric_ns(
        &self,
        ns: Option<&str>,
        query: &[f32],
        k: usize,
        metric: crate::vector::VectorMetric,
        filter_meta: Option<HashMap<String, String>>,
    ) -> Result<Vec<crate::vector::VectorSearchResult>> {
        let namespace = Self::ns(ns);
        let idx = self.vector_index(&namespace);
        let local = crate::vector::VectorIndex::from_shared(
            idx.inner.clone(),
            idx.dim.clone(),
            metric,
            idx.hnsw.clone(),
            idx.owned_store.clone(),
            idx.id_map.clone(),
            idx.rev_map.clone(),
            idx.next_id.clone(),
            idx.tombstones.clone(),
            std::sync::atomic::AtomicUsize::new(
                idx.ef_construction
                    .load(std::sync::atomic::Ordering::SeqCst),
            ),
            std::sync::atomic::AtomicUsize::new(
                idx.ef_search.load(std::sync::atomic::Ordering::SeqCst),
            ),
            idx.max_elements,
            idx.meta.clone(),
        );
        local.search_ann_filtered(query, k, filter_meta)
    }

    pub fn add_edge(&self, src: Uuid, dst: Uuid, weight: f32) -> Result<()> {
        if !self.owns(&src) {
            return Err(PieskieoError::WrongShard);
        }
        let payload = bincode::serialize(&crate::graph::Edge { src, dst, weight })?;
        self.append_record(&RecordKind::Put {
            family: DataFamily::Graph,
            key: src,
            payload,
            namespace: None,
            collection: None,
            table: None,
        })?;
        self.graph.add_edge(src, dst, weight);
        Ok(())
    }

    pub fn neighbors(&self, id: Uuid, limit: usize) -> Vec<crate::graph::Edge> {
        self.graph.neighbors(id, limit)
    }

    pub fn bfs(&self, start: Uuid, limit: usize) -> Vec<crate::graph::Edge> {
        self.graph.bfs(start, limit)
    }

    pub fn dfs(&self, start: Uuid, limit: usize) -> Vec<crate::graph::Edge> {
        self.graph.dfs(start, limit)
    }

    pub fn rebuild_vectors(&self) -> Result<()> {
        for idx in self.vectors.read().values() {
            idx.rebuild_hnsw()?;
        }
        Ok(())
    }

    pub fn get_vector(&self, id: &Uuid) -> Option<(Vec<f32>, Option<HashMap<String, String>>)> {
        let ns = self
            .vector_ns
            .read()
            .get(id)
            .cloned()
            .unwrap_or_else(Self::default_ns);
        let idx = self.vector_index(&ns);
        let vec = idx.inner.read().get(id).cloned()?;
        let meta = idx.meta.read().get(id).cloned();
        Some((vec, meta))
    }

    pub fn save_vector_snapshot(&self) -> Result<()> {
        let snap_dir = self.path.join("vectors");
        std::fs::create_dir_all(&snap_dir)?;
        for (ns, idx) in self.vectors.read().iter() {
            let snap = snap_dir.join(format!("{ns}.snapshot"));
            idx.save_snapshot(&snap)?;
            let hnsw = snap_dir.join(format!("{ns}.hnsw"));
            idx.save_hnsw(&hnsw)?;
        }
        Ok(())
    }

    pub fn set_ef_search(&self, ef: usize) {
        for idx in self.vectors.read().values() {
            idx.set_ef_search(ef);
        }
    }

    pub fn set_ef_construction(&self, ef: usize) {
        for idx in self.vectors.read().values() {
            idx.set_ef_construction(ef);
        }
    }

    pub fn set_link_top_k(&mut self, k: usize) {
        self.link_top_k = k;
    }

    pub fn remove_vector_meta_keys(&self, id: Uuid, keys: &[String]) -> Result<()> {
        let ns = {
            let map = self.vector_ns.read();
            map.get(&id).cloned().unwrap_or_else(Self::default_ns)
        };
        let idx = self.vector_index(&ns);
        let (vector, meta) = {
            let data = idx.inner.read();
            let meta = idx.meta.read();
            let Some(vec) = data.get(&id).cloned() else {
                return Err(PieskieoError::NotFound);
            };
            let mut m = meta.get(&id).cloned().unwrap_or_default();
            for k in keys {
                m.remove(k);
            }
            (vec, m)
        };
        let payload = bincode::serialize(&VecWalRecord {
            namespace: Some(ns.clone()),
            vector: vector.clone(),
            meta: Some(meta.clone()),
        })?;
        self.append_record(&RecordKind::Put {
            family: DataFamily::Vec,
            key: id,
            payload,
            namespace: Some(ns.clone()),
            collection: None,
            table: None,
        })?;
        idx.insert(id, vector, Some(meta))?;
        Ok(())
    }

    /// Compact tombstones and WAL by rewriting snapshot and truncating WAL.
    pub fn vacuum(&self) -> Result<()> {
        // drop deleted vectors from in-memory store for each namespace
        for idx in self.vectors.read().values() {
            let tomb = idx.tombstones.read().clone();
            if !tomb.is_empty() {
                let mut inner = idx.inner.write();
                for id in tomb.keys() {
                    inner.remove(id);
                }
            }
            idx.tombstones.write().clear();
        }

        // rebuild ANN for clean state
        let _ = self.rebuild_vectors();
        // persist fresh snapshots + hnsw and truncate WAL
        self.save_vector_snapshot()?;
        self.wal.write().truncate()?;
        Ok(())
    }

    pub fn flush_wal(&self) -> Result<()> {
        self.wal.write().flush_sync()
    }

    pub fn metrics(&self) -> MetricsSnapshot {
        let mut vectors = 0usize;
        let mut tomb = 0usize;
        let mut hnsw_ready = true;
        let mut ef_search = 0usize;
        let mut ef_construction = 0usize;
        for idx in self.vectors.read().values() {
            vectors += idx.inner.read().len();
            tomb += idx.tombstones.read().len();
            hnsw_ready &= idx.hnsw.read().is_some();
            ef_search = idx.ef_search.load(std::sync::atomic::Ordering::SeqCst);
            ef_construction = idx
                .ef_construction
                .load(std::sync::atomic::Ordering::SeqCst);
        }
        MetricsSnapshot {
            docs: self.data.read().docs.values().map(|m| m.len()).sum(),
            rows: self.data.read().rows.values().map(|m| m.len()).sum(),
            vectors,
            vector_tombstones: tomb,
            hnsw_ready,
            ef_search,
            ef_construction,
            wal_path: self.path.join("wal.log"),
            wal_bytes: std::fs::metadata(self.path.join("wal.log"))
                .map(|m| m.len())
                .unwrap_or(0),
            snapshot_mtime: std::fs::metadata(self.path.join("vectors"))
                .and_then(|m| m.modified())
                .ok(),
            link_top_k: self.link_top_k,
            shard_id: self.shard_id,
            shard_total: self.shard_total,
        }
    }

    fn append_record(&self, record: &RecordKind) -> Result<()> {
        self.wal.write().append(record)
    }

    pub fn wal_replay_since(&self, offset: u64) -> Result<(Vec<RecordKind>, u64)> {
        self.wal.read().replay_since(offset)
    }

    pub fn wal_dump(&self) -> Result<Vec<RecordKind>> {
        self.wal.read().replay()
    }

    pub fn apply_records(&self, records: &[RecordKind]) -> Result<()> {
        for rec in records {
            self.append_record(rec)?;
            self.apply_record(rec)?;
        }
        Ok(())
    }

    fn apply_record(&self, rec: &RecordKind) -> Result<()> {
        match rec {
            RecordKind::Put {
                family,
                key,
                payload,
                namespace,
                collection,
                table,
            } => match family {
                DataFamily::Doc => {
                    let ns = namespace.clone().unwrap_or_else(Self::default_ns);
                    let col = collection.clone().unwrap_or_else(Self::default_ns);
                    let v: Value = serde_json::from_slice(payload)?;
                    let mut guard = self.data.write();
                    guard
                        .docs
                        .entry(ns.clone())
                        .or_default()
                        .entry(col.clone())
                        .or_default()
                        .insert(*key, v.clone());
                    Self::index_upsert_doc(&mut guard, ns, col, *key, &v);
                }
                DataFamily::Row => {
                    let ns = namespace.clone().unwrap_or_else(Self::default_ns);
                    let tbl = table.clone().unwrap_or_else(Self::default_ns);
                    let v: Value = serde_json::from_slice(payload)?;
                    let mut guard = self.data.write();
                    guard
                        .rows
                        .entry(ns.clone())
                        .or_default()
                        .entry(tbl.clone())
                        .or_default()
                        .insert(*key, v.clone());
                    Self::index_upsert_row(&mut guard, ns, tbl, *key, &v);
                }
                DataFamily::Vec => match bincode::deserialize::<VecWalRecord>(payload) {
                    Ok(rec) => {
                        let ns = rec.namespace.unwrap_or_else(Self::default_ns);
                        let idx = self.vector_index(&ns);
                        idx.insert(*key, rec.vector, rec.meta)?;
                        self.vector_ns.write().insert(*key, ns);
                    }
                    Err(_) => {
                        let idx = self.vector_index(&Self::default_ns());
                        idx.insert(*key, Vec::new(), None)?;
                        self.vector_ns.write().insert(*key, Self::default_ns());
                    }
                },
                DataFamily::Graph => {
                    if let Ok(edge) = bincode::deserialize::<crate::graph::Edge>(payload) {
                        let _ = self.graph.add_edge(edge.src, edge.dst, edge.weight);
                    }
                }
            },
            RecordKind::Delete {
                family,
                key,
                namespace,
                collection,
                table,
            } => match family {
                DataFamily::Doc => {
                    let ns = namespace.clone().unwrap_or_else(Self::default_ns);
                    let col = collection.clone().unwrap_or_else(Self::default_ns);
                    let mut guard = self.data.write();
                    if let Some(map) = guard.docs.get_mut(&ns).and_then(|m| m.get_mut(&col)) {
                        map.remove(key);
                    }
                    if let Some(idx) = guard.doc_index.get_mut(&ns).and_then(|m| m.get_mut(&col)) {
                        for (_field, valmap) in idx.iter_mut() {
                            for (_v, ids) in valmap.iter_mut() {
                                ids.retain(|id| id != key);
                            }
                        }
                    }
                }
                DataFamily::Row => {
                    let ns = namespace.clone().unwrap_or_else(Self::default_ns);
                    let tbl = table.clone().unwrap_or_else(Self::default_ns);
                    let mut guard = self.data.write();
                    if let Some(map) = guard.rows.get_mut(&ns).and_then(|m| m.get_mut(&tbl)) {
                        map.remove(key);
                    }
                    if let Some(idx) = guard.row_index.get_mut(&ns).and_then(|m| m.get_mut(&tbl)) {
                        for (_field, valmap) in idx.iter_mut() {
                            for (_v, ids) in valmap.iter_mut() {
                                ids.retain(|id| id != key);
                            }
                        }
                    }
                }
                DataFamily::Vec => {
                    if let Some(ns) = self.vector_ns.read().get(key).cloned() {
                        let idx = self.vector_index(&ns);
                        idx.delete(key);
                        self.vector_ns.write().remove(key);
                    }
                }
                DataFamily::Graph => {}
            },
            RecordKind::Schema {
                family,
                namespace,
                collection,
                table,
                schema,
            } => {
                let ns = namespace.clone().unwrap_or_else(Self::default_ns);
                let def: SchemaDef = serde_json::from_slice(schema)?;
                match family {
                    DataFamily::Doc => {
                        let name = collection.clone().unwrap_or_else(Self::default_ns);
                        self.data
                            .write()
                            .doc_schema
                            .entry(ns)
                            .or_default()
                            .insert(name, def);
                    }
                    DataFamily::Row => {
                        let name = table.clone().unwrap_or_else(Self::default_ns);
                        self.data
                            .write()
                            .row_schema
                            .entry(ns)
                            .or_default()
                            .insert(name, def);
                    }
                    _ => {}
                }
            }
            RecordKind::AddEdge { src, dst, weight } => {
                self.graph.add_edge(*src, *dst, *weight);
            }
        }
        Ok(())
    }

    fn owns(&self, id: &Uuid) -> bool {
        if self.shard_total <= 1 {
            return true;
        }
        (shard_hash(id) % self.shard_total) == self.shard_id
    }
}

fn value_matches(doc: &Value, filter: &HashMap<String, Value>) -> bool {
    for (k, v) in filter {
        if let Some(field) = doc.get(k) {
            if !value_compare(field, v) {
                return false;
            }
        } else {
            return false;
        }
    }
    true
}

fn value_compare(field: &Value, cond: &Value) -> bool {
    if !cond.is_object() {
        return field == cond;
    }
    let obj = cond.as_object().unwrap();
    for (op, val) in obj {
        match op.as_str() {
            "$gt" => {
                if !cmp_values(field, val).map(|o| o.is_gt()).unwrap_or(false) {
                    return false;
                }
            }
            "$gte" => {
                if !cmp_values(field, val).map(|o| o.is_ge()).unwrap_or(false) {
                    return false;
                }
            }
            "$lt" => {
                if !cmp_values(field, val).map(|o| o.is_lt()).unwrap_or(false) {
                    return false;
                }
            }
            "$lte" => {
                if !cmp_values(field, val).map(|o| o.is_le()).unwrap_or(false) {
                    return false;
                }
            }
            "$ne" => {
                if field == val {
                    return false;
                }
            }
            "$in" => {
                if let Some(arr) = val.as_array() {
                    if !arr.iter().any(|x| x == field) {
                        return false;
                    }
                } else {
                    return false;
                }
            }
            "$nin" => {
                if let Some(arr) = val.as_array() {
                    if arr.iter().any(|x| x == field) {
                        return false;
                    }
                } else {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

fn cmp_values(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            let xf = x.as_f64()?;
            let yf = y.as_f64()?;
            xf.partial_cmp(&yf)
        }
        (Value::String(x), Value::String(y)) => Some(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

#[derive(Clone)]
struct Projection {
    source: String,
    alias: String,
}

#[derive(Clone)]
struct JoinSpec {
    right_ns: String,
    right_coll: String,
    right_is_rows: bool,
    on_left: String,
    on_right: String,
}

#[derive(Clone)]
enum AggKind {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

#[derive(Clone)]
struct AggExpr {
    alias: String,
    field: Option<String>, // None for count(*)
    kind: AggKind,
}

#[derive(Clone)]
struct Condition {
    field: String,
    op: Op,
    value: Value,
}

#[derive(Clone)]
enum Op {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
    In,
    Nin,
}

impl PieskieoDb {
    fn filter_conditions(
        &self,
        inner: &BTreeMap<Uuid, Value>,
        conds: &[Condition],
        limit: usize,
        offset: usize,
    ) -> Vec<(Uuid, Value)> {
        let mut out = Vec::new();
        let mut skipped = 0usize;
        'outer: for (id, v) in inner.iter() {
            if !self.owns(id) {
                continue;
            }
            for c in conds {
                let Some(field_val) = v.get(&c.field) else {
                    continue 'outer;
                };
                let pass = match c.op {
                    Op::Eq => field_val == &c.value,
                    Op::Ne => field_val != &c.value,
                    Op::Gt => cmp_values(field_val, &c.value)
                        .map(|o| o.is_gt())
                        .unwrap_or(false),
                    Op::Gte => cmp_values(field_val, &c.value)
                        .map(|o| o.is_ge())
                        .unwrap_or(false),
                    Op::Lt => cmp_values(field_val, &c.value)
                        .map(|o| o.is_lt())
                        .unwrap_or(false),
                    Op::Lte => cmp_values(field_val, &c.value)
                        .map(|o| o.is_le())
                        .unwrap_or(false),
                    Op::In => c
                        .value
                        .as_array()
                        .map(|arr| arr.iter().any(|x| x == field_val))
                        .unwrap_or(false),
                    Op::Nin => c
                        .value
                        .as_array()
                        .map(|arr| arr.iter().all(|x| x != field_val))
                        .unwrap_or(false),
                };
                if !pass {
                    continue 'outer;
                }
            }
            if skipped < offset {
                skipped += 1;
                continue;
            }
            out.push((*id, v.clone()));
            if out.len() >= limit {
                break;
            }
        }
        out
    }

    fn exec_select(&self, stmt: &Statement) -> Result<SqlResult> {
        let (ns, coll, conds, projections, limit, offset, order_by, join_spec, aggs, target_rows) =
            self.parse_select(stmt)?;
        let mut rows = self.collect_filtered_ns(&ns, &coll, target_rows, &conds);
        if let Some(join) = join_spec {
            let right =
                self.collect_filtered_ns(&join.right_ns, &join.right_coll, join.right_is_rows, &[]);
            let mut joined = Vec::new();
            for (lid, lv) in &rows {
                if let Some(lobj) = lv.as_object() {
                    let lv_on = lobj.get(&join.on_left);
                    for (_, rv) in &right {
                        if let Some(robj) = rv.as_object() {
                            if robj.get(&join.on_right) == lv_on {
                                let mut merged = serde_json::Map::new();
                                for (k, v) in lobj {
                                    merged.insert(k.clone(), v.clone());
                                }
                                for (k, v) in robj {
                                    let key = if merged.contains_key(k) {
                                        format!("right_{k}")
                                    } else {
                                        k.clone()
                                    };
                                    merged.insert(key, v.clone());
                                }
                                joined.push((*lid, Value::Object(merged)));
                            }
                        }
                    }
                }
            }
            rows = joined;
        }

        if !order_by.is_empty() {
            rows.sort_by(|a, b| {
                for (field, asc) in order_by.iter() {
                    let av = a.1.get(field);
                    let bv = b.1.get(field);
                    let ord = match (av, bv) {
                        (Some(x), Some(y)) => cmp_values(x, y).unwrap_or(std::cmp::Ordering::Equal),
                        (Some(_), None) => std::cmp::Ordering::Greater,
                        (None, Some(_)) => std::cmp::Ordering::Less,
                        _ => std::cmp::Ordering::Equal,
                    };
                    if ord != std::cmp::Ordering::Equal {
                        return if *asc { ord } else { ord.reverse() };
                    }
                }
                std::cmp::Ordering::Equal
            });
        }

        // aggregation (global only, no group by)
        if !aggs.is_empty() {
            let mut out_obj = serde_json::Map::new();
            for agg in aggs {
                let val = match agg.kind {
                    AggKind::Count => Value::Number((rows.len() as u64).into()),
                    AggKind::Sum => {
                        let nums = Self::collect_nums(&rows, agg.field.as_deref().unwrap_or(""));
                        let sum: f64 = nums.iter().sum();
                        Self::num_or_null(sum)
                    }
                    AggKind::Avg => {
                        let nums = Self::collect_nums(&rows, agg.field.as_deref().unwrap_or(""));
                        if nums.is_empty() {
                            Value::Null
                        } else {
                            let avg = nums.iter().sum::<f64>() / nums.len() as f64;
                            Self::num_or_null(avg)
                        }
                    }
                    AggKind::Min => {
                        let nums = Self::collect_nums(&rows, agg.field.as_deref().unwrap_or(""));
                        nums.into_iter()
                            .fold(None, |acc, v| Some(acc.map_or(v, |a: f64| a.min(v))))
                            .map(Self::num_or_null)
                            .unwrap_or(Value::Null)
                    }
                    AggKind::Max => {
                        let nums = Self::collect_nums(&rows, agg.field.as_deref().unwrap_or(""));
                        nums.into_iter()
                            .fold(None, |acc, v| Some(acc.map_or(v, |a: f64| a.max(v))))
                            .map(Self::num_or_null)
                            .unwrap_or(Value::Null)
                    }
                };
                out_obj.insert(agg.alias, val);
            }
            return Ok(SqlResult::Select(vec![(
                Uuid::nil(),
                Value::Object(out_obj),
            )]));
        }

        let start = offset.min(rows.len());
        let end = (start + limit).min(rows.len());
        let slice = &rows[start..end];

        let out = if let Some(projs) = projections {
            let mut projected = Vec::with_capacity(slice.len());
            for (id, v) in slice {
                let mut obj = serde_json::Map::new();
                for p in projs.iter() {
                    if p.source == "_id" {
                        obj.insert(p.alias.clone(), Value::String(id.to_string()));
                    } else if let Some(val) = v.get(&p.source) {
                        obj.insert(p.alias.clone(), val.clone());
                    }
                }
                projected.push((*id, Value::Object(obj)));
            }
            projected
        } else {
            slice.to_vec()
        };
        Ok(SqlResult::Select(out))
    }

    fn collect_filtered_ns(
        &self,
        ns: &str,
        coll: &str,
        target_rows: bool,
        conds: &[Condition],
    ) -> Vec<(Uuid, Value)> {
        let guard = self.data.read();
        if target_rows {
            guard
                .rows
                .get(ns)
                .and_then(|m| m.get(coll))
                .map(|map| self.filter_conditions(map, conds, usize::MAX, 0))
                .unwrap_or_default()
        } else {
            guard
                .docs
                .get(ns)
                .and_then(|m| m.get(coll))
                .map(|map| self.filter_conditions(map, conds, usize::MAX, 0))
                .unwrap_or_default()
        }
    }

    fn parse_select(
        &self,
        stmt: &Statement,
    ) -> Result<(
        String,
        String,
        Vec<Condition>,
        Option<Vec<Projection>>,
        usize,
        usize,
        Vec<(String, bool)>,
        Option<JoinSpec>,
        Vec<AggExpr>,
        bool,
    )> {
        let (select, query) = match stmt {
            Statement::Query(q) => match &*q.body {
                SetExpr::Select(s) => (s.as_ref(), q),
                _ => return Err(PieskieoError::Internal("only SELECT supported".into())),
            },
            _ => return Err(PieskieoError::Internal("only SELECT supported".into())),
        };
        // projections: None == wildcard
        let mut projections: Option<Vec<Projection>> = None;
        let mut aggs: Vec<AggExpr> = Vec::new();
        let mut saw_wildcard = false;
        for item in &select.projection {
            match item {
                SelectItem::Wildcard(_) => {
                    saw_wildcard = true;
                }
                SelectItem::UnnamedExpr(Expr::Identifier(id)) => {
                    projections.get_or_insert_with(Vec::new).push(Projection {
                        source: id.value.clone(),
                        alias: id.value.clone(),
                    });
                }
                SelectItem::ExprWithAlias {
                    expr: Expr::Identifier(id),
                    alias,
                } => {
                    projections.get_or_insert_with(Vec::new).push(Projection {
                        source: id.value.clone(),
                        alias: alias.value.clone(),
                    });
                }
                SelectItem::UnnamedExpr(Expr::Function(f)) => {
                    aggs.push(Self::parse_agg(f, None)?);
                }
                SelectItem::ExprWithAlias {
                    expr: Expr::Function(f),
                    alias,
                } => {
                    aggs.push(Self::parse_agg(f, Some(&alias.value))?);
                }
                _ => {
                    return Err(PieskieoError::Internal(
                        "projection item not supported".into(),
                    ))
                }
            }
        }
        if saw_wildcard && projections.is_some() {
            return Err(PieskieoError::Internal(
                "mixing * with explicit projections not supported".into(),
            ));
        }
        if saw_wildcard {
            projections = None;
        }
        let tbl = select
            .from
            .get(0)
            .and_then(|t| match &t.relation {
                TableFactor::Table { name, .. } => Some(name.clone()),
                _ => None,
            })
            .ok_or_else(|| PieskieoError::Internal("FROM required".into()))?;
        let (family, ns, coll) = self.split_name(&tbl)?;
        let mut conds = Vec::new();
        if let Some(selection) = &select.selection {
            self.walk_expr(selection, &mut conds)?;
        }
        let limit = query
            .limit
            .as_ref()
            .and_then(|l| match l {
                Expr::Value(sqlparser::ast::Value::Number(n, _)) => n.parse().ok(),
                _ => None,
            })
            .unwrap_or(100);
        let offset = query
            .offset
            .as_ref()
            .and_then(|o| match &o.value {
                Expr::Value(sqlparser::ast::Value::Number(n, _)) => n.parse().ok(),
                _ => None,
            })
            .unwrap_or(0);
        let mut order_by: Vec<(String, bool)> = Vec::new();
        for ob in &query.order_by {
            order_by.push(self.parse_order_by(ob)?);
        }
        let join_spec = self.parse_join(select)?;
        // family hint or prefix heuristic
        let target_rows = match family.as_deref() {
            Some("rows") | Some("tables") | Some("table") | Some("row") => true,
            Some("docs") | Some("collections") | Some("doc") => false,
            _ => {
                coll.starts_with("rows_") || coll.starts_with("table_") || coll.starts_with("tbl_")
            }
        };
        Ok((
            ns,
            coll,
            conds,
            projections,
            limit,
            offset,
            order_by,
            join_spec,
            aggs,
            target_rows,
        ))
    }

    fn parse_order_by(&self, ob: &OrderByExpr) -> Result<(String, bool)> {
        let field = match &ob.expr {
            Expr::Identifier(id) => id.value.clone(),
            _ => {
                return Err(PieskieoError::Internal(
                    "ORDER BY supports only identifiers".into(),
                ))
            }
        };
        let asc = ob.asc.unwrap_or(true);
        Ok((field, asc))
    }

    fn parse_join(&self, select: &Select) -> Result<Option<JoinSpec>> {
        if select.from.len() != 1 {
            return Err(PieskieoError::Internal(
                "only single FROM item supported".into(),
            ));
        }
        let joins = &select.from[0].joins;
        if joins.is_empty() {
            return Ok(None);
        }
        if joins.len() > 1 {
            return Err(PieskieoError::Internal("only one JOIN supported".into()));
        }
        let j = &joins[0];
        // only INNER JOIN
        let on = match &j.join_operator {
            JoinOperator::Inner(constraint) => constraint,
            _ => return Err(PieskieoError::Internal("only INNER JOIN supported".into())),
        };
        let right_name = self.extract_name_from_table_factor(&j.relation)?;
        let (rfam, rns, rcoll) = self.split_name(right_name)?;
        let right_is_rows = Self::target_is_rows(rfam.as_deref(), &rcoll);
        let (on_left, on_right) = match on {
            JoinConstraint::On(expr) => {
                if let Expr::BinaryOp {
                    left,
                    op: BinaryOperator::Eq,
                    right,
                } = expr
                {
                    let l = Self::ident_name(left)?;
                    let r = Self::ident_name(right)?;
                    (l, r)
                } else {
                    return Err(PieskieoError::Internal(
                        "JOIN ON must be equality of identifiers".into(),
                    ));
                }
            }
            _ => return Err(PieskieoError::Internal("JOIN requires ON clause".into())),
        };
        Ok(Some(JoinSpec {
            right_ns: rns,
            right_coll: rcoll,
            right_is_rows,
            on_left,
            on_right,
        }))
    }

    fn parse_agg(f: &Function, alias: Option<&str>) -> Result<AggExpr> {
        let name = f.name.to_string().to_lowercase();
        let kind = match name.as_str() {
            "count" => AggKind::Count,
            "sum" => AggKind::Sum,
            "avg" => AggKind::Avg,
            "min" => AggKind::Min,
            "max" => AggKind::Max,
            _ => {
                return Err(PieskieoError::Internal(
                    "aggregate function not supported".into(),
                ))
            }
        };
        let mut field: Option<String> = None;
        if let Some(arg) = f.args.first() {
            match arg {
                FunctionArg::Unnamed(FunctionArgExpr::Wildcard) => {
                    if !matches!(kind, AggKind::Count) {
                        return Err(PieskieoError::Internal(
                            "only count(*) supports wildcard".into(),
                        ));
                    }
                }
                FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Identifier(id))) => {
                    field = Some(id.value.clone());
                }
                _ => {
                    return Err(PieskieoError::Internal(
                        "aggregate argument not supported".into(),
                    ))
                }
            }
        }
        Ok(AggExpr {
            alias: alias.unwrap_or(&name).to_string(),
            field,
            kind,
        })
    }

    fn collect_nums(rows: &[(Uuid, Value)], field: &str) -> Vec<f64> {
        rows.iter()
            .filter_map(|(_, v)| v.get(field))
            .filter_map(|n| n.as_f64())
            .collect()
    }

    fn num_or_null(x: f64) -> Value {
        serde_json::Number::from_f64(x)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    }

    fn split_name(
        &self,
        name: &sqlparser::ast::ObjectName,
    ) -> Result<(Option<String>, String, String)> {
        let parts: Vec<String> = name.0.iter().map(|i| i.value.clone()).collect();
        match parts.len() {
            3 => Ok((Some(parts[0].clone()), parts[1].clone(), parts[2].clone())),
            2 => Ok((None, parts[0].clone(), parts[1].clone())),
            1 => Ok((None, "default".into(), parts[0].clone())),
            _ => Err(PieskieoError::Internal(
                "table name must be [family.]ns.coll".into(),
            )),
        }
    }

    fn extract_name_from_table_factor<'a>(
        &self,
        tf: &'a TableFactor,
    ) -> Result<&'a sqlparser::ast::ObjectName> {
        match tf {
            TableFactor::Table { name, .. } => Ok(name),
            _ => Err(PieskieoError::Internal("only base tables supported".into())),
        }
    }

    fn target_is_rows(family: Option<&str>, coll: &str) -> bool {
        match family {
            Some("rows") | Some("tables") | Some("table") | Some("row") => true,
            Some("docs") | Some("collections") | Some("doc") => false,
            _ => {
                coll.starts_with("rows_") || coll.starts_with("table_") || coll.starts_with("tbl_")
            }
        }
    }

    fn walk_expr(&self, expr: &Expr, out: &mut Vec<Condition>) -> Result<()> {
        match expr {
            Expr::BinaryOp { left, op, right } => match op {
                BinaryOperator::And => {
                    self.walk_expr(left, out)?;
                    self.walk_expr(right, out)?;
                    Ok(())
                }
                BinaryOperator::Eq
                | BinaryOperator::NotEq
                | BinaryOperator::Gt
                | BinaryOperator::GtEq
                | BinaryOperator::Lt
                | BinaryOperator::LtEq => {
                    let (field, value) = self.extract_field_value(left, right)?;
                    let op = match op {
                        BinaryOperator::Eq => Op::Eq,
                        BinaryOperator::NotEq => Op::Ne,
                        BinaryOperator::Gt => Op::Gt,
                        BinaryOperator::GtEq => Op::Gte,
                        BinaryOperator::Lt => Op::Lt,
                        BinaryOperator::LtEq => Op::Lte,
                        _ => unreachable!(),
                    };
                    out.push(Condition { field, op, value });
                    Ok(())
                }
                _ => Err(PieskieoError::Internal("operator not supported".into())),
            },
            Expr::InList {
                expr,
                list,
                negated,
                ..
            } => {
                let field = Self::ident_name(expr)?;
                let values: Vec<Value> = list.iter().filter_map(Self::literal_to_value).collect();
                let op = if *negated { Op::Nin } else { Op::In };
                out.push(Condition {
                    field,
                    op,
                    value: Value::Array(values),
                });
                Ok(())
            }
            _ => Err(PieskieoError::Internal("expression not supported".into())),
        }
    }

    fn ident_name(expr: &Expr) -> Result<String> {
        if let Expr::Identifier(id) = expr {
            Ok(id.value.clone())
        } else {
            Err(PieskieoError::Internal("field must be identifier".into()))
        }
    }

    fn literal_to_value(expr: &Expr) -> Option<Value> {
        match expr {
            Expr::Value(sqlparser::ast::Value::Number(n, _)) => n
                .parse::<f64>()
                .ok()
                .and_then(|f| serde_json::Number::from_f64(f))
                .map(Value::Number),
            Expr::Value(sqlparser::ast::Value::SingleQuotedString(s)) => {
                Some(Value::String(s.clone()))
            }
            Expr::Value(sqlparser::ast::Value::Boolean(b)) => Some(Value::Bool(*b)),
            _ => None,
        }
    }

    fn extract_field_value(&self, left: &Expr, right: &Expr) -> Result<(String, Value)> {
        let field = Self::ident_name(left)?;
        let value = Self::literal_to_value(right)
            .ok_or_else(|| PieskieoError::Internal("unsupported literal".into()))?;
        Ok((field, value))
    }
}

impl PieskieoDb {
    fn filter_map(
        &self,
        map: &HashMap<String, HashMap<String, BTreeMap<Uuid, Value>>>,
        ns: Option<&str>,
        coll: Option<&str>,
        filter: &HashMap<String, Value>,
        limit: usize,
        offset: usize,
    ) -> Vec<(Uuid, Value)> {
        let mut out = Vec::new();
        let mut skipped = 0usize;
        match (ns, coll) {
            (Some(ns), Some(c)) => {
                if let Some(ns_map) = map.get(ns) {
                    if let Some(inner) = ns_map.get(c) {
                        Self::collect_filtered_inner(
                            self,
                            inner,
                            filter,
                            limit,
                            offset,
                            &mut out,
                            &mut skipped,
                        );
                    }
                }
            }
            (Some(ns), None) => {
                if let Some(ns_map) = map.get(ns) {
                    for inner in ns_map.values() {
                        Self::collect_filtered_inner(
                            self,
                            inner,
                            filter,
                            limit,
                            offset,
                            &mut out,
                            &mut skipped,
                        );
                        if out.len() >= limit {
                            break;
                        }
                    }
                }
            }
            (None, Some(c)) => {
                for ns_map in map.values() {
                    if let Some(inner) = ns_map.get(c) {
                        Self::collect_filtered_inner(
                            self,
                            inner,
                            filter,
                            limit,
                            offset,
                            &mut out,
                            &mut skipped,
                        );
                        if out.len() >= limit {
                            break;
                        }
                    }
                }
            }
            (None, None) => {
                for ns_map in map.values() {
                    for inner in ns_map.values() {
                        Self::collect_filtered_inner(
                            self,
                            inner,
                            filter,
                            limit,
                            offset,
                            &mut out,
                            &mut skipped,
                        );
                        if out.len() >= limit {
                            break;
                        }
                    }
                }
            }
        };
        out
    }

    fn collect_filtered_inner(
        &self,
        inner: &BTreeMap<Uuid, Value>,
        filter: &HashMap<String, Value>,
        limit: usize,
        offset: usize,
        out: &mut Vec<(Uuid, Value)>,
        skipped: &mut usize,
    ) {
        for (id, v) in inner.iter() {
            if !self.owns(id) {
                continue;
            }
            if value_matches(v, filter) {
                if *skipped < offset {
                    *skipped += 1;
                    continue;
                }
                if out.len() < limit {
                    out.push((*id, v.clone()));
                    if out.len() >= limit {
                        return;
                    }
                }
            }
        }
    }

    /// Filter with optional equality index shortcut.
    fn filter_map_with_index(
        &self,
        map: &HashMap<String, HashMap<String, BTreeMap<Uuid, Value>>>,
        index: &HashMap<String, HashMap<String, HashMap<String, HashMap<String, Vec<Uuid>>>>>,
        ns: Option<&str>,
        coll: Option<&str>,
        filter: &HashMap<String, Value>,
        limit: usize,
        offset: usize,
    ) -> Vec<(Uuid, Value)> {
        // cost heuristic: intersect equality indexes when available, else scan.
        if let (Some(ns), Some(coll)) = (ns, coll) {
            if let Some(ns_map) = index.get(ns) {
                if let Some(col_map) = ns_map.get(coll) {
                    let total_rows = self
                        .stats
                        .read()
                        .docs
                        .get(ns)
                        .and_then(|m| m.get(coll))
                        .cloned()
                        .unwrap_or_else(|| {
                            map.get(ns)
                                .and_then(|m| m.get(coll))
                                .map(|m| m.len())
                                .unwrap_or(0)
                        });
                    let mut candidate: Option<Vec<Uuid>> = None;
                    for (field, val) in filter.iter() {
                        if let Some(key) = Self::index_key(val) {
                            if let Some(field_map) = col_map.get(field) {
                                if let Some(ids) = field_map.get(&key) {
                                    let ids = ids.clone();
                                    candidate = Some(match candidate.take() {
                                        None => ids,
                                        Some(prev) => {
                                            let set: std::collections::HashSet<Uuid> =
                                                prev.into_iter().collect();
                                            ids.into_iter().filter(|i| set.contains(i)).collect()
                                        }
                                    });
                                }
                            }
                        }
                    }
                    if let Some(ids) = candidate {
                        if let Some(inner) = map.get(ns).and_then(|m| m.get(coll)) {
                            let estimated = ids.len();
                            let threshold = (total_rows / 2).max(10);
                            // use index only if estimated hit count is lower than threshold
                            if estimated <= threshold {
                                let mut out = Vec::new();
                                let mut skipped = 0usize;
                                for id in ids {
                                    if !self.owns(&id) {
                                        continue;
                                    }
                                    if let Some(v) = inner.get(&id) {
                                        if !value_matches(v, filter) {
                                            continue;
                                        }
                                        if skipped < offset {
                                            skipped += 1;
                                            continue;
                                        }
                                        out.push((id, v.clone()));
                                        if out.len() >= limit {
                                            return out;
                                        }
                                    }
                                }
                                return out;
                            }
                        }
                    }
                }
            }
        }
        // fallback full scan
        self.filter_map(map, ns, coll, filter, limit, offset)
    }

    fn index_key(v: &Value) -> Option<String> {
        match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    }

    fn index_upsert_doc(colls: &mut Collections, ns: String, col: String, id: Uuid, json: &Value) {
        if let Some(obj) = json.as_object() {
            for (k, v) in obj {
                if let Some(key) = Self::index_key(v) {
                    let entry = colls
                        .doc_index
                        .entry(ns.clone())
                        .or_default()
                        .entry(col.clone())
                        .or_default()
                        .entry(k.clone())
                        .or_default()
                        .entry(key)
                        .or_default();
                    if !entry.contains(&id) {
                        entry.push(id);
                    }
                }
            }
        }
    }

    fn index_remove_doc(colls: &mut Collections, ns: String, col: String, id: &Uuid, json: &Value) {
        if let Some(obj) = json.as_object() {
            for (k, v) in obj {
                if let Some(key) = Self::index_key(v) {
                    if let Some(ns_map) = colls.doc_index.get_mut(&ns) {
                        if let Some(col_map) = ns_map.get_mut(&col) {
                            if let Some(field_map) = col_map.get_mut(k) {
                                if let Some(ids) = field_map.get_mut(&key) {
                                    ids.retain(|x| x != id);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn index_upsert_row(
        colls: &mut Collections,
        ns: String,
        table: String,
        id: Uuid,
        json: &Value,
    ) {
        if let Some(obj) = json.as_object() {
            for (k, v) in obj {
                if let Some(key) = Self::index_key(v) {
                    let entry = colls
                        .row_index
                        .entry(ns.clone())
                        .or_default()
                        .entry(table.clone())
                        .or_default()
                        .entry(k.clone())
                        .or_default()
                        .entry(key)
                        .or_default();
                    if !entry.contains(&id) {
                        entry.push(id);
                    }
                }
            }
        }
    }

    fn index_remove_row(
        colls: &mut Collections,
        ns: String,
        table: String,
        id: &Uuid,
        json: &Value,
    ) {
        if let Some(obj) = json.as_object() {
            for (k, v) in obj {
                if let Some(key) = Self::index_key(v) {
                    if let Some(ns_map) = colls.row_index.get_mut(&ns) {
                        if let Some(tbl_map) = ns_map.get_mut(&table) {
                            if let Some(field_map) = tbl_map.get_mut(k) {
                                if let Some(ids) = field_map.get_mut(&key) {
                                    ids.retain(|x| x != id);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
#[derive(Clone, Copy)]
pub struct VectorParams {
    pub metric: VectorMetric,
    pub ef_construction: usize,
    pub ef_search: usize,
    pub max_elements: usize,
    pub link_top_k: usize,
    pub shard_id: usize,
    pub shard_total: usize,
}

pub struct MetricsSnapshot {
    pub docs: usize,
    pub rows: usize,
    pub vectors: usize,
    pub vector_tombstones: usize,
    pub hnsw_ready: bool,
    pub ef_search: usize,
    pub ef_construction: usize,
    pub wal_path: std::path::PathBuf,
    pub wal_bytes: u64,
    pub snapshot_mtime: Option<std::time::SystemTime>,
    pub link_top_k: usize,
    pub shard_id: usize,
    pub shard_total: usize,
}

impl Default for VectorParams {
    fn default() -> Self {
        Self {
            metric: VectorMetric::L2,
            ef_construction: 200,
            ef_search: 50,
            max_elements: 100_000,
            link_top_k: 0,
            shard_id: 0,
            shard_total: 1,
        }
    }
}

impl Drop for PieskieoDb {
    fn drop(&mut self) {
        let _ = self.save_vector_snapshot();
    }
}

fn shard_hash(id: &Uuid) -> usize {
    let bytes = id.as_bytes();
    let mut arr = [0u8; 8];
    arr.copy_from_slice(&bytes[..8]);
    u64::from_le_bytes(arr) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn round_trip_doc_and_vector() -> Result<()> {
        let dir = tempdir().unwrap();
        let db = PieskieoDb::open(dir.path())?;
        let id = Uuid::new_v4();
        db.put_doc(id, serde_json::json!({"hello": "world"}))?;
        db.put_vector(id, vec![0.1, 0.2, 0.3])?;

        assert_eq!(db.get_doc(&id).unwrap()["hello"], "world");
        let hits = db.search_vector(&[0.1, 0.2, 0.3], 1)?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, id);
        Ok(())
    }

    #[tokio::test]
    async fn graph_neighbors() -> Result<()> {
        let dir = tempdir().unwrap();
        let db = PieskieoDb::open(dir.path())?;
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        db.add_edge(a, b, 1.0)?;
        let neighbors = db.neighbors(a, 10);
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0].dst, b);
        Ok(())
    }

    #[tokio::test]
    async fn auto_links_neighbors_when_enabled() -> Result<()> {
        let dir = tempdir().unwrap();
        let db = PieskieoDb::open_with_params(
            dir.path(),
            VectorParams {
                link_top_k: 1,
                ..Default::default()
            },
        )?;
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        db.put_vector(a, vec![0.0, 0.0])?;
        db.put_vector(b, vec![0.0, 0.1])?;

        let neighbors = db.neighbors(a, 10);
        assert!(
            neighbors.iter().any(|e| e.dst == b),
            "expected auto-linked neighbor"
        );
        Ok(())
    }

    #[tokio::test]
    async fn vacuum_clears_tombstones_and_wal() -> Result<()> {
        let dir = tempdir().unwrap();
        let db = PieskieoDb::open(dir.path())?;
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        db.put_vector(a, vec![0.0, 0.0])?;
        db.put_vector(b, vec![0.0, 0.1])?;
        db.delete_vector(&a)?;
        let idx = db.vector_index("default");
        assert!(idx.tombstones.read().contains_key(&a));
        db.vacuum()?;
        assert!(!idx.tombstones.read().contains_key(&a));
        Ok(())
    }

    #[tokio::test]
    async fn sql_projection_and_order_by_docs() -> Result<()> {
        let dir = tempdir().unwrap();
        let db = PieskieoDb::open(dir.path())?;
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        db.put_doc_ns(
            Some("default"),
            Some("people"),
            a,
            serde_json::json!({"name": "alice", "age": 30, "city": "ny"}),
        )?;
        db.put_doc_ns(
            Some("default"),
            Some("people"),
            b,
            serde_json::json!({"name": "bob", "age": 20, "city": "sf"}),
        )?;

        let res = db.query_sql(
            "SELECT name, age FROM docs.default.people WHERE age > 0 ORDER BY age ASC LIMIT 1",
        )?;
        let rows = match res {
            SqlResult::Select(r) => r,
            _ => panic!("expected select"),
        };
        assert_eq!(rows.len(), 1);
        let (_id, val) = &rows[0];
        let obj = val.as_object().unwrap();
        assert_eq!(obj.get("name").unwrap(), "bob");
        assert!(!obj.contains_key("city"));
        Ok(())
    }

    #[tokio::test]
    async fn sql_projection_alias_and_order_multi() -> Result<()> {
        let dir = tempdir().unwrap();
        let db = PieskieoDb::open(dir.path())?;
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let c = Uuid::new_v4();
        db.put_doc_ns(
            Some("default"),
            Some("people"),
            a,
            serde_json::json!({"first": "alice", "last": "zephyr", "age": 30, "score": 9}),
        )?;
        db.put_doc_ns(
            Some("default"),
            Some("people"),
            b,
            serde_json::json!({"first": "bob", "last": "yellow", "age": 25, "score": 9}),
        )?;
        db.put_doc_ns(
            Some("default"),
            Some("people"),
            c,
            serde_json::json!({"first": "carol", "last": "yellow", "age": 25, "score": 5}),
        )?;
        let res = db.query_sql(
            "SELECT first AS fname, _id AS id FROM default.people \
             WHERE score >= 5 ORDER BY score DESC, age ASC LIMIT 2",
        )?;
        let rows = match res {
            SqlResult::Select(r) => r,
            _ => panic!("expected select"),
        };
        assert_eq!(rows.len(), 2);
        let (_, v0) = &rows[0];
        let (_, v1) = &rows[1];
        let f0 = v0.get("fname").unwrap().as_str().unwrap();
        let f1 = v1.get("fname").unwrap().as_str().unwrap();
        // same score, so age secondary; bob(25) then alice(30)
        assert_eq!((f0, f1), ("bob", "alice"));
        assert!(v0.get("_id").is_none(), "id should be under alias only");
        assert!(v0.get("id").is_some());
        Ok(())
    }

    #[tokio::test]
    async fn sql_targets_rows_family() -> Result<()> {
        let dir = tempdir().unwrap();
        let db = PieskieoDb::open(dir.path())?;
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        db.put_row(a, &serde_json::json!({"score": 2, "x": 1}))?;
        db.put_row_ns(
            Some("analytics"),
            Some("table_sessions"),
            b,
            &serde_json::json!({"score": 5, "x": 2}),
        )?;

        let res = db.query_sql("SELECT * FROM rows.analytics.table_sessions WHERE score >= 5")?;
        let rows = match res {
            SqlResult::Select(r) => r,
            _ => panic!("expected select"),
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1["score"], 5);
        Ok(())
    }

    #[tokio::test]
    async fn pql_join_and_aggregate() -> Result<()> {
        let dir = tempdir().unwrap();
        let db = PieskieoDb::open(dir.path())?;
        let u1 = Uuid::new_v4();
        let u2 = Uuid::new_v4();
        let o1 = Uuid::new_v4();
        let o2 = Uuid::new_v4();

        db.query_sql(&format!(
            "INSERT INTO docs.default.users (_id, name, age) VALUES ('{}', 'alice', 30)",
            u1
        ))?;
        db.query_sql(&format!(
            "INSERT INTO docs.default.users (_id, name, age) VALUES ('{}', 'bob', 20)",
            u2
        ))?;
        db.query_sql(&format!(
            "INSERT INTO rows.default.orders (_id, user_id, amount) VALUES ('{}', '{}', 50)",
            o1, u1
        ))?;
        db.query_sql(&format!(
            "INSERT INTO rows.default.orders (_id, user_id, amount) VALUES ('{}', '{}', 5)",
            o2, u2
        ))?;

        // aggregation
        let res =
            db.query_sql("SELECT COUNT(*) AS c FROM rows.default.orders WHERE amount > 10")?;
        let rows = match res {
            SqlResult::Select(r) => r,
            _ => panic!("expected select"),
        };
        assert_eq!(rows.len(), 1);
        let count = rows[0].1.get("c").unwrap().as_i64().unwrap();
        assert_eq!(count, 1);

        // wal incremental tailing should see records
        db.wal.write().flush_sync()?;
        let (records, end) = db.wal_replay_since(0)?;
        assert!(end > 0);
        assert!(records.len() >= 4);
        Ok(())
    }
}
