use crate::error::Result;
use crate::graph::GraphStore;
use crate::vector::{VectorIndex, VectorMetric};
use crate::wal::{DataFamily, RecordKind, Wal};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Default)]
pub(crate) struct Collections {
    rows: BTreeMap<Uuid, Value>,
    docs: BTreeMap<Uuid, Value>,
}

pub struct KaedeDb {
    path: PathBuf,
    pub(crate) wal: RwLock<Wal>,
    pub(crate) data: Arc<RwLock<Collections>>,
    pub(crate) vectors: VectorIndex,
    pub(crate) graph: GraphStore,
    link_top_k: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpsertDoc {
    pub id: Uuid,
    pub json: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VecWalRecord {
    vector: Vec<f32>,
    meta: Option<HashMap<String, String>>,
}

impl KaedeDb {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_params(path, VectorParams::default())
    }

    pub fn open_with_params(path: impl AsRef<Path>, params: VectorParams) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let wal = Wal::open(&path)?;
        let data = Arc::new(RwLock::new(Collections::default()));
        let vectors = VectorIndex::with_params(
            params.metric,
            params.ef_construction,
            params.ef_search,
            params.max_elements,
        );
        let graph = GraphStore::new();

        for rec in wal.replay()? {
            match rec {
                RecordKind::Put {
                    family,
                    key,
                    payload,
                } => match family {
                    DataFamily::Doc => {
                        let v: Value = serde_json::from_slice(&payload)?;
                        data.write().docs.insert(key, v);
                    }
                    DataFamily::Row => {
                        let v: Value = serde_json::from_slice(&payload)?;
                        data.write().rows.insert(key, v);
                    }
                    DataFamily::Vec => match bincode::deserialize::<VecWalRecord>(&payload) {
                        Ok(rec) => {
                            let _ = vectors.insert(key, rec.vector, rec.meta);
                        }
                        Err(_) => {
                            let vec: Vec<f32> = bincode::deserialize(&payload)?;
                            let _ = vectors.insert(key, vec, None);
                        }
                    },
                    DataFamily::Graph => {
                        let edge: crate::graph::Edge = bincode::deserialize(&payload)?;
                        graph.add_edge(edge.src, edge.dst, edge.weight);
                    }
                },
                RecordKind::Delete { family, key } => match family {
                    DataFamily::Doc => {
                        data.write().docs.remove(&key);
                    }
                    DataFamily::Row => {
                        data.write().rows.remove(&key);
                    }
                    DataFamily::Vec => {
                        vectors.delete(&key);
                    }
                    DataFamily::Graph => {}
                },
                RecordKind::AddEdge { src, dst, weight } => {
                    graph.add_edge(src, dst, weight);
                }
            }
        }

        // Optional fast reload of vectors from snapshot.
        let snapshot = path.join("vectors.snapshot");
        if snapshot.exists() {
            let _ = vectors.load_snapshot(&snapshot);
            let _ = vectors.rebuild_hnsw();
        }

        Ok(Self {
            path,
            wal: RwLock::new(wal),
            data,
            vectors,
            graph,
            link_top_k: params.link_top_k,
        })
    }

    pub fn put_doc(&self, id: Uuid, json: Value) -> Result<()> {
        let payload = serde_json::to_vec(&json)?;
        self.append_record(&RecordKind::Put {
            family: DataFamily::Doc,
            key: id,
            payload,
        })?;
        self.data.write().docs.insert(id, json);
        Ok(())
    }

    pub fn delete_doc(&self, id: &Uuid) -> Result<()> {
        self.append_record(&RecordKind::Delete {
            family: DataFamily::Doc,
            key: *id,
        })?;
        self.data.write().docs.remove(id);
        Ok(())
    }

    pub fn update_doc(&self, id: Uuid, json: Value) -> Result<()> {
        self.put_doc(id, json)
    }

    pub fn put_row<T: Serialize>(&self, id: Uuid, row: &T) -> Result<()> {
        let json = serde_json::to_value(row)?;
        let payload = serde_json::to_vec(&json)?;
        self.append_record(&RecordKind::Put {
            family: DataFamily::Row,
            key: id,
            payload,
        })?;
        self.data.write().rows.insert(id, json);
        Ok(())
    }

    pub fn delete_row(&self, id: &Uuid) -> Result<()> {
        self.append_record(&RecordKind::Delete {
            family: DataFamily::Row,
            key: *id,
        })?;
        self.data.write().rows.remove(id);
        Ok(())
    }

    pub fn update_row<T: Serialize>(&self, id: Uuid, row: &T) -> Result<()> {
        self.put_row(id, row)
    }

    pub fn get_doc(&self, id: &Uuid) -> Option<Value> {
        self.data.read().docs.get(id).cloned()
    }

    pub fn get_row(&self, id: &Uuid) -> Option<Value> {
        self.data.read().rows.get(id).cloned()
    }

    pub fn put_vector(&self, id: Uuid, vector: Vec<f32>) -> Result<()> {
        self.put_vector_with_meta(id, vector, None)
    }

    pub fn put_vector_with_meta(
        &self,
        id: Uuid,
        vector: Vec<f32>,
        meta: Option<HashMap<String, String>>,
    ) -> Result<()> {
        let payload = bincode::serialize(&VecWalRecord {
            vector: vector.clone(),
            meta: meta.clone(),
        })?;
        self.append_record(&RecordKind::Put {
            family: DataFamily::Vec,
            key: id,
            payload,
        })?;
        self.vectors.insert(id, vector, meta)?;
        self.auto_link_neighbors(id);
        Ok(())
    }

