# ndxr Development Guide

## Project Overview

ndxr is a local-first context engine for AI coding agents. A single Rust binary that indexes codebases via tree-sitter, builds a dependency graph, and serves relevant code context through an MCP server over stdio. No cloud, no API keys, no accounts.

**Repository:** `git@github.com:ndxr/ndxr.git`

## Commands

**Always use Make targets instead of raw cargo commands.**

```bash
make build                        # Build debug
make build-release                # Build release (~29MB binary)
make test                         # Run all 229 tests
make lint                         # Lint (must pass: pedantic deny, all deny)
make fmt                          # Format (run before every commit)
make fmt-check                    # Verify formatting (CI)
make ci                           # Full CI pipeline (fmt-check + lint + test)
make install                      # Install binary to ~/.cargo/bin
make clean                        # Remove build artifacts
make help                         # Show all available targets
cargo run -- index                # Index current workspace
cargo run -- status               # Show index stats
cargo run -- search "query"       # Search indexed symbols
cargo run -- mcp                  # Start MCP server on stdio
cargo run -- setup --scope project # Configure Claude Code integration
```

## Architecture

```
CLI (clap)  /  MCP Server (rmcp, stdio)
                    |
              CoreEngine (Arc)
              - Mutex<Connection>
              - Mutex<Option<SymbolGraph>>
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
| `src/main.rs` | CLI dispatch (clap): index, reindex, mcp, setup, status, search, skeleton, help |
| `src/indexer/mod.rs` | `index()` / `reindex()` / `index_paths()` — full indexing pipeline |
| `src/mcp/server.rs` | `start_mcp_server()` — MCP over stdio with 8 tools, `CoreEngine` shared state |
| `src/graph/search.rs` | `hybrid_search()` — FTS5 BM25 + TF-IDF + centrality + intent scoring |
| `src/capsule/builder.rs` | `build_capsule()` — token-budgeted context packing with BFS expansion |
| `src/languages/mod.rs` | `get_language_config()` — 14 grammars, tree-sitter query registry |
| `src/storage/db.rs` | `open_or_create()` — SQLite schema, WAL, pragmas, migrations |
| `src/watcher.rs` | `FileWatcher::start()` — debounced targeted re-index via `index_paths` |

### Dependency Flow (No Cycles)

```
main.rs -> all modules
mcp/server -> capsule, config, graph, indexer, memory, skeleton, storage, watcher
capsule -> config, graph, skeleton
indexer -> graph, memory, storage, languages
graph -> indexer/tokenizer (for FTS query building)
memory -> indexer/tokenizer, storage
watcher -> indexer, graph, storage, languages, mcp/server (CoreEngine)
```

## Coding Rules

### Rust Standards

- **Edition 2024** (latest stable)
- **clippy pedantic deny, all deny, nursery warn** — zero warnings tolerated
- **No `unwrap()` in production code** — use `?`, `.context()`, `.with_context()`, `bail!()`
- **No `todo!()`, `unimplemented!()`, `unreachable!()`** in production code
- **No unsafe code**
- **`#[must_use]`** on all pure functions and constructors
- **`cargo fmt`** before every commit

### Error Handling

```rust
// CORRECT: propagate with context
let conn = Connection::open(path)
    .with_context(|| format!("cannot open database: {}", path.display()))?;

// CORRECT: bail for explicit errors
bail!("reached filesystem root without finding .git/");

// WRONG: never unwrap in production
let conn = Connection::open(path).unwrap(); // NO
```

- Use `anyhow::Result<T>` for all application-level functions
- Use `.context("static message")` for simple context
- Use `.with_context(|| format!(...))` when formatting is needed
- Handle `rusqlite::Error::QueryReturnedNoRows` explicitly, don't treat as fatal

### Documentation

Every public item must have a doc comment:

```rust
/// Walks from `start` upward until a `.git/` directory is found.
///
/// Returns the canonicalized directory containing `.git/`.
///
/// # Errors
///
/// Returns an error if no `.git/` is found before the filesystem root.
pub fn find_workspace_root(start: &Path) -> Result<PathBuf> {
```

