//! MCP protocol conformance tests.
//!
//! These tests exercise the full rmcp protocol layer by connecting an in-process
//! MCP client to the `NdxrServer` via a duplex byte channel. Every test verifies
//! real JSON-RPC round-trips through the transport — no mocks, no shortcuts.

use std::collections::HashSet;
use std::sync::Arc;

use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::{Mutex, RwLock};

use ndxr::config::NdxrConfig;
use ndxr::graph::{builder as graph_builder, centrality};
use ndxr::indexer;
use ndxr::mcp::server::{CoreEngine, NdxrServer};
use ndxr::memory::store;
use ndxr::storage::db;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Shared test state returned by the setup helper.
struct TestHarness {
    /// Keeps the temp directory alive for the duration of the test.
    _tmp: TempDir,
    /// Client-side running service; `.peer()` gives the MCP client peer.
    client: rmcp::service::RunningService<rmcp::service::RoleClient, ()>,
}

/// Creates a temp workspace, indexes it, spins up an `NdxrServer`, and connects
/// an rmcp client over a duplex byte channel.
async fn setup_protocol_test() -> TestHarness {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().to_path_buf();

    // Minimal workspace: .git dir so detection works.
    std::fs::create_dir_all(workspace.join(".git")).unwrap();
    let src = workspace.join("src");
    std::fs::create_dir_all(&src).unwrap();

    std::fs::write(
        src.join("auth.ts"),
        concat!(
            "\n",
            "/**\n",
            " * Validates an authentication token.\n",
            " */\n",
            "export function validateToken(token: string): boolean {\n",
            "    return token.length > 0;\n",
            "}\n",
            "\n",
            "/**\n",
            " * User authentication service.\n",
            " */\n",
            "export class AuthService {\n",
            "    constructor(private config: AuthConfig) {}\n",
            "\n",
            "    async login(username: string, password: string): Promise<User> {\n",
            "        return { username, token: \"abc\" };\n",
            "    }\n",
            "\n",
            "    async logout(): Promise<void> {\n",
            "        // cleanup\n",
            "    }\n",
            "}\n",
            "\n",
            "export function createAuthService(config: AuthConfig): AuthService {\n",
            "    return new AuthService(config);\n",
            "}\n",
            "\n",
            "interface AuthConfig {\n",
            "    endpoint: string;\n",
            "    timeout: number;\n",
            "}\n",
            "\n",
            "interface User {\n",
            "    username: string;\n",
            "    token: string;\n",
            "}\n",
        ),
    )
    .unwrap();

    std::fs::write(
        src.join("utils.ts"),
        concat!(
            "\n",
            "/**\n",
            " * Formats a date as ISO string.\n",
            " */\n",
            "export function formatDate(date: Date): string {\n",
            "    return date.toISOString();\n",
            "}\n",
            "\n",
            "/**\n",
            " * Parses a query string into key-value pairs.\n",
            " */\n",
            "export function parseQuery(query: string): Record<string, string> {\n",
            "    return {};\n",
            "}\n",
        ),
    )
    .unwrap();

    let config = NdxrConfig::from_workspace(workspace);

    // Index, build graph, compute PageRank.
    let stats = indexer::index(&config, None).unwrap();
    assert!(stats.files_indexed > 0);

    let conn = db::open_or_create(&config.db_path).unwrap();
    let graph = graph_builder::build_graph(&conn).unwrap();
    centrality::compute_and_store(&conn, &graph).unwrap();

    let session_id = store::create_session(&conn).unwrap();

    let engine = Arc::new(CoreEngine {
        config,
        conn: Mutex::new(conn),
        graph: RwLock::new(Some(graph)),
        embeddings_model: None,
    });

    let server = NdxrServer::new(engine, session_id);

    // Two duplex channels cross-connected:
    //   c2s: client writes -> server reads
    //   s2c: server writes -> client reads
    let (c2s_server, c2s_client) = tokio::io::duplex(65536);
    let (s2c_server, s2c_client) = tokio::io::duplex(65536);

    let (c2s_server_read, _c2s_server_write) = tokio::io::split(c2s_server);
    let (_c2s_client_read, c2s_client_write) = tokio::io::split(c2s_client);
    let (_s2c_server_read, s2c_server_write) = tokio::io::split(s2c_server);
    let (s2c_client_read, _s2c_client_write) = tokio::io::split(s2c_client);

    let server_transport = (c2s_server_read, s2c_server_write);
    let client_transport = (s2c_client_read, c2s_client_write);

    // Spawn server in the background.
    tokio::spawn(async move {
        let svc = server.serve(server_transport).await.unwrap();
        svc.waiting().await.unwrap();
    });

    // Connect client.
    let client = ().serve(client_transport).await.unwrap();

    TestHarness { _tmp: tmp, client }
}

