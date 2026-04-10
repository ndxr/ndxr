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
cargo run -- model download  # Download embedding model for semantic search
cargo run -- model status    # Show model and embedding coverage
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
| `src/main.rs` | CLI dispatch: index, reindex, mcp, setup, status, search, skeleton, activity, upgrade |
| `src/indexer/mod.rs` | `index()` / `reindex()` / `index_paths()` — full indexing pipeline |
| `src/mcp/server.rs` | MCP server with 10 tools, `CoreEngine`, `run_capsule_pipeline()`, `commit_tool_record()` |
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
| `src/embeddings/model.rs` | `ModelHandle::load()`, `embed_text()`, `embed_batch()` — ONNX model inference |
| `src/embeddings/download.rs` | `download_model()`, `verify_model()` — model download with SHA-256 |
| `src/embeddings/storage.rs` | `store_embeddings()`, `load_embeddings()` — SQLite BLOB storage |
| `src/embeddings/similarity.rs` | `cosine_similarity()`, `batch_cosine_similarity()` — vector math |

### Dependency Flow (No Cycles)

**Rule:** shared helpers must live in the module both callers already depend on — never introduce a reverse dependency arrow.

```
main.rs -> all modules
mcp/server -> capsule, config, embeddings, graph, indexer, memory, skeleton, storage, watcher
capsule -> config, graph, skeleton, storage
embeddings -> storage (for DB operations only)
indexer -> embeddings, graph, memory, storage, languages
graph -> indexer/tokenizer
memory -> indexer/tokenizer, storage
watcher -> embeddings, indexer, graph, memory, storage, languages, mcp/server (CoreEngine)
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
- `resolve_budget()` / `trim_capsule_to_budget()` / `serialize_capsule()` → `mcp/server.rs`
- `rebuild_graph_from_db()` → `graph/builder.rs` (used by watcher + MCP reindex tool)
- `build_ignore_matcher()` → `watcher.rs` | `DEFAULT_IGNORED_DIRS` → `watcher.rs`
- `symbol_to_embedding_text()` → `embeddings/model.rs`
- `cosine_similarity()` → `embeddings/similarity.rs`
- `trigram_similarity()` → `indexer/tokenizer.rs`
- `MemorySearchQuery` → `memory/search.rs` (wraps `search_memories` parameters; adding fields here is the single place that controls the API)
- `DEFAULT_MODEL` → `embeddings/download.rs` (single source of truth for model URL/SHA/filename; `onnx_filename` is also consumed by `ModelHandle::load`, and the 86 MiB size is mirrored in `README.md`, `docs/ARCHITECTURE.md`, `CLAUDE.md` "Semantic Search" section, and `src/embeddings/mod.rs` — all five must change in lockstep)

### No Dead Code / No Deferred Work

- Every public item used in production. No `_`-prefixed fields, no `TODO`/`FIXME`/`HACK`
- Functions ≤ ~50 lines of logic — extract helpers for pipeline steps

## Test Organization

- **Unit tests**: `#[cfg(test)] mod tests` at bottom of source files for internal/private logic
- **Integration tests**: `tests/` directory, test public API as separate crate
- **Test quality**: test behavior not implementation, strong assertions (specific values not `> 0`), cover edge cases + error paths
- **Shared helpers**: `tests/helpers/mod.rs` — `setup_indexed_workspace`, `create_search_project`, `create_capsule_project`, `index_and_build`
- **Network-gated tests**: tests that hit live endpoints (HF download, upstream URL probes) are `#[ignore = "hits network — …"]` so `make ci` stays offline. Run manually with `cargo test --test <file> -- --ignored`. Canonical examples: `model_download_and_embed_end_to_end` and `model_download_is_idempotent` in `tests/test_embeddings.rs`

## Key Constants

