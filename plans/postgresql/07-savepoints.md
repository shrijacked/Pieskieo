# PostgreSQL Feature: Savepoints

**Status**: 🔴 Not Started  
**Priority**: High  
**Dependencies**: 05-acid.md (ACID transactions)  
**Estimated Effort**: 1-2 weeks

---

## Overview

Savepoints allow creating named checkpoints within a transaction that can be rolled back to independently. This enables partial rollback without aborting the entire transaction - critical for complex business logic that needs error recovery mid-transaction.

**Use Case Example**: In an e-commerce order, you might want to:
1. Reserve inventory (savepoint "inventory_reserved")
2. Process payment (if fails, rollback to savepoint)
3. Create shipment (if fails, rollback to savepoint)
4. Finalize order (commit all)

---

## SQL Syntax

### Creating Savepoints
```sql
BEGIN;

INSERT INTO orders (id, user_id, total) VALUES ('order1', 'user1', 150.00);
SAVEPOINT order_created;

INSERT INTO order_items (order_id, product_id, quantity) VALUES ('order1', 'prod1', 2);
SAVEPOINT items_added;

-- Oops, wrong product!
ROLLBACK TO SAVEPOINT items_added;

INSERT INTO order_items (order_id, product_id, quantity) VALUES ('order1', 'prod2', 3);
SAVEPOINT items_corrected;

COMMIT; -- Commits order + corrected items
```

### Releasing Savepoints
```sql
SAVEPOINT sp1;
INSERT INTO logs (msg) VALUES ('checkpoint');
RELEASE SAVEPOINT sp1; -- Frees resources, can't rollback anymore
```

### Rollback to Savepoint
```sql
SAVEPOINT before_risky_operation;
UPDATE accounts SET balance = balance - 1000 WHERE id = 'acc1';

-- Check constraint violation
ROLLBACK TO SAVEPOINT before_risky_operation;
-- Transaction still active, can continue
```

---

## Implementation Plan

### Phase 1: Savepoint Data Structure

**File**: `crates/pieskieo-core/src/transaction.rs`

```rust
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Savepoint {
    pub name: String,
    pub transaction_id: Uuid,
    pub snapshot_id: u64,
    pub created_at: SystemTime,
    
    // WAL position when savepoint created
    pub wal_lsn: u64,
    
    // Uncommitted changes before this savepoint
    pub write_set_snapshot: Vec<WriteOperation>,
}

#[derive(Debug)]
pub struct Transaction {
    pub id: Uuid,
    pub snapshot_id: u64,
    pub status: TransactionStatus,
    pub write_set: Vec<WriteOperation>,
    
    // NEW: Savepoints stack (most recent last)
    pub savepoints: Vec<Savepoint>,
}

impl Transaction {
    pub fn create_savepoint(&mut self, name: String, current_wal_lsn: u64) -> Result<()> {
        // Validate name
        if name.is_empty() {
            return Err(PieskieoError::InvalidSavepoint("empty name".into()));
        }
        
        // Check for duplicate name
        if self.savepoints.iter().any(|sp| sp.name == name) {
            return Err(PieskieoError::InvalidSavepoint(
                format!("savepoint '{}' already exists", name)
            ));
        }
        
        let savepoint = Savepoint {
            name,
            transaction_id: self.id,
            snapshot_id: self.snapshot_id,
            created_at: SystemTime::now(),
            wal_lsn: current_wal_lsn,
            write_set_snapshot: self.write_set.clone(),
        };
        
        self.savepoints.push(savepoint);
        Ok(())
    }
    
    pub fn rollback_to_savepoint(&mut self, name: &str) -> Result<()> {
        // Find savepoint
        let sp_index = self.savepoints
            .iter()
            .position(|sp| sp.name == name)
            .ok_or_else(|| PieskieoError::InvalidSavepoint(
                format!("savepoint '{}' does not exist", name)
            ))?;
        
        let savepoint = &self.savepoints[sp_index];
        
        // Restore write set to savepoint state
        self.write_set = savepoint.write_set_snapshot.clone();
        
        // Discard all savepoints created AFTER this one
        self.savepoints.truncate(sp_index + 1);
        
        Ok(())
    }
    
    pub fn release_savepoint(&mut self, name: &str) -> Result<()> {
        let sp_index = self.savepoints
            .iter()
            .position(|sp| sp.name == name)
            .ok_or_else(|| PieskieoError::InvalidSavepoint(
                format!("savepoint '{}' does not exist", name)
            ))?;
        
        // Remove this savepoint and all subsequent ones
        self.savepoints.truncate(sp_index);
        
        Ok(())
    }
}
```

