# MongoDB Feature: Aggregation Pipeline

**Status**: 🔴 Not Started  
**Priority**: Critical  
**Dependencies**: Document storage (exists)  
**Estimated Effort**: 4-6 weeks

---

## Overview

The aggregation pipeline is MongoDB's framework for data processing and transformation. It processes documents through a sequence of stages, each performing a specific operation.

**Critical for Pieskieo**: This will be integrated into our unified query language, not standalone MongoDB syntax.

---

## Pipeline Stages (Priority Order)

### 1. $match Stage
**Purpose**: Filter documents (like WHERE in SQL)

**MongoDB Syntax**:
```javascript
db.products.aggregate([
  { $match: { category: "electronics", price: { $gt: 100 } } }
])
```

**Pieskieo Unified Query** (will be designed):
```
QUERY products
WHERE category = "electronics" AND price > 100
```

**Implementation**:
```rust
struct MatchStage {
    conditions: Vec<Condition>,
}

impl PipelineStage for MatchStage {
    fn execute(&self, input: Vec<Document>) -> Result<Vec<Document>> {
        input.into_iter()
            .filter(|doc| self.evaluate_conditions(doc))
            .collect()
    }
}
```

---

### 2. $project Stage
**Purpose**: Reshape documents, include/exclude fields, compute new fields

**MongoDB Syntax**:
```javascript
db.orders.aggregate([
  { $project: {
      customer_name: 1,
      total_with_tax: { $multiply: ["$total", 1.1] },
      _id: 0
  }}
])
```

**Pieskieo Unified Query**:
```
QUERY orders
SELECT customer_name, total * 1.1 AS total_with_tax
```

**Implementation**:
```rust
enum ProjectionExpr {
    Field { path: String, include: bool },
    Computed { alias: String, expr: ComputeExpr },
}

enum ComputeExpr {
    Add(Box<ComputeExpr>, Box<ComputeExpr>),
    Multiply(Box<ComputeExpr>, Box<ComputeExpr>),
    FieldRef(String),
    Literal(Value),
    Function { name: String, args: Vec<ComputeExpr> },
}

struct ProjectStage {
    projections: Vec<ProjectionExpr>,
}
```

---

### 3. $group Stage
**Purpose**: Group documents and compute aggregates

**MongoDB Syntax**:
```javascript
db.sales.aggregate([
  { $group: {
      _id: "$category",
      total_sales: { $sum: "$amount" },
      avg_price: { $avg: "$price" },
      count: { $count: {} }
  }}
])
```

**Pieskieo Unified Query**:
```
QUERY sales
GROUP BY category
COMPUTE {
    total_sales: SUM(amount),
    avg_price: AVG(price),
    count: COUNT()
}
```

**Implementation**:
```rust
struct GroupStage {
    group_by: Vec<String>, // field paths
    accumulators: Vec<Accumulator>,
}

enum Accumulator {
    Sum { field: String, alias: String },
    Avg { field: String, alias: String },
    Count { alias: String },
    Min { field: String, alias: String },
    Max { field: String, alias: String },
    Push { field: String, alias: String }, // collect into array
    AddToSet { field: String, alias: String }, // unique values
    First { field: String, alias: String },
    Last { field: String, alias: String },
}

impl GroupStage {
    fn execute(&self, input: Vec<Document>) -> Result<Vec<Document>> {
        let mut groups: HashMap<Value, GroupAccumulator> = HashMap::new();
        
        for doc in input {
            let key = self.extract_group_key(&doc)?;
            let accumulator = groups.entry(key).or_insert_with(GroupAccumulator::new);
            
            for acc in &self.accumulators {
                accumulator.update(acc, &doc)?;
            }
        }
        
        Ok(groups.into_iter().map(|(k, v)| v.finalize(k)).collect())
    }
}
```

---

### 4. $unwind Stage
**Purpose**: Deconstruct array fields into separate documents

