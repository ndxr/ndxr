<p align="center">
  <img src="assets/logo.svg" alt="ndxr" width="256" height="64">
</p>

<p align="center">Local-first context engine for AI coding agents, optimized for Claude Code.</p>

<p align="center">
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/Rust-1.94-orange.svg?logo=rust" alt="Rust"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-Apache_2.0-blue.svg" alt="License"></a>
  <a href="https://github.com/ndxr/ndxr/actions/workflows/ci.yml"><img src="https://github.com/ndxr/ndxr/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
</p>

AI coding agents waste most of their context window reading irrelevant files. ndxr fixes this. It understands your codebase structurally -- functions, classes, imports, call chains -- and delivers only the code that matters for the current task, packed into a token budget you control.

**Single binary. No cloud. No API keys. Your code never leaves your machine.**

## Why ndxr?

| Problem | Without ndxr | With ndxr |
|---------|-------------|-----------|
| Agent reads wrong files | Dumps entire files, wastes tokens | Delivers only relevant symbols with full context |
| No structural awareness | Text search misses relationships | Dependency graph knows who calls what |
| Context window overflow | Truncated or missing code | Token-budgeted capsules fit your limit |
| No session continuity | Agent forgets past decisions | Session memory persists observations across sessions |
| Refactoring blind spots | No blast radius awareness | Impact graph shows callers, callees, and risk level |
| No execution flow visibility | Can't trace call chains | Logic flow traces paths between any two symbols |
| Repeating mistakes | No behavior analysis | Anti-pattern detection warns about thrashing and dead ends |
| Vocabulary mismatch | Text search misses synonyms | Semantic embeddings bridge meaning gaps |
| Slow on large codebases | Re-reads everything | Incremental indexing -- only changed files are re-parsed |

### Key Features

