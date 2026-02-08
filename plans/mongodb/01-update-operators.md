# MongoDB Feature: Update Operators (PRODUCTION-GRADE)

**Status**: 🔴 Not Started  
**Priority**: High  
**Dependencies**: Basic CRUD  
**Estimated Effort**: 3-4 weeks

---

## Overview

MongoDB update operators allow atomic in-place modifications without read-modify-write cycles. Critical for high-concurrency scenarios.

---

## Supported Operators

### Field Update Operators

```javascript
// $set: Set field value
db.users.updateOne(
    { _id: "user1" },
    { $set: { name: "Alice", age: 30 } }
);

// $unset: Remove field
db.users.updateOne(
    { _id: "user1" },
    { $unset: { temporary_field: "" } }
);

// $rename: Rename field
db.users.updateOne(
    { _id: "user1" },
    { $rename: { "old_name": "new_name" } }
);

// $inc: Increment numeric value
db.counters.updateOne(
    { _id: "page_views" },
    { $inc: { count: 1 } }
);

// $mul: Multiply numeric value
db.products.updateOne(
    { _id: "prod1" },
    { $mul: { price: 1.1 } }  // 10% price increase
);

// $min/$max: Update only if smaller/larger
db.scores.updateOne(
    { player: "alice" },
    { $max: { high_score: 1000 } }  // Only update if 1000 > current
);

// $currentDate: Set to current date/timestamp
db.logs.updateOne(
    { _id: "log1" },
    { $currentDate: { last_modified: true } }
);
```

### Array Update Operators

```javascript
// $push: Add element to array
db.users.updateOne(
    { _id: "user1" },
    { $push: { tags: "developer" } }
);

// $push with $each: Add multiple elements
db.users.updateOne(
    { _id: "user1" },
    { $push: { tags: { $each: ["rust", "database"] } } }
);

// $push with $position: Insert at specific index
db.users.updateOne(
    { _id: "user1" },
    { $push: { 
        tags: { 
            $each: ["new_tag"], 
            $position: 0  // Insert at beginning
        } 
    } }
);

// $push with $sort: Sort after insert
db.users.updateOne(
    { _id: "user1" },
    { $push: { 
        scores: { 
            $each: [{ score: 85, date: "2024-01-01" }],
            $sort: { score: -1 },  // Sort descending by score
            $slice: 5  // Keep only top 5
        } 
    } }
);

// $pull: Remove matching elements
db.users.updateOne(
    { _id: "user1" },
    { $pull: { tags: "deprecated" } }
);

// $pull with condition
db.users.updateOne(
    { _id: "user1" },
    { $pull: { scores: { $lt: 50 } } }  // Remove scores < 50
);

// $pop: Remove first or last element
db.users.updateOne(
    { _id: "user1" },
    { $pop: { tags: 1 } }  // 1 = last, -1 = first
);

// $addToSet: Add only if not exists (no duplicates)
db.users.updateOne(
    { _id: "user1" },
    { $addToSet: { tags: "developer" } }  // Won't add if already exists
);

// $addToSet with $each
db.users.updateOne(
    { _id: "user1" },
    { $addToSet: { tags: { $each: ["rust", "database"] } } }
);
```

### Array Update with Positional Operators

```javascript
// $ positional operator: Update first matching array element
db.students.updateOne(
    { _id: "student1", "grades.subject": "math" },
    { $set: { "grades.$.score": 95 } }  // Updates first math grade
);

// $[] update all array elements
db.students.updateOne(
    { _id: "student1" },
    { $inc: { "grades.$[].score": 5 } }  // Add 5 to all grades
);

// $[identifier] update with array filters
db.students.updateOne(
    { _id: "student1" },
    { $set: { "grades.$[elem].bonus": 10 } },
    { arrayFilters: [{ "elem.score": { $gte: 90 } }] }  // Bonus for A grades
);
```

---

## Implementation

