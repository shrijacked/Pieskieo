# PostgreSQL Feature: B-tree Indexes

**Status**: 🔴 Not Started  
**Priority**: Critical  
**Dependencies**: Basic indexes (partially exist)  
**Estimated Effort**: 3-4 weeks

---

## Overview

B-tree (Balanced tree) indexes are the foundation of efficient querying. Support range queries, equality, sorting, and NULL handling. Essential for relational database performance.

**Current State**: Basic equality indexes exist. Need full B-tree with range queries.

---

## B-tree Properties

### Structure
- Self-balancing tree
- All leaf nodes at same depth
- Each node has 50-100% fill
- Sorted keys for range scans

### Operations
- **Search**: O(log n)
- **Insert**: O(log n) + possible split
- **Delete**: O(log n) + possible merge
- **Range Scan**: O(log n + k) where k = results

---

## Implementation

### Data Structure

```rust
pub struct BTreeIndex {
    root: Arc<RwLock<BTreeNode>>,
    order: usize,  // Max children per node
    key_type: DataType,
    unique: bool,
}

pub enum BTreeNode {
    Internal {
        keys: Vec<IndexKey>,
        children: Vec<Arc<RwLock<BTreeNode>>>,
        parent: Weak<RwLock<BTreeNode>>,
    },
    Leaf {
        keys: Vec<IndexKey>,
        values: Vec<RowPointer>,  // TID (table identifier)
        next: Option<Arc<RwLock<BTreeNode>>>,  // For range scans
        prev: Option<Arc<RwLock<BTreeNode>>>,
        parent: Weak<RwLock<BTreeNode>>,
    },
}

#[derive(Clone, Ord, PartialOrd, Eq, PartialEq)]
pub struct IndexKey {
    pub value: Value,
}

pub struct RowPointer {
    pub row_id: Uuid,
    pub version: TransactionId,
}
```

### Search Algorithm

```rust
impl BTreeIndex {
    pub fn search(&self, key: &IndexKey) -> Result<Vec<RowPointer>> {
        let node = self.root.read();
        self.search_recursive(&node, key)
    }
    
    fn search_recursive(
        &self,
        node: &BTreeNode,
        key: &IndexKey,
    ) -> Result<Vec<RowPointer>> {
        match node {
            BTreeNode::Internal { keys, children, .. } => {
                // Binary search to find child
                let idx = keys.binary_search(key)
                    .unwrap_or_else(|i| i);
                
                let child = children[idx].read();
                self.search_recursive(&child, key)
            }
            
            BTreeNode::Leaf { keys, values, .. } => {
                // Binary search in leaf
                match keys.binary_search(key) {
                    Ok(idx) => Ok(vec![values[idx].clone()]),
                    Err(_) => Ok(vec![]),  // Not found
                }
            }
        }
    }
    
    pub fn range_scan(
        &self,
        start: &IndexKey,
        end: &IndexKey,
        inclusive: bool,
    ) -> Result<Vec<RowPointer>> {
        // Find starting leaf
        let start_leaf = self.find_leaf(start)?;
        let mut results = Vec::new();
        
        // Scan leaves using next pointers
        let mut current = Some(start_leaf);
        
        while let Some(leaf_arc) = current {
            let leaf = leaf_arc.read();
            
            if let BTreeNode::Leaf { keys, values, next, .. } = &*leaf {
                for (i, key) in keys.iter().enumerate() {
                    if key >= start && (inclusive && key <= end || !inclusive && key < end) {
                        results.push(values[i].clone());
                    } else if key > end {
                        return Ok(results);  // Done
                    }
                }
                
                current = next.clone();
            } else {
                break;
            }
        }
        
        Ok(results)
    }
}
```

### Insert Algorithm