- **13 languages supported** -- See [full list](#supported-languages) below
- **Incremental indexing** -- Only changed files are re-parsed. Second run takes <1s
- **Live file watcher** -- Detects saves and re-indexes changed files automatically during MCP sessions
- **Intent detection** -- Understands "fix the auth bug" vs "explain the auth flow" and adjusts results
- **Transparent scoring** -- Every result includes a "why" breakdown showing exactly how it was ranked
- **Impact analysis** -- Shows blast radius before refactoring (low/medium/high based on transitive callers)
- **Logic flow tracing** -- Find execution paths between any two symbols via Yen's K-shortest paths
- **AST structural diffs** -- Tracks symbol additions, removals, signature changes, renames across sessions
- **Anti-pattern detection** -- Warns about dead-end explorations, file thrashing, and circular searches
- **Semantic search** -- Download a 23 MiB model to enable meaning-based ranking that bridges vocabulary gaps
- **Character n-gram matching** -- Partial queries like "auth" boost "authenticate" and "AuthService"
- **Auto-relaxation** -- Never returns empty results. Progressively relaxes search if needed
- **Cross-platform** -- Linux, macOS, Windows. Single static binary, no runtime dependencies

## Quick Start

```bash
# Install
make install              # or: cargo install --path .

# Navigate to your project
cd your-project

# Index the codebase (takes seconds, incremental after first run)
ndxr index

# Configure Claude Code integration (writes .mcp.json + CLAUDE.md)
ndxr setup

# Now open Claude Code and try:
# "Explain the authentication flow in this project"
```

That's it. Claude Code will now automatically call ndxr before reading or modifying files.

> **Note:** ndxr stores its index in `.ndxr/` in your workspace root. Add it to your `.gitignore`.

## How It Works

ndxr parses your codebase into symbols (functions, classes, methods, types) and edges (calls, imports, extends). It then ranks everything using up to five signals:

```
  Source Files       tree-sitter        SQLite + FTS5        petgraph
 +----------+      +----------+      +---------------+    +-----------+
 | .ts .py  |----->|  Parser  |----->| Symbols/Edges |--->| PageRank  |
 | .rs .go  |      |  13 lang |      | TF-IDF + BM25 |    | Centrality|
 +----------+      +----------+      +-------+-------+    +-----+-----+
                                             |                  |
                                             v                  v
                                     +----------------------------+
                                     |      Hybrid Search         |
                                     |  BM25 + TF-IDF + PageRank  |
                                     |  + N-gram + Semantic (opt) |
                                     |  + Intent Detection        |
                                     +------------+---------------+
                                                  |
                                                  v
                                     +----------------------------+
                                     |     Capsule Builder        |
                                     |  Pivots (full source)      |
                                     |  Skeletons (signatures)    |
                                     |  Memory (observations)     |
                                     |  Impact Hints              |
                                     +------------+---------------+
                                                  |
                                                  v
                                     +----------------------------+
                                     |       MCP Server           |
                                     |   10 tools over stdio      |
                                     +----------------------------+
```

**Hybrid Search** -- Combines full-text relevance (BM25), term similarity (TF-IDF), structural importance (PageRank), character n-gram matching, and optional semantic embeddings. Automatically adjusts scoring based on detected intent: debug, test, refactor, modify, understand, or explore.

**Context Capsules** -- Packs the most relevant code into a token budget. Pivot files get full source. Adjacent files get signature-only skeletons. Past observations and impact hints are included when relevant. The budget is never exceeded.

**Session Memory** -- Automatically captures what the agent works on. Insights and decisions persist across sessions. Stale observations are detected when linked code changes. Inactive sessions are compressed to reduce noise.

For a deeper dive into internals, see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Supported Languages

| Language | Extensions |
|----------|-----------|
| TypeScript | `.ts`, `.tsx` |
| JavaScript | `.js`, `.jsx`, `.mjs`, `.cjs` |
| Python | `.py`, `.pyi` |
| Go | `.go` |
| Rust | `.rs` |
| Java | `.java` |
| C# | `.cs` |
| Ruby | `.rb` |
| Bash | `.sh`, `.bash` |
| PHP | `.php` |
| Zig | `.zig` |
| C | `.c`, `.h` |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh`, `.hxx` |

## MCP Tools

| Tool | Description |
|------|-------------|
| `run_pipeline` | **Start here.** Full pipeline: search, build capsule, recall memory, generate impact hints. Pass `intent` (debug/test/refactor/modify/understand/explore) for optimized results. |
| `get_context_capsule` | Follow-up context query. Search + capsule + memory, without impact hints. |
| `get_skeleton` | Render files as signature-only structural outlines. Great for understanding file structure. |
| `get_impact_graph` | Show callers, callees, and blast radius for a symbol. Use before refactoring. |
| `search_logic_flow` | Trace execution paths between two symbols. Find how data or control flows from A to B. |
| `search_memory` | Search past observations and decisions across sessions. |
| `save_observation` | Persist an insight, decision, or important context to session memory. |
| `get_session_context` | Review recent session history and observations. |
| `index_status` | Health check: file, symbol, and edge counts, DB size, index age. |
| `reindex` | Force a full re-index when the index is stale (after git checkout, branch switch). |

## CLI Reference

```bash
ndxr index [--verbose]                # Incremental index (only changed files)
ndxr reindex [--verbose]              # Full re-index (preserves session memory)
ndxr mcp                              # Start MCP server on stdio
ndxr setup [--scope project|user]     # Configure Claude Code (.mcp.json + CLAUDE.md)
ndxr status [--json]                  # Show index statistics
ndxr search "query" [-n 10] [--intent debug] [--explain]
ndxr skeleton src/auth.ts [--docs true|false]
ndxr activity [--limit N] [--follow]  # Show recent MCP tool activity
ndxr upgrade [--check] [--force]      # Check for updates and self-upgrade
ndxr model download                   # Download embedding model for semantic search
ndxr model status                     # Show model and embedding coverage
```

Run `ndxr <command> --help` for detailed help on any command.

## Configuration

### .ndxrignore

Exclude paths from indexing and file watching. Uses gitignore syntax. Place in workspace root.

```gitignore
dist/
build/
*.min.js
vendor/
__fixtures__/
```

If `.ndxrignore` is not present, ndxr falls back to `.gitignore`. When both files exist, patterns from both are applied. Changes to either file take effect immediately during a session (no restart needed).

The following directories are always excluded (both indexing and file watching):

- `.git/`, `.ndxr/`, `node_modules/`, `target/`, `build/`, `bin/`, `dist/`
- Hidden files and directories

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `NDXR_MAX_TOKENS` | `20000` | Maximum token budget for MCP responses. Set to `-1` for unlimited (no cap, no trimming). |
| `NDXR_CHARS_PER_TOKEN` | `3.5` | Characters per token ratio for budget estimation. Adjusts how ndxr converts token budgets to character limits. |

### Setup Scopes

```bash
ndxr setup                  # Project scope (default) -- writes .mcp.json in workspace
ndxr setup --scope user     # User scope -- writes to ~/.claude.json (global)
```

Setup creates two files:

- **`.mcp.json`** (or `~/.claude.json` for user scope) -- MCP server configuration. If the file already exists, the ndxr entry is merged in and other servers are preserved.
- **`CLAUDE.md`** -- Agent instructions telling Claude Code to use ndxr tools. If the file already exists, the ndxr section is appended or replaced in-place. Existing content is never removed.

Safe to run multiple times -- existing configuration is merged, not overwritten.

## Building from Source

```bash
git clone git@github.com:ndxr/ndxr.git
cd ndxr
make build-release
make install
```

Requires Rust 1.94 (edition 2024).

Run `make help` for all available targets (build, test, lint, cross-compilation).

## License

[Apache License 2.0](LICENSE)
