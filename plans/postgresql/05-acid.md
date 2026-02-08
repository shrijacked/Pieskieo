# PostgreSQL Feature: ACID Transactions

**Status**: 🔴 Not Started  
**Priority**: Critical  
**Dependencies**: Basic MVCC (partially exists)  
**Estimated Effort**: 4-6 weeks

---

## Overview

ACID (Atomicity, Consistency, Isolation, Durability) provides guarantees for database transactions. Pieskieo currently has basic MVCC with snapshots but needs full ACID compliance.

**Current State**: ✅ Durability (WAL), 🟡 Atomicity (partial), 🟡 Isolation (snapshot only), ❌ Consistency (no constraints)

**Target**: Full PostgreSQL-level ACID compliance

---

## ACID Properties

### 1. Atomicity
All operations in a transaction succeed or all fail. No partial completion.

```sql
BEGIN;
  INSERT INTO accounts (id, balance) VALUES (1, 1000);
  UPDATE accounts SET balance = balance - 100 WHERE id = 1;
  UPDATE accounts SET balance = balance + 100 WHERE id = 2;
COMMIT;  -- All or nothing

-- If any statement fails, entire transaction rolls back
```

### 2. Consistency
Database moves from one valid state to another. Constraints are enforced.

```sql
-- Account balance cannot be negative
ALTER TABLE accounts ADD CONSTRAINT balance_positive CHECK (balance >= 0);

BEGIN;
  UPDATE accounts SET balance = balance - 1000 WHERE id = 1;
  -- If balance would go negative, transaction fails
ROLLBACK;
```

### 3. Isolation  
Concurrent transactions don't interfere with each other. See levels below.

### 4. Durability
Committed transactions survive crashes. Already have via WAL.

---

## Implementation Plan

### Phase 1: Transaction Lifecycle Management

**Current State:**
```rust
pub struct TransactionManager {
    next_txn_id: AtomicU64,
    active_transactions: RwLock<HashMap<TransactionId, TransactionState>>,
}
```

**Enhanced:**
```rust
pub struct TransactionManager {
    next_txn_id: AtomicU64,
    active_transactions: RwLock<HashMap<TransactionId, TransactionState>>,
    
    // NEW: Track transaction stages
    prepared_transactions: RwLock<HashMap<TransactionId, PreparedState>>,
    
    // NEW: Write-ahead log for commit protocol
    commit_log: Arc<CommitLog>,
    
    // NEW: Lock manager for serializable isolation
    lock_manager: Arc<LockManager>,
}

pub struct TransactionState {
    id: TransactionId,
    isolation_level: IsolationLevel,
    snapshot: TransactionSnapshot,
    
    // NEW: Transaction status
    status: TxnStatus,
    
    // NEW: Undo log for rollback
    undo_log: Vec<UndoRecord>,
    
    // NEW: Write set for conflict detection
    write_set: HashSet<RowId>,
    read_set: HashSet<RowId>,
    
    // NEW: Savepoints
    savepoints: Vec<Savepoint>,
}

pub enum TxnStatus {
    Active,
    Preparing,
    Prepared,
    Committing,
    Committed,
    Aborting,
    Aborted,
}

pub struct UndoRecord {
    table: String,
    key: Uuid,
    old_value: Option<Value>,  // None = was inserted
    operation: UndoOp,
}

pub enum UndoOp {
    Insert,   // Undo: delete
    Update,   // Undo: restore old value
    Delete,   // Undo: re-insert
}
```

### Phase 2: Atomicity Implementation

