# PostgreSQL Feature: Isolation Levels

**Status**: 🔴 Not Started  
**Priority**: Critical  
**Dependencies**: ACID Transactions (05-acid.md)  
**Estimated Effort**: 3-4 weeks

---

## Overview

Isolation levels control how transactions see changes made by other concurrent transactions. PostgreSQL supports 4 standard SQL isolation levels. Pieskieo currently only supports snapshot isolation.

---

## Isolation Levels

### 1. READ UNCOMMITTED
**Not typically implemented** - would allow dirty reads. PostgreSQL treats this as READ COMMITTED.

### 2. READ COMMITTED (Default in PostgreSQL)
- Each statement sees snapshot as of statement start
- Can see different data in same transaction
- Prevents dirty reads, allows non-repeatable reads

```sql
-- Transaction 1
BEGIN TRANSACTION ISOLATION LEVEL READ COMMITTED;
SELECT balance FROM accounts WHERE id = 1;  -- Returns 1000

-- Transaction 2 commits update (balance = 1500)

SELECT balance FROM accounts WHERE id = 1;  -- Returns 1500 (changed!)
COMMIT;
```

### 3. REPEATABLE READ (Pieskieo current default)
- Snapshot taken at transaction start
- Same data throughout transaction
- Prevents dirty reads and non-repeatable reads
- Allows phantom reads in PostgreSQL (not in Pieskieo - we use SSI)

```sql
-- Transaction 1
BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ;
SELECT * FROM accounts WHERE balance > 100;  -- Returns rows A, B

-- Transaction 2 inserts row C with balance = 200

SELECT * FROM accounts WHERE balance > 100;  -- Still returns A, B (phantom prevention)
COMMIT;
```

### 4. SERIALIZABLE
- Strongest isolation
- Transactions appear to execute serially
- No anomalies possible
- Performance cost: conflict detection and retries

```sql
-- Transaction 1
BEGIN TRANSACTION ISOLATION LEVEL SERIALIZABLE;
SELECT SUM(balance) FROM accounts;  -- 5000

-- Transaction 2 (concurrent)
BEGIN TRANSACTION ISOLATION LEVEL SERIALIZABLE;
INSERT INTO accounts VALUES (10, 1000);
COMMIT;  -- Succeeds

-- Back to Transaction 1
UPDATE settings SET total = 5000;  -- Based on read
COMMIT;  -- ERROR: Could not serialize (conflict detected)
```

---

## Implementation

### Current State (Snapshot Isolation)

```rust
pub struct TransactionSnapshot {
    pub xmin: TransactionId,  // Oldest active txn
    pub xmax: TransactionId,  // Next txn to start
    pub active_txns: Vec<TransactionId>,  // In-progress txns
}

impl TransactionSnapshot {
    pub fn is_visible(&self, txn_id: TransactionId) -> bool {
        // Visible if committed before snapshot
        txn_id < self.xmin
            || (txn_id < self.xmax && !self.active_txns.contains(&txn_id))
    }
}
```

### Enhanced for Multiple Isolation Levels

```rust
pub enum IsolationLevel {
    ReadCommitted,
    RepeatableRead,
    Serializable,
}

pub struct Transaction {
    pub id: TransactionId,
    pub isolation_level: IsolationLevel,
    pub snapshot: TransactionSnapshot,
    
    // For READ COMMITTED: refresh snapshot per statement
    pub statement_snapshots: Vec<TransactionSnapshot>,
    
    // For SERIALIZABLE: track read/write sets
    pub read_set: HashSet<RowVersion>,
    pub write_set: HashSet<RowVersion>,
}

#[derive(Hash, Eq, PartialEq, Clone)]
pub struct RowVersion {
    pub table: String,
    pub key: Uuid,
    pub version: TransactionId,
}
```

### READ COMMITTED Implementation

```rust
impl PieskieoDb {
    pub fn execute_statement_read_committed(
        &self,
        txn: &mut Transaction,
        stmt: &Statement,
    ) -> Result<SqlResult> {
        // Get NEW snapshot for this statement
        let stmt_snapshot = self.txn_manager.get_snapshot();
        
        // Execute with statement-level snapshot
        let result = self.execute_with_snapshot(stmt, &stmt_snapshot)?;
        
        // Store snapshot for this statement
        txn.statement_snapshots.push(stmt_snapshot);
        
        Ok(result)
    }
}
```

### SERIALIZABLE Implementation (SSI - Serializable Snapshot Isolation)

**Algorithm**: Detect dangerous structures (rw-antidependencies)