**MongoDB Syntax**:
```javascript
db.orders.aggregate([
  { $unwind: "$items" }
])

// Input: { _id: 1, items: ["a", "b", "c"] }
// Output:
//   { _id: 1, items: "a" }
//   { _id: 1, items: "b" }
//   { _id: 1, items: "c" }
```

**Pieskieo Unified Query**:
```
QUERY orders
UNWIND items
```

**Implementation**:
```rust
struct UnwindStage {
    field_path: String,
    preserve_null_and_empty: bool,
    include_array_index: Option<String>,
}

impl UnwindStage {
    fn execute(&self, input: Vec<Document>) -> Result<Vec<Document>> {
        let mut output = Vec::new();
        
        for doc in input {
            if let Some(Value::Array(arr)) = doc.get(&self.field_path) {
                if arr.is_empty() && self.preserve_null_and_empty {
                    output.push(doc.clone());
                } else {
                    for (idx, item) in arr.iter().enumerate() {
                        let mut new_doc = doc.clone();
                        new_doc.insert(self.field_path.clone(), item.clone());
                        
                        if let Some(index_field) = &self.include_array_index {
                            new_doc.insert(index_field.clone(), Value::from(idx));
                        }
                        
                        output.push(new_doc);
                    }
                }
            } else if self.preserve_null_and_empty {
                output.push(doc);
            }
        }
        
        Ok(output)
    }
}
```

---

### 5. $lookup Stage (Joins)
**Purpose**: Join with another collection

**MongoDB Syntax**:
```javascript
db.orders.aggregate([
  { $lookup: {
      from: "customers",
      localField: "customer_id",
      foreignField: "_id",
      as: "customer_info"
  }}
])
```

**Pieskieo Unified Query**:
```
QUERY orders
JOIN customers ON orders.customer_id = customers._id AS customer_info
```

**Implementation**:
```rust
struct LookupStage {
    from_collection: String,
    local_field: String,
    foreign_field: String,
    as_field: String,
    // Advanced: pipeline to apply to joined docs
    pipeline: Option<Vec<Box<dyn PipelineStage>>>,
}

impl LookupStage {
    fn execute(&self, input: Vec<Document>, db: &PieskieoDb) -> Result<Vec<Document>> {
        let foreign_docs = db.get_all_docs(&self.from_collection)?;
        
        // Build index on foreign field for O(1) lookup
        let mut foreign_index: HashMap<Value, Vec<Document>> = HashMap::new();
        for doc in foreign_docs {
            if let Some(key) = doc.get(&self.foreign_field) {
                foreign_index.entry(key.clone()).or_default().push(doc);
            }
        }
        
        let mut output = Vec::new();
        for mut doc in input {
            let local_value = doc.get(&self.local_field).cloned();
            
            let matched_docs = if let Some(val) = local_value {
                foreign_index.get(&val).cloned().unwrap_or_default()
            } else {
                Vec::new()
            };
            
            // Apply sub-pipeline if specified
            let final_docs = if let Some(pipeline) = &self.pipeline {
                self.execute_pipeline(matched_docs, pipeline)?
            } else {
                matched_docs
            };
            
            doc.insert(self.as_field.clone(), Value::Array(
                final_docs.into_iter().map(|d| Value::Object(d)).collect()
            ));
            
            output.push(doc);
        }
        
        Ok(output)
    }
}
```

---

### 6. $sort Stage
**Purpose**: Sort documents

**Implementation**:
```rust
struct SortStage {
    fields: Vec<(String, SortOrder)>,
}

enum SortOrder {
    Ascending,
    Descending,
}

impl SortStage {
    fn execute(&self, mut input: Vec<Document>) -> Result<Vec<Document>> {
        input.sort_by(|a, b| {
            for (field, order) in &self.fields {
                let cmp = self.compare_values(
                    a.get(field),
                    b.get(field)
                );
                
                let cmp = match order {
                    SortOrder::Ascending => cmp,
                    SortOrder::Descending => cmp.reverse(),
                };
                
                if cmp != Ordering::Equal {
                    return cmp;
                }
            }
            Ordering::Equal
        });
        
        Ok(input)
    }
}
```

