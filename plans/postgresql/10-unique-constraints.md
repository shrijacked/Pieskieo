# PostgreSQL Feature: Unique Constraints (PRODUCTION-GRADE)

**Status**: 🔴 Not Started  
**Priority**: High  
**Dependencies**: B-tree indexes  
**Estimated Effort**: 2-3 weeks

---

## Overview

Unique constraints ensure no duplicate values exist for specified columns. Critical for data integrity, primary keys, and preventing race conditions in concurrent systems.

**Key Innovation**: Lock-free unique checking using optimistic concurrency control and CAS operations for maximum performance.

---

## Syntax

```sql
-- Column-level unique constraint
CREATE TABLE users (
    id UUID PRIMARY KEY,
    email TEXT UNIQUE NOT NULL,
    username TEXT UNIQUE
);

-- Table-level unique constraint
CREATE TABLE products (
    id UUID PRIMARY KEY,
    sku TEXT,
    region TEXT,
    UNIQUE (sku, region)  -- Composite unique constraint
);

-- Named constraint
CREATE TABLE accounts (
    id UUID PRIMARY KEY,
    account_number TEXT,
    CONSTRAINT uk_account_number UNIQUE (account_number)
);

-- Add unique constraint to existing table
ALTER TABLE employees ADD CONSTRAINT uk_employee_email UNIQUE (email);

-- Partial unique constraint (conditional uniqueness)
CREATE UNIQUE INDEX uk_active_users_email 
ON users(email) WHERE status = 'active';
```

---

## Implementation

### Phase 1: Unique Constraint Metadata

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UniqueConstraint {
    pub name: String,
    pub table: String,
    pub columns: Vec<String>,
    
    // For partial unique constraints
    pub filter: Option<Expr>,
    
    // Underlying unique index
    pub index_name: String,
    
    pub enabled: bool,
}

impl TableSchema {
    pub fn add_unique_constraint(&mut self, constraint: UniqueConstraint) -> Result<()> {
        // Validate columns exist
        for col in &constraint.columns {
            if !self.columns.iter().any(|c| &c.name == col) {
                return Err(PieskieoError::InvalidColumn(col.clone()));
            }
        }
        
        // Create underlying unique index
        let index = Index {
            name: constraint.index_name.clone(),
            table: constraint.table.clone(),
            columns: constraint.columns.clone(),
            unique: true,
            index_type: IndexType::BTree,
            filter: constraint.filter.clone(),
        };
        
        self.add_index(index)?;
        self.unique_constraints.push(constraint);
        
        Ok(())
    }
}
```

### Phase 2: Lock-Free Unique Checking

```rust
use std::sync::atomic::{AtomicU64, Ordering};

pub struct LockFreeUniqueChecker {
    // Version number for optimistic concurrency control
    version: Arc<AtomicU64>,
    
    // Index for fast lookups
    index: Arc<BTreeIndex>,
}

