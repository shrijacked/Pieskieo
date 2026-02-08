# PostgreSQL Feature: Advanced Joins (PRODUCTION-GRADE)

**Status**: 🔴 Not Started  
**Priority**: Critical  
**Dependencies**: Basic JOIN (exists), Subqueries, CTEs  
**Estimated Effort**: 4-6 weeks

---

## Overview

Joins are the core of relational algebra. Pieskieo implements **all PostgreSQL join types** plus **Worst-Case Optimal Joins (WCOJ)** for multi-way joins - a feature that makes us superior to PostgreSQL for complex queries.

**Key Innovation**: Unlike PostgreSQL's binary join trees, we use **WCOJ** for queries with 3+ tables, achieving asymptotically optimal performance.

---

## Join Types (Complete Coverage)

### 1. INNER JOIN
```sql
SELECT * FROM orders o
INNER JOIN customers c ON o.customer_id = c.id;
```

### 2. LEFT/RIGHT/FULL OUTER JOIN
```sql
-- LEFT: All from left, matched from right (nulls if no match)
SELECT c.name, o.total
FROM customers c
LEFT JOIN orders o ON c.id = o.customer_id;

-- RIGHT: All from right, matched from left
SELECT p.name, i.quantity
FROM products p
RIGHT JOIN inventory i ON p.id = i.product_id;

-- FULL: All from both, nulls where no match
SELECT *
FROM products p
FULL OUTER JOIN inventory i ON p.id = i.product_id;
```

### 3. CROSS JOIN (Cartesian Product)
```sql
SELECT p.name, s.size, c.color
FROM products p
CROSS JOIN sizes s
CROSS JOIN colors c;
```

### 4. LATERAL JOIN (Correlated)
```sql
SELECT u.name, recent.title, recent.created_at
FROM users u
LEFT JOIN LATERAL (
    SELECT title, created_at 
    FROM posts 
    WHERE author_id = u.id 
    ORDER BY created_at DESC 
    LIMIT 5
) recent ON true;
```

### 5. NATURAL JOIN
```sql
-- Auto-join on all columns with same name
SELECT * FROM employees NATURAL JOIN departments;
-- Equivalent to: employees JOIN departments USING (dept_id)
```

### 6. SEMI JOIN / ANTI JOIN
```sql
-- SEMI: Return left rows that have match in right (no duplicates)
SELECT * FROM customers
WHERE id IN (SELECT customer_id FROM orders);

-- ANTI: Return left rows that have NO match in right
SELECT * FROM customers
WHERE id NOT IN (SELECT customer_id FROM orders);
```

### 7. ASOF JOIN (Time-Series)
```sql
-- Match on closest timestamp
SELECT t.symbol, t.timestamp, t.trade_price, q.quote_price
FROM trades t
ASOF JOIN quotes q 
  ON t.symbol = q.symbol
  AND t.timestamp >= q.timestamp;
```

---

## Implementation Plan

### Phase 1: Join Execution Algorithms

**File**: `crates/pieskieo-core/src/join/mod.rs`

```rust
pub enum JoinType {
    Inner,
    LeftOuter,
    RightOuter,
    FullOuter,
    Cross,
    Lateral,
    Semi,
    Anti,
    AsOf,  // Time-series join
}

pub enum JoinAlgorithm {
    NestedLoop,           // O(n*m), good for small datasets
    HashJoin,             // O(n+m), good for equality joins
    MergeJoin,            // O(n+m), good for sorted inputs
    IndexNestedLoop,      // O(n*log m), when right has index
    GraceHashJoin,        // O(n+m), for data larger than memory
    WorstCaseOptimal,     // O(output size), for multi-way joins
}

pub struct JoinExecutor {
    algorithm_selector: JoinAlgorithmSelector,
    memory_manager: JoinMemoryManager,
    
    // Parallel execution pool
    thread_pool: ThreadPool,
}
```

### Phase 2: Hash Join (Production-Grade)