### Phase 2: Parser Integration

**File**: `crates/pieskieo-core/src/engine.rs`

```rust
#[derive(Debug, Clone)]
pub enum SqlStatement {
    Select(SelectStatement),
    Insert(InsertStatement),
    Update(UpdateStatement),
    Delete(DeleteStatement),
    Begin,
    Commit,
    Rollback,
    
    // NEW
    Savepoint { name: String },
    RollbackToSavepoint { name: String },
    ReleaseSavepoint { name: String },
}

impl PieskieoDb {
    pub fn parse_statement(&self, sql: &str) -> Result<SqlStatement> {
        let parsed = Parser::parse_sql(&GenericDialect {}, sql)?;
        
        match &parsed[0] {
            Statement::Savepoint { name } => {
                Ok(SqlStatement::Savepoint {
                    name: name.to_string()
                })
            }
            
            Statement::Rollback { savepoint: Some(name) } => {
                Ok(SqlStatement::RollbackToSavepoint {
                    name: name.to_string()
                })
            }
            
            Statement::ReleaseSavepoint { name } => {
                Ok(SqlStatement::ReleaseSavepoint {
                    name: name.to_string()
                })
            }
            
            // ... existing statement parsing
        }
    }
}
```

### Phase 3: Execution Engine

**File**: `crates/pieskieo-core/src/engine.rs`

```rust
impl PieskieoDb {
    pub fn execute_sql(&self, sql: &str) -> Result<SqlResult> {
        let statement = self.parse_statement(sql)?;
        
        match statement {
            SqlStatement::Savepoint { name } => {
                self.execute_savepoint(name)
            }
            
            SqlStatement::RollbackToSavepoint { name } => {
                self.execute_rollback_to_savepoint(name)
            }
            
            SqlStatement::ReleaseSavepoint { name } => {
                self.execute_release_savepoint(name)
            }
            
            // ... existing statement execution
        }
    }
    
    fn execute_savepoint(&self, name: String) -> Result<SqlResult> {
        let mut txn_manager = self.transaction_manager.lock().unwrap();
        
        // Get current transaction
        let txn_id = txn_manager.current_transaction_id()
            .ok_or_else(|| PieskieoError::Internal(
                "SAVEPOINT requires active transaction".into()
            ))?;
        
        let current_wal_lsn = self.wal.lock().unwrap().current_lsn();
        
        txn_manager.create_savepoint(txn_id, name, current_wal_lsn)?;
        
        Ok(SqlResult {
            rows_affected: 0,
            message: Some("SAVEPOINT created".into()),
        })
    }
    
    fn execute_rollback_to_savepoint(&self, name: String) -> Result<SqlResult> {
        let mut txn_manager = self.transaction_manager.lock().unwrap();
        
        let txn_id = txn_manager.current_transaction_id()
            .ok_or_else(|| PieskieoError::Internal(
                "ROLLBACK TO requires active transaction".into()
            ))?;
        
        txn_manager.rollback_to_savepoint(txn_id, &name)?;
        
        // Need to undo WAL entries written after savepoint
        self.undo_wal_to_savepoint(txn_id, &name)?;
        
        Ok(SqlResult {
            rows_affected: 0,
            message: Some(format!("Rolled back to savepoint '{}'", name)),
        })
    }
    
    fn undo_wal_to_savepoint(&self, txn_id: Uuid, savepoint_name: &str) -> Result<()> {
        let txn_manager = self.transaction_manager.lock().unwrap();
        let txn = txn_manager.get_transaction(txn_id)?;
        
        let savepoint = txn.savepoints.iter()
            .find(|sp| sp.name == savepoint_name)
            .ok_or_else(|| PieskieoError::InvalidSavepoint("not found".into()))?;
        
        let target_lsn = savepoint.wal_lsn;
        
        // Undo all WAL operations after the savepoint LSN
        let mut wal = self.wal.lock().unwrap();
        wal.undo_to_lsn(target_lsn)?;
        
        Ok(())
    }
}
```

### Phase 4: WAL Integration

