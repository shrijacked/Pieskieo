# Pieskieo Agent Rules & Operating Principles

**Version**: 1.0  
**Created**: 2026-02-08  
**Purpose**: Complete ruleset for planning and implementing Pieskieo - the unified multimodal database

---

## MISSION STATEMENT

Build a **single, unified, production-grade database** that replaces PostgreSQL, MongoDB, Weaviate, LanceDB, and Kùzu with:
- **ZERO network hops** between data models
- **ONE query language** mixing relational + document + vector + graph
- **FULL feature parity** with all 5 databases
- **ZERO technical debt** from day 1
- **ZERO "we'll add this later"** - everything ships complete

---

## CORE PRINCIPLES

### 1. NO COMPROMISES - EVER

❌ **FORBIDDEN PHRASES:**
- "Initial version" vs "later version"
- "Known limitations (can be addressed later)"
- "For now" / "Initially" / "In the first iteration"
- "MVP approach"
- "We'll optimize this later"
- "Simple algorithm first, then improve"
- "Single-node first, distributed later"
- "Can be added in follow-up"

✅ **REQUIRED MINDSET:**
- Production-ready from commit 1
- Best-in-class algorithms from day 1
- All optimizations included upfront
- Distributed by default
- Zero technical debt
- One iteration, done right

### 2. PRODUCTION-GRADE REQUIREMENTS

Every feature MUST include:

#### Performance
- [ ] SIMD/vectorization where applicable
- [ ] Lock-free data structures where possible
- [ ] Optimistic concurrency control
- [ ] Memory pooling and zero-copy operations
- [ ] Compression (adaptive based on data characteristics)
- [ ] Adaptive algorithms (tune based on workload)
- [ ] CPU cache optimization
- [ ] Batch processing for bulk operations
- [ ] Lazy evaluation and streaming where appropriate

#### Distributed Systems
- [ ] Multi-node coordination from day 1
- [ ] Distributed deadlock detection
- [ ] Distributed transactions (2PC or better)
- [ ] Consensus (Raft) for metadata
- [ ] Cross-shard queries optimized
- [ ] Data rebalancing and migration
- [ ] Read replicas with consistency guarantees
- [ ] Network partition handling
- [ ] Split-brain prevention

#### Scalability
- [ ] Horizontal scaling (add nodes)
- [ ] Vertical scaling (use more cores/memory)
- [ ] Automatic sharding
- [ ] Parallel query execution
- [ ] Background compaction/maintenance
- [ ] Hot data caching with eviction policies
- [ ] Query result caching with invalidation

#### Reliability
- [ ] Full ACID guarantees (no eventual consistency shortcuts)
- [ ] WAL with fsync guarantees
- [ ] Crash recovery tested
- [ ] Checkpointing strategy
- [ ] Backup and restore (full + incremental)
- [ ] Point-in-time recovery
- [ ] Replication lag monitoring
- [ ] Automatic failover

#### Observability
- [ ] Prometheus metrics for EVERYTHING
- [ ] Structured logging (JSON)
- [ ] Distributed tracing (OpenTelemetry)
- [ ] Query explain plans with cost estimates
- [ ] Slow query logging
- [ ] Performance schema (live introspection)
- [ ] Index usage statistics
- [ ] Lock contention tracking

#### Security
- [ ] TLS 1.3 for all network traffic
- [ ] Certificate-based auth + RBAC
- [ ] Audit logging (who did what when)
- [ ] Row-level security (RLS)
- [ ] Column-level encryption (optional)
- [ ] SQL injection prevention
- [ ] Rate limiting per user/connection

---

## PLANNING RULES

### Rule 1: Best Algorithm Selection

For every algorithm choice, use the **state-of-the-art**, not "good enough":

| Component | ❌ Don't Use | ✅ Use Instead |
|-----------|-------------|----------------|
| Spatial Index | Basic R-tree | R*-tree with bulk loading |
| Graph Joins | Hash joins | Worst-Case Optimal Joins (WCOJ) |
| Vector Search | Flat index first | HNSW + IVF-PQ hybrid |
| Deadlock Detection | Timeout-based | Wait-for graph with cycle detection |
| Query Optimizer | Rule-based | Cost-based with cardinality estimation |
| Storage | Row-oriented | Hybrid (columnar for analytics, row for OLTP) |
| Compression | Single algorithm | Adaptive (LZ4/Zstd based on data) |
| Lock Manager | Coarse locks | Fine-grained + lock-free structures |

