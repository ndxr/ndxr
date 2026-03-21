# ndxr Architecture

How ndxr works internally. For usage, see [README.md](../README.md).

## Design Philosophy

ndxr solves a specific problem: AI coding agents waste context windows reading irrelevant files. The solution is a local index that understands code structure (not just text) and delivers token-budgeted context ranked by multiple signals.

**Core design decisions:**

- **Single binary** — no daemon, no server to manage. The MCP server runs as a child process of the AI tool.
- **SQLite as the only store** — code index, FTS5 search, memory system, and graph metadata all live in one `.ndxr/index.db` file. WAL mode enables concurrent reads during writes.
- **tree-sitter for parsing** — fast, incremental, language-agnostic AST extraction. Each language module defines tree-sitter queries for what constitutes a symbol, an import, and a call.
- **Structural ranking over text search** — BM25 finds candidates, but PageRank centrality and intent-aware weighting determine final ranking. A utility function called by 50 other functions ranks higher than one with the same name match.

## System Architecture

```
                      ndxr binary
                          |
            +-------------+-------------+
            |                           |
       CLI (clap)              MCP Server (rmcp)
       one-shot commands       long-running over stdio
            |                           |
            +-------------+-------------+
                          |
                    CoreEngine (Arc)
                    Mutex<Connection>  +  RwLock<SymbolGraph>
                          |
        +---------+-------+-------+---------+
        |         |               |         |
     Indexer    Graph          Memory    Watcher
     parse &    search &       capture   fs events
     store      rank           & recall  debounce
        |         |              |         |
        |    +----+----+         |         |
        |    |         |         |         |
        | Capsule   Skeleton     |         |
        | full src  signatures   |         |
        | + budget  only         |         |
        |                        |         |
        +--------+---------------+---------+
                 |
           .ndxr/index.db
           (SQLite WAL + FTS5)
```

**Two execution modes share the same `CoreEngine`:**

1. **CLI** — one-shot commands (`index`, `search`, `status`, `skeleton`). Opens DB, does work, exits.
2. **MCP Server** — long-running process over stdio. Holds `CoreEngine` with the graph in memory, a file watcher for live re-indexing, and session memory for cross-query continuity.

## Data Flow

### Indexing

The indexer transforms source files into a queryable representation:

```
source files → tree-sitter AST → symbols + edges → SQLite + FTS5 → PageRank
```

**Key design choices in the indexer:**

- **Single-read, single-parse** — each file is read from disk once and parsed by tree-sitter once. The content and BLAKE3 hash are passed through the pipeline, avoiding redundant I/O.
- **Incremental by default** — BLAKE3 content hashes detect which files changed. Only changed/new files are re-parsed. Deletions cascade through foreign keys.
- **Parallel parsing** — rayon `par_iter` distributes file parsing across threads. `Parser` is created per-thread (not shareable). All data is extracted from the AST within the parsing thread because tree-sitter `Node` is not `Send`.
- **Single transaction** — all DB writes (files, symbols, edges, TF-IDF) happen in one transaction for atomicity and performance.
- **Post-index graph rebuild** — after writing, the full dependency graph is rebuilt from the `edges` table into a petgraph `DiGraph`, and PageRank centrality is computed and stored back into `symbols.centrality`.

### Edge Resolution

Edges from the AST reference targets by name only. The edge resolver maps names to database IDs using a priority cascade: same-file match first, then globally exported symbols, then any symbol by name. Unresolved edges are silently skipped.

### Search

Search combines three orthogonal signals to rank symbols:

```
query → FTS5 candidates (BM25) → enrich with TF-IDF + centrality → intent-weighted hybrid score
```

**Why three signals?**

- **BM25** (full-text relevance) — finds symbols whose names, FQNs, signatures, or docstrings match the query terms. Good at "find me the function named X."
- **TF-IDF cosine similarity** — measures semantic overlap between the query and the symbol's term frequency vector. Good at "find code related to authentication" even if the exact word doesn't appear.
- **PageRank centrality** — structural importance in the dependency graph. A function called by many others is more likely to be relevant for refactoring or understanding.

**Intent detection** adjusts the weights between these signals. A "fix the auth bug" query (intent: Debug) weights BM25 heavily (find the exact error). A "refactor the auth system" query (intent: Refactor) weights centrality heavily (find high-impact symbols). Intent can be auto-detected from the query or explicitly passed via the `intent` parameter on `run_pipeline` and `get_context_capsule`. See `graph/intent.rs` for the keyword lists, weight tables, and `CapsuleHints`.

**Batch enrichment** — all per-candidate data (symbol metadata, term frequencies, IDF values) is loaded in batch via `WHERE IN (...)` queries before the scoring loop. No N+1 queries.

### Capsule Building

The capsule packs search results into a token budget:

```
search results → budget allocation → pivot files (full source) + skeletons (signatures) + memory
```

**Budget allocation** splits the total token budget into three pools: a small slice for memory entries, most of the remainder for pivot files (full source), and the rest for skeleton context. The pivot/skeleton ratio and BFS depth are intent-dependent via `CapsuleHints` in `graph/intent.rs` — e.g., Refactor allocates 30% to skeletons (vs 15% default) for broader structural visibility.

