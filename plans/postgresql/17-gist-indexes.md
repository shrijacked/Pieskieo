# PostgreSQL Feature: GiST Indexes (Generalized Search Tree)

**Status**: 🔴 Not Started  
**Priority**: Medium  
**Dependencies**: 15-btree-indexes.md  
**Estimated Effort**: 4-5 weeks

---

## Overview

GiST (Generalized Search Tree) is a balanced tree index structure that supports various data types and operators beyond simple equality/comparison. It's especially powerful for:
- **Geospatial data** (points, polygons, spatial queries)
- **Range types** (overlaps, contains)
- **Full-text search** (tsvector)
- **Custom data types** with user-defined operators

Unlike B-tree (good for `=`, `<`, `>`) or GIN (good for `@>`, `&&`), GiST is **extensible** - you can plug in custom logic for any data type.

---

## Use Cases

### 1. Geospatial Queries
```sql
CREATE TABLE places (
    id UUID PRIMARY KEY,
    name TEXT,
    location POINT
);

CREATE INDEX idx_places_location ON places USING GIST (location);

-- Find all places within 10km of a point
SELECT * FROM places 
WHERE location <-> POINT(37.7749, -122.4194) < 10000;
```

### 2. Range Overlaps
```sql
CREATE TABLE events (
    id UUID PRIMARY KEY,
    name TEXT,
    time_range TSTZRANGE
);

CREATE INDEX idx_events_time ON events USING GIST (time_range);

-- Find events overlapping with a time range
SELECT * FROM events 
WHERE time_range && '[2024-01-01, 2024-01-31]'::TSTZRANGE;
```

### 3. Geometric Shapes
```sql
CREATE TABLE regions (
    id UUID PRIMARY KEY,
    name TEXT,
    boundary POLYGON
);

CREATE INDEX idx_regions_boundary ON regions USING GIST (boundary);

-- Find regions containing a point
SELECT * FROM regions 
WHERE boundary @> POINT(40.7128, -74.0060);
```

---

## Implementation Plan

### Phase 1: GiST Tree Structure

**File**: `crates/pieskieo-core/src/index/gist.rs`

```rust
use serde::{Deserialize, Serialize};

/// GiST node (can be internal or leaf)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GistNode {
    pub id: Uuid,
    pub level: u32,           // 0 = leaf, >0 = internal
    pub entries: Vec<GistEntry>,
    pub parent: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GistEntry {
    pub predicate: Predicate,  // "Key" - covers all children
    pub child_ptr: GistPointer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GistPointer {
    Node(Uuid),              // Points to another GistNode
    Tuple(Uuid),             // Points to actual data row
}

/// The "predicate" is the bounding box / covering region
/// For geo: MBR (Minimum Bounding Rectangle)
/// For ranges: the union of all child ranges
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Predicate {
    BoundingBox {
        min_x: f64,
        min_y: f64,
        max_x: f64,
        max_y: f64,
    },
    TimeRange {
        start: SystemTime,
        end: SystemTime,
    },
    Custom(serde_json::Value),
}

pub struct GistIndex {
    pub name: String,
    pub table: String,
    pub column: String,
    pub root: Uuid,
    
    // GiST is extensible - different operators for different types
    pub operator_class: OperatorClass,
}

#[derive(Debug, Clone)]
pub enum OperatorClass {
    Point2D,       // <->, &&, @>, <@, etc.
    Box2D,
    Range,         // &&, @>, <@, <<, >>, etc.
    FullText,
}
```

### Phase 2: GiST Operations

**Key GiST Methods** (all pluggable based on operator class):

