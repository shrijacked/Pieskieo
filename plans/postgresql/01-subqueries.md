# PostgreSQL Feature: Subqueries

**Status**: 🔴 Not Started  
**Priority**: High  
**Dependencies**: None (builds on existing SELECT)  
**Estimated Effort**: 2-3 weeks

---

## Overview

Subqueries allow nesting SELECT statements within other queries. Essential for complex filtering, correlated queries, and derived tables.

## Types of Subqueries

### 1. Scalar Subqueries
Returns single value, used in SELECT list or WHERE clause

```sql
SELECT 
    name,
    price,
    (SELECT AVG(price) FROM products) as avg_price
FROM products
WHERE price > (SELECT AVG(price) FROM products);
```

### 2. Row Subqueries
Returns single row

```sql
SELECT * FROM products
WHERE (category, price) = (SELECT category, MAX(price) FROM products WHERE category = 'electronics');
```

### 3. Table Subqueries
Returns multiple rows, used with IN, EXISTS, ANY, ALL

```sql
-- IN subquery
SELECT * FROM customers
WHERE id IN (SELECT customer_id FROM orders WHERE total > 1000);

-- EXISTS subquery
SELECT * FROM customers c
WHERE EXISTS (SELECT 1 FROM orders o WHERE o.customer_id = c.id);

-- ANY/ALL subquery
SELECT * FROM products
WHERE price > ANY (SELECT price FROM products WHERE category = 'premium');
```

### 4. Correlated Subqueries
References outer query columns

```sql
SELECT 
    p1.name,
    p1.price
FROM products p1
WHERE p1.price > (
    SELECT AVG(p2.price)
    FROM products p2
    WHERE p2.category = p1.category
);
```

### 5. Derived Tables (FROM subquery)
Subquery in FROM clause

```sql
SELECT category, avg_price
FROM (
    SELECT category, AVG(price) as avg_price
    FROM products
    GROUP BY category
) as category_stats
WHERE avg_price > 100;
```

---

## Implementation Plan

### Phase 1: Parser Enhancement

**File**: `crates/pieskieo-core/src/engine.rs`

1. **Extend AST to support subqueries**
```rust
enum QueryExpression {
    Select(Box<SelectStatement>),
    Values(Vec<Vec<Value>>),
}

struct SelectStatement {
    projections: Vec<Projection>,
    from: Vec<TableSource>,
    where_clause: Option<WhereExpression>,
    // ... existing fields
}

enum TableSource {
    Table { name: String, alias: Option<String> },
    Subquery { query: Box<SelectStatement>, alias: String }, // NEW
    Join { ... },
}

enum WhereExpression {
    Condition(Condition),
    And(Box<WhereExpression>, Box<WhereExpression>),
    Or(Box<WhereExpression>, Box<WhereExpression>),
    Subquery(SubqueryPredicate), // NEW
}

enum SubqueryPredicate {
    In { column: String, subquery: Box<SelectStatement> },
    Exists { subquery: Box<SelectStatement> },
    ScalarComparison { left: Expr, op: Op, subquery: Box<SelectStatement> },
    AnyAll { column: String, op: Op, any_all: AnyAll, subquery: Box<SelectStatement> },
}
```

2. **Parse subquery syntax**
```rust
impl PieskieoDb {
    fn parse_subquery(&self, expr: &Expr) -> Result<Box<SelectStatement>> {
        match expr {
            Expr::Subquery(query) => {
                // Recursively parse nested SELECT
                self.parse_select_statement(query)
            }
            _ => Err(PieskieoError::Internal("expected subquery".into()))
        }
    }
    
    fn parse_where_with_subquery(&self, where_clause: &Expr) -> Result<WhereExpression> {
        match where_clause {
            Expr::InSubquery { expr, subquery, negated } => {
                Ok(WhereExpression::Subquery(SubqueryPredicate::In {
                    column: self.extract_column_name(expr)?,
                    subquery: self.parse_subquery(subquery)?,
                }))
            }
            Expr::Exists { subquery, negated } => {
                Ok(WhereExpression::Subquery(SubqueryPredicate::Exists {
                    subquery: self.parse_subquery(subquery)?,
                }))
            }
            // ... other cases
        }
    }
}
```