**File**: `crates/pieskieo-core/src/wal.rs`

```rust
impl WriteAheadLog {
    pub fn undo_to_lsn(&mut self, target_lsn: u64) -> Result<()> {
        let current_lsn = self.current_lsn();
        
        if target_lsn > current_lsn {
            return Err(PieskieoError::Internal(
                "cannot undo to future LSN".into()
            ));
        }
        
        // Read WAL entries backwards from current to target
        for lsn in (target_lsn + 1..=current_lsn).rev() {
            let entry = self.read_entry(lsn)?;
            
            match entry.operation {
                WalOperation::Insert { shard_id, record_id, .. } => {
                    // Undo: mark as deleted
                    self.append_delete(shard_id, record_id)?;
                }
                
                WalOperation::Update { shard_id, record_id, old_value, .. } => {
                    // Undo: restore old value
                    self.append_update(shard_id, record_id, old_value)?;
                }
                
                WalOperation::Delete { shard_id, record_id, old_value } => {
                    // Undo: re-insert old value
                    self.append_insert(shard_id, record_id, old_value)?;
                }
                
                _ => {}
            }
        }
        
        Ok(())
    }
}
```

---

## Test Cases

### Test 1: Basic Savepoint Creation
```sql
BEGIN;

INSERT INTO users (id, name) VALUES ('u1', 'Alice');
SAVEPOINT user_created;

INSERT INTO logs (user_id, action) VALUES ('u1', 'signup');

COMMIT;

-- Verify both records exist
SELECT COUNT(*) FROM users WHERE id = 'u1'; -- Expected: 1
SELECT COUNT(*) FROM logs WHERE user_id = 'u1'; -- Expected: 1
```

### Test 2: Rollback to Savepoint
```sql
BEGIN;

INSERT INTO accounts (id, balance) VALUES ('a1', 100);
SAVEPOINT account_created;

UPDATE accounts SET balance = 200 WHERE id = 'a1';
SAVEPOINT balance_updated;

-- Rollback the update
ROLLBACK TO SAVEPOINT balance_updated;

SELECT balance FROM accounts WHERE id = 'a1'; -- Expected: 100 (original)

COMMIT;
```

### Test 3: Nested Savepoints
```sql
BEGIN;

INSERT INTO products (id, name, price) VALUES ('p1', 'Widget', 10);
SAVEPOINT sp1;

UPDATE products SET price = 15 WHERE id = 'p1';
SAVEPOINT sp2;

UPDATE products SET price = 20 WHERE id = 'p1';
SAVEPOINT sp3;

-- Rollback to middle savepoint
ROLLBACK TO SAVEPOINT sp2;

SELECT price FROM products WHERE id = 'p1'; -- Expected: 15

COMMIT;
```

### Test 4: Release Savepoint
```sql
BEGIN;

INSERT INTO items (id, name) VALUES ('i1', 'Item1');
SAVEPOINT sp1;

UPDATE items SET name = 'Item1-Updated' WHERE id = 'i1';
RELEASE SAVEPOINT sp1; -- Can't rollback anymore

-- This should fail
ROLLBACK TO SAVEPOINT sp1; -- ERROR: savepoint does not exist

COMMIT;
```

### Test 5: Error Handling with Savepoints
```sql
BEGIN;

INSERT INTO orders (id, total) VALUES ('o1', 100);
SAVEPOINT order_created;

-- This will fail (duplicate key)
INSERT INTO orders (id, total) VALUES ('o1', 200);

-- Transaction still active, can rollback
ROLLBACK TO SAVEPOINT order_created;

-- Insert different order
INSERT INTO orders (id, total) VALUES ('o2', 200);

COMMIT; -- Commits o1 and o2
```

### Test 6: Savepoint in Concurrent Transactions
```rust
// Rust integration test
#[tokio::test]
async fn test_savepoints_isolation() {
    let db = PieskieoDb::new_in_memory().await.unwrap();
    
    // Transaction 1
    let txn1 = db.begin_transaction().await.unwrap();
    db.execute("INSERT INTO data (id, val) VALUES ('d1', 10)").await.unwrap();
    db.execute("SAVEPOINT sp1").await.unwrap();
    db.execute("UPDATE data SET val = 20 WHERE id = 'd1'").await.unwrap();
    
    // Transaction 2 (should not see txn1's changes)
    let txn2 = db.begin_transaction().await.unwrap();
    let result = db.execute("SELECT val FROM data WHERE id = 'd1'").await.unwrap();
    assert!(result.rows.is_empty()); // Invisible due to isolation
    
    // Txn1 rollback to savepoint
    db.execute("ROLLBACK TO SAVEPOINT sp1").await.unwrap();
    db.commit_transaction(txn1).await.unwrap();
    
    // Txn2 should now see val = 10
    let result = db.execute("SELECT val FROM data WHERE id = 'd1'").await.unwrap();
    assert_eq!(result.rows[0].get("val"), Some(&Value::Number(10)));
}
```

