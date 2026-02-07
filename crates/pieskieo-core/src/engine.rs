use crate::error::Result;
use crate::vector::{VectorIndex, VectorMetric};
use crate::wal::{DataFamily, RecordKind, Wal};
use crate::{error::PieskieoError, graph::GraphStore};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
}

pub struct PieskieoDb {
    path: PathBuf,
    pub(crate) wal: RwLock<Wal>,
    pub(crate) data: Arc<RwLock<Collections>>,
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
            Self::index_upsert_doc(&mut guard, ns_key, col_key, id, &json);
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
                        Self::index_remove_doc(&mut guard, ns_key, col_key, id, &old);
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
            Self::index_upsert_row(&mut guard, ns_key, tbl_key, id, &json);
        }
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
                        Self::index_remove_row(&mut guard, ns_key, tbl_key, id, &old);
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
            if field != v {
                return false;
            }
        } else {
            return false;
        }
    }
    true
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
                        Self::collect_filtered(
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
                        Self::collect_filtered(
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
                        Self::collect_filtered(
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
                        Self::collect_filtered(
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

    fn collect_filtered(
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
        // attempt index if single namespace+collection specified and single equality filter
        if let (Some(ns), Some(coll)) = (ns, coll) {
            if filter.len() == 1 {
                if let Some((fk, fv)) = filter.iter().next() {
                    if let Some(key) = Self::index_key(fv) {
                        if let Some(ns_map) = index.get(ns) {
                            if let Some(col_map) = ns_map.get(coll) {
                                if let Some(field_map) = col_map.get(fk) {
                                    if let Some(ids) = field_map.get(&key) {
                                        if let Some(inner) = map.get(ns).and_then(|m| m.get(coll)) {
                                            let mut out = Vec::new();
                                            let mut skipped = 0usize;
                                            for id in ids {
                                                if !self.owns(id) {
                                                    continue;
                                                }
                                                if let Some(v) = inner.get(id) {
                                                    if skipped < offset {
                                                        skipped += 1;
                                                        continue;
                                                    }
                                                    out.push((*id, v.clone()));
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
}
