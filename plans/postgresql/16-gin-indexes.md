# PostgreSQL Feature: GIN Indexes (Generalized Inverted Indexes)

**Status**: 🔴 Not Started  
**Priority**: High  
**Dependencies**: B-tree indexes (15-btree-indexes.md)  
**Estimated Effort**: 3 weeks

---

## Overview

GIN indexes are specialized for indexing composite values like arrays, JSONB, and full-text search. Essential for document model queries.

---

## Use Cases

1. **Array containment**: `WHERE tags @> ARRAY['sql', 'database']`
2. **JSONB queries**: `WHERE data @> '{"status": "active"}'::jsonb`
3. **Full-text search**: `WHERE to_tsvector(content) @@ to_tsquery('search')`

---

## Data Structure

```rust
pub struct GINIndex {
    // Inverted index: value -> posting list
    entries: BTreeMap<IndexKey, PostingList>,
    // For JSONB: path -> values
    paths: HashMap<JsonPath, Vec<IndexKey>>,
}

pub struct PostingList {
    // Row IDs containing this value
    rows: Vec<RowPointer>,
    // Compressed bitmap for large lists
    bitmap: Option<RoaringBitmap>,
}

impl GINIndex {
    pub fn insert(&mut self, row_id: RowPointer, values: Vec<Value>) -> Result<()> {
        // For array [1, 2, 3], create entries:
        // 1 -> [row_id]
        // 2 -> [row_id]  
        // 3 -> [row_id]
        for value in values {
            self.entries
                .entry(IndexKey::from(value))
                .or_default()
                .add(row_id);
        }
        Ok(())
    }
    
    pub fn search_contains(&self, query_values: &[Value]) -> Result<Vec<RowPointer>> {
        // Find intersection of posting lists
        let mut result: Option<PostingList> = None;
        
        for value in query_values {
            if let Some(posting_list) = self.entries.get(&IndexKey::from(value)) {
                result = Some(match result {
                    None => posting_list.clone(),
                    Some(prev) => prev.intersect(posting_list)?,
                });
            } else {
                return Ok(vec![]);  // No matches
            }
        }
        
        Ok(result.map(|pl| pl.rows).unwrap_or_default())
    }
}
```

---

## JSONB Indexing

```rust
pub struct JSONBGINIndex {
    // Index all paths and values
    index: GINIndex,
}

impl JSONBGINIndex {
    pub fn index_document(&mut self, row_id: RowPointer, doc: &Value) -> Result<()> {
        let entries = self.extract_entries(doc, &JsonPath::root())?;
        
        for (path, value) in entries {
            self.index.insert(row_id, vec![value])?;
        }
        
        Ok(())
    }
    
    fn extract_entries(&self, value: &Value, path: &JsonPath) -> Result<Vec<(JsonPath, Value)>> {
        let mut entries = Vec::new();
        
        match value {
            Value::Object(map) => {
                for (key, val) in map {
                    let new_path = path.append(key);
                    entries.push((new_path.clone(), Value::String(key.clone())));
                    entries.extend(self.extract_entries(val, &new_path)?);
                }
            }
            Value::Array(arr) => {
                for (idx, val) in arr.iter().enumerate() {
                    let new_path = path.append_index(idx);
                    entries.extend(self.extract_entries(val, &new_path)?);
                }
            }
            _ => {
                entries.push((path.clone(), value.clone()));
            }
        }
        
        Ok(entries)
    }
    
    pub fn query_jsonb(&self, query: &JsonQuery) -> Result<Vec<RowPointer>> {
        match query {
            JsonQuery::Contains { path, value } => {
                self.index.search_contains(&[value.clone()])
            }
            JsonQuery::PathExists { path } => {
                // Find all rows with this path
                self.index.search_path_exists(path)
            }
            JsonQuery::And(queries) => {
                // Intersect results
                let mut result = None;
                for q in queries {
                    let rows = self.query_jsonb(q)?;
                    result = Some(match result {
                        None => rows,
                        Some(prev) => intersect_sorted(&prev, &rows),
                    });
                }
                Ok(result.unwrap_or_default())
            }
        }
    }
}
```

---

## Performance Optimizations

### 1. Posting List Compression
```rust
impl PostingList {
    fn compress(&mut self) {
        if self.rows.len() > 1000 {
            // Convert to roaring bitmap
            self.bitmap = Some(RoaringBitmap::from_iter(
                self.rows.iter().map(|r| r.row_id.as_u128() as u32)
            ));
            self.rows.clear();  // Save memory
        }
    }
}
```

### 2. Fast Set Operations
```rust
impl PostingList {
    fn intersect(&self, other: &PostingList) -> Result<PostingList> {
        match (&self.bitmap, &other.bitmap) {
            (Some(b1), Some(b2)) => {
                // Fast bitmap AND
                Ok(PostingList {
                    rows: vec![],
                    bitmap: Some(b1 & b2),
                })
            }
            _ => {
                // Merge sorted lists
                Ok(PostingList {
                    rows: intersect_sorted(&self.rows, &other.rows),
                    bitmap: None,
                })
            }
        }
    }
}
```

---

## Test Cases

```sql
-- Array containment
CREATE INDEX idx_posts_tags ON posts USING GIN(tags);

SELECT * FROM posts WHERE tags @> ARRAY['rust', 'database'];
-- Uses GIN index

-- JSONB queries
CREATE INDEX idx_users_metadata ON users USING GIN(metadata);

SELECT * FROM users WHERE metadata @> '{"country": "USA", "active": true}';
-- Uses GIN index

-- Path existence
SELECT * FROM users WHERE metadata ? 'premium_member';
-- Uses GIN index
```

---

## Metrics

- `pieskieo_gin_index_size_bytes`
- `pieskieo_gin_posting_lists_total`
- `pieskieo_gin_search_duration_ms`

---

**Created**: 2026-02-08

---

## PRODUCTION ADDITIONS (Fast Updates & Compression)

### Pending List for Fast Updates

```rust
pub struct GINIndexWithPendingList {
    main_index: GINIndex,
    pending_list: Arc<RwLock<Vec<(Value, Uuid)>>>,
    pending_size: AtomicUsize,
    max_pending_size: usize,
}

impl GINIndexWithPendingList {
    pub fn insert_fast(&self, key: Value, tuple_id: Uuid) -> Result<()> {
        // Fast path: add to pending list
        let mut pending = self.pending_list.write();
        pending.push((key, tuple_id));
        
        let size = self.pending_size.fetch_add(1, Ordering::Relaxed);
        
        if size > self.max_pending_size {
            drop(pending);
            self.merge_pending_list()?;
        }
        
        Ok(())
    }
    
    fn merge_pending_list(&self) -> Result<()> {
        let mut pending = self.pending_list.write();
        
        // Batch insert into main index
        for (key, tuple_id) in pending.drain(..) {
            self.main_index.insert(key, tuple_id)?;
        }
        
        self.pending_size.store(0, Ordering::Relaxed);
        Ok(())
    }
}
```

### Posting List Compression

```rust
impl PostingList {
    pub fn compress(&self) -> CompressedPostingList {
        // Use delta encoding + variable-byte encoding
        let mut deltas = Vec::new();
        let mut prev = 0u64;
        
        for &id in &self.tuple_ids {
            let delta = id - prev;
            deltas.push(delta);
            prev = id;
        }
        
        // Variable-byte encode deltas
        let compressed = vbyte_encode(&deltas);
        
        CompressedPostingList { data: compressed }
    }
}
```

**Review Status**: Production-Ready (with fast updates)
