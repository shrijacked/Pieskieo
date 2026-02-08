# PostgreSQL Feature: Full-Text Search (PRODUCTION-GRADE)

**Status**: 🔴 Not Started  
**Priority**: High  
**Dependencies**: GIN indexes  
**Estimated Effort**: 4-5 weeks

---

## Overview

Full-text search with ranking, stemming, and phrase matching. Pieskieo implements BM25 scoring algorithm (superior to PostgreSQL's tf-idf) with multilingual support.

---

## Syntax

```sql
-- Create tsvector column
ALTER TABLE documents ADD COLUMN content_tsv TSVECTOR;

-- Update tsvector from text
UPDATE documents SET content_tsv = to_tsvector('english', content);

-- Create GIN index for fast search
CREATE INDEX idx_content_fts ON documents USING GIN(content_tsv);

-- Search with ranking
SELECT title, ts_rank(content_tsv, query) AS rank
FROM documents,
     to_tsquery('english', 'database & performance') AS query
WHERE content_tsv @@ query
ORDER BY rank DESC
LIMIT 10;

-- Phrase search
SELECT * FROM documents
WHERE content_tsv @@ phraseto_tsquery('english', 'machine learning');

-- Highlight matching terms
SELECT ts_headline('english', content, to_tsquery('database'))
FROM documents
WHERE content_tsv @@ to_tsquery('database');
```

---

## Implementation (BM25 Ranking)

```rust
pub struct BM25Scorer {
    k1: f64,  // Term saturation parameter (default 1.2)
    b: f64,   // Length normalization (default 0.75)
    avgdl: f64, // Average document length
}

impl BM25Scorer {
    pub fn score(
        &self,
        query_terms: &[String],
        document: &Document,
        corpus_stats: &CorpusStatistics,
    ) -> f64 {
        let mut score = 0.0;
        let doc_len = document.term_count() as f64;
        
        for term in query_terms {
            let tf = document.term_frequency(term) as f64;
            let df = corpus_stats.document_frequency(term) as f64;
            let n = corpus_stats.total_documents as f64;
            
            // IDF component
            let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
            
            // TF component with saturation and length normalization
            let tf_component = (tf * (self.k1 + 1.0)) 
                / (tf + self.k1 * (1.0 - self.b + self.b * (doc_len / self.avgdl)));
            
            score += idf * tf_component;
        }
        
        score
    }
}

pub struct FullTextIndex {
    // Term -> PostingList (doc_id, positions)
    inverted_index: HashMap<String, PostingList>,
    
    // Stemmer for language
    stemmer: Stemmer,
    
    // Stop words
    stop_words: HashSet<String>,
}

impl FullTextIndex {
    pub fn search(&self, query: &str, limit: usize) -> Vec<(Uuid, f64)> {
        let query_terms = self.tokenize_and_stem(query);
        
        // Get candidate documents
        let mut candidates = HashMap::new();
        
        for term in &query_terms {
            if let Some(posting_list) = self.inverted_index.get(term) {
                for (doc_id, positions) in &posting_list.entries {
                    *candidates.entry(*doc_id).or_insert(0.0) += 
                        self.bm25_scorer.score_term(term, positions.len());
                }
            }
        }
        
        // Sort by score
        let mut results: Vec<_> = candidates.into_iter().collect();
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        results.truncate(limit);
        
        results
    }
}
```

---

**Created**: 2026-02-08  
**Review Status**: Production-Ready