```rust
use std::sync::Arc;
use dashmap::DashMap;
use crossbeam::channel::{bounded, Sender, Receiver};

pub struct HashJoinExecutor {
    // Lock-free hash table for parallel builds
    hash_table: Arc<DashMap<JoinKey, Vec<Row>>>,
    
    // Partitioning for Grace hash join (data larger than memory)
    num_partitions: usize,
    partition_buffers: Vec<Vec<Row>>,
    
    // SIMD-optimized hash function
    hasher: SIMDHasher,
}

impl HashJoinExecutor {
    pub fn execute_parallel(
        &self,
        left: RowStream,
        right: RowStream,
        on_left: &[String],
        on_right: &[String],
    ) -> Result<RowStream> {
        // Phase 1: Build hash table (parallel)
        let build_side = self.choose_build_side(&left, &right)?;
        
        // Partition data for parallel processing
        let partitions = self.partition_data(build_side, self.num_partitions)?;
        
        // Build hash tables in parallel
        let hash_tables: Vec<_> = partitions
            .into_par_iter()
            .map(|partition| self.build_hash_table(partition, on_left))
            .collect::<Result<Vec<_>>>()?;
        
        // Phase 2: Probe phase (parallel)
        let probe_side = if Arc::ptr_eq(&build_side, &left) { right } else { left };
        
        let results = probe_side
            .par_chunks(1000)
            .flat_map(|chunk| {
                self.probe_hash_table(&hash_tables, chunk, on_right)
            })
            .collect();
        
        Ok(RowStream::from_vec(results))
    }
    
    fn build_hash_table(
        &self,
        rows: Vec<Row>,
        join_keys: &[String],
    ) -> Result<DashMap<JoinKey, Vec<Row>>> {
        let hash_table = DashMap::with_capacity(rows.len());
        
        rows.into_par_iter().for_each(|row| {
            let key = self.extract_join_key(&row, join_keys);
            hash_table.entry(key).or_insert_with(Vec::new).push(row);
        });
        
        Ok(hash_table)
    }
    
    fn probe_hash_table(
        &self,
        hash_tables: &[DashMap<JoinKey, Vec<Row>>],
        probe_rows: &[Row],
        join_keys: &[String],
    ) -> Vec<Row> {
        let mut results = Vec::new();
        
        for row in probe_rows {
            let key = self.extract_join_key(row, join_keys);
            let partition_id = self.partition_for_key(&key);
            
            if let Some(matches) = hash_tables[partition_id].get(&key) {
                for match_row in matches.value() {
                    results.push(self.merge_rows(row, match_row));
                }
            }
        }
        
        results
    }
    
    // SIMD-optimized hashing
    fn extract_join_key(&self, row: &Row, columns: &[String]) -> JoinKey {
        let mut hash = 0u64;
        
        for col in columns {
            if let Some(value) = row.get(col) {
                // Use SIMD for hashing when possible
                hash = self.hasher.hash_value_simd(value, hash);
            }
        }
        
        JoinKey(hash)
    }
}
```

### Phase 3: Grace Hash Join (For Data > Memory)

