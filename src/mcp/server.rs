//! MCP server implementation with 9 tools for AI coding agents.
//!
//! All shared state is held behind `Arc<CoreEngine>` so the server struct
//! remains `Clone` as required by rmcp. The `rusqlite::Connection` is protected
//! by `tokio::sync::Mutex` and the `SymbolGraph` by `tokio::sync::RwLock`
//! (read-heavy, written only by the file watcher after re-indexing).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use anyhow::{Context, Result};
use petgraph::Direction;
use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};
use tracing::info;

use crate::capsule::builder::{self, CapsuleRequest};
use crate::capsule::relaxation;
use crate::config::{NdxrConfig, TokenEstimator};
use crate::graph::builder::SymbolGraph;
use crate::graph::intent;
use crate::memory::{capture, compression, search as mem_search, store};
use crate::storage::db::{BATCH_PARAM_LIMIT, build_batch_placeholders};
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

/// Hard upper limit for impact graph traversal depth.
const MAX_IMPACT_DEPTH: usize = 10;

/// Hard upper limit for memory search results.
const MAX_MEMORY_LIMIT: usize = 50;

/// Hard upper limit for session context count.
const MAX_SESSION_COUNT: usize = 20;

/// Maximum observation content length (64 KiB).
const MAX_OBSERVATION_CONTENT: usize = 65_536;

/// Accepted observation kinds for `save_observation`.
const VALID_OBSERVATION_KINDS: &[&str] = &["insight", "decision", "error", "manual"];

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
    /// Blast radius classification.
    blast_radius: crate::capsule::BlastRadius,
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
/// async-aware locks so multiple tool calls can be served concurrently.
/// The graph uses `RwLock` because it is read-only during tool calls and
/// only written by the file watcher after re-indexing.
pub struct CoreEngine {
    /// Runtime configuration.
    pub config: NdxrConfig,
    /// Database connection protected by an async mutex.
    pub conn: Mutex<rusqlite::Connection>,
    /// In-memory symbol graph, rebuilt after each index operation.
    pub graph: RwLock<Option<SymbolGraph>>,
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
    /// Token budget for the response (default: 8000, max: 50000).
    max_tokens: Option<usize>,
    /// Override auto-detected intent (debug, test, refactor, modify, understand, explore).
    intent: Option<String>,
}

/// Parameters for the `get_context_capsule` tool.
#[derive(Deserialize, JsonSchema)]
struct GetContextCapsuleParams {
    /// Search query for finding relevant code.
    query: String,
    /// Token budget for the response (default: 8000, max: 50000).
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
    /// Maximum BFS traversal depth (default: 3, max: 10).
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
    /// Maximum number of results (default: 5, max: 50).
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
    /// Number of recent sessions to include (default: 3, max: 20).
    session_count: Option<usize>,
    /// Include compressed sessions (default: true).
    include_compressed: Option<bool>,
}

/// Parameters for the `search_logic_flow` tool.
#[derive(Deserialize, JsonSchema)]
struct SearchLogicFlowParams {
    /// FQN or name of the source symbol.
    from_symbol: String,
    /// FQN or name of the target symbol.
    to_symbol: String,
    /// Maximum number of paths to find (default: 3, max: 5).
    max_paths: Option<usize>,
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

#[tool_router]
impl NdxrServer {
    /// Creates a new `NdxrServer` instance.
    #[must_use]
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
        description = "Run the full ndxr pipeline: search the codebase, build a context capsule with full source for pivots and skeletons for adjacent files, recall relevant memories, and generate impact hints. Optionally pass intent to optimize results (debug, test, refactor, modify, understand, explore). Returns a comprehensive JSON context package."
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
        let graph_guard = self.engine.graph.read().await;

        let graph_ref = graph_guard
            .as_ref()
            .ok_or_else(|| rmcp::ErrorData::internal_error("symbol graph not initialized", None))?;

