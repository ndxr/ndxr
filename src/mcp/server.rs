//! MCP server implementation with 8 tools for AI coding agents.
//!
//! All shared state is held behind `Arc<CoreEngine>` so the server struct
//! remains `Clone` as required by rmcp. The `rusqlite::Connection` and
//! `SymbolGraph` are protected by `tokio::sync::Mutex` for safe async access.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use anyhow::Result;
use petgraph::Direction;
use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::info;

use crate::capsule::builder::{self, CapsuleRequest};
use crate::capsule::relaxation;
use crate::config::{NdxrConfig, TokenEstimator};
use crate::graph::builder::SymbolGraph;
use crate::graph::intent;
use crate::memory::{capture, compression, search as mem_search, store};
use crate::{graph, indexer, skeleton, storage};

/// Default token budget for MCP tool responses.
const DEFAULT_TOOL_TOKEN_BUDGET: usize = 8000;

/// Hard upper limit for user-provided `max_tokens` parameters.
const MAX_TOKEN_BUDGET: usize = 50_000;

/// Default maximum search results.
const DEFAULT_MAX_RESULTS: usize = 10;

/// Default BFS traversal depth for impact graph.
const DEFAULT_IMPACT_DEPTH: usize = 3;

/// Default memory search limit.
const DEFAULT_MEMORY_LIMIT: usize = 5;

/// Default session context count.
const DEFAULT_SESSION_COUNT: usize = 3;

// ---------------------------------------------------------------------------
// Response structs (defined at module level to satisfy items_after_statements)
// ---------------------------------------------------------------------------

/// Skeleton rendering result for JSON serialization.
#[derive(Serialize)]
struct SkeletonResult {
    /// Relative file path.
    path: String,
    /// Rendered skeleton content.
    skeleton: String,
    /// Number of symbols in the skeleton.
    symbol_count: usize,
    /// Original line count of the source file.
    original_lines: i64,
}

/// A node in the impact graph BFS traversal.
#[derive(Serialize)]
struct ImpactNode {
    /// Fully-qualified name of the symbol.
    fqn: String,
    /// Symbol kind (function, class, method, etc.).
    kind: String,
    /// Relative file path.
    file_path: String,
    /// BFS depth from the target symbol.
    depth: usize,
    /// Direction: "caller" or "callee".
    direction: String,
}

/// Impact graph traversal result.
#[derive(Serialize)]
struct ImpactResult {
    /// Target symbol FQN.
    symbol_fqn: String,
    /// Number of transitive callers found.
    callers_count: usize,
    /// Number of transitive callees found.
    callees_count: usize,
    /// Blast radius classification: "low", "medium", or "high".
    blast_radius: String,
    /// All discovered nodes.
    nodes: Vec<ImpactNode>,
}

/// Memory search result for JSON serialization.
#[derive(Serialize)]
struct MemorySearchResult {
    /// Observation database ID.
    id: i64,
    /// Observation content.
    content: String,
    /// Observation kind.
    kind: String,
    /// Session that created this observation.
    session_id: String,
    /// Unix timestamp of creation.
    created_at: i64,
    /// Relevance score.
    score: f64,
    /// Whether the observation is stale.
    is_stale: bool,
    /// Linked symbol FQNs.
    linked_fqns: Vec<String>,
}

/// Session detail for JSON serialization.
#[derive(Serialize)]
struct SessionDetail {
    /// Session UUID.
    id: String,
    /// Unix timestamp when the session started.
    started_at: i64,
    /// Unix timestamp of most recent activity.
    last_active: i64,
    /// Whether this session has been compressed.
    is_compressed: bool,
    /// Compression summary.
    summary: Option<String>,
    /// Comma-separated key terms.
    key_terms: Option<String>,
    /// Comma-separated key file paths.
    key_files: Option<String>,
    /// Observations in this session.
    observations: Vec<ObservationDetail>,
}

/// Observation detail for JSON serialization.
#[derive(Serialize)]
struct ObservationDetail {
    /// Observation database ID.
    id: i64,
    /// Observation kind.
    kind: String,
    /// Observation content.
    content: String,
    /// Optional headline.
    headline: Option<String>,
    /// Whether the observation is stale.
    is_stale: bool,
    /// Unix timestamp of creation.
    created_at: i64,
}