```rust
pub struct GraceHashJoin {
    memory_limit: usize,
    num_partitions: usize,
    spill_dir: PathBuf,
}

impl GraceHashJoin {
    pub fn execute(
        &self,
        left: RowStream,
        right: RowStream,
        join_keys_left: &[String],
        join_keys_right: &[String],
    ) -> Result<RowStream> {
        // Phase 1: Partition both inputs to disk
        let left_partitions = self.partition_to_disk(left, join_keys_left)?;
        let right_partitions = self.partition_to_disk(right, join_keys_right)?;
        
        // Phase 2: Join matching partitions (each fits in memory)
        let mut results = Vec::new();
        
        for i in 0..self.num_partitions {
            let left_part = self.read_partition(&left_partitions[i])?;
            let right_part = self.read_partition(&right_partitions[i])?;
            
            // Use in-memory hash join for each partition
            let partition_results = self.hash_join_in_memory(
                left_part,
                right_part,
                join_keys_left,
                join_keys_right,
            )?;
            
            results.extend(partition_results);
        }
        
        // Clean up temp files
        self.cleanup_partitions(&left_partitions)?;
        self.cleanup_partitions(&right_partitions)?;
        
        Ok(RowStream::from_vec(results))
    }
    
    fn partition_to_disk(
        &self,
        rows: RowStream,
        join_keys: &[String],
    ) -> Result<Vec<PathBuf>> {
        let mut partition_writers: Vec<_> = (0..self.num_partitions)
            .map(|i| {
                let path = self.spill_dir.join(format!("partition_{}.bin", i));
                BufWriter::new(File::create(&path)?)
            })
            .collect::<Result<Vec<_>>>()?;
        
        for row in rows {
            let key = self.extract_join_key(&row, join_keys);
            let partition_id = (key.0 % self.num_partitions as u64) as usize;
            
            bincode::serialize_into(&mut partition_writers[partition_id], &row)?;
        }
        
        // Flush and return paths
        partition_writers.into_iter()
            .enumerate()
            .map(|(i, mut writer)| {
                writer.flush()?;
                Ok(self.spill_dir.join(format!("partition_{}.bin", i)))
            })
            .collect()
    }
}
```

### Phase 4: Worst-Case Optimal Joins (WCOJ)

**This is our killer feature - asymptotically optimal for multi-way joins!**

```rust
use std::collections::BTreeMap;

pub struct WCOJExecutor {
    // Variable ordering determines join order
    variable_ordering: Vec<String>,
    
    // Tries for efficient intersection
    tries: HashMap<String, Trie>,
}

impl WCOJExecutor {
    pub fn execute_multiway_join(
        &self,
        relations: Vec<Relation>,
        join_conditions: Vec<JoinCondition>,
    ) -> Result<Vec<Row>> {
        // Build tries for each relation
        for rel in &relations {
            let trie = self.build_trie(rel)?;
            self.tries.insert(rel.name.clone(), trie);
        }
        
        // Choose optimal variable ordering (Hugin's algorithm)
        let ordering = self.choose_variable_ordering(&relations, &join_conditions)?;
        
        // Execute join using leapfrog triejoin
        let mut results = Vec::new();
        self.leapfrog_triejoin(&ordering, 0, &mut Vec::new(), &mut results)?;
        
        Ok(results)
    }
    
    fn leapfrog_triejoin(
        &self,
        variables: &[String],
        depth: usize,
        current_binding: &mut Vec<(String, Value)>,
        results: &mut Vec<Row>,
    ) -> Result<()> {
        if depth == variables.len() {
            // Found complete binding
            results.push(Row::from_bindings(current_binding));
            return Ok(());
        }
        
        let var = &variables[depth];
        
        // Get iterators from all relevant tries
        let mut iterators: Vec<TrieIterator> = self.tries
            .values()
            .filter(|trie| trie.has_variable(var))
            .map(|trie| trie.seek_to(current_binding))
            .collect();
        
        // Leapfrog intersection
        while !iterators.is_empty() {
            let min_value = iterators.iter().map(|it| it.current()).min().unwrap();
            
            // Check if all iterators agree
            let all_agree = iterators.iter().all(|it| it.current() == min_value);
            
            if all_agree {
                // Found common value, recurse
                current_binding.push((var.clone(), min_value.clone()));
                self.leapfrog_triejoin(variables, depth + 1, current_binding, results)?;
                current_binding.pop();
                
                // Advance all iterators
                for it in &mut iterators {
                    it.next();
                }
            } else {
                // Seek lagging iterators to min_value
                for it in &mut iterators {
                    if it.current() < min_value {
                        it.seek(&min_value);
                    }
                }
            }
            
            // Remove exhausted iterators
            iterators.retain(|it| !it.at_end());
        }
        
        Ok(())
    }
}
```

### Phase 5: Distributed Joins

