use crate::error::{KaedeDbError, Result};
use hnsw_rs::prelude::*;
use parking_lot::RwLock;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorSearchResult {
    pub id: Uuid,
    pub score: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum VectorMetric {
    L2,
    Cosine,
    Dot,
}

/// In-memory vector store + optional HNSW ANN accelerator.
pub struct VectorIndex {
    pub(crate) inner: Arc<RwLock<HashMap<Uuid, Vec<f32>>>>,
    pub(crate) dim: Arc<RwLock<Option<usize>>>,
    metric: VectorMetric,
    pub(crate) hnsw: Arc<RwLock<Option<Hnsw<'static, f32, DistL2>>>>,
    pub(crate) owned_store: Arc<RwLock<Vec<&'static [f32]>>>,
    pub(crate) id_map: Arc<RwLock<HashMap<Uuid, usize>>>,
    pub(crate) rev_map: Arc<RwLock<Vec<Uuid>>>,
    pub(crate) next_id: Arc<AtomicUsize>,
    pub(crate) tombstones: Arc<RwLock<HashMap<Uuid, ()>>>,
    pub(crate) ef_construction: AtomicUsize,
    pub(crate) ef_search: AtomicUsize,
    pub(crate) max_elements: usize,
    pub(crate) meta: Arc<RwLock<HashMap<Uuid, HashMap<String, String>>>>,
}

impl VectorIndex {
    pub fn new(metric: VectorMetric) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            dim: Arc::new(RwLock::new(None)),
            metric,
            hnsw: Arc::new(RwLock::new(None)),
            owned_store: Arc::new(RwLock::new(Vec::new())),
            id_map: Arc::new(RwLock::new(HashMap::new())),
            rev_map: Arc::new(RwLock::new(Vec::new())),
            next_id: Arc::new(AtomicUsize::new(0)),
            tombstones: Arc::new(RwLock::new(HashMap::new())),
            ef_construction: AtomicUsize::new(200),
            ef_search: AtomicUsize::new(50),
            max_elements: 100_000,
            meta: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn with_params(
        metric: VectorMetric,
        ef_construction: usize,
        ef_search: usize,
        max_elements: usize,
    ) -> Self {
        let mut v = Self::new(metric);
        v.ef_construction
            .store(ef_construction.max(4), Ordering::SeqCst);
        v.ef_search.store(ef_search.max(4), Ordering::SeqCst);
        v.max_elements = max_elements.max(1_000);
        v
    }

    pub fn from_shared(
        inner: Arc<RwLock<HashMap<Uuid, Vec<f32>>>>,
        dim: Arc<RwLock<Option<usize>>>,
        metric: VectorMetric,
        hnsw: Arc<RwLock<Option<Hnsw<'static, f32, DistL2>>>>,
        owned_store: Arc<RwLock<Vec<&'static [f32]>>>,
        id_map: Arc<RwLock<HashMap<Uuid, usize>>>,
        rev_map: Arc<RwLock<Vec<Uuid>>>,
        next_id: Arc<AtomicUsize>,
        tombstones: Arc<RwLock<HashMap<Uuid, ()>>>,
        ef_construction: AtomicUsize,
        ef_search: AtomicUsize,
        max_elements: usize,
        meta: Arc<RwLock<HashMap<Uuid, HashMap<String, String>>>>,
    ) -> Self {
        Self {
            inner,
            dim,
            metric,
            hnsw,
            owned_store,
            id_map,
            rev_map,
            next_id,
            tombstones,
            ef_construction,
            ef_search,
            max_elements,
            meta,
        }
    }

    pub fn insert(
        &self,
        id: Uuid,
        mut vector: Vec<f32>,
        meta: Option<HashMap<String, String>>,
    ) -> Result<()> {
        // Enforce consistent dimensionality.
        {
            let mut dim_guard = self.dim.write();
            if let Some(dim) = *dim_guard {
                if vector.len() != dim {
                    return Err(KaedeDbError::NotFound);
                }
            } else {
                *dim_guard = Some(vector.len());
            }
        }

        if matches!(self.metric, VectorMetric::Cosine) {
            normalize(&mut vector);
        }

        // Update primary store.
        self.inner.write().insert(id, vector.clone());
        if let Some(m) = meta {
            self.meta.write().insert(id, m);
        }
        self.tombstones.write().remove(&id);

        // Assign stable internal ID.
        let internal = {
            let mut map = self.id_map.write();
            if let Some(existing) = map.get(&id) {
                *existing
            } else {
                let new_id = self.next_id.fetch_add(1, Ordering::SeqCst);
                map.insert(id, new_id);
                let mut rev = self.rev_map.write();
                if rev.len() <= new_id {
                    rev.resize(new_id + 1, Uuid::nil());
                }
                rev[new_id] = id;
                new_id
            }
        };

        // Materialize owned backing for HNSW (leaked to 'static slice for simplicity).
        let boxed: Box<[f32]> = vector.into_boxed_slice();
        let leaked: &'static [f32] = Box::leak(boxed);
        self.owned_store.write().push(leaked);

        // Lazy-create HNSW index on first insert; use L2 space (cosine vectors are normalized).
        {
            let mut h = self.hnsw.write();
            if h.is_none() {
                let max_layer = 16;
                let hnsw = Hnsw::<f32, DistL2>::new(
                    16,
                    self.max_elements,
                    max_layer,
                    self.ef_construction.load(Ordering::SeqCst),
                    DistL2 {},
                );
                *h = Some(hnsw);
            }
            if let Some(ref mut hnsw) = *h {
                hnsw.insert((leaked, internal));
            }
        }
        Ok(())
    }

    pub fn delete(&self, id: &Uuid) {
        self.inner.write().remove(id);
        self.tombstones.write().insert(*id, ());
        self.maybe_rebuild();
    }

    pub fn search(&self, query: &[f32], k: usize) -> Result<Vec<VectorSearchResult>> {
        self.search_filtered(query, k, None)
    }

    pub fn search_filtered(
        &self,
        query: &[f32],
        k: usize,
        filter_meta: Option<HashMap<String, String>>,
    ) -> Result<Vec<VectorSearchResult>> {
        if query.is_empty() {
            return Err(KaedeDbError::NotFound);
        }
        {
            let dim_guard = self.dim.read();
            if let Some(dim) = *dim_guard {
                if query.len() != dim {
                    return Err(KaedeDbError::NotFound);
                }
            }
        }

        // Prepare query copy for cosine normalization without mutating caller buffer.
        let mut qbuf: Vec<f32> = query.to_vec();
        if matches!(self.metric, VectorMetric::Cosine) {
            normalize(&mut qbuf);
        }

        // Snapshot to minimize lock hold during compute-heavy loop.
        let snapshot: Vec<(Uuid, Vec<f32>)> = {
            let guard = self.inner.read();
            guard.iter().map(|(id, v)| (*id, v.clone())).collect()
        };

        // Parallel distance computation for better throughput on large collections.
        let mut scores: Vec<_> = snapshot
            .par_iter()
            .map(|(id, v)| {
                let d = match self.metric {
                    VectorMetric::L2 => -l2(&qbuf, v),
                    VectorMetric::Cosine => dot(&qbuf, v),
                    VectorMetric::Dot => dot(&qbuf, v),
                };
                VectorSearchResult { id: *id, score: d }
            })
            .collect();
        scores.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
        if let Some(filters) = filter_meta {
            scores.retain(|hit| {
                let meta = self.meta.read();
                if let Some(m) = meta.get(&hit.id) {
                    filters
                        .iter()
                        .all(|(k, v)| m.get(k).map(|mv| mv == v).unwrap_or(false))
                } else {
                    false
                }
            });
        }

        scores.truncate(k);
        Ok(scores)
    }

    /// Attempt ANN search using HNSW; fall back to exact if unavailable.
    pub fn search_ann(&self, query: &[f32], k: usize) -> Result<Vec<VectorSearchResult>> {
        self.search_ann_filtered(query, k, None)
    }

    pub fn search_ann_filtered(
        &self,
        query: &[f32],
        k: usize,
        filter_meta: Option<HashMap<String, String>>,
    ) -> Result<Vec<VectorSearchResult>> {
        let mut qbuf: Vec<f32> = query.to_vec();
        if matches!(self.metric, VectorMetric::Cosine) {
            normalize(&mut qbuf);
        }
        if let Some(ref hnsw) = *self.hnsw.read() {
            let results = hnsw.search(&qbuf, k, self.ef_search.load(Ordering::SeqCst));
            let hits = results
                .iter()
                .filter_map(|r| {
                    let rev = self.rev_map.read();
                    rev.get(r.d_id).copied().map(|uid| VectorSearchResult {
                        id: uid,
                        score: -(r.distance as f32),
                    })
                })
                .filter(|r| !self.tombstones.read().contains_key(&r.id))
                .collect();
            let mut filtered: Vec<_> = match filter_meta {
                None => hits,
                Some(filters) => hits
                    .into_iter()
                    .filter(|hit| {
                        let meta = self.meta.read();
                        if let Some(m) = meta.get(&hit.id) {
                            filters
                                .iter()
                                .all(|(k, v)| m.get(k).map(|mv| mv == v).unwrap_or(false))
                        } else {
                            false
                        }
                    })
                    .collect(),
            };
            filtered.truncate(k);
            return Ok(filtered);
        }
        self.search_filtered(query, k, filter_meta)
    }

    fn maybe_rebuild(&self) {
        let tomb_count = self.tombstones.read().len();
        if tomb_count > (self.max_elements / 10).max(1000) {
            let _ = self.rebuild_hnsw();
            self.tombstones.write().clear();
        }
    }

    /// Rebuild HNSW from current live vectors (drops tombstoned ids).
    pub fn rebuild_hnsw(&self) -> Result<()> {
        if self.dim.read().is_none() {
            return Ok(()); // nothing to rebuild
        }
        let max_layer = 16;
        let hnsw = Hnsw::<f32, DistL2>::new(
            16,
            self.max_elements,
            max_layer,
            self.ef_construction.load(Ordering::SeqCst),
            DistL2 {},
        );
        let mut owned = Vec::new();
        {
            let data = self.inner.read();
            let tomb = self.tombstones.read();
            for (id, vec) in data.iter() {
                if tomb.contains_key(id) {
                    continue;
                }
                // prepare backing
                let boxed: Box<[f32]> = vec.clone().into_boxed_slice();
                let leaked: &'static [f32] = Box::leak(boxed);
                owned.push(leaked);

                // ensure id mappings
                let internal = {
                    let mut map = self.id_map.write();
                    if let Some(existing) = map.get(id) {
                        *existing
                    } else {
                        let new_id = self.next_id.fetch_add(1, Ordering::SeqCst);
                        map.insert(*id, new_id);
                        let mut rev = self.rev_map.write();
                        if rev.len() <= new_id {
                            rev.resize(new_id + 1, Uuid::nil());
                        }
                        rev[new_id] = *id;
                        new_id
                    }
                };
                // insert into new hnsw
                hnsw.insert((leaked, internal));
            }
        }
        // replace owned_store and hnsw atomically
        {
            let mut store = self.owned_store.write();
            *store = owned;
        }
        *self.hnsw.write() = Some(hnsw);
        Ok(())
    }

    /// Persist vectors (ids + optional metadata) to a snapshot file for fast reload.
    pub fn save_snapshot(&self, path: impl AsRef<Path>) -> Result<()> {
        let data: Vec<(Uuid, Vec<f32>, Option<HashMap<String, String>>)> = {
            let guard = self.inner.read();
            let meta = self.meta.read();
            guard
                .iter()
                .map(|(id, v)| (*id, v.clone(), meta.get(id).cloned()))
                .collect()
        };
        let file = File::create(path)?;
        let mut w = BufWriter::new(file);
        bincode::serialize_into(&mut w, &data)?;
        w.flush()?;
        if let Some(f) = w.get_ref().try_clone().ok() {
            f.sync_all()?;
        }
        Ok(())
    }

    /// Load vectors from snapshot, rebuilding in-memory and HNSW state.
    pub fn load_snapshot(&self, path: impl AsRef<Path>) -> Result<()> {
        let bytes = std::fs::read(path)?;
        // Prefer V2 (with metadata); fallback to V1 for backward compatibility.
        let entries_v2: Result<Vec<(Uuid, Vec<f32>, Option<HashMap<String, String>>)>> =
            bincode::deserialize(&bytes).map_err(KaedeDbError::from);
        let entries_v1: Option<Vec<(Uuid, Vec<f32>)>> = if entries_v2.is_err() {
            bincode::deserialize(&bytes).ok()
        } else {
            None
        };

        // Clear existing state.
        {
            self.inner.write().clear();
            self.id_map.write().clear();
            self.rev_map.write().clear();
            self.tombstones.write().clear();
            self.next_id.store(0, Ordering::SeqCst);
            self.owned_store.write().clear();
            *self.hnsw.write() = None;
        }

        if let Ok(entries) = entries_v2 {
            for (id, vec, meta) in entries {
                self.insert(id, vec, meta)?;
            }
        } else if let Some(entries) = entries_v1 {
            for (id, vec) in entries {
                self.insert(id, vec, None)?;
            }
        } else {
            return Err(KaedeDbError::NotFound);
        }
        Ok(())
    }

    /// Persist HNSW graph to files (graph+data) in the given path (acts like a basename).
    pub fn save_hnsw(&self, path: impl AsRef<Path>) -> Result<()> {
        if let Some(ref hnsw) = *self.hnsw.read() {
            let p = path.as_ref();
            let dir = p.parent().unwrap_or_else(|| Path::new("."));
            std::fs::create_dir_all(dir)?;
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("hnsw");
            let _ = hnsw.file_dump(dir, stem);
        }
        Ok(())
    }

    pub fn set_ef_search(&self, ef: usize) {
        self.ef_search.store(ef.max(1), Ordering::SeqCst);
    }

    pub fn set_ef_construction(&self, ef: usize) {
        self.ef_construction.store(ef.max(4), Ordering::SeqCst);
    }
}

fn l2(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| {
            let d = x - y;
            d * d
        })
        .sum()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

fn normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}