/// Calls an MCP tool by name with the given JSON arguments and returns the result.
async fn call_tool(
    harness: &TestHarness,
    name: &str,
    args: Value,
) -> Result<rmcp::model::CallToolResult, rmcp::service::ServiceError> {
    let params = CallToolRequestParams::new(name.to_owned())
        .with_arguments(args.as_object().cloned().unwrap_or_default());
    harness.client.peer().call_tool(params).await
}

/// Extracts the text content from the first content block of a tool result.
fn extract_text(result: &rmcp::model::CallToolResult) -> &str {
    result
        .content
        .first()
        .expect("result should have at least one content block")
        .as_text()
        .expect("first content block should be text")
        .text
        .as_str()
}

// ===========================================================================
// Lifecycle tests
// ===========================================================================

#[tokio::test]
async fn initialize_returns_correct_server_info() {
    let harness = setup_protocol_test().await;
    let info = harness.client.peer().peer_info().unwrap();
    assert_eq!(info.server_info.name, "ndxr");
}

#[tokio::test]
async fn initialize_returns_instructions() {
    let harness = setup_protocol_test().await;
    let info = harness.client.peer().peer_info().unwrap();
    let instructions = info.instructions.as_deref().unwrap();
    assert!(
        instructions.contains("context engine"),
        "instructions should mention context engine, got: {instructions}"
    );
}

#[tokio::test]
async fn ping_responds() {
    let harness = setup_protocol_test().await;
    // The simplest way to verify the server is responsive is to call a lightweight
    // tool and confirm it responds. rmcp handles ping at the transport layer; there
    // is no explicit ping method on the client Peer. Calling index_status serves as
    // a functional ping.
    let result = call_tool(&harness, "index_status", serde_json::json!({}))
        .await
        .unwrap();
    assert!(
        !result.content.is_empty(),
        "server should respond to a tool call (functional ping)"
    );
}

// ===========================================================================
// Tools list tests
// ===========================================================================

#[tokio::test]
async fn tools_list_returns_all_ten_tools() {
    let harness = setup_protocol_test().await;
    let tools = harness.client.peer().list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 10, "expected 10 tools, got {}", tools.len());
}

#[tokio::test]
async fn tools_list_names_match_expected() {
    let harness = setup_protocol_test().await;
    let tools = harness.client.peer().list_all_tools().await.unwrap();

    let expected: HashSet<&str> = [
        "run_pipeline",
        "get_context_capsule",
        "get_skeleton",
        "get_impact_graph",
        "search_memory",
        "save_observation",
        "get_session_context",
        "search_logic_flow",
        "index_status",
        "reindex",
    ]
    .into_iter()
    .collect();

    let actual: HashSet<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(expected, actual, "tool names mismatch");
}

#[tokio::test]
async fn tools_list_schemas_have_descriptions() {
    let harness = setup_protocol_test().await;
    let tools = harness.client.peer().list_all_tools().await.unwrap();
    for tool in &tools {
        assert!(
            tool.description.is_some(),
            "tool '{}' should have a description",
            tool.name
        );
        assert!(
            !tool.description.as_deref().unwrap().is_empty(),
            "tool '{}' description should not be empty",
            tool.name
        );
    }
}

// ===========================================================================
// Happy-path tool calls
// ===========================================================================

