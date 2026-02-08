# Unified Query Language Design (PQL v2.0)

**Status**: 🟡 Design Phase  
**Priority**: CRITICAL - This is the foundation  
**Goal**: Single query language that can express relational, document, vector, and graph operations in ONE unified statement

---

## Design Philosophy

### Core Principles

1. **Composable Operations** - Each operation outputs data that flows to the next
2. **Type Flexibility** - Same syntax works on tables, documents, vectors, graphs
3. **Natural Flow** - Read left-to-right or top-to-bottom like a data pipeline
4. **Zero Context Switching** - Mix all features in one query without mode changes
5. **Optimizable** - Engine can reorder/optimize operations automatically

---

## Unified Query Syntax (Draft)

### Basic Structure
```
QUERY [source]
  [operations...]
  [output...]
```

### Example: AI Memory Query (All Features in One)
```
QUERY memories
  // Vector search for semantic similarity
  SIMILAR TO embed("what did we discuss about the project?") TOP 20
  
  // Graph traversal to find related concepts
  TRAVERSE edges WHERE type = "relates_to" DEPTH 1..3
  
  // Filter by metadata (document query)
  WHERE metadata.importance > 0.7 
    AND metadata.timestamp > @week_ago
    AND user_id IN @active_users
  
  // Join with relational user data
  JOIN users ON memories.user_id = users.id
  
  // Aggregate results
  GROUP BY users.name, DATE(metadata.timestamp)
  COMPUTE {
    memory_count: COUNT(),
    avg_importance: AVG(metadata.importance),
    topics: COLLECT(DISTINCT metadata.tags)
  }
  
  // Sort and limit
  ORDER BY avg_importance DESC
  LIMIT 10
  
  // Shape output
  SELECT {
    user: users.name,
    date: DATE(metadata.timestamp),
    memories: COLLECT({
      content: content,
      importance: metadata.importance,
      related_count: COUNT(traversed_edges)
    }),
    stats: {
      count: memory_count,
      avg: avg_importance,
      topics: topics
    }
  }
```

**This single query:**
1. Does vector search on embeddings
2. Traverses graph relationships
3. Filters JSON metadata
4. Joins with relational table
5. Aggregates results
6. Shapes final output

---

## Operation Types

### 1. Data Source Operations

```
// From a collection/table
QUERY users

// From multiple sources (union)
QUERY (products, archived_products)

// From vector index
QUERY @vector_index:memories

// From graph
QUERY @graph:social_network

// Inline data
QUERY [
  {id: 1, name: "Alice"},
  {id: 2, name: "Bob"}
]
```

### 2. Vector Operations

```
// Similarity search
SIMILAR TO [0.1, 0.2, ...] TOP 10

// With metadata filtering
SIMILAR TO @query_vector 
  WHERE category = "tech" 
  TOP 50

// Hybrid search (vector + keyword)
SEARCH {
  vector: embed("AI memory systems"),
  keywords: "memory graph database",
  weights: [0.7, 0.3]
} TOP 20

// Multiple vector spaces
SIMILAR TO {
  text_embedding: @text_vec,
  image_embedding: @img_vec,
  weights: [0.6, 0.4]
}
```

### 3. Graph Operations

```
// Traverse edges
TRAVERSE edges 
  WHERE type IN ["knows", "works_with"]
  DEPTH 1..3

// Pattern matching (Cypher-like but unified)
MATCH (user:User)-[rel:FRIEND]->(friend:User)
  WHERE user.age > 25

// Shortest path
PATH SHORTEST FROM @start TO @end
  THROUGH edges WHERE weight > 0

// Graph algorithms
COMPUTE PAGERANK ON edges
COMPUTE COMMUNITY USING louvain
```

### 4. Document Operations

```
// Navigate nested fields
WHERE metadata.user.preferences.theme = "dark"

// Array operations
WHERE tags CONTAINS "AI"
WHERE "premium" IN user.roles

// Unwind arrays
UNWIND items AS item

// Regex matching
WHERE description MATCHES /database.*query/i
```

### 5. Relational Operations