```rust
pub trait GistOperator {
    /// Compute the predicate that covers all given predicates
    fn union(&self, predicates: &[Predicate]) -> Predicate;
    
    /// Check if predicate p1 contains p2
    fn contains(&self, p1: &Predicate, p2: &Predicate) -> bool;
    
    /// Check if two predicates overlap
    fn overlaps(&self, p1: &Predicate, p2: &Predicate) -> bool;
    
    /// "Penalty" for inserting new_pred into a node with existing pred
    /// Used to choose which subtree to insert into
    fn penalty(&self, existing: &Predicate, new_pred: &Predicate) -> f64;
    
    /// Split a node into two when it overflows
    fn pick_split(&self, entries: &[GistEntry]) -> (Vec<GistEntry>, Vec<GistEntry>);
}

impl GistIndex {
    pub fn insert(&mut self, value: &serde_json::Value, tuple_id: Uuid) -> Result<()> {
        let predicate = self.value_to_predicate(value)?;
        
        // Find leaf node to insert into
        let leaf_id = self.choose_subtree(self.root, &predicate, 0)?;
        
        // Insert into leaf
        let mut leaf = self.get_node(leaf_id)?;
        leaf.entries.push(GistEntry {
            predicate: predicate.clone(),
            child_ptr: GistPointer::Tuple(tuple_id),
        });
        
        // Check if node overflow
        if leaf.entries.len() > self.max_entries_per_node() {
            self.split_node(leaf_id)?;
        }
        
        // Update parent predicates (may need to expand bounding boxes)
        self.adjust_keys(leaf_id)?;
        
        Ok(())
    }
    
    fn choose_subtree(
        &self,
        node_id: Uuid,
        predicate: &Predicate,
        target_level: u32,
    ) -> Result<Uuid> {
        let node = self.get_node(node_id)?;
        
        if node.level == target_level {
            return Ok(node_id);
        }
        
        // Choose entry with minimum penalty
        let best_entry = node.entries.iter()
            .min_by(|a, b| {
                let penalty_a = self.operator.penalty(&a.predicate, predicate);
                let penalty_b = self.operator.penalty(&b.predicate, predicate);
                penalty_a.partial_cmp(&penalty_b).unwrap()
            })
            .ok_or_else(|| PieskieoError::Internal("empty node".into()))?;
        
        match best_entry.child_ptr {
            GistPointer::Node(child_id) => {
                self.choose_subtree(child_id, predicate, target_level)
            }
            GistPointer::Tuple(_) => {
                Err(PieskieoError::Internal("unexpected tuple pointer".into()))
            }
        }
    }
    
    fn split_node(&mut self, node_id: Uuid) -> Result<()> {
        let node = self.get_node(node_id)?;
        
        // Use operator's pick_split to partition entries
        let (left_entries, right_entries) = self.operator.pick_split(&node.entries);
        
        // Create new sibling node
        let new_node = GistNode {
            id: Uuid::new_v4(),
            level: node.level,
            entries: right_entries.clone(),
            parent: node.parent,
        };
        
        // Update original node
        let mut updated_node = node.clone();
        updated_node.entries = left_entries.clone();
        
        // Update parent to point to both nodes
        if let Some(parent_id) = node.parent {
            self.update_parent_after_split(parent_id, node_id, new_node.id, &left_entries, &right_entries)?;
        } else {
            // Split root - create new root
            self.create_new_root(node_id, new_node.id, &left_entries, &right_entries)?;
        }
        
        self.save_node(&updated_node)?;
        self.save_node(&new_node)?;
        
        Ok(())
    }
    
    fn adjust_keys(&mut self, node_id: Uuid) -> Result<()> {
        let node = self.get_node(node_id)?;
        
        // Compute union of all predicates in this node
        let predicates: Vec<_> = node.entries.iter().map(|e| e.predicate.clone()).collect();
        let union_predicate = self.operator.union(&predicates);
        
        // Update parent's entry for this node
        if let Some(parent_id) = node.parent {
            let mut parent = self.get_node(parent_id)?;
            
            if let Some(entry) = parent.entries.iter_mut().find(|e| {
                matches!(e.child_ptr, GistPointer::Node(id) if id == node_id)
            }) {
                entry.predicate = union_predicate;
            }
            
            self.save_node(&parent)?;
            
            // Recursively adjust parent
            self.adjust_keys(parent_id)?;
        }
        
        Ok(())
    }
    
    pub fn search(&self, query: &GistQuery) -> Result<Vec<Uuid>> {
        let mut results = Vec::new();
        self.search_recursive(self.root, query, &mut results)?;
        Ok(results)
    }
    
    fn search_recursive(
        &self,
        node_id: Uuid,
        query: &GistQuery,
        results: &mut Vec<Uuid>,
    ) -> Result<()> {
        let node = self.get_node(node_id)?;
        
        for entry in &node.entries {
            // Check if entry matches query
            if !self.predicate_matches(&entry.predicate, query) {
                continue;
            }
            
            match &entry.child_ptr {
                GistPointer::Node(child_id) => {
                    // Recurse into child
                    self.search_recursive(*child_id, query, results)?;
                }
                GistPointer::Tuple(tuple_id) => {
                    // Found matching tuple
                    results.push(*tuple_id);
                }
            }
        }
        
        Ok(())
    }
    
    fn predicate_matches(&self, predicate: &Predicate, query: &GistQuery) -> bool {
        match query.operator {
            GistOperator::Overlaps => self.operator.overlaps(predicate, &query.value),
            GistOperator::Contains => self.operator.contains(predicate, &query.value),
            GistOperator::Within => self.operator.contains(&query.value, predicate),
            GistOperator::Distance => {
                // For k-NN searches
                self.operator.distance(predicate, &query.value) <= query.distance_threshold
            }
        }
    }
}
```

