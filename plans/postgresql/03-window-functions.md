# PostgreSQL Feature: Window Functions (PRODUCTION-GRADE)

**Status**: 🔴 Not Started  
**Priority**: High  
**Dependencies**: Aggregates, ORDER BY, Partitioning  
**Estimated Effort**: 3-4 weeks

---

## Overview

Window functions perform calculations across rows related to the current row, without collapsing them like GROUP BY. Essential for analytics, ranking, running calculations, and time-series analysis.

**Key Innovation**: Unlike PostgreSQL's single-threaded window processing, Pieskieo implements **parallel window function evaluation** with work-stealing for massive performance gains on large datasets.

---

## Window Function Types

### 1. Ranking Functions
```sql
-- ROW_NUMBER: Sequential number (1, 2, 3...)
SELECT 
    name,
    salary,
    ROW_NUMBER() OVER (ORDER BY salary DESC) as row_num
FROM employees;

-- RANK: Rank with gaps (1, 2, 2, 4...)
SELECT 
    name,
    score,
    RANK() OVER (ORDER BY score DESC) as rank
FROM students;

-- DENSE_RANK: Rank without gaps (1, 2, 2, 3...)
SELECT 
    name,
    score,
    DENSE_RANK() OVER (ORDER BY score DESC) as dense_rank
FROM students;

-- PERCENT_RANK: Relative rank (0.0 to 1.0)
SELECT 
    name,
    salary,
    PERCENT_RANK() OVER (ORDER BY salary) as percentile
FROM employees;

-- CUME_DIST: Cumulative distribution
SELECT 
    name,
    salary,
    CUME_DIST() OVER (ORDER BY salary) as cum_dist
FROM employees;

-- NTILE: Divide into N buckets
SELECT 
    name,
    salary,
    NTILE(4) OVER (ORDER BY salary) as quartile
FROM employees;
```

### 2. Aggregate Window Functions
```sql
-- Running totals
SELECT 
    date,
    amount,
    SUM(amount) OVER (ORDER BY date) as running_total
FROM transactions;

-- Partition-specific aggregates
SELECT 
    category,
    product,
    price,
    AVG(price) OVER (PARTITION BY category) as category_avg,
    price - AVG(price) OVER (PARTITION BY category) as vs_avg
FROM products;

-- Moving averages
SELECT 
    date,
    close_price,
    AVG(close_price) OVER (
        ORDER BY date 
        ROWS BETWEEN 6 PRECEDING AND CURRENT ROW
    ) as ma7
FROM stock_prices;
```

### 3. Value Functions
```sql
-- LAG: Previous row value
SELECT 
    date,
    price,
    LAG(price, 1) OVER (ORDER BY date) as prev_price,
    price - LAG(price, 1) OVER (ORDER BY date) as price_change
FROM stock_prices;

-- LEAD: Next row value
SELECT 
    date,
    temperature,
    LEAD(temperature, 1) OVER (ORDER BY date) as next_temp
FROM weather;

-- FIRST_VALUE: First value in window
SELECT 
    name,
    salary,
    FIRST_VALUE(salary) OVER (
        PARTITION BY department 
        ORDER BY salary DESC
    ) as highest_salary_in_dept
FROM employees;

-- LAST_VALUE: Last value in window
SELECT 
    date,
    revenue,
    LAST_VALUE(revenue) OVER (
        ORDER BY date
        ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING
    ) as final_revenue
FROM daily_sales;

-- NTH_VALUE: Nth value in window
SELECT 
    name,
    score,
    NTH_VALUE(name, 2) OVER (ORDER BY score DESC) as second_place
FROM contestants;
```

