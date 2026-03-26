# ndxr Development Guide

## Project Overview

ndxr is a local-first context engine for AI coding agents. Single Rust binary that indexes codebases via tree-sitter, builds a dependency graph, and serves relevant code context through an MCP server over stdio. No cloud, no API keys, no accounts.

**Repository:** `git@github.com:ndxr/ndxr.git`

## Commands

**Always use Make targets instead of raw cargo commands.**

```bash
make build            # Build debug
make build-release    # Build release (~29MB binary)
make test             # Run all tests
make lint             # Lint (must pass: pedantic deny, all deny)
make fmt              # Format (run before every commit)
make ci               # Full CI pipeline (fmt-check + lint + test)
make install          # Install binary to ~/.cargo/bin
make help             # Show all available targets
cargo run -- index    # Index current workspace
cargo run -- status   # Show index stats
cargo run -- search "query"        # Search indexed symbols
cargo run -- mcp                   # Start MCP server on stdio
cargo run -- setup --scope project # Configure Claude Code integration
cargo run -- upgrade          # Check for updates and self-upgrade
```

## Architecture

```
CLI (clap)  /  MCP Server (rmcp, stdio)
                    |
              CoreEngine (Arc)
              - Mutex<Connection>
              - RwLock<Option<SymbolGraph>>
              - NdxrConfig
                    |
    +---------------+---------------+
    |               |               |
 Indexer         Graph           Memory
 tree-sitter     petgraph        SQLite
 rayon parallel  PageRank        FTS5
 blake3 hash     BM25+TF-IDF    staleness
    |               |               |
    +-------+-------+-------+-------+
            |               |
         Capsule         Skeleton
         BFS expand      signatures
         token budget    reduction
            |
      .ndxr/index.db (SQLite WAL + FTS5)
```

### Key Entry Points

| Entry Point | What It Does |
|---|---|
| `src/main.rs` | CLI dispatch: index, reindex, mcp, setup, status, search, skeleton, upgrade |
| `src/indexer/mod.rs` | `index()` / `reindex()` / `index_paths()` — full indexing pipeline |
| `src/mcp/server.rs` | MCP server with 9 tools, `CoreEngine`, `run_capsule_pipeline()`, `commit_tool_record()` |
| `src/graph/search.rs` | `hybrid_search()` — FTS5 BM25 + TF-IDF + centrality + intent scoring |
| `src/graph/intent.rs` | `get_capsule_hints()` — intent-specific BFS depth, pivot fraction, skeleton docs |
| `src/capsule/builder.rs` | `build_capsule()` — token-budgeted context packing with BFS expansion |
| `src/languages/mod.rs` | `get_language_config()` — 13 languages (14 grammars), tree-sitter queries |
| `src/storage/db.rs` | `open_or_create()`, `BATCH_PARAM_LIMIT` — SQLite schema, WAL, pragmas, migrations |
| `src/status.rs` | `collect_index_status()` — shared index statistics (CLI + MCP) |
| `src/watcher.rs` | `FileWatcher::start()` — debounced targeted re-index via `index_paths` |
| `src/memory/changes.rs` | `snapshot_symbol_state()`, `detect_symbol_diffs()` — AST structural diff tracking |
| `src/memory/antipatterns.rs` | `run_all_detectors()` — anti-pattern detection framework |
| `src/graph/pathfinding.rs` | `find_paths()` — Yen's K-shortest paths for logic flow |
| `src/upgrade.rs` | `check_for_update()`, `download_and_verify()`, `replace_binary()` — self-upgrade via GitHub releases |

### Dependency Flow (No Cycles)

```
main.rs -> all modules
mcp/server -> capsule, config, graph, indexer, memory, skeleton, storage, watcher
capsule -> config, graph, skeleton, storage
indexer -> graph, memory, storage, languages
graph -> indexer/tokenizer
memory -> indexer/tokenizer, storage
watcher -> indexer, graph, memory, storage, languages, mcp/server (CoreEngine)
upgrade -> (external: reqwest, semver, sha2, flate2, tar, zip, self_replace)
```