### Phase 3: Point2D Operator Class

```rust
pub struct Point2DOperator;

impl GistOperator for Point2DOperator {
    fn union(&self, predicates: &[Predicate]) -> Predicate {
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        
        for pred in predicates {
            if let Predicate::BoundingBox { min_x: mx1, min_y: my1, max_x: mx2, max_y: my2 } = pred {
                min_x = min_x.min(*mx1);
                min_y = min_y.min(*my1);
                max_x = max_x.max(*mx2);
                max_y = max_y.max(*my2);
            }
        }
        
        Predicate::BoundingBox { min_x, min_y, max_x, max_y }
    }
    
    fn overlaps(&self, p1: &Predicate, p2: &Predicate) -> bool {
        match (p1, p2) {
            (
                Predicate::BoundingBox { min_x: x1, min_y: y1, max_x: x2, max_y: y2 },
                Predicate::BoundingBox { min_x: x3, min_y: y3, max_x: x4, max_y: y4 },
            ) => {
                !(x2 < x3 || x4 < x1 || y2 < y3 || y4 < y1)
            }
            _ => false,
        }
    }
    
    fn contains(&self, p1: &Predicate, p2: &Predicate) -> bool {
        match (p1, p2) {
            (
                Predicate::BoundingBox { min_x: x1, min_y: y1, max_x: x2, max_y: y2 },
                Predicate::BoundingBox { min_x: x3, min_y: y3, max_x: x4, max_y: y4 },
            ) => {
                x1 <= x3 && x2 >= x4 && y1 <= y3 && y2 >= y4
            }
            _ => false,
        }
    }
    
    fn penalty(&self, existing: &Predicate, new_pred: &Predicate) -> f64 {
        // Penalty = increase in bounding box area
        let original_area = self.area(existing);
        let expanded = self.union(&[existing.clone(), new_pred.clone()]);
        let new_area = self.area(&expanded);
        
        new_area - original_area
    }
    
    fn pick_split(&self, entries: &[GistEntry]) -> (Vec<GistEntry>, Vec<GistEntry>) {
        // Use quadratic split algorithm
        // 1. Find two entries with max "waste" (area of union - sum of areas)
        // 2. Assign remaining entries to group with min penalty
        
        let (seed1, seed2) = self.pick_seeds(entries);
        
        let mut group1 = vec![entries[seed1].clone()];
        let mut group2 = vec![entries[seed2].clone()];
        
        for (i, entry) in entries.iter().enumerate() {
            if i == seed1 || i == seed2 {
                continue;
            }
            
            let penalty1 = self.penalty(&self.union_entries(&group1), &entry.predicate);
            let penalty2 = self.penalty(&self.union_entries(&group2), &entry.predicate);
            
            if penalty1 < penalty2 {
                group1.push(entry.clone());
            } else {
                group2.push(entry.clone());
            }
        }
        
        (group1, group2)
    }
}
```