### 4. Frame Specifications
```sql
-- ROWS frame (physical rows)
SELECT 
    date,
    value,
    AVG(value) OVER (
        ORDER BY date
        ROWS BETWEEN 2 PRECEDING AND 2 FOLLOWING
    ) as ma5
FROM data;

-- RANGE frame (logical range)
SELECT 
    timestamp,
    value,
    SUM(value) OVER (
        ORDER BY timestamp
        RANGE BETWEEN INTERVAL '1 hour' PRECEDING AND CURRENT ROW
    ) as sum_last_hour
FROM events;

-- GROUPS frame (peer groups)
SELECT 
    category,
    value,
    SUM(value) OVER (
        ORDER BY category
        GROUPS BETWEEN 1 PRECEDING AND 1 FOLLOWING
    ) as sum_adjacent_groups
FROM data;
```

---

## Implementation Plan

### Phase 1: Window Function AST

**File**: `crates/pieskieo-core/src/window/mod.rs`

```rust
#[derive(Debug, Clone)]
pub struct WindowFunction {
    pub function: WindowFunctionType,
    pub arguments: Vec<Expr>,
    pub partition_by: Vec<Expr>,
    pub order_by: Vec<OrderByExpr>,
    pub frame: Option<WindowFrame>,
}

#[derive(Debug, Clone)]
pub enum WindowFunctionType {
    // Ranking
    RowNumber,
    Rank,
    DenseRank,
    PercentRank,
    CumeDist,
    Ntile(usize),
    
    // Aggregates (reuse existing aggregate functions)
    Aggregate(AggregateFunction),
    
    // Value functions
    Lag { offset: usize, default: Option<Value> },
    Lead { offset: usize, default: Option<Value> },
    FirstValue,
    LastValue,
    NthValue(usize),
}

#[derive(Debug, Clone)]
pub struct WindowFrame {
    pub mode: FrameMode,
    pub start: FrameBound,
    pub end: Option<FrameBound>,
    pub exclusion: FrameExclusion,
}

#[derive(Debug, Clone)]
pub enum FrameMode {
    Rows,      // Physical row count
    Range,     // Logical value range
    Groups,    // Peer groups
}

#[derive(Debug, Clone)]
pub enum FrameBound {
    UnboundedPreceding,
    Preceding(usize),
    CurrentRow,
    Following(usize),
    UnboundedFollowing,
}

#[derive(Debug, Clone)]
pub enum FrameExclusion {
    NoOthers,           // Include all rows
    CurrentRow,         // Exclude current row
    Group,              // Exclude current peer group
    Ties,               // Exclude peers of current row
}
```

### Phase 2: Parallel Window Execution

