# PostgreSQL Feature: Deadlock Detection

**Status**: 🔴 Not Started  
**Priority**: High  
**Dependencies**: 05-acid.md (ACID transactions), 06-isolation.md (Isolation levels)  
**Estimated Effort**: 2-3 weeks

---

## Overview

Deadlock detection identifies circular wait conditions where two or more transactions are waiting for each other's locks, causing permanent blocking. The system must detect these cycles and abort one transaction to break the deadlock.

**Classic Deadlock Example**:
```
Transaction 1: Locks Row A → Waits for Row B
Transaction 2: Locks Row B → Waits for Row A
Result: Both stuck forever unless system intervenes
```

---

## Deadlock Types

### 1. Simple Two-Transaction Deadlock
```sql
-- Transaction T1
BEGIN;
UPDATE accounts SET balance = balance - 100 WHERE id = 'acc1'; -- Locks acc1
-- ... pause ...
UPDATE accounts SET balance = balance + 100 WHERE id = 'acc2'; -- Waits for acc2

-- Transaction T2 (concurrent)
BEGIN;
UPDATE accounts SET balance = balance - 50 WHERE id = 'acc2'; -- Locks acc2
-- ... pause ...
UPDATE accounts SET balance = balance + 50 WHERE id = 'acc1'; -- Waits for acc1

-- DEADLOCK! T1 waits for T2, T2 waits for T1
```

### 2. Multi-Transaction Cycle
```
T1 locks A, waits for B
T2 locks B, waits for C
T3 locks C, waits for A

Cycle: T1 → T2 → T3 → T1
```

### 3. Index Deadlock
```sql
-- T1
UPDATE products SET price = 100 WHERE category = 'electronics' AND id = 'p1';

-- T2 (different order due to index scan)
UPDATE products SET price = 200 WHERE category = 'electronics' AND id = 'p2';

-- Can deadlock on index locks if acquired in different orders
```

---

## Implementation Plan

### Phase 1: Wait-For Graph

**File**: `crates/pieskieo-core/src/deadlock.rs`