### Rule 2: Implementation Depth

Every plan must specify:

1. **Exact Data Structures** (not "we'll use a hashmap" - which hash function? collision resolution? load factor?)
2. **Concurrency Model** (RwLock? Mutex? Lock-free? Optimistic? Be specific)
3. **Memory Layout** (cache-aligned? padding? struct ordering for efficiency?)
4. **Error Paths** (what happens if malloc fails? disk full? network timeout?)
5. **Persistence Format** (binary layout, versioning, forward/backward compatibility)
6. **Wire Protocol** (if network communication involved)
7. **Upgrade Path** (how to migrate existing data when format changes)

### Rule 3: Testing Requirements

Every feature needs:

- **Unit tests** (90%+ coverage)
- **Integration tests** (cross-component)
- **Fuzzing** (property-based testing)
- **Benchmarks** (vs PostgreSQL/MongoDB/Weaviate/etc.)
- **Stress tests** (high concurrency, large datasets)
- **Chaos engineering** (network failures, disk failures, process crashes)
- **Regression tests** (prevent performance degradation)

### Rule 4: Documentation Standards

Every plan must include:

1. **Implementation Plan** (exact Rust code with full error handling)
2. **Performance Analysis** (Big-O, cache behavior, memory usage)
3. **Test Cases** (not 2-3 examples - 20+ covering all edge cases)
4. **Failure Modes** (what can go wrong and how we handle it)
5. **Monitoring Guide** (which metrics to watch, alert thresholds)
6. **Operational Runbook** (how to diagnose issues in production)

---

## ARCHITECTURAL MANDATES

### Unified Query Language

**NOT** separate interfaces:
- ❌ SQL for relational + Cypher for graph + separate vector API
- ✅ One query language that does: `SELECT ... WHERE vector_similar(...) AND graph_traverse(...) JOIN ...`

Example of what users MUST be able to write:

```sql
-- Find influential users who discussed "AI" recently, 
-- with similar interests to user X, ranked by PageRank
SELECT 
    u.name,
    u.pagerank_score,
    similarity(u.interests_embedding, $user_x_embedding) AS interest_match
FROM users u
WHERE 
    -- Graph traversal
    EXISTS (
        SELECT 1 FROM graph_traverse(
            start: u.id,
            relationship: 'DISCUSSED',
            depth: 1..3
        ) AS t
        WHERE t.topic LIKE '%AI%'
          AND t.timestamp > NOW() - INTERVAL '7 days'
    )
    -- Vector similarity
    AND u.interests_embedding <-> $user_x_embedding < 0.3
    -- Traditional filter
    AND u.account_status = 'active'
ORDER BY u.pagerank_score DESC
LIMIT 20;
```

### Storage Architecture

**Hybrid Storage Engine:**

```
┌─────────────────────────────────────────┐
│         Unified Storage Engine          │
├─────────────────────────────────────────┤
│  Row Store     │  Columnar    │  Vector │
│  (OLTP fast)   │  (Analytics) │  (HNSW) │
├─────────────────────────────────────────┤
│         Graph CSR/CSC Layout            │
├─────────────────────────────────────────┤
│              MVCC Layer                 │
│     (snapshot isolation, no locks)      │
├─────────────────────────────────────────┤
│               WAL + Raft                │
│        (durability + consensus)         │
└─────────────────────────────────────────┘
```

- **Hot data** → Row store (mutable, fast point queries)
- **Cold analytical** → Columnar (compressed, fast scans)
- **Vectors** → HNSW graph (fast approximate NN)
- **Graph** → CSR for adjacency (cache-friendly traversal)

**AUTO-TIERING:** System automatically moves data between stores based on access patterns.

---

## CODE QUALITY STANDARDS

### Rust Best Practices