        let intent = params
            .0
            .intent
            .as_deref()
            .and_then(intent::parse_intent)
            .unwrap_or_else(|| intent::detect_intent(query));

        let mut pipeline = run_capsule_pipeline(&PipelineParams {
            conn: &conn_guard,
            graph: graph_ref,
            query,
            intent,
            budget,
            workspace_root: &self.engine.config.workspace_root,
            recency_half_life_days: self.engine.config.recency_half_life_days,
            session_id: &self.session_id,
        })?;

        pipeline.capsule.impact_hints =
            builder::generate_impact_hints(graph_ref, &pipeline.search_results);

        let record = capture::ToolCallRecord {
            tool_name: "run_pipeline".to_owned(),
            intent: Some(intent.name().to_owned()),
            query: Some(query.to_owned()),
            pivot_fqns: pipeline.pivot_fqns,
            result_summary: format!(
                "{} pivots, {} skeletons, {} memories",
                pipeline.capsule.pivots.len(),
                pipeline.capsule.skeletons.len(),
                pipeline.capsule.memories.len()
            ),
        };
        drop(graph_guard);

        commit_tool_record(&conn_guard, &self.session_id, &record);
        drop(conn_guard);

        to_json_result(&pipeline.capsule)
    }

    /// Search the codebase and build a context capsule (no impact hints).
    #[tool(
        description = "Search the codebase and build a context capsule with pivot files (full source) and skeleton files (signatures only). Includes memory recall. Optionally pass intent to optimize results (debug, test, refactor, modify, understand, explore). Does not generate impact hints. Returns JSON."
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
        let graph_guard = self.engine.graph.read().await;

        let graph_ref = graph_guard
            .as_ref()
            .ok_or_else(|| rmcp::ErrorData::internal_error("symbol graph not initialized", None))?;

        let intent = intent_override.unwrap_or_else(|| intent::detect_intent(query));

        let pipeline = run_capsule_pipeline(&PipelineParams {
            conn: &conn_guard,
            graph: graph_ref,
            query,
            intent,
            budget,
            workspace_root: &self.engine.config.workspace_root,
            recency_half_life_days: self.engine.config.recency_half_life_days,
            session_id: &self.session_id,
        })?;

        let record = capture::ToolCallRecord {
            tool_name: "get_context_capsule".to_owned(),
            intent: Some(intent.name().to_owned()),
            query: Some(query.to_owned()),
            pivot_fqns: pipeline.pivot_fqns,
            result_summary: format!(
                "{} pivots, {} skeletons",
                pipeline.capsule.pivots.len(),
                pipeline.capsule.skeletons.len()
            ),
        };
        drop(graph_guard);

        commit_tool_record(&conn_guard, &self.session_id, &record);
        drop(conn_guard);