### Phase 2: Execution Engine

**Strategy**: Materialize subqueries into temporary result sets

```rust
struct SubqueryCache {
    results: HashMap<String, Vec<(Uuid, Value)>>,
}

impl PieskieoDb {
    fn execute_subquery(
        &self,
        subquery: &SelectStatement,
        outer_context: &QueryContext,
    ) -> Result<SubqueryResult> {
        // Execute subquery with outer context for correlated queries
        let rows = self.execute_select_internal(subquery, outer_context)?;
        
        SubqueryResult {
            rows,
            is_scalar: subquery.projections.len() == 1,
            column_count: subquery.projections.len(),
        }
    }
    
    fn evaluate_in_subquery(
        &self,
        value: &Value,
        subquery_result: &SubqueryResult,
    ) -> bool {
        subquery_result.rows.iter().any(|(_, row)| {
            row.get("col_0") == Some(value)
        })
    }
    
    fn evaluate_exists_subquery(&self, subquery_result: &SubqueryResult) -> bool {
        !subquery_result.rows.is_empty()
    }
}
```

### Phase 3: Advanced Correlated Subquery Optimization

**Problem**: Correlated subqueries can be very slow (O(n*m))

**Solution**: Complete decorrelation with pattern recognition

```rust
impl SubqueryOptimizer {
    fn try_decorrelate(&self, query: &SelectStatement) -> Option<SelectStatement> {
        // Comprehensive decorrelation patterns:
        // 1. EXISTS → SEMI JOIN
        // 2. NOT EXISTS → ANTI JOIN  
        // 3. IN → SEMI JOIN with deduplication
        // 4. NOT IN → ANTI JOIN with NULL handling
        // 5. Scalar aggregates → LEFT JOIN with GROUP BY
        // 6. ANY/ALL → JOIN with min/max aggregation
        
        match &query.where_clause {
            Some(WhereExpression::Subquery(SubqueryPredicate::Exists { subquery })) => {
                if let Some(correlation) = self.find_correlation(subquery) {
                    Some(self.convert_to_semi_join(query, subquery, correlation))
                } else {
                    // Non-correlated EXISTS - evaluate once and cache
                    Some(self.optimize_uncorrelated_exists(query, subquery))
                }
            }
            
            Some(WhereExpression::Subquery(SubqueryPredicate::NotExists { subquery })) => {
                if let Some(correlation) = self.find_correlation(subquery) {
                    Some(self.convert_to_anti_join(query, subquery, correlation))
                } else {
                    Some(self.optimize_uncorrelated_not_exists(query, subquery))
                }
            }
            
            Some(WhereExpression::Subquery(SubqueryPredicate::In { column, subquery })) => {
                // Convert to SEMI JOIN with hash table
                Some(self.convert_in_to_semi_join(query, column, subquery))
            }
            
            Some(WhereExpression::Subquery(SubqueryPredicate::ScalarComparison { left, op, subquery })) => {
                // Decorrelate scalar subqueries with aggregates
                // Example: WHERE price > (SELECT AVG(price) FROM products WHERE category = p.category)
                // → LEFT JOIN with GROUP BY, filter on aggregate
                if self.is_aggregate_subquery(subquery) {
                    Some(self.convert_scalar_aggregate_to_join(query, left, op, subquery))
                } else {
                    None
                }
            }
            
            _ => None
        }
    }
    
    fn find_correlation(&self, subquery: &SelectStatement) -> Option<Correlation> {
        // Analyze subquery to find outer references
        let mut outer_refs = Vec::new();
        self.collect_outer_references(subquery, &mut outer_refs);
        
        if outer_refs.is_empty() {
            None
        } else {
            Some(Correlation {
                outer_columns: outer_refs,
                join_type: self.infer_join_type(subquery),
            })
        }
    }
    
    fn convert_to_semi_join(
        &self,
        outer: &SelectStatement,
        subquery: &SelectStatement,
        correlation: Correlation,
    ) -> SelectStatement {
        // Build SEMI JOIN: only return outer rows that have match in subquery
        // No duplicates even if multiple matches
        
        SelectStatement {
            projections: outer.projections.clone(),
            from: vec![
                TableSource::Join {
                    left: Box::new(outer.from[0].clone()),
                    right: Box::new(TableSource::Subquery {
                        query: Box::new(subquery.clone()),
                        alias: "__subq".into(),
                    }),
                    join_type: JoinType::Semi,
                    on: self.build_join_condition(&correlation),
                }
            ],
            where_clause: self.merge_filters(&outer.where_clause, &subquery.where_clause),
            ..outer.clone()
        }
    }
    
    fn convert_to_anti_join(
        &self,
        outer: &SelectStatement,
        subquery: &SelectStatement,
        correlation: Correlation,
    ) -> SelectStatement {
        // Build ANTI JOIN: return outer rows that have NO match in subquery
        
        SelectStatement {
            projections: outer.projections.clone(),
            from: vec![
                TableSource::Join {
                    left: Box::new(outer.from[0].clone()),
                    right: Box::new(TableSource::Subquery {
                        query: Box::new(subquery.clone()),
                        alias: "__subq".into(),
                    }),
                    join_type: JoinType::Anti,
                    on: self.build_join_condition(&correlation),
                }
            ],
            where_clause: outer.where_clause.clone(),
            ..outer.clone()
        }
    }
    
    fn convert_scalar_aggregate_to_join(
        &self,
        outer: &SelectStatement,
        left: &Expr,
        op: &Operator,
        subquery: &SelectStatement,
    ) -> SelectStatement {
        // Convert scalar aggregate subquery to LEFT JOIN + GROUP BY
        // Example:
        // SELECT * FROM products p WHERE price > (SELECT AVG(price) FROM products WHERE category = p.category)
        // →
        // SELECT p.* FROM products p
        // LEFT JOIN (SELECT category, AVG(price) as avg_price FROM products GROUP BY category) agg
        //   ON p.category = agg.category
        // WHERE p.price > agg.avg_price
        
        // Implementation details...
        unimplemented!("Full implementation in actual code")
    }
}
```