```rust
use std::collections::{HashMap, HashSet};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::algo::is_cyclic_directed;

#[derive(Debug, Clone)]
pub struct LockWait {
    pub waiting_txn: Uuid,           // Transaction waiting
    pub blocking_txn: Uuid,          // Transaction holding lock
    pub resource: ResourceId,        // What's being waited for
    pub wait_start: SystemTime,
}

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum ResourceId {
    Row { shard_id: u32, record_id: Uuid },
    Table { name: String },
    Index { table: String, name: String },
}

pub struct DeadlockDetector {
    // Wait-for graph: edge from waiting_txn -> blocking_txn
    wait_graph: DiGraph<Uuid, ResourceId>,
    txn_to_node: HashMap<Uuid, NodeIndex>,
    
    // All active waits
    active_waits: Vec<LockWait>,
    
    // Detection frequency
    check_interval: Duration,
    last_check: Instant,
}

impl DeadlockDetector {
    pub fn new() -> Self {
        Self {
            wait_graph: DiGraph::new(),
            txn_to_node: HashMap::new(),
            active_waits: Vec::new(),
            check_interval: Duration::from_millis(100),
            last_check: Instant::now(),
        }
    }
    
    pub fn add_wait(&mut self, wait: LockWait) {
        // Add nodes if they don't exist
        let waiting_node = self.get_or_create_node(wait.waiting_txn);
        let blocking_node = self.get_or_create_node(wait.blocking_txn);
        
        // Add edge: waiting -> blocking
        self.wait_graph.add_edge(waiting_node, blocking_node, wait.resource.clone());
        
        self.active_waits.push(wait);
    }
    
    pub fn remove_wait(&mut self, waiting_txn: Uuid, resource: &ResourceId) {
        self.active_waits.retain(|w| 
            !(w.waiting_txn == waiting_txn && &w.resource == resource)
        );
        
        // Rebuild graph (expensive but simple)
        self.rebuild_graph();
    }
    
    fn get_or_create_node(&mut self, txn_id: Uuid) -> NodeIndex {
        *self.txn_to_node.entry(txn_id).or_insert_with(|| {
            self.wait_graph.add_node(txn_id)
        })
    }
    
    fn rebuild_graph(&mut self) {
        self.wait_graph.clear();
        self.txn_to_node.clear();
        
        for wait in &self.active_waits {
            let waiting_node = self.get_or_create_node(wait.waiting_txn);
            let blocking_node = self.get_or_create_node(wait.blocking_txn);
            self.wait_graph.add_edge(waiting_node, blocking_node, wait.resource.clone());
        }
    }
    
    pub fn detect_deadlock(&mut self) -> Option<Vec<Uuid>> {
        // Only check periodically
        if self.last_check.elapsed() < self.check_interval {
            return None;
        }
        self.last_check = Instant::now();
        
        // Check if graph has cycle
        if !is_cyclic_directed(&self.wait_graph) {
            return None;
        }
        
        // Find the cycle
        self.find_cycle()
    }
    
    fn find_cycle(&self) -> Option<Vec<Uuid>> {
        use petgraph::visit::Dfs;
        
        // DFS to find cycle
        for start_node in self.wait_graph.node_indices() {
            let mut dfs = Dfs::new(&self.wait_graph, start_node);
            let mut path = Vec::new();
            let mut visited = HashSet::new();
            
            if let Some(cycle) = self.dfs_find_cycle(start_node, &mut visited, &mut path) {
                return Some(cycle);
            }
        }
        
        None
    }
    
    fn dfs_find_cycle(
        &self,
        node: NodeIndex,
        visited: &mut HashSet<NodeIndex>,
        path: &mut Vec<Uuid>,
    ) -> Option<Vec<Uuid>> {
        if visited.contains(&node) {
            // Found cycle
            if let Some(pos) = path.iter().position(|txn| {
                self.txn_to_node.get(txn).copied() == Some(node)
            }) {
                return Some(path[pos..].to_vec());
            }
        }
        
        visited.insert(node);
        let txn_id = *self.wait_graph.node_weight(node)?;
        path.push(txn_id);
        
        for neighbor in self.wait_graph.neighbors(node) {
            if let Some(cycle) = self.dfs_find_cycle(neighbor, visited, path) {
                return Some(cycle);
            }
        }
        
        path.pop();
        None
    }
}
```

### Phase 2: Victim Selection

```rust
pub struct DeadlockResolver {
    detector: DeadlockDetector,
}

impl DeadlockResolver {
    pub fn resolve_deadlock(&mut self, cycle: Vec<Uuid>) -> Uuid {
        // Choose victim using PostgreSQL's strategy:
        // 1. Prefer transaction that has done least work (lowest cost to abort)
        // 2. If tied, prefer youngest transaction
        
        let victim = cycle.iter()
            .min_by_key(|txn_id| {
                (self.get_transaction_cost(**txn_id), self.get_transaction_age(**txn_id))
            })
            .copied()
            .expect("cycle should not be empty");
        
        victim
    }
    
    fn get_transaction_cost(&self, txn_id: Uuid) -> u64 {
        // Estimate work done: number of locks held + rows modified
        let txn_manager = self.txn_manager.lock().unwrap();
        
        if let Ok(txn) = txn_manager.get_transaction(txn_id) {
            txn.write_set.len() as u64 + txn.locks_held.len() as u64
        } else {
            0
        }
    }
    
    fn get_transaction_age(&self, txn_id: Uuid) -> Duration {
        let txn_manager = self.txn_manager.lock().unwrap();
        
        if let Ok(txn) = txn_manager.get_transaction(txn_id) {
            txn.started_at.elapsed().unwrap_or_default()
        } else {
            Duration::from_secs(0)
        }
    }
}
```

### Phase 3: Integration with Lock Manager

**File**: `crates/pieskieo-core/src/lock_manager.rs`