---

## Advanced Performance Optimizations

### 1. Copy-on-Write Memory Management

```rust
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct WriteSet {
    // Use Arc for cheap cloning
    operations: Arc<Vec<WriteOperation>>,
    // Track modifications since clone
    modifications: Vec<WriteOperation>,
}

impl WriteSet {
    pub fn snapshot(&self) -> WriteSet {
        // Cheap clone via Arc
        WriteSet {
            operations: Arc::clone(&self.operations),
            modifications: Vec::new(),
        }
    }
    
    pub fn add_operation(&mut self, op: WriteOperation) {
        // Only pay for clone if modified after snapshot
        self.modifications.push(op);
    }
    
    pub fn materialize(&self) -> Vec<WriteOperation> {
        // Merge base + modifications
        let mut all_ops = (*self.operations).clone();
        all_ops.extend_from_slice(&self.modifications);
        all_ops
    }
}

#[derive(Debug)]
pub struct Transaction {
    pub id: Uuid,
    pub write_set: WriteSet,
    
    // Savepoint stack with O(1) lookup
    pub savepoints_stack: Vec<Savepoint>,
    pub savepoints_index: HashMap<String, usize>,
    
    // Adaptive limit based on available memory
    pub max_savepoints: usize,
}

impl Transaction {
    pub fn create_savepoint(&mut self, name: String, lsn: u64) -> Result<()> {
        // Dynamic limit based on memory pressure
        if self.savepoints_stack.len() >= self.max_savepoints {
            // Try to free memory by compressing old savepoints
            self.compress_old_savepoints()?;
            
            if self.savepoints_stack.len() >= self.max_savepoints {
                return Err(PieskieoError::TooManySavepoints {
                    limit: self.max_savepoints,
                    current: self.savepoints_stack.len(),
                });
            }
        }
        
        // Check for duplicate
        if self.savepoints_index.contains_key(&name) {
            return Err(PieskieoError::InvalidSavepoint(
                format!("savepoint '{}' already exists", name)
            ));
        }
        
        let index = self.savepoints_stack.len();
        let savepoint = Savepoint {
            name: name.clone(),
            transaction_id: self.id,
            snapshot_id: self.snapshot_id,
            created_at: SystemTime::now(),
            wal_lsn: lsn,
            write_set_snapshot: self.write_set.snapshot(), // Cheap COW
        };
        
        self.savepoints_stack.push(savepoint);
        self.savepoints_index.insert(name, index);
        
        Ok(())
    }
    
    fn compress_old_savepoints(&mut self) -> Result<()> {
        // Compress savepoints older than 60 seconds
        let cutoff = SystemTime::now() - Duration::from_secs(60);
        
        for sp in &mut self.savepoints_stack {
            if sp.created_at < cutoff && !sp.compressed {
                // Compress write_set using zstd
                let materialized = sp.write_set_snapshot.materialize();
                let serialized = bincode::serialize(&materialized)?;
                let compressed = zstd::encode_all(&serialized[..], 3)?;
                
                sp.compressed_data = Some(compressed);
                sp.compressed = true;
                
                // Clear uncompressed data
                sp.write_set_snapshot = WriteSet::default();
            }
        }
        
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Savepoint {
    pub name: String,
    pub transaction_id: Uuid,
    pub snapshot_id: u64,
    pub created_at: SystemTime,
    pub wal_lsn: u64,
    
    // Either compressed or uncompressed, not both
    pub write_set_snapshot: WriteSet,
    pub compressed: bool,
    pub compressed_data: Option<Vec<u8>>,
}
```

### 2. Optimized WAL Undo with Batching