// ---------------------------------------------------------------------------
// Core Engine (shared state)
// ---------------------------------------------------------------------------

/// Shared engine state for the MCP server.
///
/// Holds the database connection, in-memory graph, and configuration behind
/// async-aware mutexes so multiple tool calls can be served concurrently.
pub struct CoreEngine {
    /// Runtime configuration.
    pub config: NdxrConfig,
    /// Database connection protected by an async mutex.
    pub conn: Mutex<rusqlite::Connection>,
    /// In-memory symbol graph, rebuilt after each index operation.
    pub graph: Mutex<Option<SymbolGraph>>,
}

// ---------------------------------------------------------------------------
// MCP Server
// ---------------------------------------------------------------------------

/// MCP server exposing ndxr tools over stdio.
///
/// Wraps `CoreEngine` in an `Arc` so the struct is `Clone` as required by rmcp.
/// Each tool method acquires the necessary locks, performs synchronous DB work,
/// then releases the lock before returning.
#[derive(Clone)]
pub struct NdxrServer {
    /// Tool router generated by the `#[tool_router]` macro.
    tool_router: ToolRouter<Self>,
    /// Shared engine state.
    engine: Arc<CoreEngine>,
    /// Current session ID for auto-capture.
    session_id: String,
}

// ---------------------------------------------------------------------------
// Tool parameter structs
// ---------------------------------------------------------------------------

/// Parameters for the `run_pipeline` tool.
#[derive(Deserialize, JsonSchema)]
struct RunPipelineParams {
    /// Description of the task the agent is working on.
    task: String,
    /// Token budget for the response (default: 8000).
    max_tokens: Option<usize>,
}

/// Parameters for the `get_context_capsule` tool.
#[derive(Deserialize, JsonSchema)]
struct GetContextCapsuleParams {
    /// Search query for finding relevant code.
    query: String,
    /// Token budget for the response (default: 8000).
    max_tokens: Option<usize>,
    /// Override auto-detected intent (debug, test, refactor, modify, understand, explore).
    intent: Option<String>,
}

/// Parameters for the `get_skeleton` tool.
#[derive(Deserialize, JsonSchema)]
struct GetSkeletonParams {
    /// Relative file paths to render as skeletons.
    files: Vec<String>,
    /// Include docstrings in the output (default: true).
    include_docs: Option<bool>,
}

/// Parameters for the `get_impact_graph` tool.
#[derive(Deserialize, JsonSchema)]
struct GetImpactGraphParams {
    /// Fully qualified symbol name to analyze.
    symbol_fqn: String,
    /// Maximum BFS traversal depth (default: 3).
    depth: Option<usize>,
    /// Include callers (incoming edges) in the result (default: true).
    include_callers: Option<bool>,
    /// Include callees (outgoing edges) in the result (default: true).
    include_callees: Option<bool>,
}

/// Parameters for the `search_memory` tool.
#[derive(Deserialize, JsonSchema)]
struct SearchMemoryParams {
    /// Natural-language query to search observations.
    query: String,
    /// Maximum number of results (default: 5).
    limit: Option<usize>,
    /// Filter by observation kind (e.g. "decision", "error", "insight").
    kind: Option<String>,
    /// Include stale observations (default: false).
    include_stale: Option<bool>,
}

/// Parameters for the `save_observation` tool.
#[derive(Deserialize, JsonSchema)]
struct SaveObservationParams {
    /// Observation content text.
    content: String,
    /// Observation kind: insight, decision, error, or manual (default: "manual").
    kind: Option<String>,
    /// Fully-qualified symbol names to link to this observation.
    linked_symbols: Option<Vec<String>>,
}

/// Parameters for the `get_session_context` tool.
#[derive(Deserialize, JsonSchema)]
struct GetSessionContextParams {
    /// Number of recent sessions to include (default: 5).
    session_count: Option<usize>,
    /// Include compressed sessions (default: false).
    include_compressed: Option<bool>,
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

#[tool_router]
impl NdxrServer {
    /// Creates a new `NdxrServer` instance.
    pub fn new(engine: Arc<CoreEngine>, session_id: String) -> Self {
        Self {
            tool_router: Self::tool_router(),
            engine,
            session_id,
        }
    }