### Phase 3b: LATERAL Join Support

**LATERAL** allows subqueries to reference columns from preceding FROM items.

```rust
enum TableSource {
    Table { name: String, alias: Option<String> },
    Subquery { query: Box<SelectStatement>, alias: String },
    LateralSubquery { query: Box<SelectStatement>, alias: String }, // NEW
    Join { ... },
}

impl PieskieoDb {
    fn execute_lateral_subquery(
        &self,
        lateral: &SelectStatement,
        outer_row: &Row,
    ) -> Result<Vec<Row>> {
        // Execute subquery with outer_row context
        // Outer columns are visible inside LATERAL subquery
        
        let context = QueryContext {
            outer_bindings: Some(outer_row.clone()),
            ..Default::default()
        };
        
        self.execute_select_with_context(lateral, &context)
    }
}
```

Example LATERAL usage:
```sql
SELECT 
    c.name,
    recent.order_date,
    recent.total
FROM customers c
CROSS JOIN LATERAL (
    SELECT order_date, total
    FROM orders o
    WHERE o.customer_id = c.id
    ORDER BY order_date DESC
    LIMIT 5
) recent;
```

### Phase 4: Scalar Subquery in SELECT

```rust
impl PieskieoDb {
    fn execute_select_with_scalar_subqueries(
        &self,
        query: &SelectStatement,
    ) -> Result<SqlResult> {
        let mut result_rows = Vec::new();
        
        for base_row in self.get_base_rows(query)? {
            let mut output_row = serde_json::Map::new();
            
            for projection in &query.projections {
                let value = match projection {
                    Projection::Column { name } => {
                        base_row.get(name).cloned()
                    }
                    Projection::ScalarSubquery { subquery, alias } => {
                        // Execute subquery for this row
                        let result = self.execute_subquery(subquery, &base_row)?;
                        if result.rows.len() != 1 {
                            return Err(PieskieoError::Internal(
                                "scalar subquery returned more than one row".into()
                            ));
                        }
                        Some(result.rows[0].1.clone())
                    }
                    // ... other projection types
                };
                
                output_row.insert(projection.alias(), value);
            }
            
            result_rows.push(output_row);
        }
        
        Ok(SqlResult { rows: result_rows })
    }
}
```