---

### 7. $limit and $skip Stages
**Purpose**: Pagination

**Implementation**:
```rust
struct LimitStage(usize);
struct SkipStage(usize);

impl LimitStage {
    fn execute(&self, input: Vec<Document>) -> Result<Vec<Document>> {
        Ok(input.into_iter().take(self.0).collect())
    }
}

impl SkipStage {
    fn execute(&self, input: Vec<Document>) -> Result<Vec<Document>> {
        Ok(input.into_iter().skip(self.0).collect())
    }
}
```

---

### 8. $addFields Stage
**Purpose**: Add new fields to documents

**MongoDB Syntax**:
```javascript
db.products.aggregate([
  { $addFields: {
      total_with_tax: { $multiply: ["$price", 1.1] },
      category_upper: { $toUpper: "$category" }
  }}
])
```

**Implementation**:
```rust
struct AddFieldsStage {
    fields: Vec<(String, ComputeExpr)>,
}

impl AddFieldsStage {
    fn execute(&self, input: Vec<Document>) -> Result<Vec<Document>> {
        input.into_iter().map(|mut doc| {
            for (field_name, expr) in &self.fields {
                let value = self.evaluate_expr(expr, &doc)?;
                doc.insert(field_name.clone(), value);
            }
            Ok(doc)
        }).collect()
    }
}
```

---

### 9. $facet Stage
**Purpose**: Run multiple pipelines in parallel

**MongoDB Syntax**:
```javascript
db.products.aggregate([
  { $facet: {
      "price_ranges": [
        { $bucket: { groupBy: "$price", boundaries: [0, 50, 100, 500, 1000] } }
      ],
      "categories": [
        { $group: { _id: "$category", count: { $sum: 1 } } }
      ],
      "top_products": [
        { $sort: { sales: -1 } },
        { $limit: 10 }
      ]
  }}
])
```

**Implementation**:
```rust
struct FacetStage {
    pipelines: HashMap<String, Vec<Box<dyn PipelineStage>>>,
}

impl FacetStage {
    fn execute(&self, input: Vec<Document>) -> Result<Vec<Document>> {
        let mut result = serde_json::Map::new();
        
        // Execute each sub-pipeline in parallel (potentially)
        for (facet_name, pipeline) in &self.pipelines {
            let facet_input = input.clone();
            let facet_output = self.execute_pipeline(facet_input, pipeline)?;
            result.insert(
                facet_name.clone(),
                Value::Array(facet_output.into_iter().map(Value::Object).collect())
            );
        }
        
        // Return single document with all facet results
        Ok(vec![result])
    }
}
```

---

### 10. $bucket Stage
**Purpose**: Group into ranges/buckets

**MongoDB Syntax**:
```javascript
db.products.aggregate([
  { $bucket: {
      groupBy: "$price",
      boundaries: [0, 50, 100, 500, 1000],
      default: "other",
      output: {
        count: { $sum: 1 },
        products: { $push: "$name" }
      }
  }}
])
```

**Implementation**:
```rust
struct BucketStage {
    group_by: String,
    boundaries: Vec<Value>,
    default_bucket: Option<Value>,
    output: Vec<Accumulator>,
}
```

---

## Pipeline Execution Engine

