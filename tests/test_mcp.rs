//! Integration tests for the MCP server engine.
//!
//! Tests the core engine logic that the MCP tools delegate to, since testing
//! the rmcp protocol layer directly requires full protocol setup. Each test
//! sets up a temporary workspace, runs the indexer, and exercises the engine
//! functions.

use std::sync::Arc;

use tempfile::TempDir;
use tokio::sync::{Mutex, RwLock};

use ndxr::capsule::builder::{self, CapsuleRequest};
use ndxr::capsule::relaxation;
use ndxr::config::{NdxrConfig, TokenEstimator};
use ndxr::graph::builder as graph_builder;
use ndxr::graph::centrality;
use ndxr::graph::intent::Intent;
use ndxr::indexer;
use ndxr::mcp::server::{CoreEngine, NdxrServer};
use ndxr::memory::{changes, search as mem_search, staleness, store};
use ndxr::skeleton::reducer;
use ndxr::storage::db;

/// Creates a temp workspace with a sample TypeScript file and indexes it.
fn setup_workspace() -> (TempDir, NdxrConfig, rusqlite::Connection) {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().to_path_buf();

    // Create .git directory so workspace detection works.
    std::fs::create_dir_all(workspace.join(".git")).unwrap();

    // Create a sample source file.
    let src_dir = workspace.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    std::fs::write(
        src_dir.join("auth.ts"),
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
        src_dir.join("utils.ts"),
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

    // Run the indexer (which now also builds graph + PageRank).
    let stats = indexer::index(&config, None).unwrap();
    assert!(stats.files_indexed > 0, "should index at least one file");
    assert!(
        stats.symbols_extracted > 0,
        "should extract at least one symbol"
    );

    let conn = db::open_or_create(&config.db_path).unwrap();

    (tmp, config, conn)
}

#[test]
fn index_status_returns_correct_counts() {
    let (_tmp, _config, conn) = setup_workspace();

    let file_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
        .unwrap();
    let symbol_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
        .unwrap();
    let edge_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
        .unwrap();

    assert!(
        file_count >= 2,
        "should have at least 2 files, got {file_count}"
    );
    assert!(
        symbol_count >= 4,
        "should have at least 4 symbols, got {symbol_count}"
    );
    // The fixture files have no cross-file imports and no function calls
    // that resolve to known symbols in the index, so edge_resolver
    // inserts no edges.
    assert_eq!(
        edge_count, 0,
        "fixture has no resolvable cross-file edges, got {edge_count}"
    );
}

#[test]
fn graph_build_and_pagerank() {
    let (_tmp, _config, conn) = setup_workspace();

    let graph = graph_builder::build_graph(&conn).unwrap();
    assert!(
        graph.graph.node_count() > 0,
        "graph should have nodes after indexing"
    );

    centrality::compute_and_store(&conn, &graph).unwrap();

    // Verify at least one centrality value is set.
    let centralities: Vec<f64> = {
        let mut stmt = conn.prepare("SELECT centrality FROM symbols").unwrap();
        stmt.query_map([], |row| row.get::<_, f64>(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect()
    };

    assert!(
        !centralities.is_empty(),
        "should have centrality values after PageRank"
    );
}

#[test]
fn search_produces_results() {
    let (_tmp, _config, conn) = setup_workspace();

    let graph = graph_builder::build_graph(&conn).unwrap();
    centrality::compute_and_store(&conn, &graph).unwrap();

    let outcome = relaxation::search_with_relaxation(
        &conn,
        &graph,
        "validateToken",
        10,
        Some(Intent::Explore),
        None,
    )
    .unwrap();
    let results = outcome.results;

    assert!(
        !results.is_empty(),
        "search for 'validateToken' should return results"
    );

    let first = &results[0];
    assert!(
        first.fqn.contains("validateToken"),
        "first result should contain validateToken, got: {}",
        first.fqn
    );
}

#[test]
fn capsule_has_pivots() {
    let (_tmp, config, conn) = setup_workspace();

    let graph = graph_builder::build_graph(&conn).unwrap();
    centrality::compute_and_store(&conn, &graph).unwrap();

    let intent = Intent::Explore;
    let outcome = relaxation::search_with_relaxation(
        &conn,
        &graph,
        "AuthService login",
        10,
        Some(intent),
        None,
    )
    .unwrap();
    let results = outcome.results;

    let estimator = TokenEstimator::default();
    let req = CapsuleRequest {
        conn: &conn,
        graph: &graph,
        search_results: &results,
        query: "AuthService login",
        intent: &intent,
        token_budget: 10_000,
        estimator: &estimator,
        workspace_root: &config.workspace_root,
    };

    let (capsule, _memory_budget) = builder::build_capsule(&req).unwrap();

    assert!(
        !capsule.pivots.is_empty(),
        "capsule should have at least one pivot file"
    );
    assert!(
        capsule.stats.tokens_used <= capsule.stats.tokens_budget,
        "tokens used ({}) should not exceed budget ({})",
        capsule.stats.tokens_used,
        capsule.stats.tokens_budget
    );
}

#[test]
fn skeleton_renders_output() {
    let (_tmp, _config, conn) = setup_workspace();

    let skeletons = reducer::render_skeletons(&conn, &["src/auth.ts".to_owned()], true).unwrap();

    assert!(
        !skeletons.is_empty(),
        "should render at least one skeleton for auth.ts"
    );

    let skel = &skeletons[0];
    assert_eq!(skel.path, "src/auth.ts");
    assert!(
        skel.symbol_count > 0,
        "skeleton should have at least one symbol, got {}",
        skel.symbol_count
    );
    assert!(
        !skel.content.is_empty(),
        "skeleton content should not be empty"
    );
}

#[test]
fn save_and_search_observation_roundtrip() {
    let (_tmp, _config, conn) = setup_workspace();

    let session_id = store::create_session(&conn).unwrap();

    let obs = store::NewObservation {
        session_id,
        kind: "decision".to_owned(),
        content: "We decided to use JWT for authentication tokens".to_owned(),
        headline: Some("JWT decision".to_owned()),
        detail_level: 2,
        linked_fqns: vec!["src/auth.ts::validateToken".to_owned()],
    };

    let obs_id = store::save_observation(&conn, &obs).unwrap();
    assert!(obs_id > 0, "observation ID should be positive");

    // Search for the observation.
    let results = mem_search::search_memories(
        &conn,
        &mem_search::MemorySearchQuery {
            query: "JWT authentication",
            pivot_fqns: &[],
            limit: 5,
            include_stale: false,
            recency_half_life_days: 7.0,
            kind: None,
            exclude_auto: false,
        },
    )
    .unwrap();

    assert!(
        !results.is_empty(),
        "should find the saved observation via search"
    );
    assert!(
        results[0].observation.content.contains("JWT"),
        "first result should contain JWT"
    );
}

#[test]
fn impact_hints_for_symbols() {
    let (_tmp, _config, conn) = setup_workspace();

    let graph = graph_builder::build_graph(&conn).unwrap();
    centrality::compute_and_store(&conn, &graph).unwrap();

    let outcome = relaxation::search_with_relaxation(
        &conn,
        &graph,
        "validateToken",
        5,
        Some(Intent::Explore),
        None,
    )
    .unwrap();
    let results = outcome.results;

    let hints = builder::generate_impact_hints(&graph, &results);

    // Impact hints should have one entry per search result that exists in the graph.
    for hint in &hints {
        assert!(
            matches!(
                hint.blast_radius,
                ndxr::capsule::BlastRadius::Low
                    | ndxr::capsule::BlastRadius::Medium
                    | ndxr::capsule::BlastRadius::High
            ),
            "blast_radius should be low/medium/high, got: {}",
            hint.blast_radius
        );
    }
}

#[test]
fn session_creation_and_observation_retrieval() {
    let (_tmp, _config, conn) = setup_workspace();

    let session_id = store::create_session(&conn).unwrap();
    assert!(!session_id.is_empty(), "session ID should not be empty");

    // Save multiple observations.
    for i in 0..3 {
        let obs = store::NewObservation {
            session_id: session_id.clone(),
            kind: "insight".to_owned(),
            content: format!("Insight number {i}"),
            headline: None,
            detail_level: 1,
            linked_fqns: vec![],
        };
        store::save_observation(&conn, &obs).unwrap();
    }

    let observations = store::get_session_observations(&conn, &session_id).unwrap();
    assert_eq!(
        observations.len(),
        3,
        "should have 3 observations, got {}",
        observations.len()
    );

    let sessions = store::get_recent_sessions(&conn, 10, false).unwrap();
    assert!(
        sessions.iter().any(|s| s.id == session_id),
        "created session should appear in recent sessions"
    );
}

#[test]
fn staleness_detection_marks_observations() {
    let (_tmp, _config, conn) = setup_workspace();

    let session_id = store::create_session(&conn).unwrap();

    // Save an observation linked to a specific symbol.
    let obs = store::NewObservation {
        session_id,
        kind: "decision".to_owned(),
        content: "Important decision about auth".to_owned(),
        headline: None,
        detail_level: 2,
        linked_fqns: vec!["src/auth.ts::validateToken".to_owned()],
    };
    let obs_id = store::save_observation(&conn, &obs).unwrap();

    // Simulate that the linked symbol changed.
    let changed = vec![changes::SymbolDiff {
        fqn: "src/auth.ts::validateToken".to_owned(),
        file_path: String::new(),
        kind: changes::ChangeKind::SignatureChanged,
        old_value: None,
        new_value: None,
    }];

    let marked = staleness::detect_staleness(&conn, &changed).unwrap();
    assert_eq!(marked, 1, "should mark 1 observation stale");

    // Verify the observation is marked stale.
    let is_stale: bool = conn
        .query_row(
            "SELECT is_stale FROM observations WHERE id = ?1",
            [obs_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(is_stale, "observation should be marked stale");
}

#[tokio::test]
async fn ndxr_server_can_be_constructed() {
    let (_tmp, config, conn) = setup_workspace();

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

    // Verify the server is Clone (required by rmcp).
    let _cloned = Clone::clone(&server);
}