### Phase 4: Query Integration

**File**: `crates/pieskieo-core/src/engine.rs`

```rust
impl PieskieoDb {
    pub fn execute_gist_query(&self, query: &SelectStatement) -> Result<SqlResult> {
        // Parse spatial operators
        // WHERE location <-> POINT(x, y) < distance
        // WHERE boundary @> POINT(x, y)
        // WHERE time_range && '[start, end]'::TSTZRANGE
        
        let gist_condition = self.extract_gist_condition(&query.where_clause)?;
        
        if let Some(index) = self.find_gist_index(&query.table, &gist_condition.column)? {
            // Use GiST index
            let tuple_ids = index.search(&gist_condition.query)?;
            
            // Fetch tuples
            let mut results = Vec::new();
            for id in tuple_ids {
                if let Some(record) = self.get_record_by_id(id)? {
                    results.push(record);
                }
            }
            
            Ok(SqlResult { rows: results })
        } else {
            // Fall back to sequential scan
            self.execute_sequential_scan(query)
        }
    }
}
```

---

## Test Cases

### Test 1: Point Distance Query
```sql
CREATE TABLE restaurants (
    id UUID PRIMARY KEY,
    name TEXT,
    location POINT
);

CREATE INDEX idx_location ON restaurants USING GIST (location);

INSERT INTO restaurants VALUES
    ('r1', 'Pizza Place', POINT(37.7749, -122.4194)),
    ('r2', 'Burger Joint', POINT(37.7849, -122.4094)),
    ('r3', 'Sushi Bar', POINT(37.7649, -122.4294));

-- Find restaurants within 5km of a point
SELECT name, location <-> POINT(37.78, -122.42) AS distance_meters
FROM restaurants
WHERE location <-> POINT(37.78, -122.42) < 5000
ORDER BY distance_meters;
```

### Test 2: Bounding Box Search
```sql
CREATE TABLE properties (
    id UUID PRIMARY KEY,
    address TEXT,
    boundary BOX
);

CREATE INDEX idx_boundary ON properties USING GIST (boundary);

-- Find properties overlapping with a search area
SELECT * FROM properties
WHERE boundary && BOX(POINT(37.77, -122.43), POINT(37.78, -122.42));
```

### Test 3: Range Overlap
```sql
CREATE TABLE bookings (
    id UUID PRIMARY KEY,
    room TEXT,
    time_range TSTZRANGE
);

CREATE INDEX idx_time ON bookings USING GIST (time_range);

-- Find conflicting bookings
SELECT * FROM bookings
WHERE room = 'conference-a'
  AND time_range && '[2024-02-08 14:00, 2024-02-08 16:00]'::TSTZRANGE;
```

---

## Performance Considerations

### 1. Node Size Tuning
```rust
const GIST_MAX_ENTRIES: usize = 100; // Tunable based on workload

impl GistIndex {
    fn max_entries_per_node(&self) -> usize {
        match self.operator_class {
            OperatorClass::Point2D => 100,
            OperatorClass::Box2D => 50,    // Larger predicates
            OperatorClass::Range => 200,
            _ => 100,
        }
    }
}
```

### 2. Penalty Function Optimization
Use fast approximations for penalty calculation:
```rust
impl Point2DOperator {
    fn penalty_fast(&self, existing: &Predicate, new_pred: &Predicate) -> f64 {
        // Instead of full area calculation, use perimeter increase
        // Faster but less optimal
        self.perimeter_increase(existing, new_pred)
    }
}
```

### 3. Buffering
Cache frequently accessed nodes:
```rust
pub struct GistIndex {
    // ...
    node_cache: LruCache<Uuid, GistNode>,
}
```

---

## Metrics to Track

