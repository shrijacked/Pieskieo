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

### Phase 3: Correlated Subquery Optimization

**Problem**: Correlated subqueries can be very slow (O(n*m))

**Optimization**: Convert to JOIN when possible

```rust
impl SubqueryOptimizer {
    fn try_decorrelate(&self, query: &SelectStatement) -> Option<SelectStatement> {
        // Example: Convert EXISTS to SEMI-JOIN
        //
        // Before:
        // SELECT * FROM customers c
        // WHERE EXISTS (SELECT 1 FROM orders o WHERE o.customer_id = c.id)
        //
        // After:
        // SELECT DISTINCT c.* FROM customers c
        // SEMI JOIN orders o ON o.customer_id = c.id
        
        match &query.where_clause {
            Some(WhereExpression::Subquery(SubqueryPredicate::Exists { subquery })) => {
                // Check if subquery references outer table
                if let Some(correlation) = self.find_correlation(subquery) {
                    Some(self.convert_to_semi_join(query, subquery, correlation))
                } else {
                    None
                }
            }
            _ => None
        }
    }
}
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

## Performance Considerations

### 1. Caching
- Cache non-correlated subquery results
- Don't re-execute for each outer row

### 2. Early Termination
- For EXISTS: stop after first match
- For IN: build HashSet for O(1) lookup

### 3. Push-Down Optimization
- Push WHERE conditions into subquery when possible
- Example: `WHERE id IN (SELECT ... WHERE date > '2024-01-01')`

### 4. Index Usage
- Subqueries should use indexes when filtering
- Consider creating indexes on frequently-queried subquery columns

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

## Known Limitations (Initial Version)

1. **No lateral joins** initially (LATERAL keyword)
2. **Limited decorrelation** - won't catch all patterns
3. **No subquery in GROUP BY** initially
4. **Memory usage** - Large subquery results held in memory

These can be addressed in follow-up iterations.

---

**Created**: 2026-02-08  
**Author**: Implementation Team  
**Review Status**: Draft
