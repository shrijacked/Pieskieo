# Pieskieo Delivery Plan (snapshot – Feb 7, 2026)

## Vision
Unified multi‑model engine: relational (Postgres‑ish), document (Mongo‑ish), vector (Weaviate/Lance/Kuzu‑ish), and graph (mesh + BFS/DFS) with one PQL surface, HTTP API, CLI, and SDKs.

## Architecture (current)
- Storage: WAL + snapshots, per‑shard directories, equality indexes, HNSW with persistent graph + revmap, mesh edges.
- Query: PQL parser/planner/executor for SELECT/INSERT/UPDATE/DELETE, aggregates, ORDER BY, equality JOIN.
- Services: Axum HTTP API, metrics, sharded fan‑out, auth (users/roles, Argon2id), CLI REPL, Python SDK.

## Status vs Roadmap
- ✅ Core storage, WAL, snapshots, single‑process sharding.
- ✅ PQL (CRUD, aggregates, ORDER BY, JOIN eq).
- ✅ Docs + rows + schemas + secondary indexes.
- ✅ Vector HNSW (persistent, rebuild, vacuum, filters).
- ✅ Graph edges + BFS/DFS + neighbors.
- ✅ Auth (multi‑user, roles, Argon2id, lockout), bearer + basic, default admin fallback.
- ✅ CLI (connect/repl/serve), Python SDK (sync/async).
- ✅ Metrics endpoint.
- ⏳ TLS runtime (added, optional via `--features tls`).
- ⏳ Security hardening extras (rate‑limit, audit log).
- ⏳ Distributed reshard/replication, cost‑based optimizer v2, vector+graph co-search planner.
- ⏳ Release pipeline (dist artifacts, signing), public benchmarks.

## Near-term TODO (next moves)
1) Harden surface: TLS by default in prod, per‑IP/backoff rate limits (done), audit logs with rotation (done).  
2) Planner v2: cost model + index selection, vector+filter fusion (next).  
3) Distributed story: reshard without downtime, async replication + follower reads (incremental WAL + admin reshard endpoint + CLI follower pull loop added; future: streaming/push + pause/verify workflows).  
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