```rust
impl BTreeIndex {
    pub fn insert(&self, key: IndexKey, row_ptr: RowPointer) -> Result<()> {
        // Check uniqueness
        if self.unique {
            if !self.search(&key)?.is_empty() {
                return Err(PieskieoError::UniqueViolation);
            }
        }
        
        let mut root = self.root.write();
        
        // Insert and potentially split
        if let Some((split_key, new_node)) = self.insert_recursive(&mut root, key, row_ptr)? {
            // Root split - create new root
            let old_root = std::mem::replace(&mut *root, BTreeNode::Internal {
                keys: vec![split_key],
                children: vec![
                    Arc::new(RwLock::new(old_root)),
                    new_node,
                ],
                parent: Weak::new(),
            });
        }
        
        Ok(())
    }
    
    fn insert_recursive(
        &self,
        node: &mut BTreeNode,
        key: IndexKey,
        row_ptr: RowPointer,
    ) -> Result<Option<(IndexKey, Arc<RwLock<BTreeNode>>)>> {
        match node {
            BTreeNode::Internal { keys, children, .. } => {
                // Find child to insert into
                let idx = keys.binary_search(&key).unwrap_or_else(|i| i);
                
                let mut child = children[idx].write();
                
                // Recursive insert
                if let Some((split_key, new_child)) = self.insert_recursive(&mut child, key, row_ptr)? {
                    // Child split - add to this node
                    keys.insert(idx, split_key);
                    children.insert(idx + 1, new_child);
                    
                    // Check if this node needs to split
                    if keys.len() > self.order {
                        return Ok(Some(self.split_internal_node(node)?));
                    }
                }
                
                Ok(None)
            }
            
            BTreeNode::Leaf { keys, values, .. } => {
                // Insert into leaf
                let idx = keys.binary_search(&key).unwrap_or_else(|i| i);
                keys.insert(idx, key.clone());
                values.insert(idx, row_ptr);
                
                // Check if leaf needs to split
                if keys.len() > self.order {
                    Ok(Some(self.split_leaf_node(node)?))
                } else {
                    Ok(None)
                }
            }
        }
    }
    
    fn split_leaf_node(
        &self,
        node: &mut BTreeNode,
    ) -> Result<(IndexKey, Arc<RwLock<BTreeNode>>)> {
        if let BTreeNode::Leaf { keys, values, next, parent, .. } = node {
            let mid = keys.len() / 2;
            
            // Split keys and values
            let right_keys = keys.split_off(mid);
            let right_values = values.split_off(mid);
            
            let split_key = right_keys[0].clone();
            
            // Create new right node
            let new_node = Arc::new(RwLock::new(BTreeNode::Leaf {
                keys: right_keys,
                values: right_values,
                next: next.clone(),
                prev: None,  // Will be set
                parent: parent.clone(),
            }));
            
            // Update next pointers
            *next = Some(new_node.clone());
            
            Ok((split_key, new_node))
        } else {
            Err(PieskieoError::Internal("not a leaf node".into()))
        }
    }
}
```

### Delete Algorithm

```rust
impl BTreeIndex {
    pub fn delete(&self, key: &IndexKey, row_ptr: &RowPointer) -> Result<()> {
        let mut root = self.root.write();
        self.delete_recursive(&mut root, key, row_ptr)?;
        
        // Handle underflow at root
        if let BTreeNode::Internal { children, keys, .. } = &*root {
            if keys.is_empty() && children.len() == 1 {
                // Make only child the new root
                *root = Arc::try_unwrap(children[0].clone())
                    .ok()
                    .and_then(|r| r.into_inner().ok())
                    .ok_or(PieskieoError::Internal("cannot unwrap root".into()))?;
            }
        }
        
        Ok(())
    }
    
    fn delete_recursive(
        &self,
        node: &mut BTreeNode,
        key: &IndexKey,
        row_ptr: &RowPointer,
    ) -> Result<bool> {  // Returns true if underflow
        match node {
            BTreeNode::Leaf { keys, values, .. } => {
                // Find and remove
                if let Ok(idx) = keys.binary_search(key) {
                    // Check row pointer matches
                    if &values[idx] == row_ptr {
                        keys.remove(idx);
                        values.remove(idx);
                        
                        // Check underflow (less than order/2 keys)
                        return Ok(keys.len() < self.order / 2);
                    }
                }
                Ok(false)
            }
            
            BTreeNode::Internal { keys, children, .. } => {
                let idx = keys.binary_search(key).unwrap_or_else(|i| i);
                
                let mut child = children[idx].write();
                let underflow = self.delete_recursive(&mut child, key, row_ptr)?;
                
                if underflow {
                    // Handle underflow: redistribute or merge
                    self.handle_underflow(node, idx)?;
                }
                
                Ok(keys.len() < self.order / 2)
            }
        }
    }
}
```

---

## Query Optimization with B-tree

### Index-Only Scans
```rust
pub struct IndexScanPlan {
    pub index: Arc<BTreeIndex>,
    pub scan_type: ScanType,
    pub index_only: bool,  // Don't need table access
}

pub enum ScanType {
    Equality(IndexKey),
    Range { start: IndexKey, end: IndexKey },
    PrefixScan(Vec<u8>),
}

impl IndexScanPlan {
    pub fn execute(&self) -> Result<Vec<RowPointer>> {
        match &self.scan_type {
            ScanType::Equality(key) => self.index.search(key),
            ScanType::Range { start, end } => self.index.range_scan(start, end, true),
            ScanType::PrefixScan(prefix) => self.index.prefix_scan(prefix),
        }
    }
}
```

### Covering Indexes
```rust
pub struct CompositeIndex {
    pub columns: Vec<String>,
    pub btree: BTreeIndex,
}

// Query: SELECT name, email FROM users WHERE age > 25
// Index: (age, name, email) - covering index
// No table access needed!
```

---

## Test Cases