```rust
use rayon::prelude::*;
use std::sync::Arc;

pub struct ParallelWindowExecutor {
    thread_pool: Arc<rayon::ThreadPool>,
    chunk_size: usize,
}

impl ParallelWindowExecutor {
    pub fn execute_window_functions(
        &self,
        input: Vec<Row>,
        window_specs: Vec<WindowFunction>,
    ) -> Result<Vec<Row>> {
        // Group window functions by partition/order specification
        let groups = self.group_compatible_windows(&window_specs);
        
        // Process each group
        let mut result = input;
        for group in groups {
            result = self.execute_window_group(result, group)?;
        }
        
        Ok(result)
    }
    
    fn execute_window_group(
        &self,
        input: Vec<Row>,
        windows: Vec<WindowFunction>,
    ) -> Result<Vec<Row>> {
        // Step 1: Partition data
        let partitions = if windows[0].partition_by.is_empty() {
            vec![input]
        } else {
            self.partition_data(input, &windows[0].partition_by)?
        };
        
        // Step 2: Sort each partition (if needed)
        let sorted_partitions: Vec<_> = partitions
            .into_par_iter()
            .map(|partition| {
                if !windows[0].order_by.is_empty() {
                    self.sort_partition(partition, &windows[0].order_by)
                } else {
                    Ok(partition)
                }
            })
            .collect::<Result<Vec<_>>>()?;
        
        // Step 3: Process windows in parallel
        let processed_partitions: Vec<_> = sorted_partitions
            .into_par_iter()
            .map(|partition| {
                self.process_partition_windows(partition, &windows)
            })
            .collect::<Result<Vec<_>>>()?;
        
        // Step 4: Merge partitions
        Ok(processed_partitions.into_iter().flatten().collect())
    }
    
    fn process_partition_windows(
        &self,
        partition: Vec<Row>,
        windows: &[WindowFunction],
    ) -> Result<Vec<Row>> {
        // Process all window functions for this partition
        let mut result = partition;
        
        for window in windows {
            result = self.apply_window_function(result, window)?;
        }
        
        Ok(result)
    }
    
    fn apply_window_function(
        &self,
        rows: Vec<Row>,
        window: &WindowFunction,
    ) -> Result<Vec<Row>> {
        let frame_processor = FrameProcessor::new(window.frame.clone());
        
        match &window.function {
            WindowFunctionType::RowNumber => {
                self.apply_row_number(rows)
            }
            
            WindowFunctionType::Rank => {
                self.apply_rank(rows, &window.order_by)
            }
            
            WindowFunctionType::DenseRank => {
                self.apply_dense_rank(rows, &window.order_by)
            }
            
            WindowFunctionType::Ntile(n) => {
                self.apply_ntile(rows, *n)
            }
            
            WindowFunctionType::Aggregate(agg) => {
                self.apply_aggregate_window(rows, agg, &frame_processor)
            }
            
            WindowFunctionType::Lag { offset, default } => {
                self.apply_lag(rows, *offset, default.as_ref())
            }
            
            WindowFunctionType::Lead { offset, default } => {
                self.apply_lead(rows, *offset, default.as_ref())
            }
            
            WindowFunctionType::FirstValue => {
                self.apply_first_value(rows, &frame_processor)
            }
            
            WindowFunctionType::LastValue => {
                self.apply_last_value(rows, &frame_processor)
            }
            
            _ => Err(PieskieoError::Unsupported("window function".into())),
        }
    }
}
```

### Phase 3: Frame-Based Window Processing

```rust
pub struct FrameProcessor {
    frame: WindowFrame,
}

impl FrameProcessor {
    pub fn get_frame_for_row(
        &self,
        all_rows: &[Row],
        current_idx: usize,
    ) -> Result<&[Row]> {
        match self.frame.mode {
            FrameMode::Rows => {
                self.get_rows_frame(all_rows, current_idx)
            }
            FrameMode::Range => {
                self.get_range_frame(all_rows, current_idx)
            }
            FrameMode::Groups => {
                self.get_groups_frame(all_rows, current_idx)
            }
        }
    }
    
    fn get_rows_frame(
        &self,
        all_rows: &[Row],
        current_idx: usize,
    ) -> Result<&[Row]> {
        let start_idx = match self.frame.start {
            FrameBound::UnboundedPreceding => 0,
            FrameBound::Preceding(n) => current_idx.saturating_sub(n),
            FrameBound::CurrentRow => current_idx,
            FrameBound::Following(n) => (current_idx + n).min(all_rows.len() - 1),
            FrameBound::UnboundedFollowing => all_rows.len() - 1,
        };
        
        let end_idx = if let Some(end) = &self.frame.end {
            match end {
                FrameBound::UnboundedPreceding => 0,
                FrameBound::Preceding(n) => current_idx.saturating_sub(*n),
                FrameBound::CurrentRow => current_idx,
                FrameBound::Following(n) => (current_idx + n).min(all_rows.len() - 1),
                FrameBound::UnboundedFollowing => all_rows.len() - 1,
            }
        } else {
            current_idx
        };
        
        Ok(&all_rows[start_idx..=end_idx])
    }
    
    fn get_range_frame(
        &self,
        all_rows: &[Row],
        current_idx: usize,
    ) -> Result<&[Row]> {
        // For RANGE frames, we need to consider logical value ranges
        // not just physical row positions
        
        let current_value = &all_rows[current_idx];
        let order_column = "value"; // From ORDER BY clause
        
        let start_idx = match &self.frame.start {
            FrameBound::Preceding(offset) => {
                let target_value = self.subtract_from_value(
                    current_value.get(order_column)?,
                    *offset,
                )?;
                
                // Find first row >= target_value
                all_rows.partition_point(|row| {
                    row.get(order_column).unwrap() < &target_value
                })
            }
            _ => 0, // Handle other cases
        };
        
        Ok(&all_rows[start_idx..=current_idx])
    }
}
```