    pub fn update_vector(&self, id: Uuid, vector: Vec<f32>) -> Result<()> {
        self.put_vector(id, vector)
    }

    pub fn delete_vector(&self, id: &Uuid) -> Result<()> {
        self.append_record(&RecordKind::Delete {
            family: DataFamily::Vec,
            key: *id,
        })?;
        self.vectors.delete(id);
        Ok(())
    }

    fn auto_link_neighbors(&self, id: Uuid) {
        if self.link_top_k == 0 {
            return;
        }
        let vector = {
            let guard = self.vectors.inner.read();
            guard.get(&id).cloned()
        };
        let Some(vector) = vector else {
            return;
        };
        let mut hits = match self
            .vectors
            .search_ann_filtered(&vector, self.link_top_k + 1, None)
        {
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
        self.vectors.search(query, k)
    }

    pub fn search_vector_metric(
        &self,
        query: &[f32],
        k: usize,
        metric: crate::vector::VectorMetric,
        filter_meta: Option<HashMap<String, String>>,
    ) -> Result<Vec<crate::vector::VectorSearchResult>> {
        let local = crate::vector::VectorIndex::from_shared(
            self.vectors.inner.clone(),
            self.vectors.dim.clone(),
            metric,
            self.vectors.hnsw.clone(),
            self.vectors.owned_store.clone(),
            self.vectors.id_map.clone(),
            self.vectors.rev_map.clone(),
            self.vectors.next_id.clone(),
            self.vectors.tombstones.clone(),
            std::sync::atomic::AtomicUsize::new(
                self.vectors
                    .ef_construction
                    .load(std::sync::atomic::Ordering::SeqCst),
            ),
            std::sync::atomic::AtomicUsize::new(
                self.vectors
                    .ef_search
                    .load(std::sync::atomic::Ordering::SeqCst),
            ),
            self.vectors.max_elements,
            self.vectors.meta.clone(),
        );
        local.search_ann_filtered(query, k, filter_meta)
    }

    pub fn add_edge(&self, src: Uuid, dst: Uuid, weight: f32) -> Result<()> {
        let payload = bincode::serialize(&crate::graph::Edge { src, dst, weight })?;
        self.append_record(&RecordKind::Put {
            family: DataFamily::Graph,
            key: src,
            payload,
        })?;
        self.graph.add_edge(src, dst, weight);
        Ok(())
    }

    pub fn neighbors(&self, id: Uuid, limit: usize) -> Vec<crate::graph::Edge> {
        self.graph.neighbors(id, limit)
    }

    pub fn rebuild_vectors(&self) -> Result<()> {
        self.vectors.rebuild_hnsw()
    }

    pub fn save_vector_snapshot(&self) -> Result<()> {
        let snap = self.path.join("vectors.snapshot");
        self.vectors.save_snapshot(&snap)?;
        let hnsw = self.path.join("hnsw.bin");
        self.vectors.save_hnsw(&hnsw)
    }

    pub fn set_ef_search(&self, ef: usize) {
        self.vectors.set_ef_search(ef);
    }

    pub fn set_ef_construction(&self, ef: usize) {
        self.vectors.set_ef_construction(ef);
    }

    pub fn flush_wal(&self) -> Result<()> {
        self.wal.write().flush_sync()
    }

    pub fn metrics(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            docs: self.data.read().docs.len(),
            rows: self.data.read().rows.len(),
            vectors: self.vectors.inner.read().len(),
            vector_tombstones: self.vectors.tombstones.read().len(),
            hnsw_ready: self.vectors.hnsw.read().is_some(),
            ef_search: self
                .vectors
                .ef_search
                .load(std::sync::atomic::Ordering::SeqCst),
            ef_construction: self
                .vectors
                .ef_construction
                .load(std::sync::atomic::Ordering::SeqCst),
            wal_path: self.path.join("wal.log"),
            wal_bytes: std::fs::metadata(self.path.join("wal.log"))
                .map(|m| m.len())
                .unwrap_or(0),
            snapshot_mtime: std::fs::metadata(self.path.join("vectors.snapshot"))
                .and_then(|m| m.modified())
                .ok(),
            link_top_k: self.link_top_k,
        }
    }

    fn append_record(&self, record: &RecordKind) -> Result<()> {
        self.wal.write().append(record)
    }
}

#[derive(Clone, Copy)]
pub struct VectorParams {
    pub metric: VectorMetric,
    pub ef_construction: usize,
    pub ef_search: usize,
    pub max_elements: usize,
    pub link_top_k: usize,
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
}

impl Default for VectorParams {
    fn default() -> Self {
        Self {
            metric: VectorMetric::L2,
            ef_construction: 200,
            ef_search: 50,
            max_elements: 100_000,
            link_top_k: 0,
        }
    }
}

impl Drop for KaedeDb {
    fn drop(&mut self) {
        let snap = self.path.join("vectors.snapshot");
        let _ = self.vectors.save_snapshot(snap);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn round_trip_doc_and_vector() -> Result<()> {
        let dir = tempdir().unwrap();
        let db = KaedeDb::open(dir.path())?;
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
        let db = KaedeDb::open(dir.path())?;
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
        let db = KaedeDb::open_with_params(
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
}