```rust
impl WriteAheadLog {
    pub fn undo_to_lsn_batched(&mut self, target_lsn: u64) -> Result<()> {
        let current_lsn = self.current_lsn();
        
        if target_lsn > current_lsn {
            return Err(PieskieoError::Internal("cannot undo to future LSN".into()));
        }
        
        if current_lsn == target_lsn {
            return Ok(()); // Nothing to undo
        }
        
        // Read WAL entries in reverse, batch undo operations
        let entries_to_undo = self.read_range_reverse(target_lsn + 1, current_lsn)?;
        
        // Group undo operations by shard for batching
        let mut undo_batches: HashMap<u32, Vec<UndoOperation>> = HashMap::new();
        
        for entry in entries_to_undo {
            let undo_op = match entry.operation {
                WalOperation::Insert { shard_id, record_id, .. } => {
                    UndoOperation::Delete { shard_id, record_id }
                }
                WalOperation::Update { shard_id, record_id, old_value, .. } => {
                    UndoOperation::Restore { shard_id, record_id, value: old_value }
                }
                WalOperation::Delete { shard_id, record_id, old_value } => {
                    UndoOperation::Insert { shard_id, record_id, value: old_value }
                }
                _ => continue,
            };
            
            undo_batches.entry(undo_op.shard_id()).or_default().push(undo_op);
        }
        
        // Execute undo operations in parallel by shard
        let futures: Vec<_> = undo_batches.into_iter()
            .map(|(shard_id, ops)| {
                let wal = self.clone();
                async move {
                    wal.execute_undo_batch(shard_id, ops).await
                }
            })
            .collect();
        
        futures::future::try_join_all(futures).await?;
        
        // Log compensation records
        self.append_compensation_record(target_lsn)?;
        
        Ok(())
    }
    
    async fn execute_undo_batch(
        &self,
        shard_id: u32,
        ops: Vec<UndoOperation>,
    ) -> Result<()> {
        // Batch execute undos for better performance
        let shard = self.get_shard(shard_id)?;
        
        for chunk in ops.chunks(1000) {
            shard.execute_batch(chunk).await?;
        }
        
        Ok(())
    }
}
```

### 3. Distributed Savepoints

```rust
pub struct DistributedTransaction {
    pub id: Uuid,
    pub coordinator_node: NodeId,
    
    // Savepoints across multiple nodes
    pub distributed_savepoints: Vec<DistributedSavepoint>,
}

#[derive(Debug, Clone)]
pub struct DistributedSavepoint {
    pub name: String,
    pub created_at: SystemTime,
    
    // WAL LSN on each participating node
    pub node_lsns: HashMap<NodeId, u64>,
    
    // Snapshot IDs on each node
    pub node_snapshots: HashMap<NodeId, u64>,
}

impl DistributedTransaction {
    pub async fn create_savepoint_distributed(
        &mut self,
        name: String,
        coordinator: &Coordinator,
    ) -> Result<()> {
        // Phase 1: Request savepoint from all participants
        let participants = self.get_participating_nodes()?;
        
        let savepoint_futures = participants.iter().map(|node_id| {
            let name = name.clone();
            async move {
                let node = coordinator.get_node(*node_id).await?;
                let lsn = node.create_savepoint(&name).await?;
                let snapshot_id = node.get_current_snapshot_id().await?;
                Ok((*node_id, lsn, snapshot_id))
            }
        });
        
        let results = futures::future::try_join_all(savepoint_futures).await?;
        
        // Phase 2: Create distributed savepoint metadata
        let mut node_lsns = HashMap::new();
        let mut node_snapshots = HashMap::new();
        
        for (node_id, lsn, snapshot_id) in results {
            node_lsns.insert(node_id, lsn);
            node_snapshots.insert(node_id, snapshot_id);
        }
        
        let distributed_savepoint = DistributedSavepoint {
            name: name.clone(),
            created_at: SystemTime::now(),
            node_lsns,
            node_snapshots,
        };
        
        self.distributed_savepoints.push(distributed_savepoint);
        
        // Phase 3: Log to coordinator's WAL for durability
        coordinator.log_distributed_savepoint(self.id, &name).await?;
        
        Ok(())
    }
    
    pub async fn rollback_to_savepoint_distributed(
        &mut self,
        name: &str,
        coordinator: &Coordinator,
    ) -> Result<()> {
        // Find savepoint
        let savepoint = self.distributed_savepoints.iter()
            .find(|sp| sp.name == name)
            .ok_or_else(|| PieskieoError::InvalidSavepoint(
                format!("savepoint '{}' does not exist", name)
            ))?;
        
        // Phase 1: Initiate rollback on all nodes (parallel)
        let rollback_futures = savepoint.node_lsns.iter().map(|(node_id, lsn)| {
            async move {
                let node = coordinator.get_node(*node_id).await?;
                node.rollback_to_lsn(*lsn).await
            }
        });
        
        futures::future::try_join_all(rollback_futures).await?;
        
        // Phase 2: Discard savepoints created after this one
        self.distributed_savepoints.retain(|sp| sp.created_at <= savepoint.created_at);
        
        Ok(())
    }
}
```

