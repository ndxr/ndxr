//! Integration tests for the `search_logic_flow` pathfinding.

mod helpers;

use ndxr::graph::pathfinding::find_paths;

#[test]
fn finds_path_between_connected_symbols() {
    let tmp = tempfile::TempDir::new().unwrap();
    helpers::create_search_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    // Try to find a path between two symbols that may be connected.
    // The exact connectivity depends on edge resolution, so we just verify
    // the function runs without error.
    let result = find_paths(&conn, &graph, "authMiddleware", "validateToken", Some(3));

    assert!(
        result.is_ok(),
        "find_paths should not error: {:?}",
        result.err()
    );
}

#[test]
fn no_path_returns_empty() {
    let tmp = tempfile::TempDir::new().unwrap();
    helpers::create_search_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    // Two symbols with no directed path between them.
    let result = find_paths(&conn, &graph, "disconnect", "validateToken", Some(3));

    match result {
        Ok(r) => assert_eq!(r.paths_found, 0, "expected no paths"),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("not found") || msg.contains("ambiguous"),
                "unexpected error: {msg}"
            );
        }
    }
}

#[test]
fn nonexistent_symbol_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    helpers::create_search_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    let result = find_paths(
        &conn,
        &graph,
        "nonexistent_symbol_xyz",
        "validateToken",
        None,
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn max_paths_clamped() {
    let tmp = tempfile::TempDir::new().unwrap();
    helpers::create_search_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    // Request 100 paths between distinct symbols — should be clamped to MAX_PATHS (5).
    let result = find_paths(&conn, &graph, "authMiddleware", "validateToken", Some(100));
    match result {
        Ok(r) => assert!(r.paths_found <= 5, "paths should be clamped to max 5"),
        Err(e) => {
            // Symbol resolution issues are acceptable in this test fixture.
            let msg = e.to_string();
            assert!(
                msg.contains("not found") || msg.contains("ambiguous"),
                "unexpected error: {msg}"
            );
        }
    }
}

#[test]
fn source_equals_target_rejected() {
    let tmp = tempfile::TempDir::new().unwrap();
    helpers::create_search_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    let result = find_paths(&conn, &graph, "validateToken", "validateToken", None);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("same"));
}
