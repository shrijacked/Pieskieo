# User Vision & Requirements for Pieskieo

**Document Purpose**: Capture all instructions, ideas, ideals, plans, and thoughts from the project owner.

---

## The Core Vision

### The Problem We're Solving

Modern applications use **3-4 different databases**:
- PostgreSQL for relational data
- MongoDB for documents  
- Weaviate/Pinecone for vectors
- Neo4j/Kùzu for graphs

**Result**: 
- 3-4 network hops per query
- Data duplication and sync issues
- Complex deployment and ops
- Expensive (multiple licenses/cloud bills)
- Slow (network latency between DBs)

### The Pieskieo Solution

**ONE database** that natively supports:
- Relational (SQL)
- Document (JSONB)
- Vector (embeddings)
- Graph (Cypher)
- Columnar (analytics)

**In a SINGLE QUERY** mixing all paradigms.

---

## User's Killer Use Case: AI Memory

### The Vision

An AI agent needs to:
1. Store conversation embeddings (vector)
2. Track relationships between concepts (graph)  
3. Store structured metadata (relational)
4. Store raw JSON logs (document)

**Current Approach** (terrible):
```
User query
  → PostgreSQL (get user context)
  → Weaviate (semantic search on past conversations)  
  → Neo4j (find related topics via graph)
  → Combine results in app code
  → Return to user
```

**Total latency**: 100-200ms (network overhead)

**Pieskieo Approach** (amazing):
```sql
SELECT 
    m.content,
    m.timestamp,
    m.metadata,
    similarity(m.embedding, embed($query)) AS relevance
FROM memories m
WHERE 
    -- Vector search
    m.embedding <-> embed($query) < 0.3
    -- Graph traversal  
    AND EXISTS (
        SELECT 1 FROM graph_traverse(
            start: m.id,
            relationship: 'relates_to',
            depth: 1..3
        ) WHERE topic = $current_topic
    )
    -- Relational filter
    AND m.user_id = $user_id
    AND m.importance > 0.7
ORDER BY relevance DESC
LIMIT 10;
```

**Total latency**: 5-10ms (no network hops!)

---

## User's Non-Negotiable Requirements

### 1. Zero Compromises

**User's Exact Words**:
> "You will not leave anything for later. Ignore limitations and plan properly for production. We need it properly optimized. Once this DB is released, nothing must be pending. No optimization must be pending."

**Translation**:
- ❌ No "MVP" or "initial version"
- ❌ No "we'll optimize this later"
- ❌ No "known limitations" sections
- ✅ Production-ready from day 1
- ✅ All optimizations included upfront
- ✅ Best algorithms from the start

### 2. Full Feature Parity

User wants **100% feature parity** with:
- PostgreSQL (all SQL, ACID, indexes, partitioning, full-text)
- MongoDB (aggregation pipeline, update operators, change streams)
- Weaviate (hybrid search, multi-vector, quantization, reranking)
- LanceDB (columnar, time-travel, Arrow integration)
- Kùzu (Cypher, WCOJ, graph algorithms)

**No subset. No "most commonly used features." EVERYTHING.**

### 3. Unified Query Language

User explicitly stated:
> "Unified query language mixing vector search + graph traversal + JSON + relational in ONE statement"

Not:
- SQL for some queries
- Cypher for graph queries  
- Separate vector search API

But: **One language that does it all.**

Example user showed:
```sql
QUERY memories
  SIMILAR TO embed("project discussion") TOP 20
  TRAVERSE edges WHERE type = "relates_to" DEPTH 1..3
  WHERE metadata.importance > 0.7
  JOIN users ON memories.user_id = users.id
  GROUP BY users.name
  ORDER BY COUNT() DESC
  LIMIT 10
```

### 4. Performance Targets

While not explicitly stated numerically, user's expectations:

- **Faster than** running 4 separate databases combined
- **Latency**: Single-digit milliseconds for mixed queries
- **Throughput**: Handle AI agent workloads (thousands of queries/sec)
- **Scale**: From laptop to 100-node cluster
- **Efficiency**: No wasted resources on network serialization

---

## User's Planning Philosophy

### From User's Instructions

**Mandate**:
> "The plans should have all the full features with all the optimizations planned."

**What This Means**:

1. **Don't start simple and iterate** - Start with the best solution
2. **Don't defer hard problems** - Solve them upfront
3. **Don't write TODO comments** - Complete everything
4. **Don't leave optimization for later** - Optimize from day 1