## Coding Rules

### Rust Standards

- **Edition 2024**, clippy pedantic deny + all deny + nursery warn — zero warnings
- **No `unwrap()`/`expect()` in production** — use `?`, `.context()`, `.with_context()`, `bail!()`
- **No `todo!()`/`unimplemented!()`/`unreachable!()`** in production
- **No unsafe code**
- **`#[must_use]`** on pure functions and constructors
- **`cargo fmt`** before every commit

### Error Handling

```rust
// CORRECT
let conn = Connection::open(path)
    .with_context(|| format!("cannot open database: {}", path.display()))?;

// WRONG
let conn = Connection::open(path).unwrap(); // NO
```

- `anyhow::Result<T>` for all application-level functions
- `.context()` for static messages, `.with_context(|| format!(...))` for dynamic
- Handle `rusqlite::Error::QueryReturnedNoRows` explicitly — don't treat as fatal
- **Never** `filter_map(Result::ok)` or `.ok()` on DB queries — discriminate `QueryReturnedNoRows` from real errors, log real errors via `tracing::warn!`
- **Never** expose raw `rusqlite::Error` to MCP clients — log full error, return generic message

### Documentation

- `///` on all public functions, types, fields, constants
- `//!` module-level docs on every `mod.rs`
- `# Errors` on all fallible public functions, `# Panics` where intentional panics exist
- Do NOT doc private helpers unless complex

### Naming

- Functions: `snake_case` — Types: `PascalCase` — Constants: `SCREAMING_CASE` — Modules: `snake_case`
- Language modules use `_lang` suffix (`go_lang`, `rust_lang`, `c_lang`) to avoid keyword conflicts

### `#[allow]` Annotations

Only for genuine clippy pedantic false positives. **Always include inline justification:**

```rust
#[allow(clippy::cast_precision_loss)] // usize->f64 loss negligible for token counts
```

Approved: `cast_precision_loss`, `cast_possible_truncation`, `cast_sign_loss`, `cast_possible_wrap`, `needless_pass_by_value`, `similar_names`

**Not approved** (use a struct instead): `too_many_arguments`

### Visibility

- Modules in `lib.rs`: `pub` (used by binary + integration tests)
- Struct fields: `pub` when needed by other modules, otherwise private
- Internal helpers: `fn` (not `pub`). Shared helpers/constants used across modules: `pub(crate)`

### File Ordering

Every `.rs` file follows this top-to-bottom order. **Never mix sections.**

1. `//!` module doc → 2. `use` imports (std, external, crate, super — alphabetical within groups) → 3. Constants → 4. Types (structs, enums, traits) → 5. Impls → 6. Private helpers → 7. `#[cfg(test)] mod tests`

## Performance Rules

### Database

- **Batch queries** — `WHERE id IN (...)` with `BATCH_PARAM_LIMIT` chunking (`storage/db.rs`). Never query per-item in a loop
- **WAL mode**, 64MB cache + 256MB mmap, `PRAGMA synchronous = NORMAL`
- **Single transaction** for all write operations in index pipeline
- **Parameterized queries** everywhere — never string interpolation for values
- **FTS5** with porter stemming for full-text search

### Parallelism & Caching

- **rayon** `par_iter()` for file hashing/parsing — `Parser`/`QueryCursor` per-thread, `Node` is NOT Send
- **SymbolGraph** in `RwLock<Option<SymbolGraph>>` — concurrent reads, exclusive writes
- **IDF/TF/Symbol metadata** batch-preloaded per search query (not per-candidate)
- **PageRank** cached in `symbols.centrality`, **blake3 hashes** for incremental indexing

### File Watcher

- **Targeted re-index** via `index_paths()` — only changed files, debounce 500ms
- **Graph rebuild** after re-index via `graph.write().await` — never silently dropped

## Security Rules