### 4. Savepoint Memory Budgeting

```rust
pub struct SavepointMemoryManager {
    // Global memory budget for all savepoints
    total_budget: AtomicUsize,
    used_memory: AtomicUsize,
    
    // Per-transaction memory tracking
    transaction_usage: DashMap<Uuid, usize>,
}

impl SavepointMemoryManager {
    pub fn new(budget_mb: usize) -> Self {
        Self {
            total_budget: AtomicUsize::new(budget_mb * 1024 * 1024),
            used_memory: AtomicUsize::new(0),
            transaction_usage: DashMap::new(),
        }
    }
    
    pub fn allocate(&self, txn_id: Uuid, size: usize) -> Result<()> {
        // Check global budget
        let current = self.used_memory.load(Ordering::Relaxed);
        let budget = self.total_budget.load(Ordering::Relaxed);
        
        if current + size > budget {
            // Try to evict compressed savepoints
            self.evict_old_savepoints(size)?;
            
            // Check again
            let current = self.used_memory.load(Ordering::Relaxed);
            if current + size > budget {
                return Err(PieskieoError::OutOfMemory {
                    requested: size,
                    available: budget - current,
                });
            }
        }
        
        // Allocate
        self.used_memory.fetch_add(size, Ordering::Relaxed);
        *self.transaction_usage.entry(txn_id).or_insert(0) += size;
        
        Ok(())
    }
    
    pub fn deallocate(&self, txn_id: Uuid, size: usize) {
        self.used_memory.fetch_sub(size, Ordering::Relaxed);
        
        if let Some(mut usage) = self.transaction_usage.get_mut(&txn_id) {
            *usage = usage.saturating_sub(size);
        }
    }
    
    fn evict_old_savepoints(&self, needed: usize) -> Result<()> {
        // Evict oldest compressed savepoints first
        // This is safe because compressed savepoints can be reconstructed from WAL
        
        // Implementation details...
        Ok(())
    }
}
```

---

## Error Handling

### Error Cases
1. **Savepoint outside transaction**
   ```sql
   SAVEPOINT sp1; -- ERROR: no active transaction
   ```

2. **Duplicate savepoint name**
   ```sql
   BEGIN;
   SAVEPOINT sp1;
   SAVEPOINT sp1; -- ERROR: savepoint 'sp1' already exists
   ```

3. **Rollback to non-existent savepoint**
   ```sql
   BEGIN;
   ROLLBACK TO SAVEPOINT nonexistent; -- ERROR: savepoint does not exist
   ```

4. **Release non-existent savepoint**
   ```sql
   BEGIN;
   RELEASE SAVEPOINT nonexistent; -- ERROR: savepoint does not exist
   ```

---

## Metrics to Track

- `pieskieo_savepoints_created_total` - Counter
- `pieskieo_savepoints_rolled_back_total` - Counter
- `pieskieo_savepoints_released_total` - Counter
- `pieskieo_savepoint_depth` - Histogram (nesting level)
- `pieskieo_savepoint_rollback_duration_ms` - Histogram
- `pieskieo_savepoint_memory_bytes` - Gauge (total memory used by savepoints)

---

## Implementation Checklist

- [ ] Add Savepoint struct to transaction.rs
- [ ] Implement create_savepoint in Transaction
- [ ] Implement rollback_to_savepoint in Transaction
- [ ] Implement release_savepoint in Transaction
- [ ] Add savepoint parsing to SQL parser
- [ ] Add execute_savepoint to engine
- [ ] Add execute_rollback_to_savepoint to engine
- [ ] Add execute_release_savepoint to engine
- [ ] Implement WAL undo_to_lsn
- [ ] Add savepoint integration tests
- [ ] Test nested savepoints (depth 10+)
- [ ] Test savepoint memory limits
- [ ] Test concurrent transactions with savepoints
- [ ] Add error handling for edge cases
- [ ] Document savepoint behavior
- [ ] Add metrics collection
- [ ] Performance benchmark vs PostgreSQL

