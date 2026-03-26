# Changelog

All notable changes to ndxr will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.6.0] - 2026-03-26

### Added

- Unlimited token budget mode — set `NDXR_MAX_TOKENS=-1` to remove all caps and trimming
- `NDXR_CHARS_PER_TOKEN` environment variable — tune the characters-per-token ratio for budget estimation
- Post-serialization safety net — responses that exceed the token budget are progressively trimmed (warnings → skeletons → pivots) instead of failing
- File watcher now respects `.ndxrignore` and `.gitignore` patterns, with default exclusions for `target/`, `build/`, `bin/`, `node_modules/`, `.git/`, `dist/`

### Changed

- Default per-tool token budget raised from 8,000 to 10,000 tokens
- JSON overhead accounting — 20% of the token budget is reserved for serialization overhead, preventing oversized MCP responses
- Compact JSON serialization — MCP responses use minified JSON (~30% smaller)

### Fixed

- **Critical:** File watcher no longer wipes the entire index when re-indexing a single changed file
- **Critical:** File watcher no longer indexes build artifacts from `target/` and other ignored directories
- `skeleton --docs false` now correctly disables docstrings in CLI output
- Windows compatibility fix for tar.gz extraction test

## [0.5.0] - 2026-03-25

### Added

- `ndxr upgrade` command — check for updates and self-upgrade via GitHub releases with SHA-256 checksum verification
- Dependency vulnerability scanning (`cargo audit`) in CI and Makefile
- Clippy nursery lint enforcement in CI and Makefile

### Improved

- MCP error responses no longer leak internal error details — full errors are logged server-side, clients receive generic messages
- Tool parameter descriptions now surface valid ranges (default/max values) in the MCP schema so agents can discover bounds without trial and error

## [0.4.0] - 2026-03-21

### Added

- `search_logic_flow` tool — trace execution paths between any two symbols through the call graph (Yen's K-shortest paths, up to 5 paths ranked by hop count and centrality)
- AST structural diff tracking — the indexer now detects symbol additions, removals, signature changes, visibility changes, renames, and body changes across re-indexes
- Anti-pattern detection — warns when the agent repeats dead-end explorations (add then remove), thrashes a single file (4+ changes in 5 minutes), or runs circular searches (3+ similar queries without learning)
- Capsule responses now include `recent_changes` (what shifted since the session started) and `warnings` (detected anti-patterns)
- File watcher runs anti-pattern detectors after each re-index

### Improved

- Staleness detection uses richer structural diffs instead of simple hash comparison
- `ndxr setup` generates CLAUDE.md with the new `search_logic_flow` tool documented
- Batch placeholder generation consolidated into `build_batch_placeholders()` — 13 inline sites replaced with one shared helper
- Scoring uses `Cow<'static, str>` for constant reason strings, reducing allocations
- `compute_tf` borrows during counting, allocates only once per unique token
- Graph builder pre-allocates HashMaps based on symbol count

## [0.3.0] - 2026-03-20

### Added

- Pass `intent` to `run_pipeline` and `get_context_capsule` to get context optimized for your task (debug, test, refactor, modify, understand, explore)
- `ndxr setup` now documents all six intents with a usage table in CLAUDE.md so Claude Code knows when and how to use them
- Rust generic impl patterns, Go struct/interface classification, TS/JS arrow function detection, C++ pointer/reference declarators, Python decorated definition dedup, PHP require/include imports, C# record and delegate declarations, Zig test declaration names

### Improved

- Context capsules are now shaped by intent — refactoring gets broader structural context (more skeletons), debugging gets deeper error-path tracing, understanding includes docstrings in skeletons
- BFS expansion depth adapts to intent: depth 3 for debug/refactor (catches error propagation and blast radius), depth 2 for others
- File watcher uses true deadline-based debounce instead of interval polling, skips graph rebuild on index error
- Memory search results are now sorted by score; session compression is wrapped in a transaction
- Skeletons are sorted by BFS depth so the most closely related neighbors appear first

### Fixed

- Race condition between INSERT OR IGNORE and last_insert_rowid in indexer pipeline
- Silent data loss from filter_map(Result::ok) on database queries — errors are now logged
- Edge resolver now discriminates QueryReturnedNoRows from real errors
- IDF returns zero for unknown terms instead of producing incorrect scores
- Score normalization handles equal values correctly (returns zero instead of NaN)
- Memory recency_half_life_days is validated before use
- MCP server drops graph read lock before committing tool records (prevents lock ordering issues)
- Skeleton renderer uses exact container kind matching instead of substring
- `unix_now()` uses safe fallback instead of expect()

## [0.2.0] - 2026-03-19

### Improved

- Faster indexing — files are now read once and parsed once (previously read twice, parsed twice)
- Faster searches — all database lookups are now batched instead of per-item
- Concurrent MCP tool calls no longer block each other while reading the symbol graph
- File watcher now guarantees index updates are applied (previously could silently drop updates under load)
- Better error messages — database failures are now logged with context instead of silently ignored
- Capsule stats now report accurate memory token usage, search timing, and whether auto-relaxation was applied

### Added

- Resource limits on all MCP tool parameters (depth, result count, content size) to prevent misuse
- Input validation on `save_observation` — rejects unknown kinds and oversized content
- Impact analysis uses typed blast radius categories (low/medium/high) instead of raw strings
- Architecture documentation (`docs/ARCHITECTURE.md`)

### Fixed

- Search results no longer silently drop symbols when the database returns unexpected errors
- `ndxr status` now reports errors instead of showing misleading zero counts on database failures
- Path traversal guard in capsule builder is now tested against real files outside the workspace

## [0.1.0] - 2026-03-18

Initial release.

### Added

- Single binary, no cloud, no API keys — your code never leaves your machine
- 13 languages supported (TypeScript/TSX, JavaScript, Python, Go, Rust, Java, C#, Ruby, Bash, PHP, Zig, C, C++)
- Incremental indexing — only changed files are re-parsed, second run takes <1s
- Live file watcher — detects saves and re-indexes automatically during sessions
- Intent-aware search — understands "fix the auth bug" vs "explain the auth flow"
- Token-budgeted context capsules — full source for top results, signatures for adjacent code
- Impact analysis — shows blast radius before refactoring (low/medium/high)
- Session memory — persists observations and decisions across sessions, detects when linked code changes
- Auto-relaxation — never returns empty results
- MCP server with 8 tools over stdio for Claude Code integration
- One-command setup: `ndxr setup` configures Claude Code automatically
- CLI commands: index, reindex, search, skeleton, status, setup
- Cross-platform: Linux, macOS (x86_64 and ARM64), Windows (x86_64)