    /// Full pipeline: detect intent, search, build capsule, recall memory,
    /// generate impact hints, auto-capture, and return JSON.
    #[tool(
        description = "Run the full ndxr pipeline: detect intent from task description, search the codebase, build a context capsule with full source for pivots and skeletons for adjacent files, recall relevant memories, and generate impact hints. Returns a comprehensive JSON context package."
    )]
    async fn run_pipeline(
        &self,
        params: Parameters<RunPipelineParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let query = &params.0.task;
        let budget = params
            .0
            .max_tokens
            .unwrap_or(DEFAULT_TOOL_TOKEN_BUDGET)
            .min(MAX_TOKEN_BUDGET);

        let conn_guard = self.engine.conn.lock().await;
        let graph_guard = self.engine.graph.lock().await;

        let graph_ref = graph_guard
            .as_ref()
            .ok_or_else(|| rmcp::ErrorData::internal_error("symbol graph not initialized", None))?;

        let intent = intent::detect_intent(query);

        let results = relaxation::search_with_relaxation(
            &conn_guard,
            graph_ref,
            query,
            DEFAULT_MAX_RESULTS,
            Some(intent),
        )
        .map_err(|e| rmcp::ErrorData::internal_error(format!("search failed: {e}"), None))?;

        let estimator = TokenEstimator::default();
        let req = CapsuleRequest {
            conn: &conn_guard,
            graph: graph_ref,
            search_results: &results,
            query,
            intent: &intent,
            token_budget: budget,
            estimator: &estimator,
            workspace_root: &self.engine.config.workspace_root,
        };
        let mut capsule = builder::build_capsule(&req).map_err(|e| {
            rmcp::ErrorData::internal_error(format!("capsule build failed: {e}"), None)
        })?;

        let pivot_fqns: Vec<String> = results.iter().map(|r| r.fqn.clone()).collect();
        let memories = mem_search::search_memories(
            &conn_guard,
            query,
            &pivot_fqns,
            DEFAULT_MEMORY_LIMIT,
            false,
            self.engine.config.recency_half_life_days,
        )
        .map_err(|e| rmcp::ErrorData::internal_error(format!("memory search failed: {e}"), None))?;

        capsule.memories = memories.iter().map(memory_entry_from).collect();
        capsule.impact_hints = builder::generate_impact_hints(graph_ref, &results);

        let record = capture::ToolCallRecord {
            tool_name: "run_pipeline".to_owned(),
            intent: Some(format!("{intent:?}").to_lowercase()),
            query: Some(query.to_owned()),
            pivot_fqns,
            result_summary: format!(
                "{} pivots, {} skeletons, {} memories",
                capsule.pivots.len(),
                capsule.skeletons.len(),
                capsule.memories.len()
            ),
        };
        let _ = capture::auto_capture(&conn_guard, &self.session_id, &record);
        let _ = store::update_session_active(&conn_guard, &self.session_id);

        drop(graph_guard);
        drop(conn_guard);

        to_json_result(&capsule)
    }

    /// Search the codebase and build a context capsule (no impact hints).
    #[tool(
        description = "Search the codebase and build a context capsule with pivot files (full source) and skeleton files (signatures only). Includes memory recall. Does not generate impact hints. Returns JSON."
    )]
    async fn get_context_capsule(
        &self,
        params: Parameters<GetContextCapsuleParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let query = &params.0.query;
        let budget = params
            .0
            .max_tokens
            .unwrap_or(DEFAULT_TOOL_TOKEN_BUDGET)
            .min(MAX_TOKEN_BUDGET);
        let intent_override = params.0.intent.as_deref().and_then(intent::parse_intent);

        let conn_guard = self.engine.conn.lock().await;
        let graph_guard = self.engine.graph.lock().await;

        let graph_ref = graph_guard
            .as_ref()
            .ok_or_else(|| rmcp::ErrorData::internal_error("symbol graph not initialized", None))?;

        let intent = intent_override.unwrap_or_else(|| intent::detect_intent(query));

        let results = relaxation::search_with_relaxation(
            &conn_guard,
            graph_ref,
            query,
            DEFAULT_MAX_RESULTS,
            Some(intent),
        )
        .map_err(|e| rmcp::ErrorData::internal_error(format!("search failed: {e}"), None))?;

        let estimator = TokenEstimator::default();
        let req = CapsuleRequest {
            conn: &conn_guard,
            graph: graph_ref,
            search_results: &results,
            query,
            intent: &intent,
            token_budget: budget,
            estimator: &estimator,
            workspace_root: &self.engine.config.workspace_root,
        };
        let mut capsule = builder::build_capsule(&req).map_err(|e| {
            rmcp::ErrorData::internal_error(format!("capsule build failed: {e}"), None)
        })?;

        let pivot_fqns: Vec<String> = results.iter().map(|r| r.fqn.clone()).collect();
        let memories = mem_search::search_memories(
            &conn_guard,
            query,
            &pivot_fqns,
            DEFAULT_MEMORY_LIMIT,
            false,
            self.engine.config.recency_half_life_days,
        )
        .map_err(|e| rmcp::ErrorData::internal_error(format!("memory search failed: {e}"), None))?;

        capsule.memories = memories.iter().map(memory_entry_from).collect();

        let record = capture::ToolCallRecord {
            tool_name: "get_context_capsule".to_owned(),
            intent: Some(format!("{intent:?}").to_lowercase()),
            query: Some(query.to_owned()),
            pivot_fqns,
            result_summary: format!(
                "{} pivots, {} skeletons",
                capsule.pivots.len(),
                capsule.skeletons.len()
            ),
        };
        let _ = capture::auto_capture(&conn_guard, &self.session_id, &record);
        let _ = store::update_session_active(&conn_guard, &self.session_id);

        drop(graph_guard);
        drop(conn_guard);

        to_json_result(&capsule)
    }

    /// Render files as signature-only skeletons.
    #[tool(
        description = "Render one or more files as signature-only skeletons showing the structural outline (classes, functions, methods) without implementation bodies. Useful for understanding file structure quickly."
    )]
    async fn get_skeleton(
        &self,
        params: Parameters<GetSkeletonParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let files = &params.0.files;
        let include_docs = params.0.include_docs.unwrap_or(true);

        let conn_guard = self.engine.conn.lock().await;

        let skeletons = skeleton::reducer::render_skeletons(&conn_guard, files, include_docs)
            .map_err(|e| {
                rmcp::ErrorData::internal_error(format!("skeleton render failed: {e}"), None)
            })?;

        let result: Vec<SkeletonResult> = skeletons
            .into_iter()
            .map(|(path, content, sym_count, line_count)| SkeletonResult {
                path,
                skeleton: content,
                symbol_count: sym_count,
                original_lines: line_count,
            })
            .collect();

        let record = capture::ToolCallRecord {
            tool_name: "get_skeleton".to_owned(),
            intent: None,
            query: None,
            pivot_fqns: Vec::new(),
            result_summary: format!("{} files", result.len()),
        };
        let _ = capture::auto_capture(&conn_guard, &self.session_id, &record);
        let _ = store::update_session_active(&conn_guard, &self.session_id);

        drop(conn_guard);

        to_json_result(&result)
    }

    /// Traverse the dependency graph from a symbol to show callers and callees.
    #[tool(
        description = "Show the impact graph for a symbol: callers (who calls it), callees (what it calls), and blast radius classification. Useful for understanding change impact before refactoring."
    )]
    async fn get_impact_graph(
        &self,
        params: Parameters<GetImpactGraphParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let fqn = &params.0.symbol_fqn;
        let max_depth = params.0.depth.unwrap_or(DEFAULT_IMPACT_DEPTH);
        let include_callers = params.0.include_callers.unwrap_or(true);
        let include_callees = params.0.include_callees.unwrap_or(true);

        let conn_guard = self.engine.conn.lock().await;
        let graph_guard = self.engine.graph.lock().await;

        let graph_ref = graph_guard
            .as_ref()
            .ok_or_else(|| rmcp::ErrorData::internal_error("symbol graph not initialized", None))?;

        let sym_id: i64 = conn_guard
            .query_row("SELECT id FROM symbols WHERE fqn = ?1", [fqn], |row| {
                row.get(0)
            })
            .map_err(|_| {
                rmcp::ErrorData::invalid_params(format!("symbol not found: {fqn}"), None)
            })?;

        let start_node = graph_ref.id_to_node.get(&sym_id).ok_or_else(|| {
            rmcp::ErrorData::invalid_params(format!("symbol not in graph: {fqn}"), None)
        })?;

        let mut nodes = Vec::new();

        if include_callers {
            bfs_collect_nodes(
                &conn_guard,
                graph_ref,
                *start_node,
                Direction::Incoming,
                max_depth,
                "caller",
                &mut nodes,
            );
        }

        if include_callees {
            bfs_collect_nodes(
                &conn_guard,
                graph_ref,
                *start_node,
                Direction::Outgoing,
                max_depth,
                "callee",
                &mut nodes,
            );
        }

        let total_callers = nodes.iter().filter(|n| n.direction == "caller").count();
        let callees_count = nodes.iter().filter(|n| n.direction == "callee").count();
        let blast_radius = match total_callers {
            0..=4 => "low",
            5..=20 => "medium",
            _ => "high",
        };

        let result = ImpactResult {
            symbol_fqn: fqn.clone(),
            callers_count: total_callers,
            callees_count,
            blast_radius: blast_radius.to_owned(),
            nodes,
        };

        let record = capture::ToolCallRecord {
            tool_name: "get_impact_graph".to_owned(),
            intent: None,
            query: None,
            pivot_fqns: vec![fqn.clone()],
            result_summary: format!(
                "{total_callers} callers, {callees_count} callees, blast={blast_radius}"
            ),
        };
        let _ = capture::auto_capture(&conn_guard, &self.session_id, &record);
        let _ = store::update_session_active(&conn_guard, &self.session_id);

        drop(graph_guard);
        drop(conn_guard);

        to_json_result(&result)
    }

    /// Search session memory for relevant observations.
    #[tool(
        description = "Search session memory for relevant observations using hybrid scoring (BM25 + TF-IDF + recency + symbol proximity). No auto-capture is performed."
    )]
    async fn search_memory(
        &self,
        params: Parameters<SearchMemoryParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let query = &params.0.query;
        let limit = params.0.limit.unwrap_or(DEFAULT_MEMORY_LIMIT);
        let include_stale = params.0.include_stale.unwrap_or(false);

        let conn_guard = self.engine.conn.lock().await;

        let results = mem_search::search_memories(
            &conn_guard,
            query,
            &[],
            limit,
            include_stale,
            self.engine.config.recency_half_life_days,
        )
        .map_err(|e| rmcp::ErrorData::internal_error(format!("memory search failed: {e}"), None))?;

        let mut output: Vec<MemorySearchResult> = results
            .into_iter()
            .map(|m| MemorySearchResult {
                id: m.observation.id,
                content: m.observation.content,
                kind: m.observation.kind,
                session_id: m.observation.session_id,
                created_at: m.observation.created_at,
                score: m.memory_score,
                is_stale: m.observation.is_stale,
                linked_fqns: m.linked_fqns,
            })
            .collect();

        if let Some(ref kind_filter) = params.0.kind {
            output.retain(|r| r.kind == *kind_filter);
        }

        drop(conn_guard);

        to_json_result(&output)
    }

    /// Save a manual observation to session memory.
    #[tool(
        description = "Save an observation to session memory. Observations persist across sessions and are surfaced in future context queries. Use this to record decisions, insights, or important context. No auto-capture is performed."
    )]
    async fn save_observation(
        &self,
        params: Parameters<SaveObservationParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let kind = params.0.kind.as_deref().unwrap_or("manual");
        let linked = params.0.linked_symbols.unwrap_or_default();

        let conn_guard = self.engine.conn.lock().await;

        let obs = store::NewObservation {
            session_id: self.session_id.clone(),
            kind: kind.to_owned(),
            content: params.0.content.clone(),
            headline: None,
            detail_level: 3,
            linked_fqns: linked,
        };

        let obs_id = store::save_observation(&conn_guard, &obs)
            .map_err(|e| rmcp::ErrorData::internal_error(format!("save failed: {e}"), None))?;

        let _ = store::update_session_active(&conn_guard, &self.session_id);

        drop(conn_guard);

        let result = serde_json::json!({
            "observation_id": obs_id,
            "session_id": self.session_id,
            "kind": kind,
            "status": "saved"
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&result).unwrap_or_default(),
        )]))
    }

    /// Retrieve recent session context and observations.
    #[tool(
        description = "Retrieve recent session history with observations. Shows what the agent has been working on across sessions. Useful for resuming work or reviewing past decisions."
    )]
    async fn get_session_context(
        &self,
        params: Parameters<GetSessionContextParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let count = params.0.session_count.unwrap_or(DEFAULT_SESSION_COUNT);
        let include_compressed = params.0.include_compressed.unwrap_or(true);

        let conn_guard = self.engine.conn.lock().await;

        let sessions =
            store::get_recent_sessions(&conn_guard, count, include_compressed).map_err(|e| {
                rmcp::ErrorData::internal_error(format!("session query failed: {e}"), None)
            })?;

        let mut result = Vec::new();
        for session in sessions {
            let observations = store::get_session_observations(&conn_guard, &session.id)
                .unwrap_or_default()
                .into_iter()
                .map(|obs| ObservationDetail {
                    id: obs.id,
                    kind: obs.kind,
                    content: obs.content,
                    headline: obs.headline,
                    is_stale: obs.is_stale,
                    created_at: obs.created_at,
                })
                .collect();

            result.push(SessionDetail {
                id: session.id,
                started_at: session.started_at,
                last_active: session.last_active,
                is_compressed: session.is_compressed,
                summary: session.summary,
                key_terms: session.key_terms,
                key_files: session.key_files,
                observations,
            });
        }

        let record = capture::ToolCallRecord {
            tool_name: "get_session_context".to_owned(),
            intent: None,
            query: None,
            pivot_fqns: Vec::new(),
            result_summary: format!("{} sessions", result.len()),
        };
        let _ = capture::auto_capture(&conn_guard, &self.session_id, &record);
        let _ = store::update_session_active(&conn_guard, &self.session_id);

        drop(conn_guard);

        to_json_result(&result)
    }

    /// Show index status: file/symbol/edge counts, memory stats, DB size, index age.
    #[tool(
        description = "Show index status: file count, symbol count, edge count, memory observation count, database size, and index age. No auto-capture is performed."
    )]
    async fn index_status(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        let conn_guard = self.engine.conn.lock().await;

        let file_count: i64 = conn_guard
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
            .unwrap_or(0);
        let symbol_count: i64 = conn_guard
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
            .unwrap_or(0);
        let edge_count: i64 = conn_guard
            .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
            .unwrap_or(0);
        let observation_count: i64 = conn_guard
            .query_row("SELECT COUNT(*) FROM observations", [], |row| row.get(0))
            .unwrap_or(0);
        let session_count: i64 = conn_guard
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .unwrap_or(0);
        let oldest_index: Option<i64> = conn_guard
            .query_row("SELECT MIN(indexed_at) FROM files", [], |row| row.get(0))
            .unwrap_or(None);
        let newest_index: Option<i64> = conn_guard
            .query_row("SELECT MAX(indexed_at) FROM files", [], |row| row.get(0))
            .unwrap_or(None);

        let db_size_bytes = std::fs::metadata(&self.engine.config.db_path)
            .map(|m| m.len())
            .unwrap_or(0);

        drop(conn_guard);

        let result = serde_json::json!({
            "files": file_count,
            "symbols": symbol_count,
            "edges": edge_count,
            "observations": observation_count,
            "sessions": session_count,
            "oldest_index_at": oldest_index,
            "newest_index_at": newest_index,
            "db_size_bytes": db_size_bytes,
            "workspace_root": self.engine.config.workspace_root.display().to_string(),
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&result).unwrap_or_default(),
        )]))
    }
}