```rust
pub struct LockManager {
    // Existing fields
    locks: HashMap<ResourceId, LockState>,
    
    // NEW
    deadlock_detector: Arc<Mutex<DeadlockDetector>>,
    deadlock_check_thread: Option<JoinHandle<()>>,
}

impl LockManager {
    pub fn acquire_lock(
        &self,
        txn_id: Uuid,
        resource: ResourceId,
        lock_type: LockType,
    ) -> Result<()> {
        loop {
            {
                let mut locks = self.locks.lock().unwrap();
                
                if locks.can_grant_lock(&resource, lock_type) {
                    locks.grant_lock(resource.clone(), txn_id, lock_type);
                    return Ok(());
                }
                
                // Lock not available, register wait
                let blocking_txns = locks.get_blocking_transactions(&resource, lock_type);
                
                for blocking_txn in blocking_txns {
                    let wait = LockWait {
                        waiting_txn: txn_id,
                        blocking_txn,
                        resource: resource.clone(),
                        wait_start: SystemTime::now(),
                    };
                    
                    self.deadlock_detector.lock().unwrap().add_wait(wait);
                }
            }
            
            // Check for deadlock
            if let Some(cycle) = self.deadlock_detector.lock().unwrap().detect_deadlock() {
                if cycle.contains(&txn_id) {
                    // This transaction is in deadlock cycle
                    let resolver = DeadlockResolver::new(self.deadlock_detector.clone());
                    let victim = resolver.resolve_deadlock(cycle.clone());
                    
                    if victim == txn_id {
                        // This transaction is the victim, abort it
                        return Err(PieskieoError::Deadlock {
                            message: format!("Deadlock detected, aborting transaction {:?}", txn_id),
                            cycle,
                        });
                    }
                }
            }
            
            // Wait and retry
            thread::sleep(Duration::from_millis(10));
        }
    }
    
    pub fn release_lock(&self, txn_id: Uuid, resource: &ResourceId) {
        self.locks.lock().unwrap().release_lock(resource, txn_id);
        self.deadlock_detector.lock().unwrap().remove_wait(txn_id, resource);
    }
}
```

### Phase 4: Background Detection Thread

```rust
impl LockManager {
    pub fn start_deadlock_detector(&mut self) {
        let detector = self.deadlock_detector.clone();
        let txn_manager = self.txn_manager.clone();
        
        let handle = thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_millis(100));
                
                let mut detector = detector.lock().unwrap();
                if let Some(cycle) = detector.detect_deadlock() {
                    // Choose victim
                    let resolver = DeadlockResolver::new(txn_manager.clone());
                    let victim = resolver.resolve_deadlock(cycle.clone());
                    
                    // Abort victim transaction
                    warn!("Deadlock detected, aborting transaction {:?}", victim);
                    txn_manager.lock().unwrap().abort_transaction(victim);
                }
            }
        });
        
        self.deadlock_check_thread = Some(handle);
    }
}
```

---

## Test Cases

### Test 1: Classic Two-Transaction Deadlock
```rust
#[tokio::test]
async fn test_simple_deadlock() {
    let db = PieskieoDb::new_in_memory().await.unwrap();
    
    db.execute("INSERT INTO accounts (id, balance) VALUES ('a1', 100), ('a2', 200)")
        .await.unwrap();
    
    let db1 = db.clone();
    let db2 = db.clone();
    
    // T1: Lock a1, then try to lock a2
    let t1 = tokio::spawn(async move {
        db1.execute("BEGIN").await.unwrap();
        db1.execute("UPDATE accounts SET balance = 90 WHERE id = 'a1'").await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        db1.execute("UPDATE accounts SET balance = 210 WHERE id = 'a2'").await
    });
    
    // T2: Lock a2, then try to lock a1
    let t2 = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        db2.execute("BEGIN").await.unwrap();
        db2.execute("UPDATE accounts SET balance = 190 WHERE id = 'a2'").await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        db2.execute("UPDATE accounts SET balance = 110 WHERE id = 'a1'").await
    });
    
    let (r1, r2) = tokio::join!(t1, t2);
    
    // One should succeed, one should get deadlock error
    assert!(r1.is_err() || r2.is_err());
    assert!(r1.is_ok() || r2.is_ok());
    
    if let Err(e) = r1 {
        assert!(e.to_string().contains("Deadlock"));
    }
    if let Err(e) = r2 {
        assert!(e.to_string().contains("Deadlock"));
    }
}
```