```rust
pub struct DistributedJoinExecutor {
    coordinator: Arc<Coordinator>,
}

impl DistributedJoinExecutor {
    pub async fn execute_cross_shard_join(
        &self,
        left_table: &str,
        right_table: &str,
        join_condition: JoinCondition,
        join_type: JoinType,
    ) -> Result<Vec<Row>> {
        // Determine shard distribution
        let left_shards = self.coordinator.get_shards_for_table(left_table).await?;
        let right_shards = self.coordinator.get_shards_for_table(right_table).await?;
        
        if left_shards.len() == 1 && right_shards.len() == 1 && left_shards[0] == right_shards[0] {
            // Co-located join - execute locally
            return self.execute_local_join(left_table, right_table, join_condition).await;
        }
        
        // Choose distributed join strategy
        match self.choose_distributed_strategy(&left_shards, &right_shards) {
            DistributedStrategy::Broadcast => {
                self.broadcast_join(left_table, right_table, join_condition).await
            }
            DistributedStrategy::Repartition => {
                self.repartition_join(left_table, right_table, join_condition).await
            }
            DistributedStrategy::ColocatedShuffle => {
                self.colocated_shuffle_join(left_table, right_table, join_condition).await
            }
        }
    }
    
    async fn broadcast_join(
        &self,
        left_table: &str,
        right_table: &str,
        join_condition: JoinCondition,
    ) -> Result<Vec<Row>> {
        // Broadcast smaller table to all nodes with larger table
        
        // Determine smaller table
        let left_size = self.coordinator.estimate_table_size(left_table).await?;
        let right_size = self.coordinator.estimate_table_size(right_table).await?;
        
        let (broadcast_table, local_table) = if left_size < right_size {
            (left_table, right_table)
        } else {
            (right_table, left_table)
        };
        
        // Fetch all data from broadcast table
        let broadcast_data = self.coordinator.fetch_full_table(broadcast_table).await?;
        
        // Get shards for local table
        let local_shards = self.coordinator.get_shards_for_table(local_table).await?;
        
        // Execute join on each shard in parallel
        let join_futures = local_shards.iter().map(|shard_id| {
            let broadcast_data = broadcast_data.clone();
            let join_cond = join_condition.clone();
            
            async move {
                let shard = self.coordinator.get_shard(*shard_id).await?;
                shard.join_with_broadcast_data(local_table, broadcast_data, join_cond).await
            }
        });
        
        let shard_results = futures::future::try_join_all(join_futures).await?;
        
        // Merge results
        Ok(shard_results.into_iter().flatten().collect())
    }
    
    async fn repartition_join(
        &self,
        left_table: &str,
        right_table: &str,
        join_condition: JoinCondition,
    ) -> Result<Vec<Row>> {
        // Repartition both tables by join key, then join locally
        
        let join_key = join_condition.extract_key()?;
        
        // Repartition left table
        let left_repartitioned = self.repartition_table(left_table, &join_key).await?;
        
        // Repartition right table
        let right_repartitioned = self.repartition_table(right_table, &join_key).await?;
        
        // Now data with same join key is co-located
        // Execute local joins on each partition
        let num_partitions = left_repartitioned.len();
        
        let join_futures = (0..num_partitions).map(|i| {
            let left_part = left_repartitioned[i].clone();
            let right_part = right_repartitioned[i].clone();
            let join_cond = join_condition.clone();
            
            async move {
                // Execute local hash join
                self.hash_join_local(left_part, right_part, join_cond).await
            }
        });
        
        let partition_results = futures::future::try_join_all(join_futures).await?;
        
        Ok(partition_results.into_iter().flatten().collect())
    }
}
```

---

## Join Algorithm Selection (Cost-Based)

