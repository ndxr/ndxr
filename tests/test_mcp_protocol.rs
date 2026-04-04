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
    let stats = indexer::index(&config).unwrap();
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
async fn call_save_observation_empty_content() {
    let harness = setup_protocol_test().await;
    // The server accepts empty content (no validation rejects it), so this should
    // succeed. Verify the response is still well-formed JSON.
    let result = call_tool(
        &harness,
        "save_observation",
        serde_json::json!({"kind": "insight", "content": ""}),
    )
    .await;

    match result {
        Err(_) => {} // Protocol error — also acceptable
        Ok(res) => {
            let text = extract_text(&res);
            let parsed: Value = serde_json::from_str(text).unwrap();
            assert!(
                parsed.is_object(),
                "empty content save should return valid JSON, got: {text}"
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