### Test 2: Three-Transaction Cycle
```rust
#[tokio::test]
async fn test_three_way_deadlock() {
    let db = PieskieoDb::new_in_memory().await.unwrap();
    
    db.execute("INSERT INTO resources (id) VALUES ('r1'), ('r2'), ('r3')")
        .await.unwrap();
    
    // T1: Lock r1, wait for r2
    // T2: Lock r2, wait for r3
    // T3: Lock r3, wait for r1
    
    // One of the three should be aborted
}
```

### Test 3: No False Positives
```sql
-- This is NOT a deadlock, just sequential waiting

-- T1
BEGIN;
UPDATE items SET quantity = 10 WHERE id = 'i1';
-- waits 100ms
UPDATE items SET quantity = 20 WHERE id = 'i2';
COMMIT;

-- T2 (waits for T1, but T1 will eventually release)
BEGIN;
UPDATE items SET quantity = 30 WHERE id = 'i1';
COMMIT;

-- Should NOT detect deadlock, T2 should succeed
```

### Test 4: Victim Selection
```rust
#[tokio::test]
async fn test_victim_selection() {
    let db = PieskieoDb::new_in_memory().await.unwrap();
    
    // T1: Has modified 1000 rows (high cost)
    // T2: Has modified 1 row (low cost)
    
    // Create deadlock between T1 and T2
    // T2 should be chosen as victim (lower cost)
}
```

---

## Performance Considerations

### 1. Detection Frequency
**Trade-off**: 
- Too frequent: High CPU overhead
- Too infrequent: Long deadlock delays

**Solution**: Adaptive frequency based on contention
```rust
impl DeadlockDetector {
    pub fn adaptive_check_interval(&mut self) {
        if self.active_waits.len() > 100 {
            self.check_interval = Duration::from_millis(50); // More frequent
        } else if self.active_waits.len() < 10 {
            self.check_interval = Duration::from_millis(500); // Less frequent
        } else {
            self.check_interval = Duration::from_millis(100); // Default
        }
    }
}
```

### 2. Wait-For Graph Overhead
**Problem**: Graph construction is O(n) for n waiting transactions

**Solution**: Incremental graph updates instead of full rebuild
```rust
impl DeadlockDetector {
    pub fn remove_wait_incremental(&mut self, waiting_txn: Uuid, resource: &ResourceId) {
        // Only remove specific edge, don't rebuild entire graph
        if let Some(waiting_node) = self.txn_to_node.get(&waiting_txn) {
            // Find and remove edge
            let edges_to_remove: Vec<_> = self.wait_graph
                .edges(*waiting_node)
                .filter(|e| e.weight() == resource)
                .map(|e| e.id())
                .collect();
            
            for edge in edges_to_remove {
                self.wait_graph.remove_edge(edge);
            }
        }
    }
}
```

### 3. False Deadlock Avoidance
Use timeout-based waiting to avoid marking slow queries as deadlocks
```rust
const LOCK_TIMEOUT: Duration = Duration::from_secs(30);

impl LockManager {
    pub fn acquire_lock_with_timeout(&self, ...) -> Result<()> {
        let start = Instant::now();
        
        loop {
            // Try acquire...
            
            if start.elapsed() > LOCK_TIMEOUT {
                return Err(PieskieoError::LockTimeout);
            }
            
            // Deadlock check...
        }
    }
}
```

---

## Error Messages

### Deadlock Error Format
```json
{
  "error": "Deadlock detected",
  "code": "40P01",
  "details": {
    "victim_transaction": "550e8400-e29b-41d4-a716-446655440000",
    "cycle": [
      "550e8400-e29b-41d4-a716-446655440000",
      "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
      "6ba7b814-9dad-11d1-80b4-00c04fd430c8"
    ],
    "message": "Transaction aborted due to deadlock. The transaction has been rolled back."
  }
}
```

---

## Metrics to Track