### Planning Depth Expected

Each plan should be **4000-6000 tokens** including:

- Full Rust implementation (not pseudocode)
- All edge cases handled
- All failure modes considered
- Distributed implementation (not single-node first)
- All optimizations (SIMD, lock-free, compression, etc.)
- Complete test coverage
- Performance benchmarks
- Operational runbooks

**NOT acceptable**:
- "We'll use a simple algorithm initially" 
- "Known limitations: X, Y, Z"
- "Future work: Add optimization Q"

**Acceptable**:
- "Using R*-tree with bulk loading and adaptive splitting"
- "Lock-free queue with epoch-based memory reclamation"
- "SIMD-optimized distance calculation with AVX-512"

---

## Technical Preferences

### Storage Architecture

User mentioned:
- **Use case**: AI memory, but also **general-purpose for ANY application**
- **Implication**: Can't optimize for one workload, must handle OLTP + OLAP + vector + graph

**Design Decision**:
- Hybrid storage (row + columnar + vector + graph)
- Auto-tiering based on access patterns
- Unified MVCC layer across all models

### Consistency Model

User wants to **replace** PostgreSQL, so:
- **Must be**: Full ACID, not eventual consistency
- **Isolation**: Serializable or snapshot isolation
- **Durability**: WAL with fsync guarantees

### Deployment Target

User wants this to work **everywhere**:
- Local laptop (for dev)
- Single server (for small apps)
- Multi-node cluster (for scale)
- Cloud (AWS, GCP, Azure)
- On-prem (for regulated industries)

**Implication**: 
- No cloud-specific APIs
- No assumptions about network topology
- Must work on bare metal and VMs

---

## User's Workflow Expectations

### Planning Phase (Current)

1. Create **157 detailed plans** (one per feature)
2. Each plan: comprehensive, production-ready, no shortcuts
3. **DO NOT COMMIT** until all 157 plans complete
4. Review all plans for consistency
5. **Then** commit all plans in one atomic commit

### Implementation Phase (Future)

User expectation (not explicitly stated but implied):

1. Implement in **dependency order** (foundation first)
2. **Tests first** (TDD approach)
3. Benchmark against target databases
4. No feature is "done" until it **matches or beats** the database it replaces
5. Integration tests showing **cross-model queries working**

---

## Key Insights from Conversations

### 1. "The Last Database"

User sees Pieskieo as:
> "The single database replacing PostgreSQL, MongoDB, Weaviate, LanceDB, and Kùzu"

Not:
- "A database that can do some of what they do"
- "A good-enough alternative"
- "A prototype showing it's possible"

But:
- "The definitive replacement"
- "Strictly superior in every way"
- "The last database anyone needs to install"

### 2. Zero Network Hops Philosophy

User's core thesis:
> "Eliminate 3-4 network hops by co-locating all data models"

**Breakdown**:
- Traditional: App → DB1 → App → DB2 → App → DB3 → App (150ms)
- Pieskieo: App → Pieskieo → App (5ms)

**This 30x speedup** is the killer feature.

### 3. Use Case Generality

User specifically said:
> "AI memory storage, but also general-purpose for ANY application"

**Implication**: Don't over-optimize for embeddings. Must handle:
- E-commerce (products, orders, inventory)
- Social networks (users, posts, follows)
- Analytics (time-series, aggregations)
- Content management (documents, media)
- IoT (sensor data, time-series)

---

## User's Quality Standards

### Code Quality

From the mandate to **"plan properly for production"**:

- Rust best practices (no `unwrap()`, explicit errors)
- Zero-copy where possible
- Lock-free data structures
- SIMD for hot paths
- Benchmarks for everything
- Fuzz testing
- Property-based testing

### Documentation

User wants **157 detailed plans**, implying:

- Every feature fully documented
- Implementation guides
- Operational runbooks
- Troubleshooting guides
- Performance tuning docs

### Testing

If we're shipping "production-ready from day 1":

- Unit tests (90%+ coverage)
- Integration tests
- Stress tests (high concurrency)
- Chaos tests (network failures, crashes)
- Benchmarks vs competitors
- Correctness verification (Jepsen-style)

---

## User's Success Criteria

### At Launch

Pieskieo should be able to **completely replace**:

1. **PostgreSQL**: For a company using Postgres for their main DB
2. **MongoDB**: For a company using Mongo for document storage
3. **Weaviate**: For a company doing semantic search
4. **LanceDB**: For a company doing vector analytics
5. **Kùzu**: For a company doing graph analytics