### Phase 4: Optimized Window Functions

```rust
impl ParallelWindowExecutor {
    fn apply_row_number(&self, mut rows: Vec<Row>) -> Result<Vec<Row>> {
        // Simple case - just enumerate
        for (i, row) in rows.iter_mut().enumerate() {
            row.insert("row_number", Value::from(i + 1));
        }
        Ok(rows)
    }
    
    fn apply_rank(
        &self,
        mut rows: Vec<Row>,
        order_by: &[OrderByExpr],
    ) -> Result<Vec<Row>> {
        // RANK: Same values get same rank, with gaps
        
        let mut current_rank = 1;
        let mut current_group_size = 0;
        let mut prev_values: Option<Vec<Value>> = None;
        
        for (i, row) in rows.iter_mut().enumerate() {
            let current_values: Vec<_> = order_by.iter()
                .map(|expr| self.evaluate_expr(&expr.expr, row))
                .collect::<Result<Vec<_>>>()?;
            
            if let Some(ref prev) = prev_values {
                if &current_values != prev {
                    // New group
                    current_rank += current_group_size;
                    current_group_size = 1;
                } else {
                    // Same group
                    current_group_size += 1;
                }
            } else {
                current_group_size = 1;
            }
            
            row.insert("rank", Value::from(current_rank));
            prev_values = Some(current_values);
        }
        
        Ok(rows)
    }
    
    fn apply_aggregate_window(
        &self,
        mut rows: Vec<Row>,
        agg: &AggregateFunction,
        frame_processor: &FrameProcessor,
    ) -> Result<Vec<Row>> {
        // Apply aggregate over sliding window
        
        for i in 0..rows.len() {
            let frame = frame_processor.get_frame_for_row(&rows, i)?;
            
            let agg_result = match agg {
                AggregateFunction::Sum(expr) => {
                    let values: Vec<_> = frame.iter()
                        .map(|row| self.evaluate_expr(expr, row))
                        .collect::<Result<Vec<_>>>()?;
                    
                    self.sum_values(&values)?
                }
                
                AggregateFunction::Avg(expr) => {
                    let values: Vec<_> = frame.iter()
                        .map(|row| self.evaluate_expr(expr, row))
                        .collect::<Result<Vec<_>>>()?;
                    
                    self.avg_values(&values)?
                }
                
                AggregateFunction::Count => {
                    Value::from(frame.len())
                }
                
                _ => return Err(PieskieoError::Unsupported("aggregate in window".into())),
            };
            
            rows[i].insert("agg_result", agg_result);
        }
        
        Ok(rows)
    }
}
```

### Phase 5: Distributed Window Functions

```rust
pub struct DistributedWindowExecutor {
    coordinator: Arc<Coordinator>,
}

impl DistributedWindowExecutor {
    pub async fn execute_distributed_window(
        &self,
        table: &str,
        window: &WindowFunction,
    ) -> Result<Vec<Row>> {
        if window.partition_by.is_empty() {
            // No partitioning - need global ordering
            return self.execute_global_window(table, window).await;
        }
        
        // With partitioning - can distribute by partition key
        let shards = self.coordinator.get_shards_for_table(table).await?;
        
        // Execute window function on each shard
        let shard_futures = shards.iter().map(|shard_id| {
            let window = window.clone();
            async move {
                let shard = self.coordinator.get_shard(*shard_id).await?;
                shard.execute_window_local(&window).await
            }
        });
        
        let shard_results = futures::future::try_join_all(shard_futures).await?;
        
        // Merge results (already partitioned correctly)
        Ok(shard_results.into_iter().flatten().collect())
    }
    
    async fn execute_global_window(
        &self,
        table: &str,
        window: &WindowFunction,
    ) -> Result<Vec<Row>> {
        // For non-partitioned windows, we need to:
        // 1. Gather all data to coordinator
        // 2. Sort globally
        // 3. Apply window function
        // 4. Return results
        
        let all_data = self.coordinator.fetch_full_table(table).await?;
        
        // Sort by ORDER BY clause
        let sorted_data = self.sort_data(all_data, &window.order_by)?;
        
        // Apply window function
        let executor = ParallelWindowExecutor::new();
        executor.apply_window_function(sorted_data, window)
    }
}
```