- `pieskieo_deadlocks_detected_total` - Counter
- `pieskieo_deadlocks_resolved_total` - Counter  
- `pieskieo_deadlock_detection_duration_ms` - Histogram
- `pieskieo_lock_waits_active` - Gauge (current waiting transactions)
- `pieskieo_lock_wait_duration_ms` - Histogram
- `pieskieo_deadlock_cycle_size` - Histogram (number of transactions in cycle)

---

## Implementation Checklist

- [ ] Create DeadlockDetector struct
- [ ] Implement wait-for graph (using petgraph)
- [ ] Add add_wait and remove_wait methods
- [ ] Implement cycle detection algorithm
- [ ] Create DeadlockResolver with victim selection
- [ ] Integrate with LockManager
- [ ] Add background detection thread
- [ ] Implement adaptive detection frequency
- [ ] Add comprehensive deadlock tests
- [ ] Test two-way deadlocks
- [ ] Test multi-way deadlocks (3+ transactions)
- [ ] Test victim selection logic
- [ ] Test false positive avoidance
- [ ] Add proper error messages
- [ ] Document deadlock resolution behavior
- [ ] Add metrics and monitoring
- [ ] Performance benchmark with high contention

---

## Distributed Deadlock Detection

### Global Wait-For Graph

```rust
pub struct DistributedDeadlockDetector {
    // Local wait-for graph (this node only)
    local_detector: DeadlockDetector,
    
    // Global wait-for graph coordinator
    coordinator_addr: NodeAddr,
    
    // Distributed wait information
    global_waits: Arc<DashMap<(Uuid, Uuid), GlobalWaitInfo>>,
    
    // Heartbeat for liveness detection
    last_heartbeat: Arc<RwLock<HashMap<NodeId, Instant>>>,
}

#[derive(Debug, Clone)]
pub struct GlobalWaitInfo {
    pub waiting_txn: Uuid,
    pub waiting_node: NodeId,
    pub blocking_txn: Uuid,
    pub blocking_node: NodeId,
    pub resource: ResourceId,
    pub wait_start: SystemTime,
}

impl DistributedDeadlockDetector {
    pub async fn register_cross_node_wait(
        &self,
        waiting_txn: Uuid,
        blocking_txn: Uuid,
        blocking_node: NodeId,
        resource: ResourceId,
    ) -> Result<()> {
        let wait_info = GlobalWaitInfo {
            waiting_txn,
            waiting_node: self.local_node_id(),
            blocking_txn,
            blocking_node,
            resource: resource.clone(),
            wait_start: SystemTime::now(),
        };
        
        // Register in local cache
        self.global_waits.insert((waiting_txn, blocking_txn), wait_info.clone());
        
        // Notify coordinator
        self.send_to_coordinator(CoordinatorMessage::RegisterWait(wait_info)).await?;
        
        Ok(())
    }
    
    pub async fn detect_global_deadlock(&self) -> Option<GlobalDeadlockCycle> {
        // Request global snapshot from coordinator
        let global_graph = self.request_global_graph().await.ok()?;
        
        // Build combined local + global graph
        let mut combined_graph = self.local_detector.wait_graph.clone();
        
        for wait in global_graph.waits {
            let waiting_node = combined_graph
                .add_node(wait.waiting_txn);
            let blocking_node = combined_graph
                .add_node(wait.blocking_txn);
            
            combined_graph.add_edge(waiting_node, blocking_node, wait.resource);
        }
        
        // Detect cycle in combined graph
        if let Some(cycle) = self.find_cycle_with_nodes(&combined_graph) {
            Some(GlobalDeadlockCycle {
                transactions: cycle.transactions,
                nodes: cycle.nodes,
                resources: cycle.resources,
            })
        } else {
            None
        }
    }
    
    async fn resolve_global_deadlock(
        &self,
        cycle: GlobalDeadlockCycle,
    ) -> Result<()> {
        // Choose victim using sophisticated algorithm
        let victim = self.choose_global_victim(&cycle)?;
        
        // Coordinate abort with victim's node
        if victim.node == self.local_node_id() {
            // Local victim - abort directly
            self.abort_transaction(victim.txn_id).await?;
        } else {
            // Remote victim - send abort request
            self.request_remote_abort(victim.node, victim.txn_id).await?;
        }
        
        // Clean up wait-for graph
        self.remove_transaction_from_graph(victim.txn_id)?;
        
        Ok(())
    }
    
    fn choose_global_victim(&self, cycle: &GlobalDeadlockCycle) -> Result<VictimInfo> {
        // Sophisticated victim selection considering:
        // 1. Transaction cost (work done)
        // 2. Transaction priority
        // 3. Number of other transactions waiting
        // 4. Lock types held
        // 5. Node load (prefer aborting on less loaded nodes)
        
        let mut best_victim = None;
        let mut best_score = f64::INFINITY;
        
        for (txn_id, node_id) in cycle.transactions.iter().zip(&cycle.nodes) {
            let cost = self.get_transaction_cost(*txn_id, *node_id).await?;
            let priority = self.get_transaction_priority(*txn_id, *node_id).await?;
            let waiters = self.count_waiters(*txn_id)?;
            let node_load = self.get_node_load(*node_id).await?;
            
            // Lower score = better victim
            let score = (cost as f64 * 0.4)
                      + (priority as f64 * 0.3)
                      + (waiters as f64 * 100.0 * 0.2)
                      + (node_load * 0.1);
            
            if score < best_score {
                best_score = score;
                best_victim = Some(VictimInfo {
                    txn_id: *txn_id,
                    node: *node_id,
                    cost,
                    score,
                });
            }
        }
        
        best_victim.ok_or_else(|| PieskieoError::Internal("no victim found".into()))
    }
}

### Distributed Deadlock Coordinator

```rust
pub struct DeadlockCoordinator {
    // Aggregated wait-for graph from all nodes
    global_graph: Arc<RwLock<DiGraph<TransactionNode, ResourceId>>>,
    