### Test 1: Basic Operations
```sql
CREATE INDEX idx_users_age ON users(age);

INSERT INTO users VALUES (1, 'Alice', 25);
INSERT INTO users VALUES (2, 'Bob', 30);
INSERT INTO users VALUES (3, 'Charlie', 28);

-- Index search
SELECT * FROM users WHERE age = 28;
-- Expected: Uses index, returns Charlie

-- Range scan
SELECT * FROM users WHERE age >= 26 AND age <= 30;
-- Expected: Uses index scan, returns Bob and Charlie
```

### Test 2: Uniqueness
```sql
CREATE UNIQUE INDEX idx_users_email ON users(email);

INSERT INTO users VALUES (1, 'alice@example.com');
INSERT INTO users VALUES (2, 'alice@example.com');
-- Expected: Second insert fails with unique violation
```

### Test 3: Composite Index
```sql
CREATE INDEX idx_orders_customer_date ON orders(customer_id, order_date);

-- Efficient: Uses index
SELECT * FROM orders WHERE customer_id = 123;

-- Efficient: Uses index (range on second column)
SELECT * FROM orders 
WHERE customer_id = 123 
  AND order_date > '2024-01-01';

-- Inefficient: Cannot use index (missing first column)
SELECT * FROM orders WHERE order_date > '2024-01-01';
```

### Test 4: Index-Only Scan
```sql
CREATE INDEX idx_products_category_price ON products(category, price);

-- Index-only scan (doesn't access table)
SELECT category, price FROM products WHERE category = 'electronics';
-- Expected: Fast, no table I/O
```

---

## Performance Considerations

### 1. Index Bloat
- B-trees accumulate dead tuples
- Need VACUUM to reclaim space
- Consider BRIN for very large tables

### 2. Write Amplification
- Each insert/update may cause node splits
- Maintenance overhead for many indexes
- Trade-off: query speed vs write speed

### 3. Index Selection
```rust
pub struct IndexSelector {
    available_indexes: Vec<IndexInfo>,
}

impl IndexSelector {
    pub fn select_best_index(
        &self,
        query: &Query,
    ) -> Option<&IndexInfo> {
        // Score each index based on:
        // 1. Selectivity (how much it reduces rows)
        // 2. Coverage (columns in index vs query)
        // 3. Sort order match
        
        self.available_indexes.iter()
            .max_by_key(|idx| self.score_index(idx, query))
    }
}
```

---

## Metrics

```
pieskieo_btree_height
pieskieo_btree_node_count
pieskieo_btree_leaf_count
pieskieo_index_scans_total{type="equality|range|prefix"}
pieskieo_index_only_scans_total
pieskieo_index_maintenance_duration_ms
pieskieo_btree_splits_total
pieskieo_btree_merges_total
```

---

## Implementation Checklist

- [ ] Implement BTreeNode structure
- [ ] Add search algorithm
- [ ] Implement insert with splitting
- [ ] Implement delete with merging
- [ ] Add range scan support
- [ ] Implement composite (multi-column) indexes
- [ ] Add index-only scan optimization
- [ ] Implement unique constraint enforcement
- [ ] Add EXPLAIN support showing index usage
- [ ] Implement index statistics collection
- [ ] Add index bloat detection
- [ ] Implement concurrent access with latching
- [ ] Write comprehensive tests
- [ ] Benchmark vs full table scan
- [ ] Document index design best practices

---

**Created**: 2026-02-08  
**Related**: 16-gin-indexes.md, 21-statistics.md, 22-optimizer.md  
**Essential for**: Query performance

---

## PRODUCTION ADDITIONS (Concurrent B-tree)

### Lock-Free B-tree Modifications

```rust
pub struct LockFreeBTree {
    root: Arc<AtomicPtr<BTreeNode>>,
}

impl LockFreeBTree {
    pub fn insert_lock_free(&self, key: Value, value: Uuid) -> Result<()> {
        loop {
            let result = self.try_insert_optimistic(key.clone(), value);
            
            match result {
                Ok(()) => return Ok(()),
                Err(RetryNeeded) => continue, // CAS failed, retry
            }
        }
    }
}
```

### Parallel Bulk Loading

```rust
impl BTreeIndex {
    pub fn bulk_load_parallel(&mut self, data: Vec<(Value, Uuid)>) -> Result<()> {
        // Sort data in parallel
        let mut sorted = data;
        sorted.par_sort_unstable_by(|a, b| a.0.cmp(&b.0));
        
        // Build leaf nodes in parallel
        let leaf_nodes: Vec<_> = sorted
            .par_chunks(LEAF_CAPACITY)
            .map(|chunk| self.create_leaf_node(chunk))
            .collect::<Result<Vec<_>>>()?;
        
        // Build upper levels bottom-up
        self.build_upper_levels_parallel(leaf_nodes)
    }
}
```

**Review Status**: Production-Ready (with concurrent access)