| Constant | Value | Location |
|---|---|---|
| `DEFAULT_TOOL_TOKEN_BUDGET` | 10,000 | mcp/server.rs |
| `MAX_TOKEN_BUDGET` | 50,000 | mcp/server.rs |
| `JSON_OVERHEAD_FACTOR` | 0.80 | mcp/server.rs |
| `DEFAULT_MAX_TOKENS` | 20,000 | config.rs |
| `DEFAULT_CHARS_PER_TOKEN` | 3.5 | config.rs |
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
| `DEFAULT_IGNORED_DIRS` | 6 dirs | watcher.rs |
| `EMBEDDING_DIMENSION` | 384 | embeddings/model.rs |
| `EMBEDDING_BATCH_SIZE` | 32 | embeddings/model.rs |
| `MAX_EMBEDDING_INPUT_CHARS` | 512 | embeddings/model.rs |
| `DOCSTRING_TRUNCATION` | 200 | embeddings/model.rs |

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
| `reindex` | **No** | Full re-index + graph rebuild |

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
- **Clippy `single_match`** — `match opt { Some(x) => ..., None => ... }` denied under `clippy::all`. Use `if let` — but see `option_if_let_else` below when both arms produce `Option`
- **Clippy `option_if_let_else`** — `if let Some(x) = opt { Some(x) } else { side_effect; None }` denied under nursery. `match` is also denied (`single_match`). Use `opt.or_else(|| { side_effect; None })` or `Option::map_or_else`
- **Rustdoc private item links** — `[`PrivateConst`]` in a public function's doc creates an intra-doc link that warns because external consumers can't follow it. Use backtick-only notation (`` `PrivateConst` ``) for private items
- **Duration vs timestamp in SQL** — never pass a duration constant directly as a `WHERE timestamp > ?` parameter. Compute `unix_now() - duration` at the call site or inside the function
- **Warning dedup in both paths** — anti-pattern warnings are saved from both `enrich_warnings` (capsule pipeline) and `run_antipattern_detectors` (watcher). Both paths must deduplicate via `SELECT COUNT(*) ... LIKE '[rule]%'` before inserting
- **tar crate rejects malicious paths on creation** — `Builder::append_data` validates paths (rejects `..`, absolute). To test archive extraction security, build raw tar bytes manually (see `create_raw_tar_gz` in `upgrade.rs` tests)
- **Edition 2024 `set_var`/`remove_var` are unsafe** — wrap in `unsafe {}` blocks in tests. Consolidate all env-var-mutating assertions into a single test function to avoid parallel test races
- **Clippy `assigning_clones`** — `field = "literal".to_owned()` denied under pedantic. Use `"literal".clone_into(&mut field)` instead
- **`diff_files()` marks absent files as deleted** — designed for full workspace diffs. Never call with a partial file list (e.g. from `index_paths`) and use its deletion results, or it will wipe unrelated indexed files
- **Watcher ignore matcher hot-reloads** — rebuilt automatically when `.ndxrignore` or `.gitignore` changes. Default exclusions (`target/`, `build/`, `bin/`, `node_modules/`, `.git/`, `dist/`) always apply
- **MCP server graph is in-memory only** — built at startup from the DB. External `ndxr index`/`ndxr reindex` updates the DB but not the running server's graph. File watcher rebuilds it; otherwise restart required
- **Observation ordering needs id tiebreaker** — `ORDER BY created_at` alone is non-deterministic for rows inserted within the same second. Always add `, id ASC/DESC` as secondary sort
- **tract batch embedding is CPU-bound** — never call `embed_batch()` while holding the `Mutex<Connection>` lock. Release the lock, embed, re-acquire, store
- **`symbol_embeddings` table always exists** — created unconditionally in V3 migration regardless of model presence
- **Watcher uses `engine.embeddings_model`** — never call `ModelHandle::load()` from the watcher; use the model loaded at MCP startup
- **`tokenizers` crate requires `fancy-regex`** — `default-features = false` strips the regex backend. Always include `features = ["fancy-regex"]`
- **`CLAUDE_MD_SECTION` is MCP-tool-only** — the auto-generated section in `main.rs` documents MCP tools only. Non-MCP features (semantic search, n-gram) belong in the manually-maintained part of CLAUDE.md
- **`ScoreBreakdown` literals in 5+ locations** — adding fields to `ScoreBreakdown` requires updating: `scoring.rs`, `relaxation.rs` (fts5_fallback), `builder.rs` (test), `server.rs` (tests), `test_smoke_capsule.rs`
- **Clippy `option_if_let_else` also catches `map_or(true, ...)`** — `opt.map_or(true, |v| cond)` denied under nursery. Use `opt.is_none_or(|v| cond)`
- **Clippy `too_many_lines` limit is 100** (the ≤50 rule above is the project target). When adding stage logging or small features to pipeline functions (`index_inner`, watcher `start`), extract helpers aggressively to stay under 100
- **Rustdoc backticks flow through schemars into JSON schema** — doc comments on schema structs (e.g. `GetImpactGraphParams.symbol_fqn`) must wrap identifiers like `` `run_pipeline` `` in backticks (rustdoc `missing_backticks_doc_comments`), and the backticks are preserved literally in the tool's `input_schema` description string — tests asserting on description content see the raw `` `identifier` ``
- **`storage::db::open_or_create(path)` auto-creates missing parent directories** via `create_dir_all`. Tests that want to force it to fail need a parent that is a regular file (e.g. `tmp/not_a_dir/db.sqlite` where `not_a_dir` is a file), not just a nonexistent path
- **Source-text regression tests via `include_str!("server.rs")`** — used in `mcp::server::tests::run_capsule_pipeline_does_not_leak_error_details` to enforce "never `format!(\"...{e}\")` in `ErrorData`" by asserting the forbidden substring is absent from a specific function's body
- **HuggingFace URL pinning** — `DEFAULT_MODEL.onnx_url` uses `resolve/<40-char git SHA>` rather than `resolve/main`. Pinning gives immutable downloads (upstream renames/retrains can't break us). To update: bump the commit hash, re-download once, update both SHA-256 constants
- **tract-onnx + modern transformer ONNX exports** — every int8/fp16/graph-optimized variant from `optimum-cli`/Xenova keeps `batch_size`/`sequence_length` as symbolic `Unsqueeze` dims that tract 0.21's `into_typed()` cannot unify with in-graph constants ("Impossible to unify Sym(batch_size) with Val(1)"). Only `sentence-transformers/all-MiniLM-L6-v2/onnx/model.onnx` (FP32, 86 MiB) loads cleanly — don't waste time trying smaller variants
- **BERT ONNX export has three inputs** — `ModelHandle::load` must set `with_input_fact(0/1/2)` for `input_ids`, `attention_mask`, AND `token_type_ids`. `embed_text` must pass all three tensors to `model.run(tvec![...])`. Missing the third input is a silent tract-analysis failure, not a type error
- **Bash cwd drift after "Shell cwd was reset"** — after any `Shell cwd was reset to …` notice, Bash runs in the main checkout, NOT the worktree. Re-`cd` into the worktree and verify with `pwd && git rev-parse --abbrev-ref HEAD` before editing files. Silent-failure risk: edits land on main instead of the feature branch

## CI / Makefile Parity

`make ci` and `.github/workflows/ci.yml` must run the same checks. When adding a CI step, add a matching Makefile target (and vice versa).

## Commit Convention

`feat:` / `fix:` / `test:` / `refactor:` / `docs:` / `ci:` / `chore:` / `perf:`

## Changelog

User-facing, compact bullets. Describe what users can **do**, not internals. [Keep a Changelog](https://keepachangelog.com/) format.

**Before labeling an entry as `Fixed`**, verify it's a regression from the previous tag: `git log <prev-tag>..HEAD -- <file>` and confirm the prior release behaved differently. Behavior from a feature introduced *in this same release cycle* belongs under `Added`, even if a later commit corrected an earlier draft of that feature.

## ndxr context engine

ndxr indexes this codebase and provides you with only the relevant code for each task.

**IMPORTANT: You MUST call `mcp__ndxr__run_pipeline` BEFORE reading, modifying, or reasoning about any source file.** Do not use Read, Grep, or Glob to explore the codebase — ndxr returns exactly the context you need. Only read files that ndxr includes in its response.

### Intent

Pass `intent` to `run_pipeline` to get optimized context for your task:

| Intent | When to use | What it optimizes |
|--------|------------|-------------------|
| `debug` | Fixing bugs, errors, crashes | Error paths, high-connectivity code |
| `test` | Writing or finding tests | Test files, test fixtures |
| `refactor` | Restructuring, renaming | Public APIs, blast radius, callers |
| `modify` | Adding features, extending | Balanced text + semantic match |
| `understand` | Learning how code works | Documentation, module structure, entry points |
| `explore` | General browsing (default) | Documented, central code |

Example: `mcp__ndxr__run_pipeline({ task: "fix the auth crash", intent: "debug" })`

### Tools

- `mcp__ndxr__run_pipeline` -- call this FIRST for every task (pass intent for best results)
- `mcp__ndxr__get_context_capsule` -- follow-up context when you need more (also accepts intent)
- `mcp__ndxr__get_skeleton` -- get file signatures without bodies
- `mcp__ndxr__get_impact_graph` -- check blast radius before refactoring
- `mcp__ndxr__search_memory` -- search past session insights
- `mcp__ndxr__save_observation` -- save important decisions or insights
- `mcp__ndxr__search_logic_flow` -- trace execution paths between symbols
- `mcp__ndxr__get_session_context` -- review session history
- `mcp__ndxr__index_status` -- check if index is ready
- `mcp__ndxr__reindex` -- force full re-index when index is stale (after git checkout, branch switch)

### Semantic Search (Optional)

Run `ndxr model download` to enable embedding-based semantic search. This downloads an 86 MiB `all-MiniLM-L6-v2` FP32 model (384-d embeddings) to `.ndxr/models/` that lets ndxr find semantically related code even when queries use different vocabulary than the source (e.g., "verify credentials" finds `authenticateUser`). If the model is not downloaded, ndxr works normally using lexical search.