```rust
// ✅ GOOD: Zero-copy, explicit lifetimes, error handling
pub fn parse_query<'a>(
    input: &'a [u8],
    arena: &'a Arena,
) -> Result<Query<'a>, ParseError> {
    // Use arena allocation for zero-copy
    // Explicit error types
    // No unwrap() - always handle errors
}

// ❌ BAD: Cloning, generic errors, panics
pub fn parse_query(input: String) -> Query {
    // String allocation (slow)
    let result = parser.parse(&input).unwrap(); // panic on error!
    result.clone() // unnecessary clone
}
```

### Performance Patterns

1. **Pre-allocate** when size is known:
   ```rust
   let mut results = Vec::with_capacity(estimated_size);
   ```

2. **Use `&str` over `String`** when possible (no allocation)

3. **Batch operations** instead of one-by-one:
   ```rust
   // ✅ Batch
   db.insert_batch(&records)?;
   
   // ❌ Loop
   for r in records {
       db.insert(r)?; // N round-trips
   }
   ```

4. **Lock-free when possible:**
   ```rust
   use crossbeam::queue::ArrayQueue;
   use std::sync::atomic::{AtomicU64, Ordering};
   ```

5. **Use `MaybeUninit` for uninitialized buffers** (avoid zero-fill cost)

6. **SIMD for hot loops:**
   ```rust
   #[cfg(target_arch = "x86_64")]
   use std::arch::x86_64::*;
   ```

---

## IMPLEMENTATION WORKFLOW

### Phase 1: Planning (Current)
- Create 157 detailed plans
- Every plan: 4000-6000 tokens (comprehensive)
- Include ALL optimizations
- Include ALL failure modes
- NO "later" or "future work"

### Phase 2: Implementation
1. **Write tests FIRST** (TDD)
2. **Implement with benchmarks**
3. **Profile before optimizing** (but design for optimization)
4. **Code review** (even solo - review your own code)
5. **Fuzz test** (find edge cases)
6. **Stress test** (10k concurrent queries)

### Phase 3: Validation
- [ ] Passes all tests
- [ ] Benchmarks meet targets (specify targets in plan)
- [ ] Memory leak check (valgrind/asan)
- [ ] No data races (tsan)
- [ ] Query plans look optimal (EXPLAIN ANALYZE)
- [ ] Monitoring dashboards working
- [ ] Documentation complete

---

## FEATURE PARITY TARGETS

### PostgreSQL
- [ ] All SQL:2016 features
- [ ] All index types (B-tree, GIN, GiST, BRIN, HASH, SP-GiST)
- [ ] Full-text search (with language stemming)
- [ ] JSON/JSONB (binary format, GIN indexing)
- [ ] Partitioning (range, list, hash)
- [ ] Parallel query execution
- [ ] JIT compilation (via LLVM for hot queries)
- [ ] Logical replication
- [ ] Row-level security (RLS)

### MongoDB
- [ ] All aggregation stages ($match, $group, $lookup, $facet, etc.)
- [ ] All update operators ($set, $inc, $push, $pull, etc.)
- [ ] Change streams (CDC)
- [ ] GridFS for large files
- [ ] Time-series collections (with automatic bucketing)
- [ ] Transactions across shards
- [ ] Capped collections
- [ ] Schema validation (JSON Schema)

### Weaviate
- [ ] Multi-vector per object
- [ ] HNSW + IVF-PQ hybrid
- [ ] BM25 + vector hybrid search
- [ ] Cross-encoder reranking
- [ ] Multi-tenancy with isolation
- [ ] Automatic quantization (PQ, SQ)
- [ ] Filtered vector search (pre-filter, post-filter, HNSW with filters)
- [ ] Generative search (RAG integration)

### LanceDB
- [ ] Lance columnar format (Arrow compatible)
- [ ] Zero-copy reads via mmap
- [ ] Time-travel queries (MVCC snapshots)
- [ ] Version tagging (named snapshots)
- [ ] Predicate pushdown
- [ ] Late materialization
- [ ] Vectorized execution (Arrow compute)
- [ ] Parquet import/export

### Kùzu
- [ ] Full Cypher query language
- [ ] Worst-Case Optimal Joins (WCOJ)
- [ ] Variable-length path queries
- [ ] Graph algorithms (PageRank, Louvain, Betweenness, etc.)
- [ ] Recursive CTEs
- [ ] Pattern matching optimization
- [ ] Compressed Sparse Row (CSR) storage
- [ ] Join-free graph traversal