        to_json_result(&pipeline.capsule)
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
                tracing::error!("skeleton render failed: {e}");
                rmcp::ErrorData::internal_error("failed to render skeleton", None)
            })?;

        let result: Vec<SkeletonResult> = skeletons
            .into_iter()
            .map(|s| SkeletonResult {
                path: s.path,
                skeleton: s.content,
                symbol_count: s.symbol_count,
                original_lines: s.original_lines,
            })
            .collect();

        let record = capture::ToolCallRecord {
            tool_name: "get_skeleton".to_owned(),
            intent: None,
            query: None,
            pivot_fqns: Vec::new(),
            result_summary: format!("{} files", result.len()),
        };
        commit_tool_record(&conn_guard, &self.session_id, &record);

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
        let max_depth = params
            .0
            .depth
            .unwrap_or(DEFAULT_IMPACT_DEPTH)
            .min(MAX_IMPACT_DEPTH);
        let include_callers = params.0.include_callers.unwrap_or(true);
        let include_callees = params.0.include_callees.unwrap_or(true);

        let conn_guard = self.engine.conn.lock().await;
        let graph_guard = self.engine.graph.read().await;

        let graph_ref = graph_guard
            .as_ref()
            .ok_or_else(|| rmcp::ErrorData::internal_error("symbol graph not initialized", None))?;

        let sym_id: i64 = conn_guard
            .query_row("SELECT id FROM symbols WHERE fqn = ?1", [fqn], |row| {
                row.get(0)
            })
            .map_err(|e| {
                if e == rusqlite::Error::QueryReturnedNoRows {
                    rmcp::ErrorData::invalid_params(format!("symbol not found: {fqn}"), None)
                } else {
                    tracing::error!("database error looking up symbol {fqn}: {e}");
                    rmcp::ErrorData::internal_error("database error during symbol lookup", None)
                }
            })?;

        let start_node = graph_ref.id_to_node.get(&sym_id).ok_or_else(|| {
            rmcp::ErrorData::invalid_params(format!("symbol not in graph: {fqn}"), None)
        })?;

        let mut nodes = Vec::new();

        if include_callers {
            let caller_nodes = bfs_collect_nodes(
                &conn_guard,
                graph_ref,
                *start_node,
                Direction::Incoming,
                max_depth,
                "caller",
            );
            nodes.extend(caller_nodes);
        }

        if include_callees {
            let callee_nodes = bfs_collect_nodes(
                &conn_guard,
                graph_ref,
                *start_node,
                Direction::Outgoing,
                max_depth,
                "callee",
            );
            nodes.extend(callee_nodes);
        }

        let total_callers = nodes.iter().filter(|n| n.direction == "caller").count();
        let callees_count = nodes.iter().filter(|n| n.direction == "callee").count();
        let blast_radius = crate::capsule::BlastRadius::from_caller_count(total_callers);

        let result = ImpactResult {
            symbol_fqn: fqn.clone(),
            callers_count: total_callers,
            callees_count,
            blast_radius,
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
        drop(graph_guard);

        commit_tool_record(&conn_guard, &self.session_id, &record);
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
        let limit = params
            .0
            .limit
            .unwrap_or(DEFAULT_MEMORY_LIMIT)
            .min(MAX_MEMORY_LIMIT);
        let include_stale = params.0.include_stale.unwrap_or(false);

        let conn_guard = self.engine.conn.lock().await;

        let results = mem_search::search_memories(
            &conn_guard,
            query,
            &[],
            limit,
            include_stale,
            self.engine.config.recency_half_life_days,
            params.0.kind.as_deref(),
        )
        .map_err(|e| {
            tracing::error!("memory search failed: {e}");
            rmcp::ErrorData::internal_error("failed to search memory", None)
        })?;

        let output: Vec<MemorySearchResult> = results
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
        if params.0.content.len() > MAX_OBSERVATION_CONTENT {
            return Err(rmcp::ErrorData::invalid_params(
                format!("content exceeds maximum size of {MAX_OBSERVATION_CONTENT} bytes"),
                None,
            ));
        }

        let kind = params.0.kind.as_deref().unwrap_or("manual");

        if !VALID_OBSERVATION_KINDS.contains(&kind) {
            return Err(rmcp::ErrorData::invalid_params(
                format!(
                    "invalid kind: {kind}, expected one of: {}",
                    VALID_OBSERVATION_KINDS.join(", ")
                ),
                None,
            ));
        }

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

        let obs_id = store::save_observation(&conn_guard, &obs).map_err(|e| {
            tracing::error!("observation save failed: {e}");
            rmcp::ErrorData::internal_error("failed to save observation", None)
        })?;

        let _ = store::update_session_active(&conn_guard, &self.session_id);

        drop(conn_guard);

        let result = serde_json::json!({
            "observation_id": obs_id,
            "session_id": self.session_id,
            "kind": kind,
            "status": "saved"
        });

        to_json_result(&result)
    }

    /// Retrieve recent session context and observations.
    #[tool(
        description = "Retrieve recent session history with observations. Shows what the agent has been working on across sessions. Useful for resuming work or reviewing past decisions."
    )]
    async fn get_session_context(
        &self,
        params: Parameters<GetSessionContextParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let count = params
            .0
            .session_count
            .unwrap_or(DEFAULT_SESSION_COUNT)
            .min(MAX_SESSION_COUNT);
        let include_compressed = params.0.include_compressed.unwrap_or(true);

        let conn_guard = self.engine.conn.lock().await;

        let sessions =
            store::get_recent_sessions(&conn_guard, count, include_compressed).map_err(|e| {
                tracing::error!("session query failed: {e}");
                rmcp::ErrorData::internal_error("failed to retrieve session context", None)
            })?;

        let mut result = Vec::new();
        for session in sessions {
            let observations = store::get_session_observations(&conn_guard, &session.id)
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        "failed to load observations for session {}: {e}",
                        session.id
                    );
                    Vec::new()
                })
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
        commit_tool_record(&conn_guard, &self.session_id, &record);

        drop(conn_guard);

        to_json_result(&result)
    }

    /// Show index status: file/symbol/edge counts, memory stats, DB size, index age.
    #[tool(
        description = "Show index status: file count, symbol count, edge count, memory observation count, database size, and index age. No auto-capture is performed."
    )]
    async fn index_status(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        let conn_guard = self.engine.conn.lock().await;

        let status = crate::status::collect_index_status(&conn_guard, &self.engine.config.db_path)
            .map_err(|e| {
                tracing::error!("index status query failed: {e}");
                rmcp::ErrorData::internal_error("failed to collect index status", None)
            })?;

        drop(conn_guard);

        let result = serde_json::json!({
            "files": status.file_count,
            "symbols": status.symbol_count,
            "edges": status.edge_count,
            "observations": status.observation_count,
            "sessions": status.session_count,
            "oldest_index_at": status.oldest_indexed_at,
            "newest_index_at": status.newest_indexed_at,
            "db_size_bytes": status.db_size_bytes,
            "workspace_root": self.engine.config.workspace_root.display().to_string(),
        });

        to_json_result(&result)
    }

    /// Trace execution paths between two symbols through the call graph.
    #[tool(
        description = "Find execution paths between two symbols through the call graph. Returns up to 3 shortest paths ranked by hop count and node centrality. Useful for understanding how data or control flows from one function to another."
    )]
    async fn search_logic_flow(
        &self,
        params: Parameters<SearchLogicFlowParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let from = &params.0.from_symbol;
        let to = &params.0.to_symbol;
        let max_paths = params.0.max_paths;

        let conn_guard = self.engine.conn.lock().await;
        let graph_guard = self.engine.graph.read().await;

        let graph_ref = graph_guard
            .as_ref()
            .ok_or_else(|| rmcp::ErrorData::internal_error("symbol graph not initialized", None))?;

        let result =
            crate::graph::pathfinding::find_paths(&conn_guard, graph_ref, from, to, max_paths)
                .map_err(|e| {
                    let msg = e.to_string();
                    if msg.contains("not found") || msg.contains("ambiguous") {
                        rmcp::ErrorData::invalid_params(msg, None)
                    } else {
                        tracing::error!("logic flow search failed: {e}");
                        rmcp::ErrorData::internal_error("logic flow search failed", None)
                    }
                })?;

        let record = capture::ToolCallRecord {
            tool_name: "search_logic_flow".to_owned(),
            intent: None,
            query: Some(format!("{from} -> {to}")),
            pivot_fqns: vec![from.clone(), to.clone()],
            result_summary: format!("{} paths found", result.paths_found),
        };
        drop(graph_guard);

        commit_tool_record(&conn_guard, &self.session_id, &record);
        drop(conn_guard);

        to_json_result(&result)
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
        graph: RwLock::new(Some(graph_result)),
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

/// Persists a tool call record and updates session activity (best-effort).
fn commit_tool_record(
    conn: &rusqlite::Connection,
    session_id: &str,
    record: &capture::ToolCallRecord,
) {
    let _ = capture::auto_capture(conn, session_id, record);
    let _ = store::update_session_active(conn, session_id);
}

/// Result of the shared capsule pipeline.
struct PipelineResult {
    /// The assembled context capsule.
    capsule: crate::capsule::Capsule,
    /// FQNs of the pivot symbols from search results.
    pivot_fqns: Vec<String>,
    /// The raw search results (needed for impact hint generation).
    search_results: Vec<crate::graph::search::SearchResult>,
}

/// Parameters for [`run_capsule_pipeline`].
struct PipelineParams<'a> {
    conn: &'a rusqlite::Connection,
    graph: &'a crate::graph::builder::SymbolGraph,
    query: &'a str,
    intent: intent::Intent,
    budget: usize,
    workspace_root: &'a std::path::Path,
    recency_half_life_days: f64,
    session_id: &'a str,
}