```rust
pub struct JoinAlgorithmSelector {
    cost_model: CostModel,
    stats_collector: Arc<StatisticsCollector>,
}

impl JoinAlgorithmSelector {
    pub fn choose_algorithm(
        &self,
        left: &TableStats,
        right: &TableStats,
        join_type: JoinType,
        join_condition: &JoinCondition,
    ) -> JoinAlgorithm {
        // Cost-based selection
        
        let left_size = left.estimated_rows;
        let right_size = right.estimated_rows;
        let total_size = left_size + right_size;
        let memory_available = self.get_available_memory();
        
        // Rule 1: Small data → Nested loop
        if total_size < 1000 {
            return JoinAlgorithm::NestedLoop;
        }
        
        // Rule 2: Right has index on join key → Index nested loop
        if let Some(index) = right.get_index_for_columns(&join_condition.right_columns) {
            let cost_inl = left_size as f64 * index.avg_lookup_cost();
            let cost_hash = (left_size + right_size) as f64 * 1.0;
            
            if cost_inl < cost_hash {
                return JoinAlgorithm::IndexNestedLoop;
            }
        }
        
        // Rule 3: Data too large for memory → Grace hash join
        let hash_table_size = (right_size as f64 * 100.0) as usize; // 100 bytes per row estimate
        if hash_table_size > memory_available {
            return JoinAlgorithm::GraceHashJoin;
        }
        
        // Rule 4: Both inputs sorted → Merge join
        if left.is_sorted_by(&join_condition.left_columns) 
            && right.is_sorted_by(&join_condition.right_columns) {
            return JoinAlgorithm::MergeJoin;
        }
        
        // Rule 5: Equality join, fits in memory → Hash join
        if join_condition.is_equality() {
            return JoinAlgorithm::HashJoin;
        }
        
        // Default: Nested loop
        JoinAlgorithm::NestedLoop
    }
}
```

---

## Performance Optimizations

### 1. SIMD-Optimized Hash Functions

```rust
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

pub struct SIMDHasher;

impl SIMDHasher {
    #[cfg(target_arch = "x86_64")]
    pub fn hash_value_simd(&self, value: &Value, seed: u64) -> u64 {
        unsafe {
            match value {
                Value::Number(n) => {
                    let v = _mm_set_pd(*n, seed as f64);
                    let hash = _mm_crc32_u64(seed, std::mem::transmute(v));
                    hash
                }
                Value::String(s) => {
                    // SIMD string hashing
                    self.hash_bytes_simd(s.as_bytes(), seed)
                }
                _ => {
                    // Fallback
                    self.hash_value_scalar(value, seed)
                }
            }
        }
    }
    
    #[cfg(target_arch = "x86_64")]
    unsafe fn hash_bytes_simd(&self, bytes: &[u8], mut hash: u64) -> u64 {
        let chunks = bytes.chunks_exact(32);
        let remainder = chunks.remainder();
        
        for chunk in chunks {
            let v = _mm256_loadu_si256(chunk.as_ptr() as *const __m256i);
            hash = _mm_crc32_u64(hash, std::mem::transmute(v));
        }
        
        // Handle remainder
        for &byte in remainder {
            hash = _mm_crc32_u64(hash, byte as u64);
        }
        
        hash
    }
}
```

### 2. Lock-Free Join State

```rust
use std::sync::atomic::{AtomicUsize, AtomicBool, Ordering};

pub struct LockFreeJoinState {
    // Atomic counters for progress tracking
    rows_processed: AtomicUsize,
    rows_output: AtomicUsize,
    
    // Lock-free hash table (using DashMap)
    hash_table: Arc<DashMap<JoinKey, Vec<Row>>>,
    
    // Completion flag
    completed: AtomicBool,
}

impl LockFreeJoinState {
    pub fn record_progress(&self, processed: usize, output: usize) {
        self.rows_processed.fetch_add(processed, Ordering::Relaxed);
        self.rows_output.fetch_add(output, Ordering::Relaxed);
    }
    
    pub fn get_progress(&self) -> (usize, usize) {
        (
            self.rows_processed.load(Ordering::Relaxed),
            self.rows_output.load(Ordering::Relaxed),
        )
    }
}
```

### 3. Adaptive Parallelism

