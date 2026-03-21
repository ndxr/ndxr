//! MCP (Model Context Protocol) server exposing ndxr tools over stdio.
//!
//! Provides 9 tools for AI coding agents: `run_pipeline`, `get_context_capsule`,
//! `get_skeleton`, `get_impact_graph`, `search_logic_flow`, `search_memory`,
//! `save_observation`, `get_session_context`, and `index_status`.

pub mod server;

pub use server::start_mcp_server;