/// Runs the shared capsule pipeline: search, build capsule, recall memories, populate stats.
///
/// Used by both `run_pipeline` (which adds impact hints) and `get_context_capsule`.
fn run_capsule_pipeline(p: &PipelineParams<'_>) -> Result<PipelineResult, rmcp::ErrorData> {
    let search_start = std::time::Instant::now();
    let outcome = relaxation::search_with_relaxation(
        p.conn,
        p.graph,
        p.query,
        DEFAULT_MAX_RESULTS,
        Some(p.intent),
    )
    .map_err(|e| rmcp::ErrorData::internal_error(format!("search failed: {e}"), None))?;
    let search_time_ms = search_start.elapsed().as_millis();
    let results = outcome.results;
    let relaxation_applied = outcome.relaxation_applied;

    let estimator = TokenEstimator::default();
    let req = CapsuleRequest {
        conn: p.conn,
        graph: p.graph,
        search_results: &results,
        query: p.query,
        intent: &p.intent,
        token_budget: p.budget,
        estimator: &estimator,
        workspace_root: p.workspace_root,
    };
    let (mut capsule, memory_budget) = builder::build_capsule(&req)
        .map_err(|e| rmcp::ErrorData::internal_error(format!("capsule build failed: {e}"), None))?;

    let pivot_fqns: Vec<String> = results.iter().map(|r| r.fqn.clone()).collect();
    let memories = mem_search::search_memories(
        p.conn,
        p.query,
        &pivot_fqns,
        DEFAULT_MEMORY_LIMIT,
        false,
        p.recency_half_life_days,
        None,
    )
    .map_err(|e| rmcp::ErrorData::internal_error(format!("memory search failed: {e}"), None))?;

    let mut tokens_memories = 0;
    for memory in &memories {
        let entry = memory_entry_from(memory);
        let entry_tokens = estimator.estimate(&entry.content);
        if tokens_memories + entry_tokens > memory_budget {
            break;
        }
        tokens_memories += entry_tokens;
        capsule.memories.push(entry);
    }
    capsule.stats.memory_count = capsule.memories.len();
    capsule.stats.tokens_memories = tokens_memories;
    capsule.stats.tokens_used += tokens_memories;
    capsule.stats.search_time_ms = search_time_ms;
    capsule.stats.relaxation_applied = relaxation_applied;

    enrich_recent_changes(p.conn, &mut capsule, &pivot_fqns, p.session_id);
    enrich_warnings(p.conn, &mut capsule, p.session_id);

    Ok(PipelineResult {
        capsule,
        pivot_fqns,
        search_results: results,
    })
}