- `pieskieo_gist_index_searches_total` - Counter
- `pieskieo_gist_index_inserts_total` - Counter
- `pieskieo_gist_node_splits_total` - Counter
- `pieskieo_gist_search_duration_ms` - Histogram
- `pieskieo_gist_tree_height` - Gauge
- `pieskieo_gist_node_count` - Gauge

---

## Implementation Checklist

- [ ] Create GiST node structures
- [ ] Implement GistOperator trait
- [ ] Create Point2D operator class
- [ ] Implement GiST insert with penalty calculation
- [ ] Implement node splitting (quadratic split)
- [ ] Implement adjust_keys (predicate propagation)
- [ ] Implement GiST search
- [ ] Add Box2D operator class
- [ ] Add Range operator class
- [ ] Integrate with SQL parser (<->, &&, @>, <@ operators)
- [ ] Add CREATE INDEX ... USING GIST support
- [ ] Test point distance queries
- [ ] Test bounding box queries
- [ ] Test range overlap queries
- [ ] Benchmark vs sequential scan
- [ ] Add GiST index persistence
- [ ] Document GiST operators
- [ ] Add metrics collection

---

## R*-tree Optimizations (Production-Grade)

### Forced Reinsert Strategy

```rust
pub struct RStarTreeIndex {
    root: Uuid,
    max_entries_per_node: usize,
    
    // R*-tree specific parameters
    reinsert_factor: f64,  // Fraction of entries to reinsert (default 0.3)
    min_fill_factor: f64,  // Minimum node fill (default 0.4)
}

impl RStarTreeIndex {
    fn insert_with_forced_reinsert(
        &mut self,
        predicate: Predicate,
        tuple_id: Uuid,
        level: u32,
    ) -> Result<()> {
        let target_node = self.choose_subtree_r_star(&predicate, level)?;
        
        let mut node = self.get_node(target_node)?;
        node.entries.push(GistEntry {
            predicate: predicate.clone(),
            child_ptr: GistPointer::Tuple(tuple_id),
        });
        
        if node.entries.len() > self.max_entries_per_node {
            // Instead of immediate split, try forced reinsert first
            if !node.reinserted_on_this_level {
                self.forced_reinsert(&mut node, level)?;
                node.reinserted_on_this_level = true;
            } else {
                // Already reinserted, now split
                self.split_r_star(node.id, level)?;
            }
        }
        
        self.save_node(&node)?;
        Ok(())
    }
    
    fn forced_reinsert(&mut self, node: &mut GistNode, level: u32) -> Result<()> {
        // R*-tree innovation: remove and reinsert some entries
        // This improves tree balance and reduces overlap
        
        let reinsert_count = (node.entries.len() as f64 * self.reinsert_factor) as usize;
        
        // Sort entries by distance from node center
        let center = self.compute_center(&node.entries);
        let mut distances: Vec<(usize, f64)> = node.entries.iter()
            .enumerate()
            .map(|(i, entry)| (i, self.distance_from_center(&entry.predicate, &center)))
            .collect();
        
        distances.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        
        // Remove farthest entries
        let to_reinsert: Vec<_> = distances.iter()
            .take(reinsert_count)
            .map(|(i, _)| node.entries[*i].clone())
            .collect();
        
        // Keep closest entries
        let remaining_indices: HashSet<_> = distances.iter()
            .skip(reinsert_count)
            .map(|(i, _)| *i)
            .collect();
        
        node.entries = node.entries.iter()
            .enumerate()
            .filter(|(i, _)| remaining_indices.contains(i))
            .map(|(_, e)| e.clone())
            .collect();
        
        // Reinsert removed entries (may go to different nodes)
        for entry in to_reinsert {
            self.insert_with_forced_reinsert(entry.predicate, entry.get_tuple_id()?, level)?;
        }
        
        Ok(())
    }
}
```

### Advanced Split Algorithms