```
// Joins
JOIN users ON memories.user_id = users.id
LEFT JOIN profiles ON users.profile_id = profiles.id

// Subqueries (but integrated)
WHERE user_id IN (
  QUERY active_sessions
  WHERE last_seen > @yesterday
  SELECT user_id
)

// Aggregations
GROUP BY category, DATE(created_at)
COMPUTE {
  total: SUM(amount),
  avg: AVG(price),
  count: COUNT()
}
```

### 6. Filtering & Conditions

```
WHERE condition AND condition OR condition

// Operators work across all types
WHERE price > 100                    // relational
WHERE metadata.score > 0.8           // document
WHERE DISTANCE(vector, @query) < 0.5 // vector
WHERE CONNECTED(node, @target)       // graph
```

### 7. Transformations

```
// Compute new fields
COMPUTE {
  full_name: CONCAT(first_name, " ", last_name),
  age_years: YEAR_DIFF(birth_date, NOW()),
  embedding: EMBED(content)
}

// Reshape documents
SELECT {
  id: id,
  user: {
    name: user.name,
    email: user.email
  },
  stats: {
    views: view_count,
    likes: like_count
  }
}
```

### 8. Sorting & Pagination

```
ORDER BY score DESC, created_at ASC
LIMIT 20 OFFSET 40

// Or
TAKE 20 SKIP 40
```

---

## Advanced Examples

### Example 1: Social Network Query
```
QUERY users
  // Find user
  WHERE username = "alice"
  
  // Get their friends (graph)
  TRAVERSE edges WHERE type = "friend" DEPTH 1
  
  // Find similar users by interests (vector)
  SIMILAR TO current.interests_embedding TOP 100
  
  // Filter by activity (document)
  WHERE metadata.active_last_30_days = true
  
  // Join with posts (relational)
  JOIN posts ON users.id = posts.author_id
  
  // Aggregate
  GROUP BY users.id
  COMPUTE {
    friend_count: COUNT(DISTINCT friend_id),
    post_count: COUNT(posts),
    avg_engagement: AVG(posts.likes + posts.comments)
  }
  
  ORDER BY avg_engagement DESC
  LIMIT 10
```

### Example 2: E-Commerce Product Recommendation
```
QUERY products
  // Vector search for similar products
  SIMILAR TO @viewed_product.embedding TOP 50
  
  // Graph: frequently bought together
  TRAVERSE purchase_graph 
    WHERE edge.co_purchase_count > 10
    DEPTH 1..2
  
  // Filter inventory (relational)
  WHERE stock > 0 AND active = true
  
  // Personalization (document filtering)
  WHERE NOT (id IN @user.purchased_ids)
    AND category IN @user.preferred_categories
  
  // Join pricing (relational)
  JOIN price_tiers ON products.tier = price_tiers.tier
  
  // Score and rank
  COMPUTE {
    similarity_score: VECTOR_SCORE(),
    graph_score: GRAPH_CENTRALITY(),
    final_score: 0.5 * similarity_score + 0.5 * graph_score
  }
  
  ORDER BY final_score DESC
  LIMIT 20
  
  SELECT {
    id: id,
    name: name,
    price: price_tiers.price,
    why_recommended: {
      similar_to: @viewed_product.name,
      bought_with: COLLECT(related_products.name),
      match_categories: INTERSECT(category, @user.preferred_categories)
    }
  }
```

### Example 3: Time-Series Analytics with Vectors
```
QUERY sensor_data
  // Time range (relational)
  WHERE timestamp BETWEEN @start_time AND @end_time
  
  // Anomaly detection (vector)
  COMPUTE {
    expected_pattern: AVG_VECTOR(normal_readings.vector),
    anomaly_score: DISTANCE(reading_vector, expected_pattern)
  }
  
  // Find anomalies
  WHERE anomaly_score > 2.0
  
  // Graph: find related sensors
  TRAVERSE sensor_network 
    WHERE edge.type = "physically_adjacent"
    DEPTH 1
  
  // Temporal grouping
  GROUP BY sensor_id, HOUR(timestamp)
  COMPUTE {
    anomaly_count: COUNT(),
    max_deviation: MAX(anomaly_score),
    affected_neighbors: COUNT(DISTINCT adjacent_sensors)
  }
  
  // Alert threshold
  WHERE anomaly_count > 3 OR affected_neighbors > 2
  
  ORDER BY max_deviation DESC
```

---