**BFS expansion** discovers adjacent symbols by traversing the dependency graph from pivot symbols (both callers and callees). Expansion depth is intent-dependent: Debug and Refactor use depth 3 to catch error propagation paths and blast radius; other intents use depth 2. Adjacent files are rendered as signature-only skeletons (with optional docstrings for Understand intent).

**Invariants:** no file appears in both pivots and skeletons, and `tokens_used` never exceeds `tokens_budget`.

### Memory System

Session memory gives AI agents continuity across interactions:

```
tool call → auto-capture observation → link to pivot FQNs
                                           ↓
                              next search → recall relevant observations
                                           ↓
                              code changes → detect staleness
                                           ↓
                              session idle → compress (discard auto, keep manual)
```

**Auto-capture** records what the agent worked on (tool name, query, pivot symbols) as `auto` observations. Manual observations (`insight`, `decision`, `error`) are saved explicitly via `save_observation`.

**Memory search** ranks observations using a composite of BM25, TF-IDF, recency decay, and symbol proximity to the current search context. Stale observations (linked to changed code) are penalized. See `memory/search.rs` for the weight table.

**Compression** runs on sessions inactive beyond a configurable threshold. It extracts key terms and file paths as metadata, deletes noisy `auto` observations, and preserves manually-saved ones. This keeps the memory system from growing unboundedly.

**Staleness detection** runs after each index. When a symbol's signature or body changes, all observations linked to that symbol's FQN are marked stale. This prevents the agent from acting on outdated context.

**AST structural diffs** — beyond staleness, the indexer now detects six types of structural changes (added, removed, signature changed, visibility changed, renamed, body changed) and stores them in the `symbol_changes` table. Recent changes are surfaced in capsule responses so agents see what shifted since the session started.

**Anti-pattern detection** — three built-in detectors analyze agent behavior history: dead-end exploration (symbol added then removed), file thrashing (4+ changes to one file in 5 minutes), and circular search (3+ similar queries without learning). Warnings are surfaced in capsules and persisted as observations. The framework is extensible via the `PatternRule` trait.

## MCP Server Design

### Shared State

`CoreEngine` holds three things behind `Arc`:

- **`Mutex<Connection>`** — single SQLite connection for all DB work. Locked briefly for sync operations, never held across `.await`.
- **`RwLock<Option<SymbolGraph>>`** — the in-memory petgraph. Read-locked by concurrent tool calls, write-locked only by the watcher after graph rebuild.
- **`NdxrConfig`** — immutable after startup.

### Tool Architecture

Nine tools, split into two categories:

- **Auto-capture tools** (6) — record what the agent does. `run_pipeline` and `get_context_capsule` share a common pipeline via `run_capsule_pipeline()`. `search_logic_flow` traces execution paths between symbols using Yen's K-shortest paths on the dependency graph. Auto-capture boilerplate is consolidated in `commit_tool_record()`.
- **Manual tools** (3) — `search_memory`, `save_observation`, `index_status`. No auto-capture, used for explicit agent actions.

### File Watcher

The watcher uses `notify` for filesystem events, debounced via a tokio interval + pending `HashSet`. On each tick, pending paths are drained, `index_paths()` re-indexes only the changed files, and the graph is rebuilt on a separate connection. The new graph is stored via `graph.write().await` — never silently dropped. After successful re-indexing, anti-pattern detectors run against the session's change history to surface warnings early.

## Security Model

ndxr runs locally and trusts the MCP client (typically an AI agent), but still enforces defense-in-depth:

- **SQL injection** — all queries use parameterized placeholders. Batch `IN (...)` clauses use `format!` for placeholder positions only, never for values.
- **FTS5 injection** — user queries are sanitized through `build_fts_query()` which strips all FTS5 special characters and wraps terms in double quotes.
- **Path traversal** — `read_file_content()` canonicalizes both the resolved path and workspace root, then verifies `starts_with()`. The file walker disables symlink following.
- **Resource limits** — all user-provided parameters (token budget, BFS depth, search limit, session count, observation content size) are clamped to hard caps before use.
- **Error isolation** — raw database errors are logged to stderr and replaced with generic messages before returning to MCP clients.

## Skeleton Renderer

Renders symbols as signature-only structural outlines. Symbols are loaded from the database, sorted by source position within each file, and grouped under container symbols (classes, structs, traits, etc.) detected by line-range containment. Container lookup uses pre-sorted ranges with binary search for efficiency.

## Testing Strategy

- **Unit tests** (`#[cfg(test)]` in source files) — test internal/private logic, edge cases, scoring math
- **Integration tests** (`tests/` directory) — test the public API as a separate crate with real temp directories and SQLite databases
- **Shared helpers** (`tests/helpers/mod.rs`) — common workspace fixtures and index setup, extracted when patterns repeated across multiple test files
- **CI pipeline** — `make ci` runs format check + clippy pedantic + all tests. Zero warnings tolerated.