---

## Production Deployment & Monitoring

### Configuration Options

```toml
[savepoints]
# Memory budget for all savepoints globally (MB)
global_memory_budget = 512

# Per-transaction savepoint limit (adaptive based on memory)
max_savepoints_per_txn = 200

# Compress savepoints older than (seconds)
compression_threshold_secs = 60

# Compression level (1-22, higher = better compression, slower)
compression_level = 3

# Enable distributed savepoints (multi-node transactions)
distributed_savepoints = true
```

### Monitoring Metrics

```rust
// Detailed metrics for production monitoring
metrics::counter!("pieskieo_savepoints_created_total", 
                  "distributed" => is_distributed).increment(1);
metrics::counter!("pieskieo_savepoints_rolled_back_total",
                  "distributed" => is_distributed).increment(1);
metrics::counter!("pieskieo_savepoints_released_total").increment(1);
metrics::counter!("pieskieo_savepoints_compressed_total").increment(1);

// Memory tracking
metrics::gauge!("pieskieo_savepoint_memory_bytes_total").set(total_memory);
metrics::gauge!("pieskieo_savepoint_memory_bytes_compressed").set(compressed_memory);
metrics::histogram!("pieskieo_savepoint_memory_per_txn_bytes").record(txn_memory);

// Performance metrics
metrics::histogram!("pieskieo_savepoint_create_duration_ms").record(create_duration);
metrics::histogram!("pieskieo_savepoint_rollback_duration_ms").record(rollback_duration);
metrics::histogram!("pieskieo_savepoint_depth").record(depth);

// Distributed savepoint metrics
metrics::histogram!("pieskieo_distributed_savepoint_coordination_ms").record(coord_time);
metrics::counter!("pieskieo_distributed_savepoint_failures_total",
                  "phase" => phase).increment(1);
```

### Crash Recovery for Savepoints

```rust
impl TransactionRecovery {
    pub fn recover_savepoints_after_crash(&self) -> Result<()> {
        // Scan WAL for active transactions with savepoints
        let active_txns = self.find_active_transactions()?;
        
        for txn_id in active_txns {
            let savepoint_records = self.find_savepoint_records(txn_id)?;
            
            if savepoint_records.is_empty() {
                continue;
            }
            
            // Reconstruct savepoint state from WAL
            let savepoints = self.reconstruct_savepoints(txn_id, savepoint_records)?;
            
            // Restore transaction with savepoints
            let mut txn = self.restore_transaction(txn_id)?;
            txn.savepoints_stack = savepoints;
            
            // Resume or abort based on policy
            self.resume_or_abort_transaction(txn)?;
        }
        
        Ok(())
    }
}
```

### Operational Runbook

#### Scenario 1: Savepoint Memory Exhaustion

**Symptoms**:
- `pieskieo_savepoint_memory_bytes_total` approaching limit
- `TooManySavepoints` errors in logs
- Slow transaction processing

**Resolution**:
```bash
# Check current memory usage
curl http://localhost:9090/metrics | grep savepoint_memory

# Increase global budget
pieskieo-cli config set savepoints.global_memory_budget 1024

# Force compression of old savepoints
pieskieo-cli admin compress-savepoints --age 30s

# Restart with new config (graceful)
systemctl reload pieskieo
```

#### Scenario 2: Distributed Savepoint Coordination Failure

**Symptoms**:
- `pieskieo_distributed_savepoint_failures_total` increasing
- Transactions hanging on SAVEPOINT command
- Network errors in logs

**Resolution**:
```bash
# Check node connectivity
pieskieo-cli cluster health

# Identify failed nodes
pieskieo-cli cluster nodes --status

# Abort hanging transactions
pieskieo-cli admin abort-txn --txn-id <uuid>

# Disable distributed savepoints temporarily
pieskieo-cli config set savepoints.distributed_savepoints false
```

---

**Created**: 2026-02-08  
**Author**: Implementation Team  
**Review Status**: Production-Ready
