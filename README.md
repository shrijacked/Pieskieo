# Pieskieo

A Rust-first multimodal database engine combining document (Mongo-like), row (Postgres-ish), vector (Weaviate/Lance/Kuzu-ish), and mesh graph primitives in one binary. Current state: production-leaning MVP with HNSW ANN, metadata filters, automatic mesh links, snapshot/WAL durability, and internal sharding.

## Layout
- `crates/pieskieo-core`: storage engine (WAL, snapshot, HNSW, vector metadata, graph mesh, auto-linking, shard enforcement).
- `crates/pieskieo-server`: Axum HTTP API with transparent intra-process sharding and fan-out search, metrics, and load generator (`src/bin/load.rs`).
- `tools/`: local toolchain helpers (mingw/llvm downloads).

## Build & run (Windows)
1. Rust toolchain installed.
2. Linker: VS Build Tools (Desktop C++) **or** MinGW (set `PATH` to MinGW bin).
3. Build: `cargo build --release`
4. Run server (HTTP API):
```
PIESKIEO_DATA=./data \
PIESKIEO_LISTEN=0.0.0.0:8000 \
PIESKIEO_SHARD_TOTAL=1 \
PIESKIEO_EF_SEARCH=50 \
PIESKIEO_EF_CONSTRUCTION=200 \
cargo run -p pieskieo-server --release
```
5. CLI (embedded or server starter):
   - Embedded shell: `cargo run -p pieskieo-cli -- --repl`
   - Start server via CLI: `cargo run -p pieskieo-cli -- --serve --data-dir ./data --listen 0.0.0.0:8000`

## Key features
- HNSW ANN with persistence (graph + revmap saved/reloaded).
- Vector metadata upsert, filter, delete-keys.
- Mesh graph with auto KNN linking per insert (configurable `PIESKIEO_LINK_K`).
- Transparent sharding inside one process (hash on UUID); fan-out search merges top-k.
- WAL + snapshot; vacuum to drop tombstones and truncate WAL.
- Metrics endpoint (Prometheus text) including per-shard gauges.
- Secondary equality indexes for docs/rows (string/number/bool) scoped per namespace+collection/table for faster filtered queries.
- Namespaces + collections/tables, plus per-namespace vector indexes.
- Python SDK (sync + async) with Pydantic models.

## HTTP API (JSON)
- Health: `GET /healthz`
- Docs/rows: `POST /v1/doc`, `GET/DELETE /v1/doc/:id`; `POST /v1/row`, `GET/DELETE /v1/row/:id`
- Vectors:
  - `POST /v1/vector` `{id, vector, meta?}`
  - `POST /v1/vector/bulk` `[{id, vector, meta?}]`
  - `POST /v1/vector/search` `{query, k?, metric?, ef_search?, filter_ids?, filter_meta?}`
  - `POST /v1/vector/:id/meta` `{meta}` (merge)
  - `POST /v1/vector/:id/meta/delete` `{keys}`
  - `GET /v1/vector/:id`
  - `DELETE /v1/vector/:id`
  - `POST /v1/vector/rebuild` | `POST /v1/vector/vacuum` | `POST /v1/vector/snapshot/save`
- Graph: `POST /v1/graph/edge` `{src,dst,weight?}`, `GET /v1/graph/:id`
- Shard info: `GET /v1/shard/which/:id`
- Metrics: `GET /metrics`

## Benchmark tools
- Core bench: `cargo run -p pieskieo-core --bin bench --release -- <n> <dim> [ef_c] [ef_s]`
- HTTP load: `cargo run -p pieskieo-server --bin load --release -- <url> <dim> <n_vec> <searches>`

## License
GPL-2.0-only (see LICENSE).