- **SQL injection**: all queries use `?1, ?2` placeholders with `params![]`
- **FTS5 injection**: always sanitize via `indexer::tokenizer::build_fts_query()` before MATCH
- **Path traversal**: `canonicalize()` + `starts_with(canonical_root)` in `read_file_content()`. Walker: `follow_links(false)`
- **Resource limits**: `MAX_TOKEN_BUDGET=50k`, `MAX_IMPACT_DEPTH=10`, `MAX_MEMORY_LIMIT=50`, `MAX_SESSION_COUNT=20`, `MAX_OBSERVATION_CONTENT=64KiB`, `FTS_CANDIDATE_LIMIT=100`
- **MCP safety**: stdout is JSON-RPC only — no `println!()`. All logging to stderr via `tracing`. No raw errors to clients
- **MCP error pattern**: `tracing::error!("context: {e}"); rmcp::ErrorData::internal_error("generic message", None)` — never `format!("...{e}")` in error data

## Code Smell Prevention

### DRY — Single Source of Truth

- `unix_now()` → `util.rs` | `parse_intent()` → `graph::intent` | `build_fts_query()` → `indexer::tokenizer` | `build_batch_placeholders()` → `storage/db.rs`
- `BATCH_PARAM_LIMIT` → `storage/db.rs` | `collect_index_status()` → `status.rs`
- `run_capsule_pipeline()` / `commit_tool_record()` → `mcp/server.rs`
- `get_capsule_hints()` → `graph/intent.rs` | `CapsuleHints` default values live here only
- Test helpers → `tests/helpers/mod.rs`
- `u32_child_count()` / `u32_named_child_count()` → `indexer/symbols.rs` (tree-sitter `u32` ↔ `usize` bridge)
- `format_relative_time()` → `mcp/server.rs` | `run_all_detectors()` → `memory/antipatterns.rs`

### No Dead Code / No Deferred Work

- Every public item used in production. No `_`-prefixed fields, no `TODO`/`FIXME`/`HACK`
- Functions ≤ ~50 lines of logic — extract helpers for pipeline steps

## Test Organization

- **Unit tests**: `#[cfg(test)] mod tests` at bottom of source files for internal/private logic
- **Integration tests**: `tests/` directory, test public API as separate crate
- **Test quality**: test behavior not implementation, strong assertions (specific values not `> 0`), cover edge cases + error paths
- **Shared helpers**: `tests/helpers/mod.rs` — `setup_indexed_workspace`, `create_search_project`, `create_capsule_project`, `index_and_build`

## Key Constants

| Constant | Value | Location |
|---|---|---|
| `DEFAULT_TOOL_TOKEN_BUDGET` | 10,000 | mcp/server.rs |
| `MAX_TOKEN_BUDGET` | 50,000 | mcp/server.rs |
| `JSON_OVERHEAD_FACTOR` | 0.80 | mcp/server.rs |
| `DEFAULT_MAX_FILE_SIZE` | 1 MiB | indexer/walker.rs |
| `BATCH_PARAM_LIMIT` | 900 | storage/db.rs |
| `FTS_CANDIDATE_LIMIT` | 100 / 50 | graph/search.rs / memory/search.rs |
| `CapsuleHints.bfs_depth` | 2–3 (intent-dependent) | graph/intent.rs |
| `MAX_IMPACT_DEPTH` | 10 | mcp/server.rs |
| `MAX_MEMORY_LIMIT` | 50 | mcp/server.rs |
| `MAX_SESSION_COUNT` | 20 | mcp/server.rs |
| `MAX_OBSERVATION_CONTENT` | 64 KiB | mcp/server.rs |
| `DAMPING_FACTOR` / `ITERATIONS` | 0.85 / 100 | graph/centrality.rs |
| `MEMORY_FRACTION` | 0.10 | capsule/builder.rs |
| `CapsuleHints.pivot_fraction` | 0.70–0.85 (intent-dependent) | graph/intent.rs |
| BM25 weights | name=10, fqn=5, doc=1, sig=3 | graph/search.rs |
| `DEFAULT_MAX_PATHS` | 3 | graph/pathfinding.rs |
| `MAX_PATHS` | 5 | graph/pathfinding.rs |
| `DEFAULT_WINDOW_SECS` | 300 | memory/antipatterns.rs |
| `CORRELATION_WINDOW_SECS` | 120 | memory/changes.rs |

