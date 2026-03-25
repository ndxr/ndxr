//! ndxr — local-first context engine for AI coding agents.
//!
//! Indexes codebases via tree-sitter, builds a dependency graph, and serves
//! relevant code context through an MCP server over stdio.

pub mod capsule;
pub mod config;
pub mod graph;
pub mod indexer;
pub mod languages;
pub mod mcp;
pub mod memory;
pub mod skeleton;
pub mod status;
pub mod storage;
pub mod upgrade;
pub mod util;
pub mod watcher;
pub mod workspace;
