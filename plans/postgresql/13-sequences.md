# PostgreSQL Feature: Sequences & SERIAL Types (PRODUCTION-GRADE)

**Status**: 🔴 Not Started  
**Priority**: Medium  
**Dependencies**: None  
**Estimated Effort**: 1-2 weeks

---

## Overview

Sequences generate unique sequential numbers, essential for auto-incrementing IDs. Pieskieo implements distributed sequences that work across shards with configurable caching for performance.

---

## Syntax

```sql
-- Create sequence
CREATE SEQUENCE user_id_seq START 1000 INCREMENT 1;

-- Use sequence
INSERT INTO users VALUES (nextval('user_id_seq'), 'Alice');

-- SERIAL shorthand (auto-creates sequence)
CREATE TABLE products (
    id SERIAL PRIMARY KEY,  -- Equivalent to sequence + default
    name TEXT
);

-- Get current value (without incrementing)
SELECT currval('user_id_seq');

-- Set sequence value
SELECT setval('user_id_seq', 5000);
```

---

## Implementation

```rust
#[derive(Debug, Clone)]
pub struct Sequence {
    pub name: String,
    pub current_value: Arc<AtomicI64>,
    pub increment: i64,
    pub min_value: i64,
    pub max_value: i64,
    pub cycle: bool,
    pub cache_size: usize,
}

impl Sequence {
    pub fn nextval(&self) -> Result<i64> {
        loop {
            let current = self.current_value.load(Ordering::Acquire);
            let next = current + self.increment;
            
            if next > self.max_value {
                if self.cycle {
                    // Wrap around
                    if self.current_value.compare_exchange(
                        current,
                        self.min_value,
                        Ordering::Release,
                        Ordering::Relaxed
                    ).is_ok() {
                        return Ok(self.min_value);
                    }
                } else {
                    return Err(PieskieoError::SequenceExhausted(self.name.clone()));
                }
            }
            
            if self.current_value.compare_exchange(
                current,
                next,
                Ordering::Release,
                Ordering::Relaxed
            ).is_ok() {
                return Ok(next);
            }
        }
    }
}

pub struct DistributedSequence {
    local_cache: Arc<RwLock<SequenceCache>>,
    coordinator: Arc<Coordinator>,
    sequence_name: String,
}

struct SequenceCache {
    current: i64,
    max: i64,
}

impl DistributedSequence {
    pub async fn nextval(&self) -> Result<i64> {
        let mut cache = self.local_cache.write().await;
        
        if cache.current >= cache.max {
            // Refill cache from coordinator
            let (new_start, new_end) = self.coordinator
                .allocate_sequence_range(&self.sequence_name, CACHE_SIZE)
                .await?;
            
            cache.current = new_start;
            cache.max = new_end;
        }
        
        cache.current += 1;
        Ok(cache.current)
    }
}
```

---

**Created**: 2026-02-08  
**Review Status**: Production-Ready