#[tokio::test]
async fn call_run_pipeline_returns_json() {
    let harness = setup_protocol_test().await;
    let result = call_tool(
        &harness,
        "run_pipeline",
        serde_json::json!({"task": "find auth code"}),
    )
    .await
    .unwrap();

    let text = extract_text(&result);
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert!(parsed.get("capsule").is_some() || parsed.get("pivots").is_some());
}

#[tokio::test]
async fn call_get_context_capsule_returns_json() {
    let harness = setup_protocol_test().await;
    let result = call_tool(
        &harness,
        "get_context_capsule",
        serde_json::json!({"query": "authentication"}),
    )
    .await
    .unwrap();

    let text = extract_text(&result);
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert!(parsed.get("capsule").is_some() || parsed.get("pivots").is_some());
}

#[tokio::test]
async fn call_get_skeleton_returns_json() {
    let harness = setup_protocol_test().await;
    let result = call_tool(
        &harness,
        "get_skeleton",
        serde_json::json!({"files": ["src/auth.ts"]}),
    )
    .await
    .unwrap();

    let text = extract_text(&result);
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.is_array(),
        "skeleton response should be a JSON array, got: {text}"
    );
    let arr = parsed.as_array().unwrap();
    assert!(!arr.is_empty(), "skeleton array should not be empty");
    assert!(
        arr[0].get("path").is_some(),
        "skeleton entry should have a 'path' field"
    );
}

#[tokio::test]
async fn call_get_impact_graph_returns_json() {
    let harness = setup_protocol_test().await;
    let result = call_tool(
        &harness,
        "get_impact_graph",
        serde_json::json!({"symbol_fqn": "src/auth.ts::validateToken"}),
    )
    .await
    .unwrap();

    let text = extract_text(&result);
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("target").is_some() || parsed.get("nodes").is_some(),
        "impact graph response should have target or nodes, got: {text}"
    );
}

#[tokio::test]
async fn call_search_memory_returns_json() {
    let harness = setup_protocol_test().await;
    let result = call_tool(
        &harness,
        "search_memory",
        serde_json::json!({"query": "test"}),
    )
    .await
    .unwrap();

    let text = extract_text(&result);
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("results").is_some() || parsed.is_array(),
        "memory search should return results structure, got: {text}"
    );
}

#[tokio::test]
async fn call_save_observation_returns_json() {
    let harness = setup_protocol_test().await;
    let result = call_tool(
        &harness,
        "save_observation",
        serde_json::json!({"kind": "decision", "content": "test observation"}),
    )
    .await
    .unwrap();

    let text = extract_text(&result);
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("observation_id").is_some() || parsed.get("id").is_some(),
        "save_observation should return an id, got: {text}"
    );
}

#[tokio::test]
async fn call_get_session_context_returns_json() {
    let harness = setup_protocol_test().await;
    let result = call_tool(&harness, "get_session_context", serde_json::json!({}))
        .await
        .unwrap();

    let text = extract_text(&result);
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.is_array() || parsed.get("sessions").is_some(),
        "session context should return a JSON array or object, got: {text}"
    );
}

#[tokio::test]
async fn call_search_logic_flow_returns_json() {
    let harness = setup_protocol_test().await;
    let result = call_tool(
        &harness,
        "search_logic_flow",
        serde_json::json!({"from_symbol": "validateToken", "to_symbol": "AuthService"}),
    )
    .await
    .unwrap();

    let text = extract_text(&result);
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.is_object(),
        "logic flow should return a JSON object, got: {text}"
    );
}

#[tokio::test]
async fn call_index_status_returns_json() {
    let harness = setup_protocol_test().await;
    let result = call_tool(&harness, "index_status", serde_json::json!({}))
        .await
        .unwrap();

    let text = extract_text(&result);
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("files").is_some() || parsed.get("file_count").is_some(),
        "index_status should report file info, got: {text}"
    );
}

#[tokio::test]
async fn call_reindex_returns_json() {
    let harness = setup_protocol_test().await;
    let result = call_tool(&harness, "reindex", serde_json::json!({}))
        .await
        .unwrap();

    let text = extract_text(&result);
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.is_object(),
        "reindex should return a JSON object, got: {text}"
    );
}