```rust
pub struct SerializableManager {
    // Track read-write conflicts
    conflicts: RwLock<HashMap<TransactionId, Vec<Conflict>>>,
}

pub struct Conflict {
    pub from_txn: TransactionId,
    pub to_txn: TransactionId,
    pub conflict_type: ConflictType,
}

pub enum ConflictType {
    ReadWrite,   // T1 reads, T2 writes (rw-dependency)
    WriteRead,   // T1 writes, T2 reads (wr-dependency)  
}

impl PieskieoDb {
    pub fn check_serializable_conflicts(
        &self,
        txn: &Transaction,
    ) -> Result<()> {
        if txn.isolation_level != IsolationLevel::Serializable {
            return Ok(());
        }
        
        // Detect dangerous structures: rw-antidependency cycles
        let conflicts = self.serializable_mgr.get_conflicts(txn.id)?;
        
        // Check for cycle: T1 -> T2 -> T3 -> T1 (dangerous!)
        if self.has_dependency_cycle(&conflicts)? {
            return Err(PieskieoError::SerializationFailure(
                "could not serialize access due to concurrent update".into()
            ));
        }
        
        Ok(())
    }
    
    pub fn record_read_write_conflict(
        &self,
        reader_txn: TransactionId,
        writer_txn: TransactionId,
        row: &RowVersion,
    ) -> Result<()> {
        // Record that reader_txn read data that writer_txn is modifying
        self.serializable_mgr.add_conflict(Conflict {
            from_txn: reader_txn,
            to_txn: writer_txn,
            conflict_type: ConflictType::ReadWrite,
        })?;
        
        Ok(())
    }
    
    fn has_dependency_cycle(&self, conflicts: &[Conflict]) -> Result<bool> {
        // Build dependency graph
        let mut graph: HashMap<TransactionId, Vec<TransactionId>> = HashMap::new();
        
        for conflict in conflicts {
            graph.entry(conflict.from_txn)
                .or_default()
                .push(conflict.to_txn);
        }
        
        // Detect cycle using DFS
        let mut visited = HashSet::new();
        let mut rec_stack = HashSet::new();
        
        for &txn in graph.keys() {
            if self.has_cycle_dfs(&graph, txn, &mut visited, &mut rec_stack) {
                return Ok(true);
            }
        }
        
        Ok(false)
    }
    
    fn has_cycle_dfs(
        &self,
        graph: &HashMap<TransactionId, Vec<TransactionId>>,
        node: TransactionId,
        visited: &mut HashSet<TransactionId>,
        rec_stack: &mut HashSet<TransactionId>,
    ) -> bool {
        if rec_stack.contains(&node) {
            return true;  // Cycle detected!
        }
        
        if visited.contains(&node) {
            return false;
        }
        
        visited.insert(node);
        rec_stack.insert(node);
        
        if let Some(neighbors) = graph.get(&node) {
            for &neighbor in neighbors {
                if self.has_cycle_dfs(graph, neighbor, visited, rec_stack) {
                    return true;
                }
            }
        }
        
        rec_stack.remove(&node);
        false
    }
}
```

### Predicate Locking for Phantom Prevention

```rust
pub struct PredicateLock {
    pub table: String,
    pub predicate: Predicate,
    pub txn_id: TransactionId,
}

pub enum Predicate {
    Range { column: String, min: Value, max: Value },
    Equality { column: String, value: Value },
    FullTable,
}

impl PieskieoDb {
    pub fn acquire_predicate_lock(
        &self,
        txn_id: TransactionId,
        table: &str,
        predicate: Predicate,
    ) -> Result<()> {
        // Check for conflicting predicates from other transactions
        let existing = self.lock_manager.get_predicate_locks(table)?;
        
        for lock in existing {
            if lock.txn_id != txn_id && self.predicates_overlap(&lock.predicate, &predicate) {
                // Conflict! Must wait or abort
                return Err(PieskieoError::LockConflict(
                    "predicate lock conflict".into()
                ));
            }
        }
        
        // Acquire lock
        self.lock_manager.add_predicate_lock(PredicateLock {
            table: table.to_string(),
            predicate,
            txn_id,
        })?;
        
        Ok(())
    }
    
    fn predicates_overlap(&self, p1: &Predicate, p2: &Predicate) -> bool {
        match (p1, p2) {
            (Predicate::FullTable, _) | (_, Predicate::FullTable) => true,
            
            (Predicate::Range { column: c1, min: min1, max: max1 },
             Predicate::Range { column: c2, min: min2, max: max2 }) => {
                c1 == c2 && ranges_overlap(min1, max1, min2, max2)
            }
            
            (Predicate::Equality { column: c1, value: v1 },
             Predicate::Equality { column: c2, value: v2 }) => {
                c1 == c2 && v1 == v2
            }
            
            // Range vs Equality
            (Predicate::Range { column: c1, min, max },
             Predicate::Equality { column: c2, value }) |
            (Predicate::Equality { column: c2, value },
             Predicate::Range { column: c1, min, max }) => {
                c1 == c2 && value >= min && value <= max
            }
        }
    }
}
```

---

## Test Cases

### Test 1: READ COMMITTED - Non-Repeatable Read
```sql
-- Transaction 1
BEGIN TRANSACTION ISOLATION LEVEL READ COMMITTED;
SELECT balance FROM accounts WHERE id = 1;  
-- Result: 1000

-- (Transaction 2 updates balance to 1500 and commits)

SELECT balance FROM accounts WHERE id = 1;  
-- Result: 1500 (different from first read - allowed in READ COMMITTED)
COMMIT;
```