    // Node liveness tracking
    node_heartbeats: Arc<DashMap<NodeId, Instant>>,
    
    // Detection frequency (adaptive based on contention)
    check_interval: Arc<RwLock<Duration>>,
}

impl DeadlockCoordinator {
    pub async fn start(self: Arc<Self>) {
        // Background task: periodic deadlock detection
        let detector = self.clone();
        tokio::spawn(async move {
            loop {
                let interval = *detector.check_interval.read().await;
                tokio::time::sleep(interval).await;
                
                if let Some(cycle) = detector.detect_global_deadlock().await {
                    detector.resolve_deadlock(cycle).await.ok();
                }
                
                // Adaptive interval based on contention
                detector.adjust_check_interval().await;
            }
        });
        
        // Background task: clean up stale waits
        let cleaner = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                cleaner.clean_stale_waits().await.ok();
            }
        });
    }
    
    async fn detect_global_deadlock(&self) -> Option<GlobalDeadlockCycle> {
        let graph = self.global_graph.read().await;
        
        // Use Tarjan's algorithm for cycle detection (O(V+E))
        self.find_cycle_tarjan(&graph)
    }
    
    async fn clean_stale_waits(&self) -> Result<()> {
        let mut graph = self.global_graph.write().await;
        let now = Instant::now();
        
        // Remove waits from nodes that haven't sent heartbeat
        let stale_nodes: Vec<_> = self.node_heartbeats.iter()
            .filter(|entry| now.duration_since(*entry.value()) > Duration::from_secs(30))
            .map(|entry| *entry.key())
            .collect();
        
        for node_id in stale_nodes {
            // Remove all edges from stale node
            graph.retain_edges(|_, edge| {
                !self.edge_involves_node(edge, node_id)
            });
        }
        
        Ok(())
    }
}
```

## Advanced Deadlock Prevention (Proactive)

### Wait-Die Scheme (Timestamp Ordering)

```rust
pub struct WaitDiePreventionPolicy {
    // Transaction timestamps for ordering
    txn_timestamps: Arc<DashMap<Uuid, SystemTime>>,
}