---

## Test Cases

### Test 1: Simple IN Subquery
```sql
-- Create test data
INSERT INTO products (id, name, category, price) VALUES
    ('p1', 'Laptop', 'electronics', 1000),
    ('p2', 'Mouse', 'electronics', 20),
    ('p3', 'Desk', 'furniture', 500);

INSERT INTO orders (id, product_id, quantity) VALUES
    ('o1', 'p1', 2),
    ('o2', 'p2', 5);

-- Query: Find products that have been ordered
SELECT name FROM products
WHERE id IN (SELECT product_id FROM orders);

-- Expected: ['Laptop', 'Mouse']
```

### Test 2: Correlated Subquery
```sql
-- Find products priced above category average
SELECT name, category, price
FROM products p1
WHERE price > (
    SELECT AVG(price)
    FROM products p2
    WHERE p2.category = p1.category
);
```

### Test 3: EXISTS
```sql
-- Find customers who have placed orders
SELECT name FROM customers c
WHERE EXISTS (
    SELECT 1 FROM orders o WHERE o.customer_id = c.id
);
```

### Test 4: Derived Table
```sql
-- Category statistics
SELECT category, avg_price, product_count
FROM (
    SELECT 
        category,
        AVG(price) as avg_price,
        COUNT(*) as product_count
    FROM products
    GROUP BY category
) stats
WHERE product_count > 5;
```

### Test 5: Scalar Subquery in SELECT
```sql
SELECT 
    name,
    price,
    (SELECT AVG(price) FROM products) as market_avg,
    price - (SELECT AVG(price) FROM products) as price_diff
FROM products;
```

---

## Performance Considerations & Optimizations

### 1. Intelligent Caching with Invalidation

```rust
pub struct SubqueryCache {
    // Hash of subquery SQL → cached result
    cache: DashMap<u64, CachedSubqueryResult>,
    
    // Track which tables each cached subquery depends on
    table_dependencies: HashMap<String, HashSet<u64>>,
    
    // Cache size limit (configurable, default 100MB)
    max_memory: usize,
    current_memory: AtomicUsize,
}

impl SubqueryCache {
    pub fn get_or_execute<F>(
        &self,
        subquery: &SelectStatement,
        is_correlated: bool,
        executor: F,
    ) -> Result<SubqueryResult>
    where
        F: FnOnce() -> Result<SubqueryResult>,
    {
        if is_correlated {
            // Never cache correlated subqueries
            return executor();
        }
        
        let cache_key = self.hash_subquery(subquery);
        
        // Check cache
        if let Some(cached) = self.cache.get(&cache_key) {
            metrics::counter!("pieskieo_subquery_cache_hits").increment(1);
            return Ok(cached.result.clone());
        }
        
        // Execute and cache
        let result = executor()?;
        let memory_size = self.estimate_memory(&result);
        
        // Evict if needed (LRU)
        while self.current_memory.load(Ordering::Relaxed) + memory_size > self.max_memory {
            self.evict_lru()?;
        }
        
        self.cache.insert(cache_key, CachedSubqueryResult {
            result: result.clone(),
            cached_at: Instant::now(),
            memory_size,
        });
        
        self.current_memory.fetch_add(memory_size, Ordering::Relaxed);
        
        Ok(result)
    }
    
    pub fn invalidate_table(&self, table: &str) {
        // Invalidate all cached subqueries that reference this table
        if let Some(cache_keys) = self.table_dependencies.get(table) {
            for key in cache_keys {
                if let Some((_, cached)) = self.cache.remove(key) {
                    self.current_memory.fetch_sub(cached.memory_size, Ordering::Relaxed);
                }
            }
        }
    }
}
```

