# Pieskieo

A Rust-first multimodal database engine combining document (Mongo-like), row (Postgres-ish), vector (Weaviate/Lance/Kuzu-ish), and mesh graph primitives in one binary. Current state: production-leaning MVP with HNSW ANN, metadata filters, automatic mesh links, snapshot/WAL durability, and internal sharding.

## Layout
- `crates/pieskieo-core`: storage engine (WAL, snapshot, HNSW, vector metadata, graph mesh, auto-linking, shard enforcement).
- `crates/pieskieo-server`: Axum HTTP API with transparent intra-process sharding and fan-out search, metrics, and load generator (`src/bin/load.rs`).
- `tools/`: local toolchain helpers (mingw/llvm downloads).

## PQL (Pieskieo Query Language)
- SQL-ish syntax over all models: `SELECT`, `INSERT`, `UPDATE`, `DELETE`, aliases, multi `ORDER BY`, aggregates (`COUNT/SUM/AVG/MIN/MAX`), equality `JOIN`.
- Works for rows and docs; vector search is JSON API today, PQL hooks coming.
- Example:
```sql
SELECT u.id, o.total
FROM users u
JOIN orders o ON u.id = o.user_id
WHERE o.total > 50
ORDER BY o.total DESC;
```

## Build & run (Windows)
1) Rust toolchain installed.  
2) Linker: VS Build Tools (Desktop C++) **or** MinGW (`tools/mingw64/bin` on PATH).  
3) Build: `cargo build --release`  
4) Run server (HTTP API, plaintext):
```powershell
$env:PIESKIEO_DATA=".\data"
$env:PIESKIEO_LISTEN="0.0.0.0:8000"
cargo run -p pieskieo-server --release
```
5) Enable TLS (requires `--features tls` at build time):
```powershell
$env:PIESKIEO_TLS_CERT="certs/server.crt"
$env:PIESKIEO_TLS_KEY="certs/server.key"
cargo run -p pieskieo-server --release --features tls
```
6) CLI (network shell, psql‑style):
```powershell
cargo run -p pieskieo-cli -- --connect pieskieo@localhost --port 8000 -W
```
`-W` prompts for password; use bearer with `-t <token>`. The REPL accepts raw PQL.

## Installers
- Windows: `pwsh install/install.ps1` (installs to `%ProgramData%\Pieskieo\bin` or `~\.local\bin`)
- Linux/macOS: `bash install/install.sh` (installs to `/usr/local/bin` or `~/.local/bin`)
Both scripts build from source with `--release` and copy `pieskieo-server`, `pieskieo`, `load`, `bench`.

### Prebuilt (auto-download, defaults to v0.1.2)
- Linux/macOS:  
  `curl -fsSL https://raw.githubusercontent.com/DarsheeeGamer/Pieskieo/main/install/get-pieskieo.sh | bash`
- Windows (PowerShell):  
  `iwr https://raw.githubusercontent.com/DarsheeeGamer/Pieskieo/main/install/get-pieskieo.ps1 -UseBasicParsing | iex`
Options: set `PIESKIEO_VERSION` to override (default v0.1.2); `PIESKIEO_PREFIX` to change install prefix.

## Key features
- HNSW ANN with persistence (graph + revmap saved/reloaded).
- Vector metadata upsert, filter, delete-keys.
- Mesh graph with auto KNN linking per insert (configurable `PIESKIEO_LINK_K`).
- Transparent sharding inside one process (hash on UUID); fan-out search merges top-k.
- WAL + snapshot; vacuum to drop tombstones and truncate WAL.
- Metrics endpoint (Prometheus text) including per-shard gauges.
- Secondary equality indexes for docs/rows (string/number/bool) scoped per namespace+collection/table; **planner picks the most selective bucket using live cardinality stats** (RUST_LOG=planner=debug to trace decisions).
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