```rust
pub struct AggregationPipeline {
    stages: Vec<Box<dyn PipelineStage>>,
}

trait PipelineStage: Send + Sync {
    fn execute(&self, input: Vec<Document>, ctx: &ExecutionContext) -> Result<Vec<Document>>;
    fn name(&self) -> &str;
    fn can_push_down(&self) -> bool { false }
}

impl AggregationPipeline {
    pub fn new() -> Self {
        Self { stages: Vec::new() }
    }
    
    pub fn add_stage(&mut self, stage: Box<dyn PipelineStage>) {
        self.stages.push(stage);
    }
    
    pub fn execute(&self, input: Vec<Document>) -> Result<Vec<Document>> {
        let mut current = input;
        
        for (idx, stage) in self.stages.iter().enumerate() {
            tracing::debug!(
                stage = stage.name(),
                input_count = current.len(),
                "executing pipeline stage"
            );
            
            let start = std::time::Instant::now();
            current = stage.execute(current, &ctx)?;
            
            tracing::debug!(
                stage = stage.name(),
                output_count = current.len(),
                duration_ms = start.elapsed().as_millis(),
                "stage completed"
            );
            
            // Early termination for empty results
            if current.is_empty() && stage.can_short_circuit() {
                tracing::debug!("early termination - no documents to process");
                break;
            }
        }
        
        Ok(current)
    }
    
    pub fn optimize(&mut self) {
        // Optimization passes:
        // 1. Push $match as early as possible
        // 2. Combine adjacent $match stages
        // 3. Push $limit before $sort when possible
        // 4. Convert $match + $lookup to join pushdown
        
        self.push_match_stages();
        self.combine_adjacent_stages();
        self.reorder_limit_sort();
    }
}
```

---

## Advanced Features

### Expression Language
```rust
pub enum AggExpr {
    // Arithmetic
    Add(Vec<AggExpr>),
    Subtract(Box<AggExpr>, Box<AggExpr>),
    Multiply(Vec<AggExpr>),
    Divide(Box<AggExpr>, Box<AggExpr>),
    Mod(Box<AggExpr>, Box<AggExpr>),
    
    // Comparison
    Eq(Box<AggExpr>, Box<AggExpr>),
    Gt(Box<AggExpr>, Box<AggExpr>),
    Gte(Box<AggExpr>, Box<AggExpr>),
    Lt(Box<AggExpr>, Box<AggExpr>),
    Lte(Box<AggExpr>, Box<AggExpr>),
    Cmp(Box<AggExpr>, Box<AggExpr>),
    
    // Logical
    And(Vec<AggExpr>),
    Or(Vec<AggExpr>),
    Not(Box<AggExpr>),
    
    // String
    Concat(Vec<AggExpr>),
    Substr(Box<AggExpr>, Box<AggExpr>, Box<AggExpr>),
    ToUpper(Box<AggExpr>),
    ToLower(Box<AggExpr>),
    
    // Date
    DateToString { date: Box<AggExpr>, format: String },
    DateDiff { start: Box<AggExpr>, end: Box<AggExpr>, unit: String },
    
    // Conditional
    Cond { if_expr: Box<AggExpr>, then_expr: Box<AggExpr>, else_expr: Box<AggExpr> },
    IfNull(Box<AggExpr>, Box<AggExpr>),
    Switch { branches: Vec<(AggExpr, AggExpr)>, default: Box<AggExpr> },
    
    // Field reference
    Field(String),
    
    // Literal
    Literal(Value),
}
```

---

## Test Cases

### Test 1: Multi-Stage Pipeline
```javascript
// MongoDB syntax (for reference)
db.orders.aggregate([
  { $match: { status: "completed" } },
  { $unwind: "$items" },
  { $group: {
      _id: "$items.product_id",
      total_quantity: { $sum: "$items.quantity" },
      total_revenue: { $sum: { $multiply: ["$items.quantity", "$items.price"] } }
  }},
  { $sort: { total_revenue: -1 } },
  { $limit: 10 }
])
```

Expected: Top 10 products by revenue

---

## Performance Optimizations

1. **Index Usage**: $match stages should use indexes
2. **Projection Pushdown**: Only load fields needed by pipeline
3. **Parallel Execution**: $facet stages can run in parallel
4. **Memory Management**: Stream large pipelines, don't materialize all at once
5. **Query Rewriting**: Convert patterns to more efficient operations

---

## Metrics

- `pieskieo_pipeline_executions_total{stage="match"}` - Counter per stage type
- `pieskieo_pipeline_stage_duration_ms` - Histogram per stage
- `pieskieo_pipeline_documents_processed` - Counter
- `pieskieo_pipeline_optimizations_applied` - Counter

---

**Created**: 2026-02-08  
**Dependencies**: Document storage, expression evaluation  
**Next**: Integrate into unified query language
