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

    pub fn bfs(&self, start: Uuid, limit: usize) -> Vec<Edge> {
        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        let mut out = Vec::new();
        queue.push_back(start);
        visited.insert(start);
        while let Some(node) = queue.pop_front() {
            for e in self.neighbors(node, limit) {
                if visited.insert(e.dst) {
                    out.push(e.clone());
                    if out.len() >= limit {
                        return out;
                    }
                    queue.push_back(e.dst);
                }
            }
        }
        out
    }

    pub fn dfs(&self, start: Uuid, limit: usize) -> Vec<Edge> {
        let mut visited = std::collections::HashSet::new();
        let mut stack = Vec::new();
        let mut out = Vec::new();
        stack.push(start);
        while let Some(node) = stack.pop() {
            if !visited.insert(node) {
                continue;
            }
            for e in self.neighbors(node, limit).into_iter().rev() {
                out.push(e.clone());
                if out.len() >= limit {
                    return out;
                }
                stack.push(e.dst);
            }
        }
        out
    }
}