- `///` on all public functions, types, fields, constants
- `//!` module-level docs on every `mod.rs`
- `# Errors` section on all fallible public functions
- `# Panics` section where intentional panics exist
- Do NOT add doc comments to private helper functions unless complex

### Naming Conventions

- Functions: `snake_case` — `parse_file`, `build_graph`, `compute_and_store`
- Types: `PascalCase` — `SearchResult`, `SymbolGraph`, `CoreEngine`
- Constants: `SCREAMING_CASE` — `DEFAULT_MAX_TOKENS`, `FTS_CANDIDATE_LIMIT`
- Modules: `snake_case` — `edge_resolver`, `go_lang`, `rust_lang`, `c_lang`
- Language modules use `_lang` suffix to avoid Rust keyword conflicts (`go_lang`, `rust_lang`, `c_lang`)

### `#[allow]` Annotations

Only permitted for genuine clippy pedantic false positives. Always include an inline justification:

```rust
#[allow(clippy::cast_precision_loss)] // usize->f64 loss negligible for token counts
#[allow(clippy::cast_possible_truncation)] // tree-sitter ASTs cannot have >4B children
```

Approved patterns:
- `cast_precision_loss` — token estimation, scoring normalization
- `cast_possible_truncation` — tree-sitter node counts, file sizes
- `cast_sign_loss` — BM25 score abs(), time deltas
- `cast_possible_wrap` — small usize to i64 for SQL params
- `needless_pass_by_value` — closures requiring ownership for async move

### Visibility

- Modules in `lib.rs` are `pub` (used by both binary and integration tests)
- Struct fields are `pub` when needed by other modules, otherwise private
- Internal helper functions are `fn` (not `pub`)
- Constants are module-private unless shared (`pub(crate)` for shared constants)

## Performance Rules

### Database

- **WAL mode** for concurrent reads during writes
- **64MB cache + 256MB mmap** for fast queries
- **Single transaction** for all write operations in index pipeline
- **Parameterized queries** everywhere (never string interpolation)
- **Indexes** on all columns used in WHERE/JOIN clauses
- **FTS5** with porter stemming for full-text search
- **`PRAGMA synchronous = NORMAL`** — safe with WAL, faster than FULL

### Parallelism

- **rayon** `par_iter()` for file hashing and parsing — create Parser per thread (not shareable)
- **tree-sitter thread safety**: `Parser` and `QueryCursor` are per-thread (cheap to create). `Language`/`LanguageFn` are `Send + Sync`. `Node` is NOT Send — extract all data within the parsing thread.

### Caching

- **PageRank** computed once after indexing, cached in `symbols.centrality` column
- **TF-IDF** term frequencies pre-computed at index time in `term_frequencies` table
- **SymbolGraph** held in memory (`Mutex<Option<SymbolGraph>>`) for the MCP session lifetime
- **IDF values** batch-preloaded per search query (not per-candidate)
- **blake3 hashes** compared for incremental indexing — skip unchanged files

### File Watcher

- **Targeted re-index** via `index_paths()` — only processes changed files, skips full workspace walk
- **Debounce** at 500ms — batches rapid edits into single re-index pass
- **Graph rebuild** after re-index on a separate connection, stored in shared `CoreEngine`

## Security Rules

### SQL Injection Prevention

```rust
// CORRECT: parameterized query
conn.execute("DELETE FROM files WHERE path = ?1", [path])?;
conn.query_row("SELECT id FROM symbols WHERE fqn = ?1", params![fqn], ...)?;

// WRONG: string interpolation
conn.execute(&format!("DELETE FROM files WHERE path = '{path}'"), [])?; // NEVER
```

Every SQL query in the codebase MUST use `?1`, `?2` placeholders with `rusqlite::params![]`.

### FTS5 Query Sanitization