```rust
impl RStarTreeIndex {
    fn split_r_star(&mut self, node_id: Uuid, level: u32) -> Result<()> {
        let node = self.get_node(node_id)?;
        
        // R*-tree uses sophisticated split algorithm:
        // 1. Try all possible split axes (x, y for 2D)
        // 2. For each axis, try different split positions
        // 3. Choose split minimizing perimeter sum (not area!)
        
        let (axis, split_index) = self.choose_split_axis_and_index(&node.entries)?;
        
        // Split along chosen axis and index
        let (left_entries, right_entries) = self.split_along_axis(&node.entries, axis, split_index)?;
        
        // Create new sibling node
        let new_node = GistNode {
            id: Uuid::new_v4(),
            level: node.level,
            entries: right_entries.clone(),
            parent: node.parent,
            reinserted_on_this_level: false,
        };
        
        // Update original node
        let mut updated_node = node.clone();
        updated_node.entries = left_entries.clone();
        
        self.save_node(&updated_node)?;
        self.save_node(&new_node)?;
        
        // Update parent
        self.update_parent_after_split_r_star(node_id, new_node.id, &left_entries, &right_entries)?;
        
        Ok(())
    }
    
    fn choose_split_axis_and_index(&self, entries: &[GistEntry]) -> Result<(SplitAxis, usize)> {
        let mut best_axis = SplitAxis::X;
        let mut best_index = 0;
        let mut best_perimeter_sum = f64::INFINITY;
        
        for axis in &[SplitAxis::X, SplitAxis::Y] {
            // Sort entries by lower bound on this axis
            let mut sorted = entries.to_vec();
            self.sort_by_axis(&mut sorted, axis);
            
            // Try different split positions
            let min_entries = (entries.len() as f64 * self.min_fill_factor) as usize;
            let max_start = entries.len() - min_entries;
            
            for split_idx in min_entries..max_start {
                let (left, right) = sorted.split_at(split_idx);
                
                let left_mbr = self.compute_mbr(left);
                let right_mbr = self.compute_mbr(right);
                
                // R*-tree criterion: minimize perimeter sum
                let perimeter_sum = self.perimeter(&left_mbr) + self.perimeter(&right_mbr);
                
                if perimeter_sum < best_perimeter_sum {
                    best_perimeter_sum = perimeter_sum;
                    best_axis = *axis;
                    best_index = split_idx;
                }
            }
        }
        
        Ok((best_axis, best_index))
    }
}
```

## Parallel Index Construction

```rust
pub struct ParallelGistBuilder {
    thread_pool: ThreadPool,
    max_parallelism: usize,
}

impl ParallelGistBuilder {
    pub fn build_parallel(
        &self,
        data: Vec<(Predicate, Uuid)>,
    ) -> Result<RStarTreeIndex> {
        let chunk_size = data.len() / self.max_parallelism;
        
        // Phase 1: Build subtrees in parallel
        let subtree_futures: Vec<_> = data.chunks(chunk_size)
            .map(|chunk| {
                let chunk = chunk.to_vec();
                self.thread_pool.spawn(move || {
                    Self::build_subtree(chunk)
                })
            })
            .collect();
        
        let subtrees: Vec<_> = subtree_futures.into_iter()
            .map(|f| f.join().unwrap())
            .collect::<Result<Vec<_>>>()?;
        
        // Phase 2: Merge subtrees into final tree
        self.merge_subtrees(subtrees)
    }
    
    fn build_subtree(data: Vec<(Predicate, Uuid)>) -> Result<RStarTreeIndex> {
        // Use Sort-Tile-Recursive (STR) algorithm for bulk loading
        let mut index = RStarTreeIndex::new();
        
        // Sort data spatially using Hilbert curve
        let mut sorted_data = data;
        sorted_data.sort_by_cached_key(|(pred, _)| {
            self.hilbert_index(pred)
        });
        
        // Pack into leaf nodes
        let leaf_nodes = self.pack_into_leaves(sorted_data)?;
        
        // Build upper levels bottom-up
        self.build_upper_levels(leaf_nodes, &mut index)?;
        
        Ok(index)
    }
    
    fn hilbert_index(&self, predicate: &Predicate) -> u64 {
        // Map 2D coordinates to 1D Hilbert curve index
        // This preserves spatial locality
        
        if let Predicate::BoundingBox { min_x, min_y, .. } = predicate {
            hilbert::xy_to_index(*min_x as u32, *min_y as u32, 16)
        } else {
            0
        }
    }
    
    fn merge_subtrees(&self, subtrees: Vec<RStarTreeIndex>) -> Result<RStarTreeIndex> {
        // Merge multiple subtrees into one
        // This is parallelizable as well
        
        let mut merged = RStarTreeIndex::new();
        
        for subtree in subtrees {
            merged.merge(subtree)?;
        }
        
        Ok(merged)
    }
}
```