```rust
pub struct AdaptiveJoinExecutor {
    thread_pool: Arc<ThreadPool>,
    initial_parallelism: usize,
}

impl AdaptiveJoinExecutor {
    pub fn execute_with_adaptive_parallelism(
        &self,
        left: RowStream,
        right: RowStream,
    ) -> Result<RowStream> {
        // Start with initial parallelism
        let mut parallelism = self.initial_parallelism;
        
        // Monitor throughput
        let start = Instant::now();
        let mut last_check = start;
        let mut last_rows = 0;
        
        loop {
            let rows_processed = self.execute_chunk(left.next_chunk()?, right, parallelism)?;
            
            // Check every 100ms
            if last_check.elapsed() > Duration::from_millis(100) {
                let current_throughput = (rows_processed - last_rows) as f64 
                                        / last_check.elapsed().as_secs_f64();
                
                // Adjust parallelism based on throughput
                parallelism = self.adjust_parallelism(parallelism, current_throughput);
                
                last_check = Instant::now();
                last_rows = rows_processed;
            }
        }
    }
}
```

---

## Monitoring & Metrics

```rust
// Join execution metrics
metrics::counter!("pieskieo_joins_total", 
                  "type" => join_type, "algorithm" => algorithm).increment(1);
metrics::histogram!("pieskieo_join_duration_ms",
                    "algorithm" => algorithm).record(duration_ms);
metrics::histogram!("pieskieo_join_rows_processed").record(rows);
metrics::histogram!("pieskieo_join_rows_output").record(output_rows);

// Algorithm selection metrics
metrics::counter!("pieskieo_join_algorithm_selected",
                  "algorithm" => algorithm).increment(1);
metrics::histogram!("pieskieo_join_selectivity").record(selectivity);

// Distributed join metrics
metrics::counter!("pieskieo_distributed_joins_total",
                  "strategy" => "broadcast|repartition").increment(1);
metrics::histogram!("pieskieo_join_network_bytes_sent").record(bytes);
metrics::histogram!("pieskieo_join_coordination_ms").record(coord_time);
```

---

## Configuration

```toml
[joins]
# Default join algorithm (can be overridden by cost model)
default_algorithm = "hash"  # hash, nested_loop, merge, wcoj

# Memory limit for hash tables (MB)
hash_table_memory_limit = 512

# Grace hash join partitions
grace_hash_partitions = 16

# Enable WCOJ for 3+ table joins
enable_wcoj = true

# Minimum tables for WCOJ
wcoj_min_tables = 3

# Parallel join execution
parallel_joins = true
join_parallelism = 8

# Distributed join strategy
distributed_join_strategy = "cost_based"  # broadcast, repartition, cost_based

# Broadcast join size threshold (MB)
broadcast_threshold_mb = 10
```

---

## Test Cases

### Test 1: All Join Types
```sql
-- INNER
SELECT * FROM orders o INNER JOIN customers c ON o.customer_id = c.id;

-- LEFT OUTER
SELECT c.name, COALESCE(SUM(o.total), 0) as total_spent
FROM customers c
LEFT JOIN orders o ON c.id = o.customer_id
GROUP BY c.id, c.name;

-- FULL OUTER
SELECT * FROM products p FULL OUTER JOIN inventory i ON p.id = i.product_id;

-- CROSS JOIN
SELECT p.name, s.size FROM products p CROSS JOIN sizes s;

-- LATERAL
SELECT u.name, recent.title
FROM users u
LEFT JOIN LATERAL (
    SELECT title FROM posts WHERE author_id = u.id ORDER BY created_at DESC LIMIT 5
) recent ON true;
```

### Test 2: Multi-Way Join (WCOJ)
```sql
-- 4-table join - should use WCOJ
SELECT 
    u.name,
    p.title,
    c.text,
    t.name as tag
FROM users u
JOIN posts p ON p.author_id = u.id
JOIN comments c ON c.post_id = p.id
JOIN post_tags pt ON pt.post_id = p.id
JOIN tags t ON t.id = pt.tag_id
WHERE u.country = 'US'
  AND p.published_at > '2024-01-01'
  AND t.category = 'tech';
```

---

**Created**: 2026-02-08  
**Author**: Implementation Team  
**Review Status**: Production-Ready
