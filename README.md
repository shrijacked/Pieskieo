# KaedeDB (alpha)

A Rust-first multimodal database engine that unifies document (Mongo-like), relational (Postgres-like), vector (Weaviate-like), and graph (mesh) primitives under one binary. The current drop provides an MVP storage core and HTTP API scaffold ready to extend toward production.

## Layout
- `crates/kaededb-core`: storage engine (WAL, in-memory collections, vector index, graph adjacency) with MVCC-ready WAL replay.
- `crates/kaededb-server`: Axum-based HTTP API exposing docs, vectors, and graph edges.
- `tools/` (local only): toolchain helper downloads (e.g., llvm-mingw) used during setup.

## Build & run (Windows)
1. Install Rust (already done via `rustup`).
2. Install a C/C++ linker:
   - Recommended: Visual Studio Build Tools 2022 with *Desktop C++* workload (gives `link.exe`).
   - Alternative: MinGW-w64 + Rust `stable-x86_64-pc-windows-gnu` toolchain; set `CARGO_BUILD_TARGET=x86_64-pc-windows-gnu` and ensure `x86_64-w64-mingw32-gcc` with import libs is on `PATH`.
3. From repo root: `cargo build` (or `cargo build --release`).
4. Run server: `cargo run -p kaededb-server` (env: `KAEDEDB_DATA=./data`, `KAEDEDB_LISTEN=0.0.0.0:8000`).

## API sketch (HTTP JSON)
- `GET /healthz` ? `"ok"`
- `POST /v1/doc` body `{ id?: uuid, data: json }` ? returns generated/used `id`.
- `GET /v1/doc/:id` ? stored JSON.
- `POST /v1/vector` body `{ id: uuid, vector: number[] }` ? store vector tied to id.
- `POST /v1/vector/search` body `{ query: number[], k?: number }` ? top-k vector hits (linear L2 for now).
- `POST /v1/graph/edge` body `{ src: uuid, dst: uuid, weight?: number }` ? add directed edge.
- `GET /v1/graph/:id` ? neighbors for vertex id.

## Engine notes
- WAL: length-prefixed bincode records (`wal.log` per data directory); replay bootstraps in-memory state.
- Collections: `Doc` and `Row` buckets stored as JSON values for now; ready for columnar/LSM upgrade.
- Vector index: in-memory store with L2 scoring (placeholder for HNSW/IVF/quantization).
- Graph store: adjacency list with weight; integrates with WAL for durability.
- Concurrency: coarse `RwLock` protecting collections; vector/graph stores are internally locked.

## Next steps
- Swap linear vector search with HNSW + filters; add background indexer fed by WAL.
- Implement SSTable/LSM flush/compaction and on-disk indices.
- Add Cypher-lite planner and SQL/JSON bridging.
- Cluster mesh: gossip membership + consistent hashing for shard routing.
- Observability: tracing spans, metrics, structured errors.

## Testing
- Core crate includes async tests for doc/vector roundtrip and graph neighbors (`cargo test -p kaededb-core`).
- Build currently blocked on missing Windows linker; install VS Build Tools or MinGW to run the suite.
