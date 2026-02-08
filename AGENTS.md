# AGENTS.md - Pieskieo Development Guide

This guide is for AI coding agents working on the Pieskieo codebase.

## Mission Statement

**Pieskieo is the LAST database anyone will ever need to install.**

We are building a **single, unified, production-grade database** that completely replaces:
- **PostgreSQL** (relational + SQL)
- **MongoDB** (documents + aggregations)
- **Weaviate** (vector search + hybrid)
- **LanceDB** (columnar analytics)
- **Kùzu/Neo4j** (graph + Cypher)

With **ZERO network hops**, **ONE query language**, and **FULL feature parity** with all five databases.

## Project Structure

```
Pieskieo/
├── crates/
│   ├── pieskieo-core/     # Core storage engine (WAL, HNSW, vector, graph)
│   ├── pieskieo-server/   # Axum HTTP API server
│   └── pieskieo-cli/      # Network REPL client
├── sdk/pieskieo/          # Python SDK
├── plans/                 # Feature planning docs (157 features mapped)
└── knowledge/             # Project documentation (USER_VISION.md, AGENT.md)
```

## CRITICAL RULES - READ FIRST

### 1. NO COMPROMISES EVER

❌ **FORBIDDEN PHRASES AND APPROACHES:**
- "Initial version" / "Later version" / "For now" / "Initially"
- "Known limitations (can be addressed later)"
- "We'll optimize this later"
- "Simple algorithm first, then improve"
- "Single-node first, distributed later"
- "MVP approach" / "Can be added in follow-up"
- `TODO:` comments in production code
- Placeholder implementations
- Partial feature implementations

✅ **REQUIRED MINDSET:**
- Production-ready from commit 1
- Best-in-class algorithms from day 1
- All optimizations included upfront
- Distributed by default
- Zero technical debt
- Complete implementations only

### 2. COMPLETE FEATURE PARITY MANDATE

We are building **100% feature parity** with ALL of:
- **PostgreSQL**: ALL SQL features, indexes, transactions, partitioning, full-text, JSON
- **MongoDB**: ALL aggregation stages, update operators, change streams, indexes
- **Weaviate**: ALL hybrid search, multi-vector, quantization, reranking
- **LanceDB**: ALL columnar features, time-travel, Arrow, predicate pushdown
- **Kùzu/Neo4j**: ALL Cypher features, graph algorithms, WCOJ, CSR storage

**Not "most commonly used features" - EVERYTHING.**

### 3. PLANNING BEFORE IMPLEMENTATION

All 157 features must be planned in `plans/` directory:
- Each plan: 4000-6000 tokens of detail
- Full Rust implementation specs
- All optimizations specified
- All edge cases handled
- Complete test scenarios
- No "we'll figure it out later"

Before writing ANY code:
1. Check if feature plan exists in `plans/`
2. If not, CREATE the plan first
3. Get plan reviewed
4. Then implement exactly as planned

### 4. UNIFIED QUERY LANGUAGE

Users must write queries like:
```sql
QUERY memories
  SIMILAR TO embed("search term") TOP 20
  TRAVERSE edges WHERE type = "relates_to" DEPTH 1..3
  WHERE metadata.importance > 0.7
  JOIN users ON memories.user_id = users.id
  GROUP BY users.name
  ORDER BY COUNT() DESC
```

**ONE language** that mixes vector + graph + document + relational in a SINGLE query.

## Build, Test, and Lint Commands

### Build Commands
```bash
# Standard debug build
cargo build

# Release build (preferred for testing performance)
cargo build --release

# Build with TLS support
cargo build --release --features tls -p pieskieo-server

# Build specific package
cargo build -p pieskieo-core
cargo build -p pieskieo-server
cargo build -p pieskieo-cli

# Clean build artifacts
cargo clean
```

### Test Commands
```bash
# Run all tests in workspace
cargo test

# Run all tests with output
cargo test -- --nocapture

# Run tests for specific package
cargo test -p pieskieo-core
cargo test -p pieskieo-server
cargo test -p pieskieo-cli

# Run a single test by name
cargo test -p pieskieo-core round_trip_doc_and_vector

# Run tests matching pattern
cargo test -p pieskieo-core graph

# Run tests in release mode (faster)
cargo test --release
```

### Lint and Format Commands
```bash
# Format all code (required before commits)
cargo fmt

# Check formatting without modifying
cargo fmt -- --check

# Run clippy linter
cargo clippy

# Run clippy on all targets and features
cargo clippy --all-targets --all-features

# Auto-fix clippy warnings where possible
cargo fix
cargo clippy --fix
```

### Run Commands
```bash
# Run server (development)
cargo run -p pieskieo-server

# Run server (release mode)
PIESKIEO_DATA=./data cargo run -p pieskieo-server --release

# Run CLI
cargo run -p pieskieo-cli -- --repl

# Run benchmarks
cargo run -p pieskieo-core --bin bench --release -- <n> <dim> [ef_c] [ef_s]
cargo run -p pieskieo-server --bin load --release -- <url> <dim> <n_vec> <searches>
```

## Code Style Guidelines

### Language and Edition
- **Rust Edition**: 2021
- **Minimum Rust Version**: 1.92.0 (2025)
- Follow standard Rust conventions and idioms

