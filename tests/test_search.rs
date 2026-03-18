//! Integration tests for the hybrid search pipeline.

use std::fs;

use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helper: create a multi-file TypeScript project for search tests.
// ---------------------------------------------------------------------------

fn create_search_project(tmp: &TempDir) {
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::create_dir_all(tmp.path().join("src")).unwrap();

    fs::write(
        tmp.path().join("src/auth.ts"),
        r"
/** Validates authentication tokens */
export function validateToken(token: string): boolean {
    return token.length > 0;
}

/** Handles authentication errors */
export function handleAuthError(error: Error): void {
    console.error(error);
}

export class AuthService {
    validate(token: string): boolean {
        return validateToken(token);
    }
}
",
    )
    .unwrap();

    fs::write(
        tmp.path().join("src/database.ts"),
        r"
/** Database connection manager */
export class DatabaseConnection {
    connect(url: string): void {}
    query(sql: string): any[] { return []; }
    disconnect(): void {}
}

export function createConnection(url: string): DatabaseConnection {
    return new DatabaseConnection();
}
",
    )
    .unwrap();

    fs::write(
        tmp.path().join("src/middleware.ts"),
        r"
import { validateToken } from './auth';
import { DatabaseConnection } from './database';

export function authMiddleware(req: any): boolean {
    return validateToken(req.token);
}
",
    )
    .unwrap();
}

fn index_and_build_graph(
    tmp: &TempDir,
) -> (
    ndxr::config::NdxrConfig,
    rusqlite::Connection,
    ndxr::graph::builder::SymbolGraph,
) {
    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    ndxr::indexer::index(&config).unwrap();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let graph = ndxr::graph::builder::build_graph(&conn).unwrap();
    ndxr::graph::centrality::compute_and_store(&conn, &graph).unwrap();
    (config, conn, graph)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn search_finds_auth_symbols() {
    let tmp = TempDir::new().unwrap();
    create_search_project(&tmp);
    let (_config, conn, graph) = index_and_build_graph(&tmp);

    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "authentication", 10, None).unwrap();
    assert!(!results.is_empty(), "should find auth-related symbols");
    // At least one result should be auth-related.
    assert!(results.iter().any(|r| r.fqn.contains("auth")
        || r.fqn.contains("Auth")
        || r.name.contains("auth")
        || r.name.contains("Auth")));
}

#[test]
fn search_finds_database_symbols() {
    let tmp = TempDir::new().unwrap();
    create_search_project(&tmp);
    let (_config, conn, graph) = index_and_build_graph(&tmp);

    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "database connection", 10, None).unwrap();
    assert!(!results.is_empty(), "should find database-related symbols");
}

#[test]
fn search_results_have_breakdown() {
    let tmp = TempDir::new().unwrap();
    create_search_project(&tmp);
    let (_config, conn, graph) = index_and_build_graph(&tmp);

    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "validate token", 5, None).unwrap();
    for result in &results {
        assert!(result.score >= 0.0);
        assert!(!result.why.intent.is_empty());
        assert!(!result.why.reason.is_empty());
    }
}

#[test]
fn intent_affects_results() {
    let tmp = TempDir::new().unwrap();
    create_search_project(&tmp);
    let (_config, conn, graph) = index_and_build_graph(&tmp);

    let debug_results = ndxr::graph::search::hybrid_search(
        &conn,
        &graph,
        "auth",
        5,
        Some(ndxr::graph::intent::Intent::Debug),
    )
    .unwrap();
    let understand_results = ndxr::graph::search::hybrid_search(
        &conn,
        &graph,
        "auth",
        5,
        Some(ndxr::graph::intent::Intent::Understand),
    )
    .unwrap();

    // Both should return results.
    assert!(!debug_results.is_empty());
    assert!(!understand_results.is_empty());
    // Intent should be reflected in breakdown.
    assert_eq!(debug_results[0].why.intent, "debug");
    assert_eq!(understand_results[0].why.intent, "understand");
}

#[test]
fn empty_query_returns_empty() {
    let tmp = TempDir::new().unwrap();
    create_search_project(&tmp);
    let (_config, conn, graph) = index_and_build_graph(&tmp);

    let results = ndxr::graph::search::hybrid_search(&conn, &graph, "", 10, None).unwrap();
    assert!(results.is_empty(), "empty query should return no results");
}

#[test]
fn nonexistent_term_returns_empty() {
    let tmp = TempDir::new().unwrap();
    create_search_project(&tmp);
    let (_config, conn, graph) = index_and_build_graph(&tmp);

    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "zzzznonexistentterm", 10, None).unwrap();
    assert!(
        results.is_empty(),
        "query for nonexistent term should return no results"
    );
}

#[test]
fn search_with_special_characters_does_not_crash() {
    let tmp = TempDir::new().unwrap();
    create_search_project(&tmp);
    let (_config, conn, graph) = index_and_build_graph(&tmp);

    // These should all return Ok, not crash
    let queries = [
        "foo(bar)",
        "test's value",
        "a && b || c",
        "()",
        r#""; DROP TABLE symbols; --"#,
        "hello \"world\"",
        "",
        "   ",
    ];
    for query in &queries {
        let result = ndxr::graph::search::hybrid_search(&conn, &graph, query, 5, None);
        assert!(result.is_ok(), "search should not crash for query: {query}");
    }
}