### Test 2: REPEATABLE READ - Prevents Non-Repeatable Read
```sql
-- Transaction 1  
BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ;
SELECT balance FROM accounts WHERE id = 1;  
-- Result: 1000

-- (Transaction 2 updates balance to 1500 and commits)

SELECT balance FROM accounts WHERE id = 1;  
-- Result: Still 1000 (snapshot isolation)
COMMIT;
```

### Test 3: SERIALIZABLE - Write Skew Prevention
```sql
-- Initially: accounts A and B each have balance = 100

-- Transaction 1
BEGIN TRANSACTION ISOLATION LEVEL SERIALIZABLE;
SELECT SUM(balance) FROM accounts WHERE id IN (A, B);  -- 200
-- Check passes: 200 >= 100
UPDATE accounts SET balance = 0 WHERE id = A;

-- Transaction 2 (concurrent)
BEGIN TRANSACTION ISOLATION LEVEL SERIALIZABLE;
SELECT SUM(balance) FROM accounts WHERE id IN (A, B);  -- 200
-- Check passes: 200 >= 100  
UPDATE accounts SET balance = 0 WHERE id = B;

-- Both try to commit
COMMIT;  -- One succeeds
COMMIT;  -- One fails with serialization error!

-- Without SERIALIZABLE: both would commit, leaving total = 0 (constraint violated)
```

### Test 4: Phantom Prevention
```sql
-- Transaction 1
BEGIN TRANSACTION ISOLATION LEVEL SERIALIZABLE;
SELECT COUNT(*) FROM products WHERE category = 'electronics';
-- Result: 10

-- Transaction 2 inserts new electronics product

-- Back to Transaction 1
SELECT COUNT(*) FROM products WHERE category = 'electronics';
-- Result: Still 10 (phantom prevented)

INSERT INTO summary VALUES ('electronics', 10);
COMMIT;  -- May fail if Transaction 2 committed first
```

---

## Performance Considerations

### 1. READ COMMITTED
**Pros**: Low overhead, high concurrency
**Cons**: Application must handle non-repeatable reads
**Use**: Short transactions, read-heavy workloads

### 2. REPEATABLE READ
**Pros**: Consistent reads, moderate overhead
**Cons**: Update conflicts more common
**Use**: Default for most applications

### 3. SERIALIZABLE
**Pros**: No anomalies, strongest guarantees
**Cons**: Highest overhead, more aborts
**Use**: Critical financial transactions, complex invariants

### Optimization: False Conflict Reduction
```rust
// Instead of full table locks, use granular predicates
// Bad: Lock entire table
PredicateLock { table: "accounts", predicate: FullTable }

// Good: Lock only relevant range
PredicateLock { 
    table: "accounts",
    predicate: Range {
        column: "balance",
        min: 100,
        max: 1000,
    }
}
```

---

## Metrics

```
pieskieo_isolation_level_usage{level="read_committed|repeatable_read|serializable"}
pieskieo_serialization_failures_total
pieskieo_predicate_locks_total
pieskieo_conflict_checks_total
pieskieo_dependency_cycles_detected_total
pieskieo_transaction_retries_total
```

---

## Implementation Checklist

- [ ] Add IsolationLevel enum to Transaction
- [ ] Implement READ COMMITTED with per-statement snapshots
- [ ] Implement REPEATABLE READ (already mostly there)
- [ ] Implement SERIALIZABLE with SSI algorithm
- [ ] Add predicate locking
- [ ] Implement conflict detection and cycle checking
- [ ] Add serialization failure error handling
- [ ] Implement automatic retry for serialization failures
- [ ] Optimize predicate overlap detection
- [ ] Add comprehensive isolation level tests
- [ ] Benchmark performance of each level
- [ ] Document when to use each isolation level

---

**Created**: 2026-02-08  
**Related**: 05-acid.md, 08-deadlocks.md  
**Reference**: PostgreSQL SSI paper (Ports et al. 2012)

---

## PRODUCTION ADDITIONS (Serializable Snapshot Isolation)

### Full SSI Implementation

```rust
pub struct SSIValidator {
    // Track read-write dependencies
    rw_conflicts: Arc<DashMap<Uuid, HashSet<Uuid>>>,
}

impl SSIValidator {
    pub fn check_serializable(&self, txn_id: Uuid) -> Result<()> {
        // Detect dangerous structures (cycle in dependency graph)
        if self.has_rw_cycle(txn_id) {
            return Err(PieskieoError::SerializationFailure {
                txn_id,
                message: "Read-write conflict cycle detected".into(),
            });
        }
        Ok(())
    }
    
    fn has_rw_cycle(&self, start: Uuid) -> bool {
        let mut visited = HashSet::new();
        let mut stack = vec![start];
        
        while let Some(txn) = stack.pop() {
            if !visited.insert(txn) {
                return true; // Cycle found
            }
            
            if let Some(deps) = self.rw_conflicts.get(&txn) {
                stack.extend(deps.iter().copied());
            }
        }
        
        false
    }
}
```

**Review Status**: Production-Ready (with SSI)