## MCP Tools

| Tool | Auto-Capture | Description |
|---|---|---|
| `run_pipeline` | Yes | Full pipeline: intent → search → capsule → memory → impact |
| `get_context_capsule` | Yes | Search → capsule → memory (no impact hints) |
| `get_skeleton` | Yes | Signature-only file rendering |
| `get_impact_graph` | Yes | BFS callers/callees with blast radius |
| `get_session_context` | Yes | Recent session history |
| `search_memory` | **No** | Cross-session observation search |
| `save_observation` | **No** | Manual observation persistence |
| `search_logic_flow` | Yes | Trace execution paths between symbols |
| `index_status` | **No** | Health check and statistics |

## Gotchas

- **stdout is MCP transport** — `println!()` in lib code corrupts JSON-RPC. Use `tracing::info!()`
- **tree-sitter `Node` is not Send** — extract all data within the rayon thread that parsed the AST
- **`schemars` is v1, not v0.8** — rmcp 1.2 requires schemars 1.x for `#[derive(JsonSchema)]`
- **FTS5 MATCH panics on raw input** — always sanitize via `build_fts_query()`
- **`rusqlite::Connection` not `Clone`** — shared via `Mutex<Connection>`. Lock, do sync work, drop. Never hold across `.await`
- **`index_paths()` skips graph rebuild** — watcher rebuilds separately via `graph.write().await`
- **`ParseResult.path` is relative** — relative to workspace_root, not absolute
- **Edition 2024 let-chains** — `if let Ok(x) = foo() && let Ok(y) = bar(x)` used throughout
- **tree-sitter `named_child(u32)` vs `named_child_count() -> usize`** — use `u32_named_child_count()` helper in `symbols.rs`. When adding language query patterns, verify node types against `node-types.json` in the cargo registry, don't assume names
- **Clippy `ignored_unit_patterns`** — `tokio::select!` arms must use `() = async { ... }` not `_ = async { ... }`
- **Clippy `double_must_use`** — if a struct is `#[must_use]`, functions returning it must NOT also be `#[must_use]` (denied under `clippy::all`)
- **Clippy `too_many_arguments`** — not on the approved `#[allow]` list. Wrap parameters in a struct instead (see `CapsuleRequest`, `PipelineParams`)
- **Clippy `case_sensitive_file_extension_comparisons`** — `.ends_with(".zip")` denied under pedantic. Use `Path::new(s).extension().is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))`
- **Clippy `nonminimal_bool`** — `!x.is_some_and(|v| cond)` denied. Use `x.is_none_or(|v| !cond)` instead
- **Rustdoc private item links** — `[`PrivateConst`]` in a public function's doc creates an intra-doc link that warns because external consumers can't follow it. Use backtick-only notation (`` `PrivateConst` ``) for private items
- **Duration vs timestamp in SQL** — never pass a duration constant directly as a `WHERE timestamp > ?` parameter. Compute `unix_now() - duration` at the call site or inside the function
- **Warning dedup in both paths** — anti-pattern warnings are saved from both `enrich_warnings` (capsule pipeline) and `run_antipattern_detectors` (watcher). Both paths must deduplicate via `SELECT COUNT(*) ... LIKE '[rule]%'` before inserting
- **tar crate rejects malicious paths on creation** — `Builder::append_data` validates paths (rejects `..`, absolute). To test archive extraction security, build raw tar bytes manually (see `create_raw_tar_gz` in `upgrade.rs` tests)

## CI / Makefile Parity

`make ci` and `.github/workflows/ci.yml` must run the same checks. When adding a CI step, add a matching Makefile target (and vice versa).

## Commit Convention

`feat:` / `fix:` / `test:` / `refactor:` / `docs:` / `ci:` / `chore:` / `perf:`

## Changelog

User-facing, compact bullets. Describe what users can **do**, not internals. [Keep a Changelog](https://keepachangelog.com/) format.