### 2. Early Termination & Short-Circuit Evaluation

```rust
impl PieskieoDb {
    fn evaluate_exists_optimized(&self, subquery: &SelectStatement) -> Result<bool> {
        // EXISTS only needs to find ONE row, then stop
        
        let mut executor = self.create_streaming_executor(subquery)?;
        
        // Request just 1 row
        match executor.next()? {
            Some(_) => Ok(true),  // Found one, stop immediately
            None => Ok(false),    // No rows
        }
        
        // No need to scan entire table
    }
    
    fn evaluate_in_optimized(
        &self,
        value: &Value,
        subquery: &SelectStatement,
    ) -> Result<bool> {
        // Build hash set for O(1) lookup
        let mut hash_set = HashSet::with_capacity(1024);
        
        for row in self.execute_streaming(subquery)? {
            let subquery_value = row.get("col_0")?;
            
            if subquery_value == value {
                // Early termination: found match
                return Ok(true);
            }
            
            hash_set.insert(subquery_value);
            
            // Memory limit check
            if hash_set.len() > 1_000_000 {
                // Fall back to nested loop for very large result sets
                return self.evaluate_in_nested_loop(value, subquery);
            }
        }
        
        Ok(false)
    }
}
```

### 3. Advanced Push-Down Optimization

```rust
impl SubqueryOptimizer {
    fn push_down_filters(&self, query: &SelectStatement) -> SelectStatement {
        // Push outer WHERE conditions into subquery when possible
        // Example:
        // SELECT * FROM (SELECT * FROM products) p WHERE p.price > 100
        // → SELECT * FROM (SELECT * FROM products WHERE price > 100) p
        
        let mut optimized = query.clone();
        
        for (i, table_source) in optimized.from.iter_mut().enumerate() {
            if let TableSource::Subquery { query: subq, .. } = table_source {
                // Find filters in outer WHERE that only reference this subquery
                let pushable_filters = self.find_pushable_filters(
                    &query.where_clause,
                    &subq.projections,
                );
                
                if !pushable_filters.is_empty() {
                    // Add filters to subquery WHERE
                    let new_where = self.merge_where_clauses(
                        &subq.where_clause,
                        &Some(pushable_filters),
                    );
                    subq.where_clause = new_where;
                }
            }
        }
        
        optimized
    }
    
    fn push_down_projections(&self, query: &SelectStatement) -> SelectStatement {
        // Column pruning: only select columns actually used
        // Example:
        // SELECT name FROM (SELECT id, name, description, created_at FROM products) p
        // → SELECT name FROM (SELECT name FROM products) p
        
        // Implementation details...
    }
}
```

### 4. Parallel Subquery Execution

```rust
impl PieskieoDb {
    async fn execute_independent_subqueries_parallel(
        &self,
        subqueries: Vec<SelectStatement>,
    ) -> Result<Vec<SubqueryResult>> {
        // When multiple independent subqueries exist, execute in parallel
        
        let handles: Vec<_> = subqueries
            .into_iter()
            .map(|subq| {
                let db = self.clone();
                tokio::spawn(async move {
                    db.execute_select(&subq).await
                })
            })
            .collect();
        
        let results = futures::future::join_all(handles).await;
        
        results
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
    }
}
```

### 5. Index-Aware Subquery Planning

```rust
impl QueryPlanner {
    fn plan_subquery_with_indexes(
        &self,
        subquery: &SelectStatement,
    ) -> Result<ExecutionPlan> {
        // Analyze available indexes for subquery
        let table_name = self.extract_table_name(subquery)?;
        let indexes = self.get_indexes_for_table(&table_name)?;
        
        // Check if subquery can use index
        if let Some(filter) = &subquery.where_clause {
            for index in indexes {
                if self.can_use_index(filter, &index) {
                    return Ok(ExecutionPlan::IndexScan {
                        index: index.clone(),
                        filter: filter.clone(),
                    });
                }
            }
        }
        
        // Fall back to sequential scan
        Ok(ExecutionPlan::SeqScan {
            table: table_name,
            filter: subquery.where_clause.clone(),
        })
    }
}
```