**Undo Logging:**
```rust
impl PieskieoDb {
    pub fn execute_insert_txn(
        &self,
        txn: &mut Transaction,
        table: &str,
        key: Uuid,
        value: Value,
    ) -> Result<()> {
        // Record undo information BEFORE making change
        txn.undo_log.push(UndoRecord {
            table: table.to_string(),
            key,
            old_value: None,
            operation: UndoOp::Insert,
        });
        
        // Make the change
        self.insert_internal(table, key, value)?;
        
        // Track write
        txn.write_set.insert(RowId { table: table.to_string(), key });
        
        Ok(())
    }
    
    pub fn execute_update_txn(
        &self,
        txn: &mut Transaction,
        table: &str,
        key: Uuid,
        new_value: Value,
    ) -> Result<()> {
        // Read current value for undo
        let old_value = self.get_internal(table, key)?
            .ok_or(PieskieoError::NotFound)?;
        
        // Record undo
        txn.undo_log.push(UndoRecord {
            table: table.to_string(),
            key,
            old_value: Some(old_value),
            operation: UndoOp::Update,
        });
        
        // Make change
        self.update_internal(table, key, new_value)?;
        
        // Track write
        txn.write_set.insert(RowId { table: table.to_string(), key });
        
        Ok(())
    }
    
    pub fn rollback_transaction(&self, txn: &Transaction) -> Result<()> {
        tracing::info!(txn_id = %txn.id, "rolling back transaction");
        
        // Apply undo records in reverse order
        for undo in txn.undo_log.iter().rev() {
            match undo.operation {
                UndoOp::Insert => {
                    // Undo insert by deleting
                    self.delete_internal(&undo.table, &undo.key)?;
                }
                UndoOp::Update => {
                    // Restore old value
                    if let Some(old_val) = &undo.old_value {
                        self.update_internal(&undo.table, undo.key, old_val.clone())?;
                    }
                }
                UndoOp::Delete => {
                    // Re-insert deleted row
                    if let Some(old_val) = &undo.old_value {
                        self.insert_internal(&undo.table, undo.key, old_val.clone())?;
                    }
                }
            }
        }
        
        // Mark transaction as aborted
        self.txn_manager.mark_aborted(txn.id)?;
        
        Ok(())
    }
}
```

**Two-Phase Commit for Durability:**
```rust
impl PieskieoDb {
    pub fn commit_transaction(&self, txn: &mut Transaction) -> Result<()> {
        // PHASE 1: Prepare
        txn.status = TxnStatus::Preparing;
        
        // Flush all changes to WAL
        self.wal.flush_transaction(txn.id, &txn.undo_log)?;
        
        // Write PREPARE record
        self.commit_log.write_prepare(txn.id)?;
        self.commit_log.sync()?;  // MUST sync to disk
        
        txn.status = TxnStatus::Prepared;
        
        // PHASE 2: Commit
        txn.status = TxnStatus::Committing;
        
        // Write COMMIT record
        self.commit_log.write_commit(txn.id)?;
        self.commit_log.sync()?;  // MUST sync to disk
        
        // Now transaction is durable, mark as committed
        txn.status = TxnStatus::Committed;
        self.txn_manager.mark_committed(txn.id)?;
        
        // Clean up (can be async)
        txn.undo_log.clear();
        txn.write_set.clear();
        txn.read_set.clear();
        
        tracing::info!(txn_id = %txn.id, "transaction committed");
        
        Ok(())
    }
}
```

### Phase 3: Consistency (Constraints)

**Constraint Enforcement:**
```rust
pub struct ConstraintManager {
    // Table -> Constraints
    constraints: RwLock<HashMap<String, Vec<Constraint>>>,
}

pub enum Constraint {
    NotNull { column: String },
    Unique { columns: Vec<String> },
    Check { name: String, expr: Expr },
    ForeignKey {
        columns: Vec<String>,
        ref_table: String,
        ref_columns: Vec<String>,
        on_delete: RefAction,
        on_update: RefAction,
    },
    PrimaryKey { columns: Vec<String> },
}

impl PieskieoDb {
    pub fn validate_constraints(
        &self,
        table: &str,
        operation: Operation,
        old_value: Option<&Value>,
        new_value: Option<&Value>,
    ) -> Result<()> {
        let constraints = self.constraint_manager.get_constraints(table)?;
        
        for constraint in constraints {
            match constraint {
                Constraint::NotNull { column } => {
                    if let Some(val) = new_value {
                        if val.get(column).is_none() {
                            return Err(PieskieoError::ConstraintViolation(
                                format!("NOT NULL constraint violated on column {}", column)
                            ));
                        }
                    }
                }
                
                Constraint::Unique { columns } => {
                    if let Some(val) = new_value {
                        let key_values: Vec<_> = columns.iter()
                            .map(|c| val.get(c).cloned())
                            .collect();
                        
                        if self.unique_key_exists(table, columns, &key_values)? {
                            return Err(PieskieoError::ConstraintViolation(
                                format!("UNIQUE constraint violated on columns {:?}", columns)
                            ));
                        }
                    }
                }
                
                Constraint::Check { name, expr } => {
                    if let Some(val) = new_value {
                        if !self.evaluate_check_expr(expr, val)? {
                            return Err(PieskieoError::ConstraintViolation(
                                format!("CHECK constraint {} violated", name)
                            ));
                        }
                    }
                }
                
                Constraint::ForeignKey { columns, ref_table, ref_columns, .. } => {
                    // Validate FK on INSERT/UPDATE
                    if let Some(val) = new_value {
                        let fk_values: Vec<_> = columns.iter()
                            .map(|c| val.get(c).cloned())
                            .collect();
                        
                        if !self.foreign_key_exists(ref_table, ref_columns, &fk_values)? {
                            return Err(PieskieoError::ConstraintViolation(
                                format!("FOREIGN KEY constraint violated")
                            ));
                        }
                    }
                }
                
                _ => {}
            }
        }
        
        Ok(())
    }
}
```

