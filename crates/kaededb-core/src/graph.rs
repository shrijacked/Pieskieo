use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub src: Uuid,
    pub dst: Uuid,
    pub weight: f32,
}

#[derive(Default, Clone)]
pub struct GraphStore {
    adj: Arc<RwLock<HashMap<Uuid, Vec<Edge>>>>,
}

impl GraphStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_edge(&self, src: Uuid, dst: Uuid, weight: f32) {
        let mut adj = self.adj.write();
        let entry = adj.entry(src).or_insert_with(Vec::new);
        if let Some(existing) = entry.iter_mut().find(|e| e.dst == dst) {
            existing.weight = weight;
        } else {
            entry.push(Edge { src, dst, weight });
        }
    }

    pub fn neighbors(&self, id: Uuid, limit: usize) -> Vec<Edge> {
        let adj = self.adj.read();
        adj.get(&id)
            .map(|edges| edges.iter().take(limit).cloned().collect())
            .unwrap_or_default()
    }
}