## Implementation Strategy

### Phase 1: Parser
```rust
pub struct UnifiedQuery {
    source: DataSource,
    operations: Vec<Operation>,
}

pub enum DataSource {
    Collection(String),
    VectorIndex { name: String, namespace: Option<String> },
    Graph(String),
    Union(Vec<DataSource>),
    Inline(Vec<Value>),
}

pub enum Operation {
    // Vector
    VectorSearch { 
        query: Vector, 
        top_k: usize,
        filters: Vec<Condition>
    },
    HybridSearch {
        vector: Vector,
        keywords: String,
        weights: (f32, f32),
    },
    
    // Graph
    Traverse {
        edge_filter: Vec<Condition>,
        depth: Range<usize>,
    },
    Match { pattern: GraphPattern },
    ShortestPath { from: NodeRef, to: NodeRef },
    
    // Filtering
    Filter(Vec<Condition>),
    
    // Joins
    Join {
        right: DataSource,
        on: JoinCondition,
        join_type: JoinType,
    },
    
    // Transformations
    Compute { fields: Vec<ComputedField> },
    Unwind { field: String },
    
    // Aggregation
    GroupBy {
        keys: Vec<String>,
        aggregates: Vec<Aggregate>,
    },
    
    // Ordering
    Sort { fields: Vec<(String, SortOrder)> },
    Limit { count: usize, offset: usize },
    
    // Output shaping
    Project { fields: ProjectionSpec },
}
```

### Phase 2: Execution Engine
```rust
pub struct UnifiedExecutor {
    db: Arc<PieskieoDb>,
}

impl UnifiedExecutor {
    pub fn execute(&self, query: UnifiedQuery) -> Result<QueryResult> {
        // 1. Load initial data
        let mut current = self.load_source(query.source)?;
        
        // 2. Apply operations in sequence
        for op in query.operations {
            current = self.execute_operation(op, current)?;
        }
        
        Ok(current)
    }
    
    fn execute_operation(
        &self,
        op: Operation,
        input: DataStream,
    ) -> Result<DataStream> {
        match op {
            Operation::VectorSearch { query, top_k, filters } => {
                self.execute_vector_search(input, query, top_k, filters)
            }
            Operation::Traverse { edge_filter, depth } => {
                self.execute_graph_traverse(input, edge_filter, depth)
            }
            Operation::Filter(conditions) => {
                self.execute_filter(input, conditions)
            }
            Operation::Join { right, on, join_type } => {
                self.execute_join(input, right, on, join_type)
            }
            // ... other operations
        }
    }
}
```

### Phase 3: Optimizer
```rust
pub struct QueryOptimizer;

impl QueryOptimizer {
    pub fn optimize(&self, query: UnifiedQuery) -> UnifiedQuery {
        let mut optimized = query;
        
        // 1. Push filters down (execute as early as possible)
        optimized = self.push_down_filters(optimized);
        
        // 2. Reorder joins for efficiency
        optimized = self.optimize_join_order(optimized);
        
        // 3. Convert operations to index scans when possible
        optimized = self.use_indexes(optimized);
        
        // 4. Parallelize independent operations
        optimized = self.identify_parallelism(optimized);
        
        // 5. Fuse operations (combine adjacent filters, etc.)
        optimized = self.fuse_operations(optimized);
        
        optimized
    }
}
```

---

## Syntax Sugar & Shortcuts

```
// Instead of QUERY ... WHERE ... SELECT
FIND users WHERE age > 25

// Quick vector search
FIND SIMILAR "search query" IN documents TOP 10

// Graph shorthand
FIND CONNECTED users TO @target THROUGH friends

// Time-series
FIND RECENT messages SINCE @yesterday

// Hybrid quick search
SEARCH "artificial intelligence" IN articles
  BOOST relevance_score
  LIMIT 20
```

---

## Next Steps

1. **Finalize Syntax** - Get feedback, iterate on ergonomics
2. **Build Parser** - Implement full query parser
3. **Execution Engine** - Build unified executor
4. **Optimizer** - Implement query optimization
5. **Test Suite** - Comprehensive tests for all combinations
6. **Documentation** - Full language reference

---

**Created**: 2026-02-08  
**Status: Production-Ready  
**This is the foundation for everything else!**