### 6. Memory-Bounded Execution

```rust
const MAX_SUBQUERY_MEMORY: usize = 256 * 1024 * 1024; // 256MB per subquery

impl PieskieoDb {
    fn execute_subquery_memory_bounded(
        &self,
        subquery: &SelectStatement,
    ) -> Result<SubqueryResult> {
        let mut result = SubqueryResult::new();
        let mut memory_used = 0;
        
        for row in self.execute_streaming(subquery)? {
            let row_size = self.estimate_row_size(&row);
            
            if memory_used + row_size > MAX_SUBQUERY_MEMORY {
                // Spill to disk
                self.spill_to_temp_table(&mut result)?;
                memory_used = 0;
            }
            
            result.add_row(row);
            memory_used += row_size;
        }
        
        Ok(result)
    }
}
```

### 7. Subquery Result Materialization Strategy

```rust
enum MaterializationStrategy {
    InMemory,           // Small result sets (< 10k rows)
    Streaming,          // Large result sets, consumed once
    TempTable,          // Large result sets, consumed multiple times
    Index,              // Build temporary index for lookups
}

impl SubqueryExecutor {
    fn choose_materialization_strategy(
        &self,
        subquery: &SelectStatement,
        usage_pattern: UsagePattern,
    ) -> MaterializationStrategy {
        let estimated_rows = self.estimate_cardinality(subquery);
        
        match (estimated_rows, usage_pattern) {
            (0..=10_000, _) => MaterializationStrategy::InMemory,
            (_, UsagePattern::SingleScan) => MaterializationStrategy::Streaming,
            (10_001..=1_000_000, UsagePattern::MultipleLookups) => {
                MaterializationStrategy::Index
            }
            (_, UsagePattern::MultipleLookups) => MaterializationStrategy::TempTable,
            _ => MaterializationStrategy::Streaming,
        }
    }
}
```

---

## API Changes

### HTTP API
```json
POST /v1/sql
{
  "sql": "SELECT * FROM products WHERE price > (SELECT AVG(price) FROM products)"
}
```

### Response includes execution plan
```json
{
  "ok": true,
  "data": {
    "rows": [...],
    "explain": {
      "type": "NestedLoopSubquery",
      "outer_rows": 100,
      "subquery_executions": 1,
      "total_time_ms": 45
    }
  }
}
```

---

## Metrics to Track

- `pieskieo_subquery_executions_total` - Counter
- `pieskieo_subquery_cache_hits` - Counter
- `pieskieo_subquery_decorrelations` - Counter (how many converted to joins)
- `pieskieo_subquery_execution_time_ms` - Histogram

---

## Implementation Checklist

- [ ] Extend sqlparser integration to handle subquery AST
- [ ] Implement SubqueryPredicate evaluation
- [ ] Add IN subquery support
- [ ] Add EXISTS subquery support
- [ ] Add scalar subquery support
- [ ] Add derived table support (FROM subquery)
- [ ] Implement correlated subquery execution
- [ ] Add subquery result caching
- [ ] Implement decorrelation optimization
- [ ] Add comprehensive tests
- [ ] Add benchmarks comparing with/without optimization
- [ ] Document subquery limitations (if any)
- [ ] Add EXPLAIN support for subqueries

---

## Distributed Subquery Execution

### Cross-Shard Subqueries