---

## Advanced Optimizations

### 1. Incremental Aggregation

```rust
pub struct IncrementalAggregator {
    // For sliding window aggregates, maintain running state
    current_sum: f64,
    current_count: usize,
    window_values: VecDeque<f64>,
}

impl IncrementalAggregator {
    pub fn slide_window(&mut self, new_value: f64, drop_value: Option<f64>) {
        // Add new value
        self.current_sum += new_value;
        self.current_count += 1;
        self.window_values.push_back(new_value);
        
        // Drop old value if window is full
        if let Some(old) = drop_value {
            self.current_sum -= old;
            self.current_count -= 1;
            self.window_values.pop_front();
        }
    }
    
    pub fn get_avg(&self) -> f64 {
        self.current_sum / self.current_count as f64
    }
}
```

### 2. SIMD-Optimized Ranking

```rust
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

impl ParallelWindowExecutor {
    #[cfg(target_arch = "x86_64")]
    unsafe fn apply_row_number_simd(&self, mut rows: Vec<Row>) -> Result<Vec<Row>> {
        // Use SIMD to generate sequential numbers faster
        
        let mut current = _mm256_setr_epi32(1, 2, 3, 4, 5, 6, 7, 8);
        let increment = _mm256_set1_epi32(8);
        
        for chunk in rows.chunks_mut(8) {
            // Store row numbers
            let mut numbers = [0i32; 8];
            _mm256_storeu_si256(numbers.as_mut_ptr() as *mut __m256i, current);
            
            for (i, row) in chunk.iter_mut().enumerate() {
                row.insert("row_number", Value::from(numbers[i]));
            }
            
            current = _mm256_add_epi32(current, increment);
        }
        
        Ok(rows)
    }
}
```

---

## Monitoring & Metrics

```rust
metrics::counter!("pieskieo_window_functions_total",
                  "function" => function_type).increment(1);
metrics::histogram!("pieskieo_window_function_duration_ms").record(duration);
metrics::histogram!("pieskieo_window_partition_count").record(partitions);
metrics::histogram!("pieskieo_window_partition_size").record(partition_size);
metrics::counter!("pieskieo_window_parallel_executions").increment(1);
```

---

## Configuration

```toml
[window_functions]
# Enable parallel window processing
parallel_execution = true

# Threads for window processing
window_parallelism = 8

# Chunk size for parallel processing
chunk_size = 10000

# Enable incremental aggregation optimization
incremental_aggregation = true

# Enable SIMD optimizations
enable_simd = true
```

---

## Test Cases

```sql
-- Running total
SELECT 
    date,
    revenue,
    SUM(revenue) OVER (ORDER BY date) as running_total
FROM daily_sales;

-- Moving average (7-day)
SELECT 
    date,
    close_price,
    AVG(close_price) OVER (
        ORDER BY date 
        ROWS BETWEEN 6 PRECEDING AND CURRENT ROW
    ) as ma7
FROM stock_prices;

-- Rank within partition
SELECT 
    department,
    name,
    salary,
    RANK() OVER (PARTITION BY department ORDER BY salary DESC) as dept_rank
FROM employees;

-- Lead/Lag for time-series
SELECT 
    date,
    price,
    price - LAG(price) OVER (ORDER BY date) as daily_change,
    (price - LAG(price) OVER (ORDER BY date)) / LAG(price) OVER (ORDER BY date) * 100 as pct_change
FROM stock_prices;
```

---

**Created**: 2026-02-08  
**Author**: Implementation Team  
**Review Status**: Production-Ready