---

## PERFORMANCE TARGETS

### Latency (p99)
- Point query: < 1ms
- Range scan (1000 rows): < 10ms
- Vector search (top 10): < 5ms
- Graph traversal (3 hops): < 20ms
- Complex JOIN (3 tables): < 50ms
- Aggregation (1M rows): < 100ms

### Throughput
- Inserts: > 100k/sec (single node)
- Point queries: > 500k/sec
- Range scans: > 1GB/sec
- Vector search: > 10k qps
- Mixed workload: > 50k tps

### Scalability
- Linear scale to 100 nodes
- Handle 100TB datasets
- Support 10k concurrent connections
- Replication lag: < 10ms (same DC)

### Resource Usage
- Memory: < 1GB baseline + dataset-dependent
- CPU: < 5% idle overhead
- Disk: < 2x data size (including indexes)
- Network: < 10% overhead vs payload

---

## ANTI-PATTERNS TO AVOID

### ❌ Never Do This:

1. **String concatenation for SQL**
   ```rust
   // ❌ SQL injection risk
   let query = format!("SELECT * FROM users WHERE id = {}", user_input);
   ```

2. **Unbounded loops without backpressure**
   ```rust
   // ❌ Can OOM
   loop {
       data.push(read_from_network());
   }
   ```

3. **Synchronous I/O on hot path**
   ```rust
   // ❌ Blocks executor
   let file = std::fs::read("config.json")?;
   ```

4. **Allocations in tight loops**
   ```rust
   // ❌ Slow
   for item in items {
       let s = format!("Processing {}", item); // allocates!
   }
   ```

5. **Holding locks across await points**
   ```rust
   // ❌ Deadlock risk
   let guard = mutex.lock().await;
   some_async_fn().await;
   drop(guard);
   ```

6. **Not handling partial reads/writes**
   ```rust
   // ❌ Assumes full write
   socket.write(&buf)?;
   // Should be: write_all() with retry logic
   ```

---

## COMMIT STANDARDS

Every commit must:
- [ ] Pass all tests (zero flaky tests allowed)
- [ ] Pass clippy with zero warnings
- [ ] Pass rustfmt
- [ ] Include tests for new code
- [ ] Update relevant documentation
- [ ] Update CHANGELOG if user-visible
- [ ] Include benchmark results if perf-critical

---

## ERROR HANDLING PHILOSOPHY

### Fail Fast, Recover Gracefully

```rust
// User errors: return Result
pub fn execute_query(sql: &str) -> Result<QueryResult, QueryError> {
    // Syntax error, constraint violation, etc.
}

// Internal errors: panic or abort
if !self.validate_invariant() {
    panic!("Invariant violation: corrupted index detected");
}

// System errors: retry with backoff
for attempt in 0..MAX_RETRIES {
    match network_call() {
        Ok(response) => return Ok(response),
        Err(e) if e.is_retryable() => {
            tokio::time::sleep(backoff_duration(attempt)).await;
        }
        Err(e) => return Err(e),
    }
}
```

---

## MONITORING REQUIREMENTS

Every component must expose:

### RED Metrics
- **Rate**: Requests per second
- **Errors**: Error rate (%)
- **Duration**: Latency histogram

### USE Metrics (for resources)
- **Utilization**: % of capacity used
- **Saturation**: Queue depth / backlog
- **Errors**: Error count

### Example:
```rust
// Every query must record:
metrics::counter!("pieskieo_queries_total", "type" => query_type).increment(1);
metrics::histogram!("pieskieo_query_duration_ms", "type" => query_type).record(duration_ms);
metrics::gauge!("pieskieo_active_connections").set(conn_count);
```

---

## FINAL WORD

**This database will be the LAST database anyone ever needs to install.**

Users should NEVER think:
- "I need Postgres for this part and Weaviate for that part"
- "This would be easier with a graph database but we're on Mongo"
- "Let me add another database to the stack"

They should think:
- "Pieskieo does everything I need, optimally, in one query"

**We're not building a prototype. We're building the future of databases.**

---

**Last Updated**: 2026-02-08  
**Review Frequency**: Before every planning session  
**Adherence**: Mandatory, zero exceptions