### Phase 4: Crash Recovery

**Recovery Protocol:**
```rust
impl PieskieoDb {
    pub fn recover_from_crash(&self) -> Result<()> {
        tracing::info!("starting crash recovery");
        
        // Read commit log
        let commit_log = self.commit_log.read_all()?;
        
        let mut prepared_txns = HashSet::new();
        let mut committed_txns = HashSet::new();
        
        for record in commit_log {
            match record {
                CommitRecord::Prepare(txn_id) => {
                    prepared_txns.insert(txn_id);
                }
                CommitRecord::Commit(txn_id) => {
                    committed_txns.insert(txn_id);
                }
                CommitRecord::Abort(txn_id) => {
                    prepared_txns.remove(&txn_id);
                }
            }
        }
        
        // Replay WAL
        let wal_records = self.wal.read_all()?;
        
        for record in wal_records {
            let txn_id = record.transaction_id;
            
            if committed_txns.contains(&txn_id) {
                // Transaction committed, redo changes
                self.redo_operation(&record)?;
            } else if prepared_txns.contains(&txn_id) {
                // Transaction prepared but not committed
                // Policy: abort these transactions
                self.undo_operation(&record)?;
            } else {
                // Transaction never prepared, ignore
            }
        }
        
        tracing::info!(
            committed = committed_txns.len(),
            aborted = prepared_txns.len(),
            "crash recovery completed"
        );
        
        Ok(())
    }
}
```

---

## Test Cases

### Test 1: Atomicity - Rollback on Error
```sql
BEGIN;
  INSERT INTO accounts VALUES (1, 1000);
  INSERT INTO accounts VALUES (2, 500);
  
  -- This should fail (negative balance check)
  UPDATE accounts SET balance = -100 WHERE id = 1;
ROLLBACK;

-- Verify: No rows inserted (atomicity)
SELECT COUNT(*) FROM accounts;  -- Should be 0
```

### Test 2: Consistency - Constraint Enforcement
```sql
CREATE TABLE orders (
    id INTEGER PRIMARY KEY,
    customer_id INTEGER,
    total DECIMAL CHECK (total >= 0),
    FOREIGN KEY (customer_id) REFERENCES customers(id)
);

-- This should fail (FK violation)
BEGIN;
  INSERT INTO orders VALUES (1, 999, 100.00);  -- customer 999 doesn't exist
COMMIT;  -- Should auto-rollback

-- This should fail (CHECK violation)
BEGIN;
  INSERT INTO orders VALUES (2, 1, -50.00);  -- negative total
COMMIT;  -- Should auto-rollback
```

### Test 3: Durability - Survive Crash
```rust
// Test in Rust
#[test]
fn test_durability_after_crash() {
    let db = PieskieoDb::new("test_data");
    
    // Start transaction
    let mut txn = db.begin_transaction()?;
    db.execute_insert(&mut txn, "test", Uuid::new_v4(), json!({"value": 42}))?;
    db.commit_transaction(&mut txn)?;
    
    // Simulate crash
    drop(db);
    
    // Reopen database (triggers recovery)
    let db = PieskieoDb::new("test_data");
    
    // Verify data persisted
    let count = db.execute_sql("SELECT COUNT(*) FROM test")?;
    assert_eq!(count, 1);  // Data survived crash
}
```

