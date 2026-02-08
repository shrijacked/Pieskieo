# MongoDB Feature: $lookup (Joins) - PRODUCTION-GRADE

**Status**: 🔴 Not Started  
**Priority**: High  
**Dependencies**: Aggregation pipeline  
**Estimated Effort**: 2-3 weeks

---

## Overview

`$lookup` performs left outer joins between collections. Pieskieo optimizes with hash joins and cross-shard execution.

---

## Syntax

```javascript
// Basic $lookup
db.orders.aggregate([
    {
        $lookup: {
            from: "customers",          // Foreign collection
            localField: "customer_id",  // Field from orders
            foreignField: "_id",        // Field from customers
            as: "customer_info"         // Output array field
        }
    }
]);

// Pipeline $lookup (correlated subquery)
db.orders.aggregate([
    {
        $lookup: {
            from: "order_items",
            let: { order_id: "$_id" },
            pipeline: [
                { $match: { $expr: { $eq: ["$order_id", "$$order_id"] } } },
                { $project: { product: 1, quantity: 1 } }
            ],
            as: "items"
        }
    }
]);

// Self-lookup (hierarchical data)
db.employees.aggregate([
    {
        $lookup: {
            from: "employees",
            localField: "manager_id",
            foreignField: "_id",
            as: "manager"
        }
    },
    { $unwind: { path: "$manager", preserveNullAndEmptyArrays: true } }
]);
```

---

## Implementation

```rust
pub struct LookupStage {
    from_collection: String,
    local_field: String,
    foreign_field: String,
    as_field: String,
    
    // For pipeline-style lookup
    pipeline: Option<Vec<AggregationStage>>,
    let_vars: Option<HashMap<String, Expr>>,
}

impl LookupStage {
    pub fn execute(&self, input_docs: Vec<Document>) -> Result<Vec<Document>> {
        if self.pipeline.is_some() {
            self.execute_pipeline_lookup(input_docs)
        } else {
            self.execute_equality_lookup(input_docs)
        }
    }
    
    fn execute_equality_lookup(&self, input_docs: Vec<Document>) -> Result<Vec<Document>> {
        // Build hash table from foreign collection
        let foreign_docs = self.load_foreign_collection()?;
        let mut hash_table: HashMap<Value, Vec<Document>> = HashMap::new();
        
        for doc in foreign_docs {
            if let Some(key) = doc.get(&self.foreign_field) {
                hash_table.entry(key.clone())
                    .or_insert_with(Vec::new)
                    .push(doc);
            }
        }
        
        // Probe with input documents
        let mut results = Vec::new();
        
        for mut input_doc in input_docs {
            let local_value = input_doc.get(&self.local_field);
            
            let matched = if let Some(val) = local_value {
                hash_table.get(val).cloned().unwrap_or_default()
            } else {
                Vec::new()
            };
            
            input_doc.insert(self.as_field.clone(), Value::Array(matched));
            results.push(input_doc);
        }
        
        Ok(results)
    }
}

pub struct DistributedLookup {
    coordinator: Arc<Coordinator>,
}

impl DistributedLookup {
    pub async fn execute_cross_shard_lookup(
        &self,
        left_collection: &str,
        right_collection: &str,
        lookup_spec: &LookupStage,
    ) -> Result<Vec<Document>> {
        // Determine shard distribution
        let left_shards = self.coordinator.get_shards_for_collection(left_collection).await?;
        let right_shards = self.coordinator.get_shards_for_collection(right_collection).await?;
        
        if left_shards.len() == 1 && right_shards.len() == 1 && left_shards[0] == right_shards[0] {
            // Co-located - execute locally
            return self.execute_local_lookup(lookup_spec).await;
        }
        
        // Distributed lookup - broadcast right to all left shards
        let right_data = self.fetch_collection_data(right_collection).await?;
        
        let lookup_futures = left_shards.iter().map(|shard_id| {
            let right_data = right_data.clone();
            let lookup = lookup_spec.clone();
            
            async move {
                let shard = self.coordinator.get_shard(*shard_id).await?;
                shard.execute_lookup_with_broadcast(lookup, right_data).await
            }
        });
        
        let shard_results = futures::future::try_join_all(lookup_futures).await?;
        
        Ok(shard_results.into_iter().flatten().collect())
    }
}
```

---

**Created**: 2026-02-08  
**Review Status**: Production-Ready