// ===========================================================================
// Error-path tests
// ===========================================================================

#[tokio::test]
async fn call_unknown_tool_returns_error() {
    let harness = setup_protocol_test().await;
    let result = call_tool(&harness, "nonexistent_tool", serde_json::json!({})).await;

    assert!(
        result.is_err(),
        "calling an unknown tool should return an error"
    );
}

#[tokio::test]
async fn call_run_pipeline_missing_required_param() {
    let harness = setup_protocol_test().await;
    // run_pipeline requires "task" — call without it.
    let result = call_tool(&harness, "run_pipeline", serde_json::json!({})).await;

    // Should either be a transport-level error or an error result.
    match result {
        Err(_) => {} // Protocol error — acceptable
        Ok(res) => {
            assert!(
                res.is_error == Some(true),
                "missing required param should produce an error result"
            );
        }
    }
}

#[tokio::test]
async fn call_save_observation_invalid_kind() {
    let harness = setup_protocol_test().await;
    let result = call_tool(
        &harness,
        "save_observation",
        serde_json::json!({"kind": "INVALID_KIND", "content": "test"}),
    )
    .await;

    match result {
        Err(_) => {} // Protocol error — acceptable
        Ok(res) => {
            assert!(
                res.is_error == Some(true),
                "invalid kind should produce an error result"
            );
        }
    }
}

#[tokio::test]
async fn save_observation_accepts_and_persists_headline() {
    let harness = setup_protocol_test().await;
    let _ = call_tool(
        &harness,
        "save_observation",
        serde_json::json!({
            "kind": "insight",
            "content": "a detailed observation body that explains the thing",
            "headline": "terse summary v1",
        }),
    )
    .await
    .unwrap();

    // Pull it back via get_session_context and verify headline is not null.
    let result = call_tool(&harness, "get_session_context", serde_json::json!({}))
        .await
        .unwrap();
    let text = extract_text(&result);
    let parsed: Value = serde_json::from_str(text).unwrap();
    let sessions = parsed.as_array().unwrap();
    let found = sessions.iter().any(|s| {
        s["observations"].as_array().is_some_and(|obs| {
            obs.iter()
                .any(|o| o["headline"].as_str() == Some("terse summary v1"))
        })
    });
    assert!(
        found,
        "expected to find observation with headline 'terse summary v1', got: {text}"
    );
}

#[tokio::test]
async fn save_observation_rejects_empty_content() {
    let harness = setup_protocol_test().await;
    let result = call_tool(
        &harness,
        "save_observation",
        serde_json::json!({
            "kind": "insight",
            "content": "   \t\n  ",
        }),
    )
    .await;

    match result {
        Err(_) => {} // protocol-level error acceptable
        Ok(res) => {
            assert_eq!(
                res.is_error,
                Some(true),
                "empty content should produce an error result"
            );
        }
    }
}

#[tokio::test]
async fn call_get_impact_graph_unknown_symbol() {
    let harness = setup_protocol_test().await;
    let result = call_tool(
        &harness,
        "get_impact_graph",
        serde_json::json!({"symbol_fqn": "nonexistent::symbol::path"}),
    )
    .await;

    match result {
        Err(_) => {} // Protocol error — acceptable
        Ok(res) => {
            assert!(
                res.is_error == Some(true),
                "unknown symbol should produce an error result"
            );
        }
    }
}

// ===========================================================================
// Concurrent + format tests
// ===========================================================================

#[tokio::test]
async fn concurrent_tool_calls_do_not_deadlock() {
    let harness = setup_protocol_test().await;

    let timeout = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let (r1, r2, r3, r4, r5) = tokio::join!(
            call_tool(&harness, "index_status", serde_json::json!({})),
            call_tool(&harness, "index_status", serde_json::json!({})),
            call_tool(&harness, "index_status", serde_json::json!({})),
            call_tool(&harness, "index_status", serde_json::json!({})),
            call_tool(&harness, "index_status", serde_json::json!({})),
        );
        r1.unwrap();
        r2.unwrap();
        r3.unwrap();
        r4.unwrap();
        r5.unwrap();
    });

    Box::pin(timeout)
        .await
        .expect("concurrent tool calls should complete within 15 seconds");
}