User search queries must be sanitized before FTS5 MATCH:

```rust
// Use the shared sanitizer in indexer/tokenizer.rs
let fts_query = crate::indexer::tokenizer::build_fts_query(raw_query);
// This strips all FTS5 special chars: " ' ( ) { } [ ] * : ^ - + ~ | & . , ; ! ? @ # $ % \ /
// Wraps each term in double quotes, joins with OR
```

### Path Traversal Prevention

```rust
// CORRECT: validate resolved path stays under workspace root
let canonical = abs_path.canonicalize()?;
let canonical_root = workspace_root.canonicalize()?;
anyhow::ensure!(canonical.starts_with(&canonical_root), "path traversal detected");
```

- File walker: `follow_links(false)` — no symlink following
- Capsule builder: `read_file_content()` validates path stays under workspace root
- Watcher: `should_process_path()` checks `path.starts_with(workspace_root)`

### Resource Limits

- `MAX_TOKEN_BUDGET = 50_000` — hard cap on user-provided `max_tokens`
- `DEFAULT_MAX_FILE_SIZE = 1_048_576` (1 MiB) — files larger are skipped
- `FTS_CANDIDATE_LIMIT = 100` — max candidates before ranking
- All MCP tool responses clamped: `.min(MAX_TOKEN_BUDGET)`

### MCP Server Safety

- **stdout is exclusively MCP JSON-RPC transport** — NEVER `println!()` in production
- All logging goes to **stderr** via `tracing` with `with_writer(std::io::stderr)`
- Errors wrapped as `rmcp::ErrorData::internal_error()` — no raw stack traces to clients

## Code Smell Prevention

### DRY — No Duplication

- `unix_now()` lives in `util.rs` — single source of truth
- `parse_intent()` lives in `graph::intent` — used by both CLI and MCP server
- `build_fts_query()` and `is_fts_special()` live in `indexer::tokenizer` — used by search, relaxation, memory
- Shared test helpers extracted when patterns repeat across >2 test files

### No Dead Code

- Every public module is used in production code paths (not just tests)
- Every public function is called from production code
- No `_` prefixed fields on structs (they indicate unused state)

### No Deferred Work

- Zero `TODO`, `FIXME`, `HACK`, `XXX`, `future`, `later`, `placeholder`, `stub` markers
- Everything is implemented completely — no feature flags, no conditional compilation gates

### Function Size

- Functions should not exceed ~50 lines of logic
- Extract helpers for distinct steps in a pipeline
- Use `write_index_results()`, `compute_tfidf()`, `snapshot_symbol_hashes()` as examples

## Test Organization

### Unit Tests (inline)

Place `#[cfg(test)] mod tests` at the bottom of source files for testing internal/private logic:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_estimator_default() { ... }
}
```

### Integration Tests (`tests/` directory)

Test the public API from the outside (as a separate crate using `ndxr::*`):

```rust
// tests/test_indexer.rs
use tempfile::TempDir;
use std::fs;