// ---------------------------------------------------------------------------
// ServerHandler implementation
// ---------------------------------------------------------------------------

#[tool_handler]
impl ServerHandler for NdxrServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2025_03_26)
            .with_server_info(Implementation::new("ndxr", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "ndxr is a local-first context engine for AI coding agents. \
                 Use run_pipeline for comprehensive context, get_context_capsule for \
                 targeted search, get_skeleton for file overviews, get_impact_graph \
                 for change analysis, and the memory tools for cross-session persistence.",
            )
    }
}

// ---------------------------------------------------------------------------
// Server startup
// ---------------------------------------------------------------------------

/// Starts the MCP server on stdio.
///
/// Initializes the database, auto-indexes if needed, builds the symbol graph,
/// computes `PageRank` centrality, creates a session, compresses inactive
/// sessions, and begins serving MCP tool calls over stdin/stdout.
///
/// All logging goes to stderr to keep stdout clean for the MCP protocol.
///
/// # Errors
///
/// Returns an error if the database cannot be opened, indexing fails,
/// or the MCP transport encounters an error.
pub async fn start_mcp_server(config: NdxrConfig) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let conn = storage::db::open_or_create(&config.db_path)?;

    let file_count: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))?;
    if file_count == 0 {
        info!("no files indexed — running initial index");
        drop(conn);
        indexer::index(&config)?;
    } else {
        drop(conn);
    }

    let conn = storage::db::open_or_create(&config.db_path)?;

    let graph_result = graph::builder::build_graph(&conn)?;
    graph::centrality::compute_and_store(&conn, &graph_result)?;
    info!(
        nodes = graph_result.graph.node_count(),
        edges = graph_result.graph.edge_count(),
        "graph ready"
    );

    let session_id = store::create_session(&conn)?;
    info!(session_id = %session_id, "session created");

    let compressed = compression::compress_inactive_sessions(&conn, config.compression_age_secs)?;
    if compressed > 0 {
        info!(compressed, "inactive sessions compressed");
    }

    let engine = Arc::new(CoreEngine {
        config,
        conn: Mutex::new(conn),
        graph: Mutex::new(Some(graph_result)),
    });

    // Start file watcher for incremental re-indexing
    let _watcher = crate::watcher::FileWatcher::start(
        engine.config.workspace_root.clone(),
        Arc::clone(&engine),
    )?;
    info!("file watcher active");

    let server = NdxrServer::new(engine, session_id);

    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| anyhow::anyhow!("MCP server failed to start: {e}"))?;
    service
        .waiting()
        .await
        .map_err(|e| anyhow::anyhow!("MCP server error: {e}"))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Converts a memory search result to a capsule memory entry.