#[tokio::test]
async fn tool_responses_are_valid_content_text() {
    let harness = setup_protocol_test().await;
    let result = call_tool(&harness, "index_status", serde_json::json!({}))
        .await
        .unwrap();

    assert!(
        !result.content.is_empty(),
        "response should have at least one content block"
    );

    let first = &result.content[0];
    assert!(
        first.as_text().is_some(),
        "first content block should be of type text"
    );
}

#[tokio::test]
async fn no_extraneous_output_on_transport() {
    let harness = setup_protocol_test().await;
    let result = call_tool(&harness, "index_status", serde_json::json!({}))
        .await
        .unwrap();

    let text = extract_text(&result);
    // The text content must parse as valid JSON — no stray log lines or debug output.
    let parsed: Result<Value, _> = serde_json::from_str(text);
    assert!(
        parsed.is_ok(),
        "tool response text should be valid JSON, got: {text}"
    );
}

#[tokio::test]
async fn run_pipeline_description_mentions_first_for_every_task() {
    let harness = setup_protocol_test().await;
    let tools = harness.client.peer().list_all_tools().await.unwrap();
    let run_pipeline = tools
        .iter()
        .find(|t| t.name.as_ref() == "run_pipeline")
        .expect("run_pipeline tool present");
    let desc = run_pipeline.description.as_deref().unwrap_or("");
    assert!(
        desc.contains("FIRST"),
        "run_pipeline description should direct agents to call it first, got: {desc}"
    );
    assert!(
        desc.contains("intent"),
        "run_pipeline description should mention intent parameter, got: {desc}"
    );
}

#[tokio::test]
async fn get_context_capsule_description_links_to_run_pipeline() {
    let harness = setup_protocol_test().await;
    let tools = harness.client.peer().list_all_tools().await.unwrap();
    let tool = tools
        .iter()
        .find(|t| t.name.as_ref() == "get_context_capsule")
        .expect("get_context_capsule tool present");
    let desc = tool.description.as_deref().unwrap_or("");
    assert!(
        desc.contains("run_pipeline"),
        "get_context_capsule should reference run_pipeline, got: {desc}"
    );
    assert!(
        desc.to_lowercase().contains("follow-up") || desc.to_lowercase().contains("additional"),
        "get_context_capsule should be described as follow-up, got: {desc}"
    );
}

#[tokio::test]
async fn search_memory_description_explains_auto_exclusion() {
    let harness = setup_protocol_test().await;
    let tools = harness.client.peer().list_all_tools().await.unwrap();
    let tool = tools
        .iter()
        .find(|t| t.name.as_ref() == "search_memory")
        .expect("search_memory tool present");
    let desc = tool.description.as_deref().unwrap_or("");
    assert!(
        desc.contains("auto"),
        "search_memory description should mention auto exclusion behavior, got: {desc}"
    );
}

#[tokio::test]
async fn save_observation_description_lists_valid_kinds() {
    let harness = setup_protocol_test().await;
    let tools = harness.client.peer().list_all_tools().await.unwrap();
    let tool = tools
        .iter()
        .find(|t| t.name.as_ref() == "save_observation")
        .expect("save_observation tool present");
    let desc = tool.description.as_deref().unwrap_or("");
    for kind in &["insight", "decision", "error", "manual"] {
        assert!(
            desc.contains(kind),
            "save_observation description should mention kind '{kind}', got: {desc}"
        );
    }
    assert!(
        desc.contains("headline"),
        "save_observation description should mention headline param, got: {desc}"
    );
}

#[tokio::test]
async fn reindex_description_mentions_watcher_and_checkout() {
    let harness = setup_protocol_test().await;
    let tools = harness.client.peer().list_all_tools().await.unwrap();
    let tool = tools
        .iter()
        .find(|t| t.name.as_ref() == "reindex")
        .expect("reindex tool present");
    let desc = tool.description.as_deref().unwrap_or("");
    assert!(
        desc.contains("watcher"),
        "reindex description should mention the file watcher, got: {desc}"
    );
    assert!(
        desc.contains("checkout") || desc.contains("branch"),
        "reindex description should mention checkout/branch use case, got: {desc}"
    );
}

