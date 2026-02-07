# Pieskieo Delivery Plan (snapshot – Feb 7, 2026)

## Vision
Unified multi‑model engine: relational (Postgres‑ish), document (Mongo‑ish), vector (Weaviate/Lance/Kuzu‑ish), and graph (mesh + BFS/DFS) with one PQL surface, HTTP API, CLI, and SDKs.

## Architecture (current)
- Storage: WAL + snapshots, per‑shard directories, equality indexes, HNSW with persistent graph + revmap, mesh edges.
- Query: PQL parser/planner/executor for SELECT/INSERT/UPDATE/DELETE, aggregates, ORDER BY, equality JOIN.
- Services: Axum HTTP API, metrics, sharded fan‑out, auth (users/roles, Argon2id), CLI REPL, Python SDK.

## Status vs Roadmap
- [x] Core storage, WAL, snapshots, single-process sharding.
- [x] PQL (CRUD, aggregates, ORDER BY, equality JOIN).
- [x] Docs + rows + schemas + secondary indexes.
- [x] Vector HNSW (persistent, rebuild, vacuum, filters).
- [x] Graph edges + BFS/DFS + neighbors.
- [x] Auth (multi-user, roles, Argon2id, lockout), bearer + basic, default admin fallback.
- [x] CLI (connect/repl/serve), Python SDK (sync/async).
- [x] Metrics endpoint.
- [x] Cost-based optimizer v2 (metrics-driven equality index selection).
- [x] Resharding with verification + streaming replication (push/pull); TLS optional via feature flag.
- [ ] Vector+graph co-search planner; auto-rebalance + follower-reads; release pipeline (dist artifacts, signing), published benchmarks.

## Near-term TODO (next moves)
1) Harden surface: ship TLS-on-by-default presets, finalize log + audit shipping.  
2) Planner v3: vector+filter fusion, join costing.  
3) Distributed story: auto-rebalance + follower reads; finish end-to-end WAL tail apply with rate limiting; multi-line CLI + auth retry shipped.  
4) Benchmarks: publish latency/recall for 768 & 3072 dims; crash/chaos matrix.  
5) Release: cargo-dist artifacts, checksums, signed releases, docs site.  

## Security defaults
- Argon2id hashing; password complexity enforced on create; lockout 5 tries / 15m then 5m block (env tunable).
- TLS via `PIESKIEO_TLS_CERT` + `PIESKIEO_TLS_KEY` when built with `--features tls`.

## How to run (prod-ish)
```
PIESKIEO_DATA=/var/lib/pieskieo \
PIESKIEO_LISTEN=0.0.0.0:8443 \
PIESKIEO_TLS_CERT=/etc/pieskieo/cert.pem \
PIESKIEO_TLS_KEY=/etc/pieskieo/key.pem \
PIESKIEO_USERS='[{"user":"admin","pass":"S3cure!Pwd","role":"admin"}]' \
PIESKIEO_SHARD_TOTAL=4 \
PIESKIEO_EF_SEARCH=50 \
PIESKIEO_EF_CONSTRUCTION=200 \
cargo run -p pieskieo-server --release --features tls
```