## Additional Operator Classes

### Full-Text Search (tsvector)

```rust
pub struct TsVectorOperator;

impl GistOperator for TsVectorOperator {
    fn union(&self, predicates: &[Predicate]) -> Predicate {
        // Union of term sets
        let mut all_terms = HashSet::new();
        
        for pred in predicates {
            if let Predicate::TsVector { terms } = pred {
                all_terms.extend(terms.iter().cloned());
            }
        }
        
        Predicate::TsVector {
            terms: all_terms.into_iter().collect(),
        }
    }
    
    fn overlaps(&self, p1: &Predicate, p2: &Predicate) -> bool {
        match (p1, p2) {
            (Predicate::TsVector { terms: t1 }, Predicate::TsVector { terms: t2 }) => {
                t1.iter().any(|term| t2.contains(term))
            }
            _ => false,
        }
    }
    
    // ... other methods
}
```

### Geometric Shapes (Circles, Polygons)

```rust
pub struct GeometryOperator;

impl GistOperator for GeometryOperator {
    fn union(&self, predicates: &[Predicate]) -> Predicate {
        // Compute minimal bounding box for all geometries
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        
        for pred in predicates {
            let (mx1, my1, mx2, my2) = self.get_bounds(pred);
            min_x = min_x.min(mx1);
            min_y = min_y.min(my1);
            max_x = max_x.max(mx2);
            max_y = max_y.max(my2);
        }
        
        Predicate::BoundingBox { min_x, min_y, max_x, max_y }
    }
    
    fn overlaps(&self, p1: &Predicate, p2: &Predicate) -> bool {
        match (p1, p2) {
            (Predicate::Circle { center: c1, radius: r1 }, 
             Predicate::Circle { center: c2, radius: r2 }) => {
                let distance = ((c1.x - c2.x).powi(2) + (c1.y - c2.y).powi(2)).sqrt();
                distance < r1 + r2
            }
            
            (Predicate::Polygon { points: p1 }, Predicate::Polygon { points: p2 }) => {
                // Use Separating Axis Theorem (SAT)
                self.polygons_overlap_sat(p1, p2)
            }
            
            _ => false,
        }
    }
}
```

## Production Operations & Monitoring

### Configuration

```toml
[gist_indexes]
# Use R*-tree algorithm (vs basic R-tree)
use_r_star = true

# Forced reinsert parameter (0.0-1.0)
reinsert_factor = 0.3

# Minimum node fill factor
min_fill_factor = 0.4

# Maximum entries per node
max_entries = 100

# Parallel index build
parallel_build = true
parallel_threads = 8

# Bulk loading algorithm
bulk_load_algorithm = "str"  # or "hilbert", "rstree"
```

### Metrics

```rust
metrics::counter!("pieskieo_gist_searches_total",
                  "operator_class" => op_class).increment(1);
metrics::counter!("pieskieo_gist_inserts_total").increment(1);
metrics::counter!("pieskieo_gist_node_splits_total",
                  "split_type" => "normal|forced_reinsert").increment(1);
metrics::histogram!("pieskieo_gist_search_duration_ms").record(duration);
metrics::histogram!("pieskieo_gist_tree_height").record(height);
metrics::gauge!("pieskieo_gist_node_count").set(node_count);
metrics::gauge!("pieskieo_gist_avg_node_fill_factor").set(fill_factor);
```

---

**Created**: 2026-02-08  
**Author**: Implementation Team  
**Review Status**: Production-Ready