#[tokio::test]
async fn index_status_includes_human_timestamps_and_staleness() {
    let harness = setup_protocol_test().await;
    let result = call_tool(&harness, "index_status", serde_json::json!({}))
        .await
        .unwrap();
    let text = extract_text(&result);
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("newest_index_human").is_some(),
        "index_status should expose newest_index_human, got: {text}"
    );
    assert!(
        parsed.get("oldest_index_human").is_some(),
        "index_status should expose oldest_index_human, got: {text}"
    );
    assert!(
        parsed.get("is_stale").is_some(),
        "index_status should expose is_stale boolean, got: {text}"
    );
    // Index was just created, so stale must be false
    assert_eq!(parsed["is_stale"].as_bool(), Some(false));
}

#[tokio::test]
async fn save_observation_then_search_returns_created_at_human() {
    let harness = setup_protocol_test().await;
    // Save a manual observation
    let _ = call_tool(
        &harness,
        "save_observation",
        serde_json::json!({
            "kind": "insight",
            "content": "needle marker for human timestamp test",
        }),
    )
    .await
    .unwrap();

    // Search for it
    let result = call_tool(
        &harness,
        "search_memory",
        serde_json::json!({"query": "needle marker"}),
    )
    .await
    .unwrap();
    let text = extract_text(&result);
    let parsed: Value = serde_json::from_str(text).unwrap();
    let arr = parsed.as_array().expect("search_memory returns array");
    assert!(!arr.is_empty(), "expected at least one result, got: {text}");
    assert!(
        arr[0].get("created_at_human").is_some(),
        "MemorySearchResult should have created_at_human, got: {text}"
    );
}

#[tokio::test]
async fn get_session_context_includes_human_timestamps() {
    let harness = setup_protocol_test().await;
    // Save an observation so there is at least one
    let _ = call_tool(
        &harness,
        "save_observation",
        serde_json::json!({"kind": "insight", "content": "session ctx test"}),
    )
    .await
    .unwrap();
    let result = call_tool(&harness, "get_session_context", serde_json::json!({}))
        .await
        .unwrap();
    let text = extract_text(&result);
    let parsed: Value = serde_json::from_str(text).unwrap();
    let arr = parsed.as_array().expect("session context returns array");
    assert!(
        !arr.is_empty(),
        "expected at least one session, got: {text}"
    );
    let session = &arr[0];
    assert!(
        session.get("started_at_human").is_some(),
        "SessionDetail should have started_at_human, got: {text}"
    );
    assert!(
        session.get("last_active_human").is_some(),
        "SessionDetail should have last_active_human, got: {text}"
    );
    let obs_arr = session["observations"].as_array().unwrap();
    if !obs_arr.is_empty() {
        assert!(
            obs_arr[0].get("created_at_human").is_some(),
            "ObservationDetail should have created_at_human, got: {text}"
        );
    }
}

#[tokio::test]
async fn get_impact_graph_symbol_fqn_schema_requires_exact_fqn() {
    // Parameter descriptions flow from schemars doc comments into the
    // tool's JSON schema. Verify the symbol_fqn property description
    // contains the new "exact FQN" wording.
    let harness = setup_protocol_test().await;
    let tools = harness.client.peer().list_all_tools().await.unwrap();
    let tool = tools
        .iter()
        .find(|t| t.name.as_ref() == "get_impact_graph")
        .expect("get_impact_graph tool present");

    // `input_schema` is an Arc<JsonObject>; navigate to properties.symbol_fqn.description.
    let schema: &serde_json::Map<String, Value> = tool.input_schema.as_ref();
    let schema_value: Value = Value::Object(schema.clone());
    let prop_desc = schema_value
        .pointer("/properties/symbol_fqn/description")
        .and_then(Value::as_str)
        .expect("symbol_fqn property must have a description in the JSON schema");

    assert!(
        prop_desc.contains("exact FQN"),
        "symbol_fqn description should require an exact FQN, got: {prop_desc}"
    );
    assert!(
        prop_desc.contains("run_pipeline"),
        "symbol_fqn description should direct users to run_pipeline results, got: {prop_desc}"
    );
}