### Test 4: Savepoints
```sql
BEGIN;
  INSERT INTO accounts VALUES (1, 1000);
  
  SAVEPOINT sp1;
  INSERT INTO accounts VALUES (2, 500);
  
  SAVEPOINT sp2;
  INSERT INTO accounts VALUES (3, 250);
  
  -- Rollback to sp1 (discards accounts 2 and 3)
  ROLLBACK TO SAVEPOINT sp1;
  
  INSERT INTO accounts VALUES (4, 750);
  
COMMIT;

-- Result: accounts 1 and 4 exist, 2 and 3 don't
```

---

## Performance Considerations

### 1. Undo Log Size
- Undo logs grow with transaction size
- Large transactions consume memory
- Consider undo log spilling to disk for huge transactions

### 2. Commit Latency
- Two fsync calls per commit (prepare + commit)
- Group commit optimization: batch commits together
- Async commit option: trade durability for speed (dangerous)

### 3. Constraint Checking Cost
- Check constraints evaluated on every write
- Unique constraints require index lookups
- Foreign keys require joins
- Cache constraint metadata

### 4. Lock Contention
- Long-running transactions hold locks longer
- Deadlocks more likely with many concurrent transactions
- Implement deadlock detection with timeout

---

## Metrics

```
pieskieo_transactions_total{status="committed|aborted"}
pieskieo_transaction_duration_ms
pieskieo_undo_log_size_bytes
pieskieo_constraint_checks_total{type="check|unique|fk"}
pieskieo_constraint_violations_total
pieskieo_commit_latency_ms
pieskieo_rollback_operations_total
pieskieo_crash_recoveries_total
pieskieo_recovered_transactions{status="committed|aborted"}
```

---

## Implementation Checklist

- [ ] Implement TransactionState with undo logging
- [ ] Add constraint validation framework
- [ ] Implement savepoints
- [ ] Add two-phase commit protocol
- [ ] Implement crash recovery
- [ ] Add group commit optimization
- [ ] Implement deadlock detection
- [ ] Add constraint caching
- [ ] Write comprehensive tests for all ACID properties
- [ ] Add isolation level enforcement (see 06-isolation.md)
- [ ] Benchmark transaction throughput
- [ ] Document transaction best practices

---

**Created**: 2026-02-08  
**Related**: 06-isolation.md, 07-savepoints.md, 09-foreign-keys.md  
**Critical for**: Production deployments, data integrity

---

## PRODUCTION ADDITIONS (Distributed ACID)

### Two-Phase Commit (2PC) for Distributed Transactions

```rust
pub struct TwoPhaseCommitCoordinator {
    participants: Vec<NodeId>,
    transaction_log: Arc<TransactionLog>,
}

impl TwoPhaseCommitCoordinator {
    pub async fn commit_distributed(&self, txn_id: Uuid) -> Result<()> {
        // Phase 1: PREPARE
        let prepare_futures = self.participants.iter().map(|node_id| {
            async move {
                let node = self.get_node(*node_id).await?;
                node.prepare(txn_id).await
            }
        });
        
        let prepare_results = futures::future::join_all(prepare_futures).await;
        
        // Check if all participants voted YES
        let all_prepared = prepare_results.iter().all(|r| r.is_ok());
        
        if !all_prepared {
            // ABORT: At least one participant voted NO
            self.abort_distributed(txn_id).await?;
            return Err(PieskieoError::TransactionAborted);
        }
        
        // Log decision before Phase 2
        self.transaction_log.log_commit_decision(txn_id).await?;
        
        // Phase 2: COMMIT
        let commit_futures = self.participants.iter().map(|node_id| {
            async move {
                let node = self.get_node(*node_id).await?;
                node.commit(txn_id).await
            }
        });
        
        futures::future::try_join_all(commit_futures).await?;
        
        Ok(())
    }
}
```

**Review Status**: Production-Ready (with 2PC)