#[test]
fn full_index_creates_symbols_and_edges() {
    let tmp = TempDir::new().unwrap();
    // ... create project, index, verify DB
}
```

### Test Quality Standards

- Test **behavior**, not implementation details
- Use **strong assertions** — verify specific values, not just `!is_empty()` or `> 0`
- Cover **edge cases**: empty input, zero budget, missing files, special characters
- Cover **error paths**: nonexistent files, corrupt input, invalid parameters
- Cover **cascading effects**: DELETE CASCADE + FTS5 trigger consistency

## Key Constants Reference

| Constant | Value | Location |
|---|---|---|
| `DEFAULT_TOOL_TOKEN_BUDGET` | 8,000 | mcp/server.rs |
| `MAX_TOKEN_BUDGET` | 50,000 | mcp/server.rs |
| `DEFAULT_MAX_FILE_SIZE` | 1,048,576 (1 MiB) | indexer/walker.rs |
| `DEFAULT_DEBOUNCE_MS` | 500 | config.rs |
| `DEFAULT_COMPRESSION_AGE_SECS` | 86,400 (24h) | config.rs |
| `DEFAULT_RECENCY_HALF_LIFE_DAYS` | 7.0 | config.rs |
| `FTS_CANDIDATE_LIMIT` (search) | 100 | graph/search.rs |
| `FTS_CANDIDATE_LIMIT` (memory) | 50 | memory/search.rs |
| `DAMPING_FACTOR` | 0.85 | graph/centrality.rs |
| `ITERATIONS` (PageRank) | 100 | graph/centrality.rs |
| `BFS_MAX_DEPTH` | 2 | capsule/builder.rs |
| `MEMORY_FRACTION` | 0.10 | capsule/builder.rs |
| `PIVOT_FRACTION` | 0.85 | capsule/builder.rs |
| `MAX_MEMORY_TOKENS` | 500 | capsule/builder.rs |
| `STALENESS_PENALTY` | 0.30 | memory/search.rs |
| BM25 column weights | name=10, fqn=5, doc=1, sig=3 | graph/search.rs |

## MCP Tools

| Tool | Auto-Capture | Description |
|---|---|---|
| `run_pipeline` | Yes | Full pipeline: intent -> search -> capsule -> memory -> impact |
| `get_context_capsule` | Yes | Search -> capsule -> memory (no impact hints) |
| `get_skeleton` | Yes | Signature-only file rendering |
| `get_impact_graph` | Yes | BFS callers/callees with blast radius |
| `get_session_context` | Yes | Recent session history |
| `search_memory` | **No** | Cross-session observation search |
| `save_observation` | **No** | Manual observation persistence |
| `index_status` | **No** | Health check and statistics |

## Gotchas

- **stdout is MCP transport** — any `println!()` in lib code corrupts the JSON-RPC stream. Use `tracing::info!()` (goes to stderr).
- **tree-sitter `Node` is not Send** — all data must be extracted within the rayon thread that parsed the AST. Don't try to return `Node` references from `par_iter` closures.
- **`schemars` is v1, not v0.8** — rmcp 1.2 requires schemars 1.x for `#[derive(JsonSchema)]` on tool parameter structs.
- **FTS5 MATCH panics on raw user input** — always use `indexer::tokenizer::build_fts_query()` to sanitize before passing to MATCH.
- **`rusqlite::Connection` is not `Clone`** — shared via `tokio::sync::Mutex<Connection>` in `CoreEngine`. Lock, do sync DB work, drop lock. Never hold across `.await`.
- **`index_paths()` skips graph rebuild** — the watcher rebuilds the graph separately on its own connection and stores it in `CoreEngine`. Don't add graph rebuild to `index_paths`.
- **Language modules use `_lang` suffix** — `go_lang.rs`, `rust_lang.rs`, `c_lang.rs` because `go`, `rust`, `c` are Rust keywords or reserved.
- **`ParseResult.path` is relative** — relative to workspace_root, not absolute. The indexer strips the prefix before storing.
- **Edition 2024 enables let-chains** — `if let Ok(x) = foo() && let Ok(y) = bar(x)` is used throughout. Requires `edition = "2024"` in Cargo.toml.

## Commit Convention

```
feat: add hybrid search pipeline
fix: resolve clippy pedantic warnings
test: add CASCADE delete verification
refactor: extract shared FTS query builder
docs: add comprehensive README
ci: add GitHub Actions workflow
chore: final polish
```

## Changelog and Release Notes

CHANGELOG.md and GitHub release notes are **user-facing**. Write them for end users, not developers:

- Be precise but compact — short bullet points, not paragraphs
- Describe what the user can **do**, not how it works internally
- Avoid internal implementation details (FTS5, BM25, PageRank, WAL mode, rayon, petgraph, etc.)
- Only mention technical details when they directly affect the user (e.g., "14 languages supported")
- Follow [Keep a Changelog](https://keepachangelog.com/) format with `## [version] - date` headers