/// Fills `capsule.recent_changes` with symbol diffs detected since the session started.
fn enrich_recent_changes(
    conn: &rusqlite::Connection,
    capsule: &mut crate::capsule::Capsule,
    pivot_fqns: &[String],
    session_id: &str,
) {
    let session_start = match conn.query_row(
        "SELECT started_at FROM sessions WHERE id = ?1",
        [session_id],
        |row| row.get::<_, i64>(0),
    ) {
        Ok(ts) => ts,
        Err(rusqlite::Error::QueryReturnedNoRows) => 0,
        Err(e) => {
            tracing::warn!("failed to query session start: {e}");
            0
        }
    };

    let recent_changes =
        crate::memory::changes::query_recent_changes(conn, pivot_fqns, session_start, 20)
            .unwrap_or_default();

    let now = crate::util::unix_now();
    capsule.recent_changes = recent_changes
        .into_iter()
        .map(|c| crate::capsule::RecentChange {
            fqn: c.fqn,
            change: c.change_kind,
            old: c.old_value,
            new: c.new_value,
            when: format_relative_time(now, c.detected_at),
        })
        .collect();
}

/// Runs anti-pattern detectors and fills `capsule.warnings`, persisting new warnings
/// as observations to avoid duplicates on subsequent runs.
fn enrich_warnings(
    conn: &rusqlite::Connection,
    capsule: &mut crate::capsule::Capsule,
    session_id: &str,
) {
    let detectors = crate::memory::antipatterns::default_detectors();
    let det_ctx = crate::memory::antipatterns::DetectionContext {
        conn,
        session_id,
        window_secs: crate::memory::antipatterns::DEFAULT_WINDOW_SECS,
    };
    let anti_patterns =
        crate::memory::antipatterns::run_all_detectors(&det_ctx, &detectors).unwrap_or_default();

    for pattern in &anti_patterns {
        // Deduplicate: don't repeat warnings already stored in this session.
        let already_warned: bool = match conn.query_row(
            "SELECT COUNT(*) FROM observations \
             WHERE session_id = ?1 AND kind = 'warning' AND content LIKE ?2",
            rusqlite::params![session_id, format!("[{}]%", pattern.rule_name)],
            |row| row.get::<_, i64>(0),
        ) {
            Ok(n) => n > 0,
            Err(e) => {
                tracing::warn!("failed to check warning deduplication: {e}");
                false
            }
        };

        if !already_warned {
            let obs = crate::memory::store::NewObservation {
                session_id: session_id.to_owned(),
                kind: "warning".to_owned(),
                content: format!("[{}] {}", pattern.rule_name, pattern.summary),
                headline: Some(pattern.summary.clone()),
                detail_level: 2,
                linked_fqns: pattern.involved_fqns.clone(),
            };
            let _ = crate::memory::store::save_observation(conn, &obs);
        }

        capsule.warnings.push(crate::capsule::Warning {
            rule: pattern.rule_name.clone(),
            summary: pattern.summary.clone(),
            severity: pattern.severity.as_str().to_owned(),
        });
    }
}

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

