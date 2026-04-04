//! Integration tests for the hybrid search pipeline.

mod helpers;

use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn search_finds_auth_symbols() {
    let tmp = TempDir::new().unwrap();
    helpers::create_search_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "authentication", 10, None, None)
            .unwrap();
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
    helpers::create_search_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "database connection", 10, None, None)
            .unwrap();
    assert!(!results.is_empty(), "should find database-related symbols");
}

#[test]
fn search_results_have_breakdown() {
    let tmp = TempDir::new().unwrap();
    helpers::create_search_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "validate token", 5, None, None).unwrap();
    for result in &results {
        assert!(result.score >= 0.0);
        assert!(!result.why.intent.is_empty());
        assert!(!result.why.reason.is_empty());
    }
}

#[test]
fn intent_affects_results() {
    let tmp = TempDir::new().unwrap();
    helpers::create_search_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    let debug_results = ndxr::graph::search::hybrid_search(
        &conn,
        &graph,
        "auth",
        5,
        Some(ndxr::graph::intent::Intent::Debug),
        None,
    )
    .unwrap();
    let understand_results = ndxr::graph::search::hybrid_search(
        &conn,
        &graph,
        "auth",
        5,
        Some(ndxr::graph::intent::Intent::Understand),
        None,
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
    helpers::create_search_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    let results = ndxr::graph::search::hybrid_search(&conn, &graph, "", 10, None, None).unwrap();
    assert!(results.is_empty(), "empty query should return no results");
}

#[test]
fn nonexistent_term_returns_empty() {
    let tmp = TempDir::new().unwrap();
    helpers::create_search_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "zzzznonexistentterm", 10, None, None)
            .unwrap();
    assert!(
        results.is_empty(),
        "query for nonexistent term should return no results"
    );
}

#[test]
fn search_with_special_characters_does_not_crash() {
    let tmp = TempDir::new().unwrap();
    helpers::create_search_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

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
        let result = ndxr::graph::search::hybrid_search(&conn, &graph, query, 5, None, None);
        assert!(result.is_ok(), "search should not crash for query: {query}");
    }
}

#[test]
fn partial_query_boosts_full_match() {
    let tmp = TempDir::new().unwrap();
    helpers::create_search_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "auth", 10, None, None).unwrap();
    assert!(
        !results.is_empty(),
        "partial query 'auth' should return results"
    );
    // At least one result should have a non-zero ngram score.
    let has_ngram = results.iter().any(|r| r.why.ngram > 0.0);
    assert!(
        has_ngram,
        "at least one result should have a non-zero ngram score for partial match 'auth'"
    );
}