## Auth & security
- Default admin (only if nothing configured): user `Pieskieo` / password `pieskieo`.
- Production: set users via `PIESKIEO_USERS='[{"user":"alice","pass":"S3cure!Pwd","role":"admin"}]'`
  or `PIESKIEO_AUTH_USER` / `PIESKIEO_AUTH_PASSWORD`.
- Passwords are Argon2id hashed; creation enforces upper+lower+digit+symbol and length ≥ 8.
- Lockout: 5 failed attempts within 15 minutes triggers a 5 minute lock (tunable via `PIESKIEO_AUTH_*` envs).
- Basic auth for per-user, Bearer token via `PIESKIEO_TOKEN` for admin automation.
- Enable TLS with `PIESKIEO_TLS_CERT` / `PIESKIEO_TLS_KEY` (PEM).
- Per-IP rate limit middleware (default 300 requests / 60s); tune via `PIESKIEO_RATE_MAX` and `PIESKIEO_RATE_WINDOW_SECS`.
- Rate-limit responses return `429` with `Retry-After` seconds.
- Audit log written to `<data>/logs/audit.log` (rotates daily/10MB, env `PIESKIEO_AUDIT_MAX_MB`) with timestamp, ip, method, path, status, role, latency.
- Basic replication hooks: `GET /v1/replica/wal` (admin) returns base64 WAL records; `POST /v1/replica/apply` accepts `{records:[..]}` to apply to followers.
- Incremental replication: `/v1/replica/wal?since=<offset>` returns per-shard slices and `end_offset`; pull/apply in a loop to stay in sync.
- Resharding (admin): `POST /v1/admin/reshard` with `{ "shards": N }` rebuilds shard set from WAL and atomically swaps the pool.

## CLI quickstart
- Connect: `pieskieo connect -H db.example.com -p 8443 -U alice -W` (prompts password; retries on failure)
- Server starter: `pieskieo connect --serve --data-dir ./data --listen 0.0.0.0:8000`
- REPL: raw PQL; multi-line supported (enter continues, semicolon sends). `quit` to exit.

## Config essentials (env)
- `PIESKIEO_DATA` data dir (defaults: `$XDG_DATA_HOME/pieskieo` or `~/.local/share/pieskieo` on Linux/macOS, `%APPDATA%/Pieskieo` on Windows)
- `PIESKIEO_LISTEN` listen addr (default `0.0.0.0:8000`)
- `PIESKIEO_SHARD_TOTAL` shard count (default 1)
- `PIESKIEO_EF_SEARCH` / `PIESKIEO_EF_CONSTRUCTION` HNSW knobs
- `PIESKIEO_BODY_LIMIT_MB` request body limit (default 10)
- `PIESKIEO_TLS_CERT`, `PIESKIEO_TLS_KEY` enable TLS (requires `--features tls`)
- `PIESKIEO_RATE_MAX`, `PIESKIEO_RATE_WINDOW_SECS` per-IP throttling
- `PIESKIEO_AUDIT_MAX_MB` audit log rotation size (daily files)
- Logging: `PIESKIEO_LOG_MODE=stdout|file|both` (default stdout); `PIESKIEO_LOG_DIR` overrides log path (else `<data>/logs/pieskieo.log`)
- `PIESKIEO_AUTH_*` lockout/complexity (see Auth & security)
- Replication uses admin auth; optionally poll with `since=end_offset` to tail WAL.
- Reshard uses admin auth and rebuilds shards from WAL; set `PIESKIEO_SHARD_TOTAL` for fresh starts, or use the admin endpoint for live changes. Status endpoint now reports verification and per-shard counts before/after.

## Benchmark tools
- Core bench: `cargo run -p pieskieo-core --bin bench --release -- <n> <dim> [ef_c] [ef_s]`
- HTTP load: `cargo run -p pieskieo-server --bin load --release -- <url> <dim> <n_vec> <searches>`

## License
GPL-2.0-only (see LICENSE).
