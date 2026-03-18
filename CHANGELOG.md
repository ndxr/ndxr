# Changelog

All notable changes to ndxr will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-03-18

Initial release.

### Added

- Single binary, no cloud, no API keys — your code never leaves your machine
- 14 languages supported (TypeScript, JavaScript, Python, Go, Rust, Java, C#, Ruby, Bash, PHP, Zig, C, C++, TSX)
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