fn memory_entry_from(m: &mem_search::MemoryResult) -> crate::capsule::MemoryEntry {
    crate::capsule::MemoryEntry {
        id: m.observation.id,
        content: m.observation.content.clone(),
        kind: m.observation.kind.clone(),
        session_id: m.observation.session_id.clone(),
        created_at: m.observation.created_at,
        memory_score: m.memory_score,
        is_stale: m.observation.is_stale,
    }
}

/// Serializes a value to pretty JSON and wraps it in a `CallToolResult`.
fn to_json_result<T: Serialize>(value: &T) -> Result<CallToolResult, rmcp::ErrorData> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| rmcp::ErrorData::internal_error(format!("serialization failed: {e}"), None))?;
    Ok(CallToolResult::success(vec![Content::text(json)]))
}

/// BFS traversal collecting nodes in a given direction from a start node.
fn bfs_collect_nodes(
    conn: &rusqlite::Connection,
    graph: &SymbolGraph,
    start: petgraph::graph::NodeIndex,
    direction: Direction,
    max_depth: usize,
    direction_label: &str,
    nodes: &mut Vec<ImpactNode>,
) {
    let mut queue: VecDeque<(petgraph::graph::NodeIndex, usize)> = VecDeque::new();
    queue.push_back((start, 0));
    let mut visited = HashSet::new();
    visited.insert(start);

    while let Some((node, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        for neighbor in graph.graph.neighbors_directed(node, direction) {
            if visited.insert(neighbor)
                && let Some(&id) = graph.node_to_id.get(&neighbor)
                && let Ok((node_fqn, kind, file_path)) = conn.query_row(
                    "SELECT s.fqn, s.kind, f.path FROM symbols s \
                     JOIN files f ON s.file_id = f.id WHERE s.id = ?1",
                    [id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
            {
                nodes.push(ImpactNode {
                    fqn: node_fqn,
                    kind,
                    file_path,
                    depth: depth + 1,
                    direction: direction_label.to_owned(),
                });
                queue.push_back((neighbor, depth + 1));
            }
        }
    }
}
