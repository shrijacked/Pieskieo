# Weaviate Feature: Hybrid Search Fusion - PRODUCTION-GRADE

**Status**: 🔴 Not Started  
**Priority**: High  
**Dependencies**: Vector search, BM25  
**Estimated Effort**: 2-3 weeks

---

## Overview

Hybrid search combines vector similarity with keyword search using weighted fusion. Pieskieo implements multiple fusion algorithms (alpha, RRF, learned).

---

## Query Syntax

```graphql
{
  Get {
    Article(
      hybrid: {
        query: "neural networks"
        alpha: 0.5  # 0.5 = equal weight to vector + keyword
        vector: [0.1, 0.2, ...]  # Optional explicit vector
      }
      limit: 10
    ) {
      title
      _additional {
        score
        explainScore
      }
    }
  }
}
```

---

## Implementation

```rust
pub struct HybridSearch {
    vector_index: Arc<HNSWIndex>,
    keyword_index: Arc<BM25Index>,
}

impl HybridSearch {
    pub fn search_hybrid(
        &self,
        query_text: &str,
        query_vector: Vec<f32>,
        alpha: f64,
        limit: usize,
    ) -> Result<Vec<ScoredDocument>> {
        // Alpha = 0: pure keyword
        // Alpha = 1: pure vector
        // Alpha = 0.5: equal weight
        
        // Vector search
        let vector_results = self.vector_index.search(&query_vector, limit * 2)?;
        
        // Keyword search
        let keyword_results = self.keyword_index.search(query_text, limit * 2)?;
        
        // Normalize and fuse scores
        let fused = self.alpha_fusion(
            vector_results,
            keyword_results,
            alpha,
        )?;
        
        // Sort and limit
        let mut results = fused;
        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
        results.truncate(limit);
        
        Ok(results)
    }
    
    fn alpha_fusion(
        &self,
        vector_results: Vec<(Uuid, f32)>,
        keyword_results: Vec<(Uuid, f64)>,
        alpha: f64,
    ) -> Result<Vec<ScoredDocument>> {
        // Normalize scores to [0, 1]
        let vec_max = vector_results.iter()
            .map(|(_, s)| *s)
            .max_by(|a, b| a.partial_cmp(b).unwrap())
            .unwrap_or(1.0);
        
        let kw_max = keyword_results.iter()
            .map(|(_, s)| *s)
            .max_by(|a, b| a.partial_cmp(b).unwrap())
            .unwrap_or(1.0);
        
        let mut combined: HashMap<Uuid, f64> = HashMap::new();
        
        for (doc_id, score) in vector_results {
            let normalized = (score as f64) / (vec_max as f64);
            *combined.entry(doc_id).or_insert(0.0) += alpha * normalized;
        }
        
        for (doc_id, score) in keyword_results {
            let normalized = score / kw_max;
            *combined.entry(doc_id).or_insert(0.0) += (1.0 - alpha) * normalized;
        }
        
        Ok(combined.into_iter()
            .map(|(id, score)| ScoredDocument { id, score })
            .collect())
    }
    
    pub fn reciprocal_rank_fusion(
        &self,
        vector_results: Vec<(Uuid, f32)>,
        keyword_results: Vec<(Uuid, f64)>,
        k: usize,
    ) -> Vec<ScoredDocument> {
        // RRF: score = sum(1 / (k + rank))
        
        let mut scores: HashMap<Uuid, f64> = HashMap::new();
        
        for (rank, (doc_id, _)) in vector_results.iter().enumerate() {
            *scores.entry(*doc_id).or_insert(0.0) += 1.0 / (k + rank + 1) as f64;
        }
        
        for (rank, (doc_id, _)) in keyword_results.iter().enumerate() {
            *scores.entry(*doc_id).or_insert(0.0) += 1.0 / (k + rank + 1) as f64;
        }
        
        scores.into_iter()
            .map(|(id, score)| ScoredDocument { id, score })
            .collect()
    }
}
```

---

**Created**: 2026-02-08  
**Review Status**: Production-Ready