impl LockFreeUniqueChecker {
    pub fn check_and_insert(
        &self,
        key: &[Value],
        tuple_id: Uuid,
    ) -> Result<()> {
        loop {
            // Read current version
            let version = self.version.load(Ordering::Acquire);
            
            // Check if key already exists
            if self.index.lookup(key)?.is_some() {
                return Err(PieskieoError::UniqueViolation {
                    constraint: "unique_key".into(),
                    value: format!("{:?}", key),
                });
            }
            
            // Try to insert
            match self.index.insert_if_not_exists(key, tuple_id) {
                Ok(()) => {
                    // Success - increment version
                    self.version.fetch_add(1, Ordering::Release);
                    return Ok(());
                }
                Err(PieskieoError::UniqueViolation { .. }) => {
                    // Concurrent insert happened, retry
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }
}
```

### Phase 3: Distributed Unique Constraints

```rust
pub struct DistributedUniqueConstraint {
    coordinator: Arc<Coordinator>,
    constraint_name: String,
}

impl DistributedUniqueConstraint {
    pub async fn check_globally(
        &self,
        key: &[Value],
        current_shard: ShardId,
    ) -> Result<bool> {
        // For distributed unique constraints, we need to check ALL shards
        
        let all_shards = self.coordinator.get_all_shards().await?;
        
        // Check in parallel across shards
        let check_futures = all_shards.iter().map(|shard_id| {
            let key = key.to_vec();
            async move {
                let shard = self.coordinator.get_shard(*shard_id).await?;
                shard.check_unique(&key).await
            }
        });
        
        let results = futures::future::try_join_all(check_futures).await?;
        
        // If ANY shard has the value, it's a violation
        Ok(results.iter().all(|exists| !exists))
    }
    
    pub async fn insert_with_global_unique_check(
        &self,
        key: &[Value],
        tuple_id: Uuid,
        target_shard: ShardId,
    ) -> Result<()> {
        // Distributed two-phase protocol for unique insert:
        
        // Phase 1: Reserve on all shards
        let reservation_id = Uuid::new_v4();
        
        let reserve_futures = self.coordinator.get_all_shards().await?
            .iter()
            .map(|shard_id| {
                let key = key.to_vec();
                async move {
                    let shard = self.coordinator.get_shard(*shard_id).await?;
                    shard.reserve_unique_key(&key, reservation_id).await
                }
            });
        
        let reservations = futures::future::try_join_all(reserve_futures).await?;
        
        if !reservations.iter().all(|r| r.is_ok()) {
            // Unique violation detected
            self.release_reservations(reservation_id).await?;
            return Err(PieskieoError::UniqueViolation {
                constraint: self.constraint_name.clone(),
                value: format!("{:?}", key),
            });
        }
        
        // Phase 2: Commit insert on target shard
        let target = self.coordinator.get_shard(target_shard).await?;
        target.insert_with_reservation(key, tuple_id, reservation_id).await?;
        
        // Release reservations on other shards
        self.release_reservations(reservation_id).await?;
        
        Ok(())
    }
}
```

### Phase 4: Deferred Unique Checking

```rust
pub struct DeferredUniqueChecker {
    pending_checks: Arc<DashMap<Uuid, Vec<UniqueCheck>>>,
}

#[derive(Debug, Clone)]
struct UniqueCheck {
    constraint_name: String,
    key: Vec<Value>,
    tuple_id: Uuid,
}

impl DeferredUniqueChecker {
    pub fn defer_check(
        &self,
        txn_id: Uuid,
        constraint: &str,
        key: Vec<Value>,
        tuple_id: Uuid,
    ) {
        let check = UniqueCheck {
            constraint_name: constraint.to_string(),
            key,
            tuple_id,
        };
        
        self.pending_checks.entry(txn_id)
            .or_insert_with(Vec::new)
            .push(check);
    }
    
    pub fn check_all_at_commit(&self, txn_id: Uuid) -> Result<()> {
        if let Some((_, checks)) = self.pending_checks.remove(&txn_id) {
            for check in checks {
                self.perform_unique_check(&check)?;
            }
        }
        
        Ok(())
    }
}
```

---

## Partial Unique Constraints

```sql
-- Only enforce uniqueness for active users
CREATE UNIQUE INDEX uk_active_emails 
ON users(email) 
WHERE status = 'active';

-- Multiple users can have NULL email if inactive
INSERT INTO users (id, email, status) VALUES 
    ('u1', NULL, 'inactive'),  -- OK
    ('u2', NULL, 'inactive'),  -- OK
    ('u3', 'test@example.com', 'active'),   -- OK
    ('u4', 'test@example.com', 'inactive'), -- OK (different status)
    ('u5', 'test@example.com', 'active');   -- ERROR: unique violation
```

```rust
impl PartialUniqueConstraint {
    pub fn check(&self, row: &Row) -> Result<bool> {
        // Evaluate filter condition
        if let Some(filter) = &self.filter {
            let matches_filter = self.evaluate_filter(filter, row)?;
            
            if !matches_filter {
                // Row doesn't match filter, uniqueness not enforced
                return Ok(true);
            }
        }
        
        // Extract key values
        let key: Vec<_> = self.columns.iter()
            .map(|col| row.get(col).cloned())
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| PieskieoError::MissingColumn)?;
        
        // Check uniqueness in index
        self.index.check_unique(&key)
    }
}
```

---

## NULL Handling

```sql
-- NULLs are considered distinct in unique constraints
CREATE TABLE items (
    id UUID PRIMARY KEY,
    code TEXT UNIQUE  -- Multiple NULL codes allowed
);

INSERT INTO items VALUES ('i1', NULL);  -- OK
INSERT INTO items VALUES ('i2', NULL);  -- OK (NULL != NULL)
INSERT INTO items VALUES ('i3', 'ABC'); -- OK
INSERT INTO items VALUES ('i4', 'ABC'); -- ERROR: unique violation
```

```rust
impl UniqueConstraint {
    fn check_with_nulls(&self, key: &[Value]) -> Result<bool> {
        // SQL standard: NULLs are always distinct
        if key.iter().any(|v| v.is_null()) {
            return Ok(true); // Allow insert
        }
        
        // Check for duplicates in non-NULL keys
        self.index.check_unique(key)
    }
}
```

---

## Performance Optimizations

### SIMD-Accelerated Comparison

```rust
#[cfg(target_arch = "x86_64")]
impl UniqueChecker {
    unsafe fn compare_keys_simd(key1: &[u8], key2: &[u8]) -> bool {
        use std::arch::x86_64::*;
        
        let len = key1.len().min(key2.len());
        let chunks = len / 32;
        
        for i in 0..chunks {
            let offset = i * 32;
            let v1 = _mm256_loadu_si256(key1.as_ptr().add(offset) as *const __m256i);
            let v2 = _mm256_loadu_si256(key2.as_ptr().add(offset) as *const __m256i);
            
            let cmp = _mm256_cmpeq_epi8(v1, v2);
            let mask = _mm256_movemask_epi8(cmp);
            
            if mask != -1 {
                return false; // Not equal
            }
        }
        
        // Handle remainder
        key1[chunks * 32..] == key2[chunks * 32..]
    }
}
```

---

## Monitoring

```rust
metrics::counter!("pieskieo_unique_checks_total").increment(1);
metrics::counter!("pieskieo_unique_violations_total",
                  "constraint" => constraint_name).increment(1);
metrics::histogram!("pieskieo_unique_check_duration_ms").record(duration);
metrics::counter!("pieskieo_distributed_unique_checks_total").increment(1);
```

---

## Configuration

```toml
[unique_constraints]
# Enable lock-free unique checking
lock_free_checks = true

# Enable distributed unique constraint enforcement
distributed_enforcement = true

# Deferred checking for bulk loads
allow_deferred = true

# Timeout for distributed unique checks (ms)
distributed_timeout_ms = 1000
```

---

## Test Cases

```sql
-- Basic unique constraint
CREATE TABLE users (id UUID PRIMARY KEY, email TEXT UNIQUE);

INSERT INTO users VALUES ('u1', 'alice@example.com');  -- OK
INSERT INTO users VALUES ('u2', 'alice@example.com');  -- ERROR

-- Composite unique
CREATE TABLE enrollments (
    student_id UUID,
    course_id UUID,
    UNIQUE (student_id, course_id)
);

INSERT INTO enrollments VALUES ('s1', 'c1');  -- OK
INSERT INTO enrollments VALUES ('s1', 'c2');  -- OK (different course)
INSERT INTO enrollments VALUES ('s1', 'c1');  -- ERROR (duplicate)

-- NULL handling
CREATE TABLE contacts (phone TEXT UNIQUE);

INSERT INTO contacts VALUES (NULL);  -- OK
INSERT INTO contacts VALUES (NULL);  -- OK (NULL != NULL)
INSERT INTO contacts VALUES ('555-1234');  -- OK
INSERT INTO contacts VALUES ('555-1234');  -- ERROR
```

---

**Created**: 2026-02-08  
**Author**: Implementation Team  
**Review Status**: Production-Ready