/// Builds an MCP harness whose engine has `graph: None`, so every tool
/// that requires the symbol graph must return the recovery-hint error.
async fn setup_harness_without_graph() -> TestHarness {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().to_path_buf();
    std::fs::create_dir_all(workspace.join(".git")).unwrap();

    let config = NdxrConfig::from_workspace(workspace);
    // Open the DB so tables exist; we intentionally do NOT build the graph.
    let conn = db::open_or_create(&config.db_path).unwrap();
    let session_id = store::create_session(&conn).unwrap();

    let engine = Arc::new(CoreEngine {
        config,
        conn: Mutex::new(conn),
        graph: RwLock::new(None), // the scenario under test
        embeddings_model: None,
    });

    let server = NdxrServer::new(engine, session_id);

    let (c2s_server, c2s_client) = tokio::io::duplex(65536);
    let (s2c_server, s2c_client) = tokio::io::duplex(65536);
    let (c2s_server_read, _c2s_server_write) = tokio::io::split(c2s_server);
    let (_c2s_client_read, c2s_client_write) = tokio::io::split(c2s_client);
    let (_s2c_server_read, s2c_server_write) = tokio::io::split(s2c_server);
    let (s2c_client_read, _s2c_client_write) = tokio::io::split(s2c_client);

    let server_transport = (c2s_server_read, s2c_server_write);
    let client_transport = (s2c_client_read, c2s_client_write);

    tokio::spawn(async move {
        let svc = server.serve(server_transport).await.unwrap();
        svc.waiting().await.unwrap();
    });

    let client = ().serve(client_transport).await.unwrap();
    TestHarness { _tmp: tmp, client }
}

/// Asserts that a tool-call result / error contains the recovery hint.
fn assert_contains_recovery_hint(
    result: Result<rmcp::model::CallToolResult, rmcp::service::ServiceError>,
    tool: &str,
) {
    match result {
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("reindex"),
                "{tool}: expected recovery hint in protocol error, got: {msg}"
            );
        }
        Ok(res) => {
            assert_eq!(
                res.is_error,
                Some(true),
                "{tool}: expected an error result when graph is None"
            );
            let text = extract_text(&res);
            assert!(
                text.contains("reindex"),
                "{tool}: expected recovery hint in error text, got: {text}"
            );
        }
    }
}

#[tokio::test]
async fn run_pipeline_without_graph_returns_recovery_hint() {
    let harness = setup_harness_without_graph().await;
    let result = call_tool(
        &harness,
        "run_pipeline",
        serde_json::json!({"task": "anything"}),
    )
    .await;
    assert_contains_recovery_hint(result, "run_pipeline");
}

#[tokio::test]
async fn get_context_capsule_without_graph_returns_recovery_hint() {
    let harness = setup_harness_without_graph().await;
    let result = call_tool(
        &harness,
        "get_context_capsule",
        serde_json::json!({"query": "anything"}),
    )
    .await;
    assert_contains_recovery_hint(result, "get_context_capsule");
}

#[tokio::test]
async fn get_impact_graph_without_graph_returns_recovery_hint() {
    let harness = setup_harness_without_graph().await;
    let result = call_tool(
        &harness,
        "get_impact_graph",
        serde_json::json!({"symbol_fqn": "src/foo.rs::bar"}),
    )
    .await;
    assert_contains_recovery_hint(result, "get_impact_graph");
}

#[tokio::test]
async fn search_logic_flow_without_graph_returns_recovery_hint() {
    let harness = setup_harness_without_graph().await;
    let result = call_tool(
        &harness,
        "search_logic_flow",
        serde_json::json!({"from_symbol": "a", "to_symbol": "b"}),
    )
    .await;
    assert_contains_recovery_hint(result, "search_logic_flow");
}