/// Formats a unix timestamp as relative time (e.g., "2m ago", "1h ago").
fn format_relative_time(now: i64, then: i64) -> String {
    let diff = (now - then).max(0);
    if diff < 60 {
        format!("{diff}s ago")
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else {
        format!("{}d ago", diff / 86400)
    }
}

/// Serializes a value to pretty JSON and wraps it in a `CallToolResult`.
fn to_json_result<T: Serialize>(value: &T) -> Result<CallToolResult, rmcp::ErrorData> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| rmcp::ErrorData::internal_error(format!("serialization failed: {e}"), None))?;
    Ok(CallToolResult::success(vec![Content::text(json)]))
}

/// BFS traversal collecting nodes in a given direction from a start node.
///
/// Separates graph traversal from database metadata lookup: first collects
/// all reachable `(NodeIndex, depth)` pairs via BFS with no DB queries, then
/// batch-loads metadata for all discovered symbol IDs in a single query.
fn bfs_collect_nodes(
    conn: &rusqlite::Connection,
    graph: &SymbolGraph,
    start: petgraph::graph::NodeIndex,
    direction: Direction,
    max_depth: usize,
    direction_label: &str,
) -> Vec<ImpactNode> {
    // 1. BFS traversal (graph-only, no DB queries).
    let mut queue: VecDeque<(petgraph::graph::NodeIndex, usize)> = VecDeque::new();
    queue.push_back((start, 0));
    let mut visited = HashSet::new();
    visited.insert(start);
    let mut collected: Vec<(petgraph::graph::NodeIndex, usize)> = Vec::new();

    while let Some((node, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        for neighbor in graph.graph.neighbors_directed(node, direction) {
            if visited.insert(neighbor) {
                collected.push((neighbor, depth + 1));
                queue.push_back((neighbor, depth + 1));
            }
        }
    }

    // 2. Collect symbol IDs from the traversal.
    let id_depth_pairs: Vec<(i64, usize)> = collected
        .iter()
        .filter_map(|(node_idx, depth)| graph.node_to_id.get(node_idx).map(|&id| (id, *depth)))
        .collect();
    let sym_ids: Vec<i64> = id_depth_pairs.iter().map(|(id, _)| *id).collect();
    let id_to_depth: HashMap<i64, usize> = id_depth_pairs.into_iter().collect();

    // 3. Batch-query metadata for all symbol IDs.
    let metadata = batch_load_impact_metadata(conn, &sym_ids).unwrap_or_else(|e| {
        tracing::warn!("batch_load_impact_metadata failed: {e}");
        HashMap::new()
    });

    // 4. Build ImpactNode objects from the HashMap.
    let mut nodes = Vec::with_capacity(sym_ids.len());
    for sym_id in &sym_ids {
        if let Some((fqn, kind, file_path)) = metadata.get(sym_id) {
            let depth = id_to_depth.get(sym_id).copied().unwrap_or(1);
            nodes.push(ImpactNode {
                fqn: fqn.clone(),
                kind: kind.clone(),
                file_path: file_path.clone(),
                depth,
                direction: direction_label.to_owned(),
            });
        }
    }
    nodes
}

/// Batch-loads `(fqn, kind, file_path)` for a set of symbol IDs.
///
/// Chunks IDs into groups of `BATCH_PARAM_LIMIT` to stay within the `SQLite`
/// parameter limit.
fn batch_load_impact_metadata(
    conn: &rusqlite::Connection,
    ids: &[i64],
) -> anyhow::Result<HashMap<i64, (String, String, String)>> {
    let mut result = HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(BATCH_PARAM_LIMIT) {
        let placeholders = build_batch_placeholders(chunk.len());
        let sql = format!(
            "SELECT s.id, s.fqn, s.kind, f.path FROM symbols s \
             JOIN files f ON s.file_id = f.id \
             WHERE s.id IN ({placeholders})"
        );
        let mut stmt = conn
            .prepare(&sql)
            .context("prepare batch_load_impact_metadata")?;
        let params: Vec<&dyn rusqlite::types::ToSql> = chunk
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt
            .query_map(params.as_slice(), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .context("query batch impact metadata")?;
        for row in rows {
            let (id, fqn, kind, path) = row.context("read impact metadata row")?;
            result.insert(id, (fqn, kind, path));
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_relative_time_seconds() {
        assert_eq!(format_relative_time(10000, 9970), "30s ago");
        assert_eq!(format_relative_time(10000, 9941), "59s ago");
    }

    #[test]
    fn format_relative_time_minutes() {
        assert_eq!(format_relative_time(10000, 9940), "1m ago");
        assert_eq!(format_relative_time(10000, 6401), "59m ago");
    }

    #[test]
    fn format_relative_time_hours() {
        assert_eq!(format_relative_time(100_000, 96400), "1h ago");
        assert_eq!(format_relative_time(100_000, 13601), "23h ago");
    }

    #[test]
    fn format_relative_time_days() {
        assert_eq!(format_relative_time(200_000, 113_600), "1d ago");
    }

    #[test]
    fn format_relative_time_negative_clamped() {
        // Clock skew: then > now — should clamp to 0s ago.
        assert_eq!(format_relative_time(1000, 1005), "0s ago");
    }
}