**And do it with**:
- Better performance (lower latency, higher throughput)
- Lower ops complexity (one DB instead of 3-4)
- Lower cost (one license, one cloud bill)
- Better developer experience (one query language)

### Killer Demos

User wants to show queries like:

```sql
-- Find similar products, with trending tags from social graph,
-- grouped by category analytics, with JSON metadata filtering
SELECT 
    p.name,
    p.metadata->>'brand' AS brand,
    COUNT(DISTINCT g.user_id) AS social_mentions,
    similarity(p.embedding, $search_embedding) AS relevance
FROM products p
JOIN (
    SELECT product_id, user_id 
    FROM graph_traverse(
        start: $user_id,
        relationship: 'RECOMMENDED',
        depth: 1..2
    )
) g ON g.product_id = p.id
WHERE 
    p.embedding <-> $search_embedding < 0.4
    AND p.metadata @> '{"in_stock": true}'
    AND p.category IN (
        SELECT category FROM categories 
        WHERE trend_score > 0.8
    )
GROUP BY p.id, p.name, p.metadata, p.embedding
ORDER BY relevance DESC, social_mentions DESC
LIMIT 20;
```

**And have it run in < 10ms.**

---

## User's Constraints & Requirements

### What User WILL Accept

✅ Taking time to plan properly (157 detailed plans)
✅ Multiple responses to create all plans (due to token limits)
✅ Asking clarifying questions when ambiguous
✅ Proposing better algorithms than originally suggested

### What User WILL NOT Accept

❌ "Let's start simple and iterate"
❌ "Known limitations" in plan documents
❌ "We'll optimize this later"
❌ "For the initial version, we'll skip X"
❌ Anything that isn't production-ready

---

## Open Questions (To Clarify with User)

### 1. Concurrency Control

- **Snapshot Isolation** (like Postgres)? 
- OR **Serializable** (stricter, slower)?
- OR **Optimistic Concurrency Control** (better for low contention)?

### 2. Replication

- **Synchronous** (strong consistency, slower)?
- OR **Asynchronous** (eventual consistency, faster)?
- OR **Tunable** (user choice per query)?

### 3. Sharding Strategy

- **Hash-based** (even distribution)?
- OR **Range-based** (ordered, easier range scans)?
- OR **Learned** (ML-based, optimal for workload)?

### 4. Vector Index

- **HNSW only** (fast, high recall)?
- OR **IVF-PQ** (memory-efficient, lower recall)?
- OR **Hybrid** (HNSW for hot, IVF for cold)?

*(These should be clarified before finalizing core architecture plans)*

---

## Evolution of Requirements

### Initial Understanding (2026-02-08 morning)

- Build Pieskieo with PostgreSQL/MongoDB/Vector/Graph features
- Create plans before implementation
- Aim for production quality

### Refined Understanding (2026-02-08 afternoon)

- **ZERO compromises** - production from day 1
- **ALL optimizations** included upfront
- **NO "known limitations"** sections
- **157 features** fully planned before ANY code
- **Best algorithms** from the start, not "good enough first"

**Key realization**: User doesn't want an MVP, they want **the definitive database solution** to replace 5 industry leaders.

---

## User's Commitment

User is willing to:
- ✅ Wait for comprehensive planning (all 157 plans)
- ✅ Accept multiple responses due to token limits
- ✅ Review all plans before implementation starts
- ✅ Invest time in getting it right upfront

User is NOT willing to:
- ❌ Accept shortcuts
- ❌ Defer hard problems
- ❌ Ship with known limitations
- ❌ "Move fast and break things"

**Philosophy**: Measure twice, cut once. Plan thoroughly, implement perfectly.

---

## Action Items for Agent

### Immediate (Current Session)

1. ✅ Create this documentation (AGENT.md + USER_VISION.md)
2. ⏳ Revise 14 existing plans to remove all limitations
3. ⏳ Continue creating remaining 143 plans with production-ready approach

### Before Implementation

1. Cross-reference all 157 plans for consistency
2. Validate dependency graph (implementation order)
3. Get user approval on all plans
4. Create master implementation roadmap

### During Implementation

1. Follow TDD (tests first)
2. Benchmark every component
3. Profile and optimize
4. Document as we go
5. No shortcuts, ever

---

**Document Owner**: Project Owner  
**Last Updated**: 2026-02-08  
**Status**: Living document (update as user provides more guidance)