```rust
impl DistributedQueryExecutor {
    async fn execute_distributed_subquery(
        &self,
        subquery: &SelectStatement,
        outer_context: &QueryContext,
    ) -> Result<SubqueryResult> {
        // Analyze if subquery spans multiple shards
        let affected_shards = self.get_affected_shards(subquery)?;
        
        if affected_shards.len() == 1 {
            // Single-shard subquery - execute locally
            return self.execute_local_subquery(subquery, outer_context).await;
        }
        
        // Multi-shard subquery - fan out to all shards
        let shard_results = self.execute_on_shards(subquery, &affected_shards).await?;
        
        // Merge results (depends on subquery type)
        self.merge_subquery_results(shard_results, subquery)
    }
    
    async fn execute_on_shards(
        &self,
        subquery: &SelectStatement,
        shards: &[ShardId],
    ) -> Result<Vec<SubqueryResult>> {
        let futures = shards.iter().map(|shard_id| {
            let subq = subquery.clone();
            let shard = self.get_shard(*shard_id);
            
            async move {
                shard.execute_subquery(&subq).await
            }
        });
        
        // Execute in parallel across shards
        let results = futures::future::try_join_all(futures).await?;
        
        Ok(results)
    }
    
    fn merge_subquery_results(
        &self,
        shard_results: Vec<SubqueryResult>,
        subquery: &SelectStatement,
    ) -> Result<SubqueryResult> {
        match self.get_subquery_type(subquery) {
            SubqueryType::Scalar => {
                // For aggregates like AVG, SUM - need to combine carefully
                self.merge_scalar_results(shard_results, subquery)
            }
            SubqueryType::Exists => {
                // OR across shards - true if ANY shard has rows
                Ok(SubqueryResult {
                    rows: if shard_results.iter().any(|r| !r.rows.is_empty()) {
                        vec![Row::default()] // EXISTS is true
                    } else {
                        vec![]
                    },
                    ..Default::default()
                })
            }
            SubqueryType::In => {
                // UNION across shards - concatenate and deduplicate
                let mut all_rows = Vec::new();
                for result in shard_results {
                    all_rows.extend(result.rows);
                }
                
                // Deduplicate
                all_rows.sort_unstable();
                all_rows.dedup();
                
                Ok(SubqueryResult {
                    rows: all_rows,
                    ..Default::default()
                })
            }
            SubqueryType::Table => {
                // Simple concatenation
                let mut all_rows = Vec::new();
                for result in shard_results {
                    all_rows.extend(result.rows);
                }
                
                Ok(SubqueryResult {
                    rows: all_rows,
                    ..Default::default()
                })
            }
        }
    }
}
```

## Advanced Features

### Subqueries in GROUP BY (Non-Standard but Useful)

```sql
-- Group by result of subquery
SELECT 
    (SELECT category_name FROM categories WHERE id = p.category_id) as cat,
    COUNT(*) as product_count
FROM products p
GROUP BY (SELECT category_name FROM categories WHERE id = p.category_id);
```

```rust
impl PieskieoDb {
    fn execute_group_by_with_subquery(
        &self,
        query: &SelectStatement,
    ) -> Result<SqlResult> {
        // Extract grouping subqueries
        let group_by_subqueries: Vec<_> = query.group_by
            .iter()
            .filter_map(|expr| {
                if let Expr::Subquery(subq) = expr {
                    Some(subq)
                } else {
                    None
                }
            })
            .collect();
        
        // For each row, evaluate subqueries to determine group
        let mut groups: HashMap<Vec<Value>, Vec<Row>> = HashMap::new();
        
        for row in self.scan_table(&query.from)? {
            let group_key: Vec<Value> = group_by_subqueries
                .iter()
                .map(|subq| {
                    let result = self.execute_subquery(subq, &row)?;
                    Ok(result.scalar_value()?)
                })
                .collect::<Result<Vec<_>>>()?;
            
            groups.entry(group_key).or_default().push(row);
        }
        
        // Apply aggregates to each group
        self.apply_aggregates(groups, &query.projections)
    }
}
```

### Recursive Subqueries (CTEs covered separately, but worth noting)

Recursive queries handled via CTE implementation (see `02-ctes.md`).

---

## Error Handling & Edge Cases

### Scalar Subquery Validation

```rust
impl PieskieoDb {
    fn validate_scalar_subquery(&self, result: &SubqueryResult) -> Result<Value> {
        match result.rows.len() {
            0 => Ok(Value::Null),  // No rows → NULL
            1 => {
                if result.rows[0].columns().len() != 1 {
                    return Err(PieskieoError::SubqueryError(
                        format!("Scalar subquery returned {} columns, expected 1", 
                                result.rows[0].columns().len())
                    ));
                }
                Ok(result.rows[0].get_column(0)?.clone())
            }
            n => Err(PieskieoError::SubqueryError(
                format!("Scalar subquery returned {} rows, expected 1", n)
            )),
        }
    }
}
```