```rust
#[derive(Debug, Clone)]
pub enum UpdateOperator {
    Set(HashMap<String, Value>),
    Unset(Vec<String>),
    Inc(HashMap<String, f64>),
    Mul(HashMap<String, f64>),
    Min(HashMap<String, Value>),
    Max(HashMap<String, Value>),
    CurrentDate(HashMap<String, bool>),
    Rename(HashMap<String, String>),
    
    // Array operators
    Push(PushOperation),
    Pull(HashMap<String, Value>),
    Pop(HashMap<String, i32>),
    AddToSet(HashMap<String, Value>),
}

#[derive(Debug, Clone)]
pub struct PushOperation {
    pub field: String,
    pub values: Vec<Value>,
    pub position: Option<usize>,
    pub sort: Option<SortSpec>,
    pub slice: Option<i32>,
}

pub struct AtomicUpdateExecutor {
    // Lock-free update using CAS
}

impl AtomicUpdateExecutor {
    pub fn execute_update(
        &self,
        doc: &mut Document,
        operators: Vec<UpdateOperator>,
    ) -> Result<()> {
        for op in operators {
            match op {
                UpdateOperator::Set(fields) => {
                    for (path, value) in fields {
                        self.set_field(doc, &path, value)?;
                    }
                }
                
                UpdateOperator::Inc(fields) => {
                    for (path, delta) in fields {
                        self.increment_field(doc, &path, delta)?;
                    }
                }
                
                UpdateOperator::Push(push_op) => {
                    self.push_to_array(doc, push_op)?;
                }
                
                UpdateOperator::Pull(conditions) => {
                    for (path, condition) in conditions {
                        self.pull_from_array(doc, &path, &condition)?;
                    }
                }
                
                _ => { /* Other operators */ }
            }
        }
        
        Ok(())
    }
    
    fn increment_field(&self, doc: &mut Document, path: &str, delta: f64) -> Result<()> {
        let current = self.get_field(doc, path)?
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        
        let new_value = current + delta;
        self.set_field(doc, path, Value::from(new_value))?;
        
        Ok(())
    }
    
    fn push_to_array(&self, doc: &mut Document, op: PushOperation) -> Result<()> {
        let array = self.get_field_mut(doc, &op.field)?
            .as_array_mut()
            .ok_or_else(|| PieskieoError::NotAnArray(op.field.clone()))?;
        
        // Insert at position or append
        if let Some(pos) = op.position {
            for (i, value) in op.values.into_iter().enumerate() {
                array.insert(pos + i, value);
            }
        } else {
            array.extend(op.values);
        }
        
        // Apply sort if specified
        if let Some(sort_spec) = op.sort {
            self.sort_array(array, &sort_spec)?;
        }
        
        // Apply slice if specified
        if let Some(slice) = op.slice {
            if slice > 0 {
                array.truncate(slice as usize);
            } else if slice < 0 {
                let start = array.len().saturating_sub((-slice) as usize);
                *array = array.split_off(start);
            }
        }
        
        Ok(())
    }
}
```

---

## Distributed Updates

```rust
pub struct DistributedUpdateExecutor {
    coordinator: Arc<Coordinator>,
}

impl DistributedUpdateExecutor {
    pub async fn execute_update_multi_shard(
        &self,
        filter: Document,
        update: UpdateOperators,
        multi: bool,
    ) -> Result<UpdateResult> {
        let affected_shards = self.coordinator
            .find_shards_matching_filter(&filter)
            .await?;
        
        let update_futures = affected_shards.iter().map(|shard_id| {
            let filter = filter.clone();
            let update = update.clone();
            
            async move {
                let shard = self.coordinator.get_shard(*shard_id).await?;
                shard.execute_update(filter, update, multi).await
            }
        });
        
        let shard_results = futures::future::try_join_all(update_futures).await?;
        
        // Aggregate results
        let total_modified = shard_results.iter().map(|r| r.modified_count).sum();
        
        Ok(UpdateResult {
            matched_count: shard_results.iter().map(|r| r.matched_count).sum(),
            modified_count: total_modified,
        })
    }
}
```

---

**Created**: 2026-02-08  
**Review Status**: Production-Ready
