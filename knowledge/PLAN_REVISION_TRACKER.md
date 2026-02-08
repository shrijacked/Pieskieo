# Plan Revision Tracker

**Goal**: Remove ALL "Known Limitations" sections and upgrade ALL plans to production-grade

**Revision Date**: 2026-02-08  
**Reason**: User mandate - "Nothing must be pending. No optimization must be pending."

---

## Files to Revise (14 total)

### PostgreSQL Plans (11 files)
- [ ] `01-subqueries.md` - Add distributed subquery execution, full decorrelation
- [ ] `02-ctes.md` - Add CTE materialization optimization, recursive CTE limits handling
- [ ] `03-window-functions.md` - Add distributed window functions, parallel execution
- [ ] `04-joins.md` - Upgrade to WCOJ (Worst-Case Optimal Joins), distributed joins
- [ ] `05-acid.md` - Add distributed 2PC, Paxos/Raft integration
- [ ] `06-isolation.md` - Add full SSI (Serializable Snapshot Isolation)
- [ ] `07-savepoints.md` - Add distributed savepoints, compression of write-sets
- [ ] `08-deadlock-detection.md` - Add distributed deadlock detection
- [ ] `09-foreign-keys.md` - Add cross-shard FK support, deferred constraints
- [ ] `15-btree-indexes.md` - Add concurrent B-tree modifications, bulk loading
- [ ] `16-gin-indexes.md` - Add fast updates, compression
- [ ] `17-gist-indexes.md` - Upgrade to R*-tree, parallel build
- [ ] `26-json.md` - Add SIMD-optimized JSONB operations, compression

### MongoDB Plans (1 file)
- [ ] `AGGREGATION_PIPELINE.md` - Complete all 30+ pipeline stages

### Core Features (1 file)
- [ ] `UNIFIED_QUERY_LANGUAGE.md` - Add parser architecture, execution engine

---

## Revision Checklist (Apply to Each Plan)

### Remove These Sections Entirely
- ❌ "Known Limitations (Initial Version)"
- ❌ "These can be addressed in follow-up iterations"
- ❌ Any mention of "initially", "for now", "later", "future work"

### Add These Sections
- ✅ **Distributed Implementation** (multi-node from day 1)
- ✅ **Advanced Optimizations** (SIMD, lock-free, compression, caching)
- ✅ **Failure Modes & Recovery** (crash recovery, network partition, disk full)
- ✅ **Monitoring & Observability** (detailed metrics, tracing, profiling)
- ✅ **Production Deployment** (upgrade paths, backward compatibility)

### Upgrade These Areas

#### Algorithms
- Before: "Use basic algorithm X"
- After: "Use state-of-the-art algorithm Y with optimizations A, B, C"

#### Concurrency
- Before: "Single-threaded initially"
- After: "Parallel execution with work-stealing scheduler"

#### Storage
- Before: "In-memory first, persist later"
- After: "Durable storage with WAL, checkpointing, and crash recovery"

#### Distributed
- Before: "Single-node only"
- After: "Distributed with Raft consensus and automatic sharding"

---

## Specific Upgrades Needed

### 01-subqueries.md
**Remove**: "No lateral joins initially", "Limited decorrelation"
**Add**: 
- Full LATERAL support with correlated references
- Advanced decorrelation (EXISTS → SEMI JOIN, NOT EXISTS → ANTI JOIN)
- Distributed subquery execution with cost-based placement
- Parallel subquery execution when independent

### 04-joins.md
**Remove**: "Basic nested loop and hash join"
**Add**:
- Worst-Case Optimal Joins (WCOJ) for multi-way joins
- Adaptive join selection based on cardinality
- Distributed joins with data locality optimization
- SIMD-optimized hash table probing

### 05-acid.md
**Remove**: "Single-node transactions only"
**Add**:
- Distributed transactions with 2PC/3PC
- Distributed deadlock detection (global wait-for graph)
- Cross-shard atomic commits
- Spanner-style TrueTime integration (optional for global ordering)

### 17-gist-indexes.md
**Remove**: "Basic R-tree", "No parallel build"
**Add**:
- R*-tree with forced reinsert and bulk loading
- Parallel index construction with thread-per-subtree
- Adaptive splitting based on data distribution
- Compression of bounding boxes

### 26-json.md
**Remove**: "No compression initially"
**Add**:
- Adaptive compression (LZ4 for large objects, dictionary for repetitive)
- SIMD-optimized JSON parsing (simdjson-style)
- JIT compilation for hot JSON paths
- Columnar layout for analytics on JSONB arrays

---

## Timeline

- **Phase 1** (Current session): Revise first 7 plans
- **Phase 2** (Next session): Revise remaining 7 plans
- **Phase 3**: Create remaining 143 plans with production-grade approach

---

**Status**: In Progress  
**Completed**: 0/14  
**Blocker**: None