### Import Organization
Organize imports in this order (separated by blank lines):
```rust
// 1. Crate-local imports
use crate::error::{PieskieoError, Result};
use crate::vector::VectorIndex;

// 2. External crate imports (alphabetical)
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// 3. Standard library imports (alphabetical)
use std::collections::HashMap;
use std::sync::Arc;
```

### Formatting
- Use `cargo fmt` (rustfmt) with default settings
- No custom `.rustfmt.toml` - stick to Rust defaults
- 4-space indentation (Rust standard)
- Max line length: 100 characters (Rust default)

### Type Conventions
```rust
// Result type alias pattern
pub type Result<T> = std::result::Result<T, PieskieoError>;

// Error handling with thiserror
#[derive(Debug, Error)]
pub enum PieskieoError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("validation error: {0}")]
    Validation(String),
}

// Serialization derives
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MyType {
    // fields...
}
```

### Naming Conventions
- **Functions/variables**: `snake_case`
- **Types/structs/enums**: `PascalCase`
- **Constants**: `SCREAMING_SNAKE_CASE`
- **Modules**: `snake_case`
- **Database types**: Suffix with `Db` (e.g., `PieskieoDb`)

### Concurrency Patterns
```rust
// Prefer parking_lot over std::sync for performance
use parking_lot::{RwLock, Mutex};
use std::sync::Arc;

// Shared state pattern
let state = Arc::new(RwLock::new(State::default()));

// Atomics for counters
use std::sync::atomic::{AtomicUsize, AtomicBool, AtomicU64, Ordering};
let counter = AtomicUsize::new(0);
counter.fetch_add(1, Ordering::Relaxed);
```

### Async/Await
```rust
// Use tokio for async runtime
use tokio;

// Async functions
async fn process_data() -> Result<()> {
    // async operations
}

// Tokio tests
#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_something() -> Result<()> {
        // test code
    }
}
```

### Error Handling
- Always use `Result<T>` for operations that can fail
- Use `?` operator for error propagation
- Add context to errors with `.map_err()` when needed
- Use `thiserror` for custom error types
- Avoid `.unwrap()` except in tests or when panic is intentional

### Module Organization
- Public API exported from `lib.rs`
- Use `pub use` to re-export key types
- Binaries go in `src/bin/` directory
- Tests live alongside code in `#[cfg(test)]` modules

### Environment Variables
- All config via environment variables
- Prefix: `PIESKIEO_*`
- Provide sensible defaults with `.unwrap_or()` or `.unwrap_or_else()`
- Document all env vars in README.md

## Testing Standards
- Write tests in `#[cfg(test)]` modules in the same file
- Use `#[tokio::test]` for async tests
- Use `tempfile::tempdir()` for temporary directories in tests
- Test file location: Same file as implementation (Rust convention)
- Ensure all public APIs have test coverage

## Performance Considerations
- Use `parking_lot` locks instead of `std::sync`
- Prefer `Arc<RwLock<T>>` for shared state
- Use `rayon` for parallel iteration when appropriate
- Build in `--release` mode for benchmarks
- Profile before optimizing

## Dependencies
- Async runtime: `tokio` (v1, "full" features)
- HTTP framework: `axum` (v0.7)
- Serialization: `serde`, `serde_json`, `bincode`
- SQL parsing: `sqlparser` (v0.45)
- Vector search: `hnsw_rs` (v0.3.3)
- Sync primitives: `parking_lot` (v0.12)
- Error handling: `thiserror`, `anyhow`
- Logging: `tracing`, `tracing-subscriber`

## Common Development Tasks

### Adding a New Feature (MANDATORY PROCESS)

**CRITICAL**: Never implement features without a detailed plan first!

1. **Check if plan exists** in `plans/` directory
   - Look in appropriate category: `postgresql/`, `mongodb/`, `weaviate/`, `lancedb/`, `kuzu/`, `core-features/`
   - See `plans/MASTER_INDEX.md` for complete feature list

2. **If plan doesn't exist, CREATE IT FIRST**
   - Plan must be 4000-6000 tokens with full implementation details
   - Include ALL optimizations, edge cases, distributed scenarios
   - NO placeholders, NO "we'll add this later"
   - Follow examples in `plans/postgresql/` and `knowledge/AGENT.md`
   - Get plan reviewed before writing ANY code

3. **Implement exactly as planned**
   - Follow plan's data structures, algorithms, and approaches
   - Include all optimizations from day 1 (SIMD, lock-free, etc.)
   - Implement distributed features, not single-node first
   - No shortcuts or "initial versions"

4. **Add comprehensive tests**
   - Unit tests (90%+ coverage)
   - Integration tests (cross-component)
   - Stress tests (high concurrency)
   - Edge case tests (all failure modes)
   - Benchmark tests (vs competitors)

5. **Update public API and documentation**
   - Export types in `lib.rs` if needed
   - Update README.md with new capabilities
   - Add examples showing the feature in action

6. **Verify quality standards**
   ```bash
   cargo test            # All tests must pass
   cargo fmt             # Code must be formatted
   cargo clippy          # Zero warnings allowed
   cargo bench           # Performance targets met
   ```

### Debugging
- Use `RUST_LOG=debug` for detailed logging
- Use `RUST_LOG=planner=debug` to trace query planner decisions
- Enable backtraces: `RUST_BACKTRACE=1`

### Before Committing
```bash
cargo fmt              # Format code
cargo clippy          # Lint code
cargo test            # Run all tests
```

## License
GPL-2.0-only - Ensure any contributions comply with this license.
