# Changelog

All notable changes to ndxr will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