impl LockManager {
    fn acquire_lock_wait_die(
        &self,
        txn_id: Uuid,
        resource: ResourceId,
        lock_type: LockType,
    ) -> Result<()> {
        let txn_timestamp = self.get_or_create_timestamp(txn_id);
        
        loop {
            let locks = self.locks.read();
            
            if locks.can_grant_lock(&resource, lock_type) {
                drop(locks);
                let mut locks = self.locks.write();
                locks.grant_lock(resource.clone(), txn_id, lock_type);
                return Ok(());
            }
            
            // Get blocking transactions
            let blockers = locks.get_blocking_transactions(&resource, lock_type);
            
            for blocker_id in blockers {
                let blocker_timestamp = self.get_or_create_timestamp(blocker_id);
                
                if txn_timestamp < blocker_timestamp {
                    // Older transaction - wait
                    continue;
                } else {
                    // Younger transaction - die (abort)
                    return Err(PieskieoError::Deadlock {
                        message: format!("Transaction {:?} aborted by wait-die", txn_id),
                        cycle: vec![txn_id],
                    });
                }
            }
            
            drop(locks);
            
            // Wait briefly before retry
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
```

### Wound-Wait Scheme (Alternative)

```rust
impl LockManager {
    fn acquire_lock_wound_wait(
        &self,
        txn_id: Uuid,
        resource: ResourceId,
        lock_type: LockType,
    ) -> Result<()> {
        let txn_timestamp = self.get_or_create_timestamp(txn_id);
        
        loop {
            let locks = self.locks.read();
            
            if locks.can_grant_lock(&resource, lock_type) {
                drop(locks);
                let mut locks = self.locks.write();
                locks.grant_lock(resource.clone(), txn_id, lock_type);
                return Ok(());
            }
            
            let blockers = locks.get_blocking_transactions(&resource, lock_type);
            
            for blocker_id in blockers {
                let blocker_timestamp = self.get_or_create_timestamp(blocker_id);
                
                if txn_timestamp < blocker_timestamp {
                    // Older transaction - wound (abort) younger blocker
                    drop(locks);
                    self.abort_transaction(blocker_id)?;
                    // Retry after aborting blocker
                    break;
                } else {
                    // Younger transaction - wait
                    continue;
                }
            }
            
            drop(locks);
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
```

## Production Monitoring & Operations

### Advanced Metrics

```rust
// Deadlock detection metrics
metrics::counter!("pieskieo_deadlocks_detected_total",
                  "type" => "local|global").increment(1);
metrics::counter!("pieskieo_deadlocks_resolved_total",
                  "victim_selection" => "cost_based|wait_die|wound_wait").increment(1);
metrics::histogram!("pieskieo_deadlock_detection_duration_ms").record(duration);
metrics::histogram!("pieskieo_deadlock_cycle_size").record(cycle_len);
metrics::histogram!("pieskieo_deadlock_resolution_duration_ms").record(resolution_time);

// Wait-for graph metrics
metrics::gauge!("pieskieo_lock_waits_active",
                "type" => "local|cross_node").set(wait_count);
metrics::gauge!("pieskieo_wait_for_graph_nodes").set(node_count);
metrics::gauge!("pieskieo_wait_for_graph_edges").set(edge_count);
metrics::histogram!("pieskieo_lock_wait_duration_ms").record(wait_duration);

// Prevention metrics
metrics::counter!("pieskieo_wait_die_aborts_total").increment(1);
metrics::counter!("pieskieo_wound_wait_aborts_total").increment(1);
```

### Operational Configuration

```toml
[deadlock_detection]
# Detection strategy: "reactive" (detect after), "wait_die", "wound_wait"
strategy = "reactive"

# Check interval for reactive detection (ms)
check_interval_ms = 100

# Adaptive interval: adjust based on contention
adaptive_interval = true

# Min/max intervals for adaptive mode
min_interval_ms = 50
max_interval_ms = 1000

# Distributed deadlock detection
distributed = true
coordinator_node = "node-1"

# Victim selection algorithm
victim_selection = "cost_based"  # or "youngest", "random"

# Memory limit for wait-for graph (MB)
graph_memory_limit_mb = 100
```

---

**Created**: 2026-02-08  
**Author**: Implementation Team  
**Review Status**: Production-Ready