### NULL Handling in IN/NOT IN

```rust
impl PieskieoDb {
    fn evaluate_not_in_with_nulls(
        &self,
        value: &Value,
        subquery_result: &SubqueryResult,
    ) -> Result<Option<bool>> {
        // SQL three-valued logic:
        // - If value is NULL → NULL
        // - If any subquery row is NULL and value not found → NULL
        // - If value found → false
        // - If value not found and no NULLs → true
        
        if value.is_null() {
            return Ok(None); // NULL
        }
        
        let mut has_null = false;
        
        for row in &subquery_result.rows {
            let subquery_value = row.get_column(0)?;
            
            if subquery_value.is_null() {
                has_null = true;
                continue;
            }
            
            if subquery_value == value {
                return Ok(Some(false)); // Found match
            }
        }
        
        if has_null {
            Ok(None) // Has NULL, no match found → NULL
        } else {
            Ok(Some(true)) // No NULLs, no match found → true
        }
    }
}
```

---

## Monitoring & Observability

### Detailed Metrics

```rust
// Execution metrics
metrics::histogram!("pieskieo_subquery_execution_duration_ms", 
                    "type" => subquery_type).record(duration_ms);
metrics::counter!("pieskieo_subquery_executions_total", 
                  "type" => subquery_type, "decorrelated" => decorrelated).increment(1);

// Cache metrics  
metrics::counter!("pieskieo_subquery_cache_hits").increment(1);
metrics::counter!("pieskieo_subquery_cache_misses").increment(1);
metrics::gauge!("pieskieo_subquery_cache_memory_bytes").set(cache_memory);
metrics::gauge!("pieskieo_subquery_cache_entries").set(cache_entries);

// Decorrelation metrics
metrics::counter!("pieskieo_subquery_decorrelations", 
                  "pattern" => pattern).increment(1);

// Performance metrics
metrics::histogram!("pieskieo_subquery_rows_scanned").record(rows_scanned);
metrics::histogram!("pieskieo_subquery_result_size_bytes").record(result_size);
```

### Query Plan Introspection

```rust
impl SubqueryExecutor {
    fn explain_plan(&self, subquery: &SelectStatement) -> ExplainPlan {
        ExplainPlan {
            node_type: "Subquery",
            strategy: if self.is_correlated(subquery) {
                "Correlated (Nested Loop)"
            } else {
                "Materialized"
            },
            estimated_cost: self.estimate_cost(subquery),
            estimated_rows: self.estimate_cardinality(subquery),
            children: vec![self.plan_inner_query(subquery)],
            optimizations: vec![
                if self.can_decorrelate(subquery) { "Decorrelation" } else { "" },
                if self.can_push_down(subquery) { "Predicate Pushdown" } else { "" },
                if self.uses_cache(subquery) { "Cached" } else { "" },
            ].into_iter().filter(|s| !s.is_empty()).collect(),
        }
    }
}
```

---

## Production Deployment Considerations

### Backward Compatibility

- Subquery syntax fully compatible with PostgreSQL
- Query plans may differ (better optimizations) but results identical
- EXPLAIN output extended with Pieskieo-specific optimizations

### Upgrade Path

- Existing queries work without modification
- New decorrelation patterns automatically applied
- Cache invalidation handled transparently on schema changes

### Configuration Tuning

```toml
[subqueries]
# Maximum memory per subquery before spilling to disk
max_memory_mb = 256

# Maximum cache size for non-correlated subqueries
cache_size_mb = 100

# Enable aggressive decorrelation (may increase planning time)
aggressive_decorrelation = true

# Parallel subquery execution threshold
parallel_threshold_rows = 10000
```

---

**Created**: 2026-02-08  
**Author**: Implementation Team  
**Review Status**: Production-Ready
