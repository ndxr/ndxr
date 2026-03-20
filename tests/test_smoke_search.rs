//! Smoke tests: search, graph, and scoring edge cases.

mod helpers;

use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Search edge-case tests
// ---------------------------------------------------------------------------

#[test]
fn search_unicode_query_does_not_crash() {
    let tmp = TempDir::new().unwrap();
    helpers::create_search_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    let unicode_queries = [
        "cafe\u{0301} authentication \u{8BA4}\u{8BC1} \u{1F512}",
        "\u{1F680}\u{1F30D}\u{1F4A5}",
        "\u{00E9}\u{00E8}\u{00EA}\u{00EB}",
        "\u{4F60}\u{597D}\u{4E16}\u{754C}",
        "\u{0410}\u{0411}\u{0412}\u{0413}",
    ];
    for query in &unicode_queries {
        let result = ndxr::graph::search::hybrid_search(&conn, &graph, query, 5, None);
        assert!(
            result.is_ok(),
            "search should not crash for Unicode query: {query}"
        );
    }
}

#[test]
fn search_very_long_query() {
    let tmp = TempDir::new().unwrap();
    helpers::create_search_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    let long_query = "authentication ".repeat(667); // ~10,005 characters
    assert!(
        long_query.len() >= 10_000,
        "query should be at least 10,000 chars"
    );

    let result = ndxr::graph::search::hybrid_search(&conn, &graph, &long_query, 5, None);
    assert!(
        result.is_ok(),
        "search should not crash for 10,000-char query"
    );
    // The query is valid (contains real terms), so it may return results.
    let results = result.unwrap();
    for r in &results {
        assert!(r.score >= 0.0, "scores should be non-negative");
    }
}

#[test]
fn search_whitespace_only_returns_empty() {
    let tmp = TempDir::new().unwrap();
    helpers::create_search_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    let results = ndxr::graph::search::hybrid_search(&conn, &graph, "   \t\n  ", 10, None).unwrap();
    assert!(
        results.is_empty(),
        "whitespace-only query should return empty, got {} results",
        results.len()
    );
}

#[test]
fn search_newlines_and_tabs_in_query() {
    let tmp = TempDir::new().unwrap();
    helpers::create_search_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    let result =
        ndxr::graph::search::hybrid_search(&conn, &graph, "auth\nvalidate\ttoken", 10, None);
    assert!(
        result.is_ok(),
        "search should not crash with newlines/tabs in query"
    );
    // The tokenizer should extract real terms ("auth", "validate", "token")
    // and find matching symbols.
    let results = result.unwrap();
    assert!(
        !results.is_empty(),
        "query with embedded newlines/tabs should still find results"
    );
    assert!(
        results.iter().any(|r| r.name.contains("auth")
            || r.name.contains("Auth")
            || r.name.contains("validate")
            || r.name.contains("Validate")
            || r.name.contains("token")
            || r.name.contains("Token")),
        "should find auth/validate/token-related symbols"
    );
}

#[test]
fn search_max_results_zero_returns_empty() {
    let tmp = TempDir::new().unwrap();
    helpers::create_search_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "authentication", 0, None).unwrap();
    assert!(
        results.is_empty(),
        "max_results=0 should return empty, got {} results",
        results.len()
    );
}

#[test]
fn search_max_results_exceeds_candidates() {
    let tmp = TempDir::new().unwrap();
    helpers::create_search_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    let normal_results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "auth", 10, None).unwrap();
    let large_results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "auth", 1000, None).unwrap();

    // Should not crash and should return the same results (we have fewer than 1000 symbols).
    assert_eq!(
        normal_results.len(),
        large_results.len(),
        "max_results=1000 should return all available, same as max_results=10"
    );
    // Verify the result contents match.
    for (n, l) in normal_results.iter().zip(large_results.iter()) {
        assert_eq!(n.symbol_id, l.symbol_id, "result order should be identical");
    }
}

// ---------------------------------------------------------------------------
// Intent tests
// ---------------------------------------------------------------------------

#[test]
fn search_all_intents_produce_results() {
    let tmp = TempDir::new().unwrap();
    helpers::create_search_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    let intents = [
        (ndxr::graph::intent::Intent::Debug, "debug"),
        (ndxr::graph::intent::Intent::Understand, "understand"),
        (ndxr::graph::intent::Intent::Modify, "modify"),
        (ndxr::graph::intent::Intent::Refactor, "refactor"),
        (ndxr::graph::intent::Intent::Test, "test"),
        (ndxr::graph::intent::Intent::Explore, "explore"),
    ];

    for (intent, expected_name) in &intents {
        let results =
            ndxr::graph::search::hybrid_search(&conn, &graph, "auth", 5, Some(*intent)).unwrap();
        assert!(
            !results.is_empty(),
            "intent {expected_name} should produce results for 'auth'"
        );
        assert_eq!(
            results[0].why.intent, *expected_name,
            "intent name should be '{expected_name}', got '{}'",
            results[0].why.intent
        );
        // All scores should be non-negative.
        for r in &results {
            assert!(
                r.score >= 0.0,
                "score should be non-negative for intent {expected_name}, got {}",
                r.score
            );
        }
    }
}

#[test]
fn search_intent_detection_substring_behavior() {
    // "fixing" contains "fix" -> should trigger Debug intent.
    let intent = ndxr::graph::intent::detect_intent("fixing the auth module");
    assert_eq!(
        intent,
        ndxr::graph::intent::Intent::Debug,
        "'fixing' should trigger Debug via substring match on 'fix'"
    );

    // "refactoring" contains "refactor" -> should trigger Refactor intent.
    let intent = ndxr::graph::intent::detect_intent("refactoring the middleware");
    assert_eq!(
        intent,
        ndxr::graph::intent::Intent::Refactor,
        "'refactoring' should trigger Refactor via substring match on 'refactor'"
    );

    // "testing" contains "test" -> should trigger Test intent.
    let intent = ndxr::graph::intent::detect_intent("testing the validator");
    assert_eq!(
        intent,
        ndxr::graph::intent::Intent::Test,
        "'testing' should trigger Test via substring match on 'test'"
    );
}

// ---------------------------------------------------------------------------
// Relaxation tests
// ---------------------------------------------------------------------------

#[test]
fn search_with_relaxation_for_gibberish() {
    let tmp = TempDir::new().unwrap();
    helpers::create_search_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    // Gibberish query: no direct FTS5 match expected.
    let outcome = ndxr::capsule::relaxation::search_with_relaxation(
        &conn,
        &graph,
        "xyzzy_nonexistent",
        5,
        None,
    )
    .unwrap();

    // search_with_relaxation either returns results via relaxation/fallback,
    // or returns empty if even the FTS5 fallback finds nothing. The key
    // assertion is that it does not crash. Since our terms are truly
    // gibberish, FTS5 fallback will also find nothing.
    // We verify the function completes successfully.
    for r in &outcome.results {
        assert!(r.score >= 0.0, "scores should be non-negative");
    }
}

// ---------------------------------------------------------------------------
// Graph topology tests
// ---------------------------------------------------------------------------

#[test]
fn graph_self_loop_does_not_crash() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.db");
    let conn = ndxr::storage::db::open_or_create(&db_path).unwrap();

    // Insert a file and a single symbol with a self-loop edge.
    conn.execute_batch(
        "INSERT INTO files (path, language, blake3_hash, line_count, byte_size, indexed_at)
         VALUES ('test.ts', 'typescript', 'abc123', 10, 100, 1000);",
    )
    .unwrap();
    let file_id: i64 = conn.last_insert_rowid();

    conn.execute(
        "INSERT INTO symbols (file_id, name, kind, fqn, start_line, end_line, is_exported)
         VALUES (?1, 'recursive', 'function', 'test::recursive', 1, 5, 1)",
        [file_id],
    )
    .unwrap();
    let sym_id = conn.last_insert_rowid();

    // Self-loop: A -> A.
    conn.execute(
        "INSERT INTO edges (from_id, to_id, kind) VALUES (?1, ?2, 'calls')",
        [sym_id, sym_id],
    )
    .unwrap();

    let graph = ndxr::graph::builder::build_graph(&conn).unwrap();

    // Graph should have 1 node and 1 edge (the self-loop).
    assert_eq!(graph.graph.node_count(), 1);
    assert_eq!(graph.graph.edge_count(), 1);

    // Centrality computation must not crash on self-loops.
    let result = ndxr::graph::centrality::compute_and_store(&conn, &graph);
    assert!(result.is_ok(), "centrality should handle self-loops");

    // Verify centrality was written and is in [0, 1].
    let centrality: f64 = conn
        .query_row(
            "SELECT centrality FROM symbols WHERE id = ?1",
            [sym_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        (0.0..=1.0).contains(&centrality),
        "centrality should be in [0, 1], got {centrality}"
    );
}

#[test]
fn graph_cycle_does_not_infinite_loop() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.db");
    let conn = ndxr::storage::db::open_or_create(&db_path).unwrap();

    // Insert a file and three symbols forming a cycle: A -> B -> C -> A.
    conn.execute_batch(
        "INSERT INTO files (path, language, blake3_hash, line_count, byte_size, indexed_at)
         VALUES ('test.ts', 'typescript', 'abc123', 30, 300, 1000);",
    )
    .unwrap();
    let file_id: i64 = conn.last_insert_rowid();

    conn.execute(
        "INSERT INTO symbols (file_id, name, kind, fqn, start_line, end_line, is_exported)
         VALUES (?1, 'alpha', 'function', 'test::alpha', 1, 10, 1)",
        [file_id],
    )
    .unwrap();
    let sym_a = conn.last_insert_rowid();

    conn.execute(
        "INSERT INTO symbols (file_id, name, kind, fqn, start_line, end_line, is_exported)
         VALUES (?1, 'beta', 'function', 'test::beta', 11, 20, 1)",
        [file_id],
    )
    .unwrap();
    let sym_b = conn.last_insert_rowid();

    conn.execute(
        "INSERT INTO symbols (file_id, name, kind, fqn, start_line, end_line, is_exported)
         VALUES (?1, 'gamma', 'function', 'test::gamma', 21, 30, 1)",
        [file_id],
    )
    .unwrap();
    let sym_c = conn.last_insert_rowid();

    // Cycle: A -> B -> C -> A.
    conn.execute(
        "INSERT INTO edges (from_id, to_id, kind) VALUES (?1, ?2, 'calls')",
        [sym_a, sym_b],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO edges (from_id, to_id, kind) VALUES (?1, ?2, 'calls')",
        [sym_b, sym_c],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO edges (from_id, to_id, kind) VALUES (?1, ?2, 'calls')",
        [sym_c, sym_a],
    )
    .unwrap();

    let graph = ndxr::graph::builder::build_graph(&conn).unwrap();
    assert_eq!(graph.graph.node_count(), 3);
    assert_eq!(graph.graph.edge_count(), 3);

    // Centrality computation must terminate and not infinite-loop.
    let result = ndxr::graph::centrality::compute_and_store(&conn, &graph);
    assert!(
        result.is_ok(),
        "centrality should handle cycles without infinite loop"
    );

    // All centrality values should be in [0, 1].
    let mut stmt = conn.prepare("SELECT centrality FROM symbols").unwrap();
    let centralities: Vec<f64> = stmt
        .query_map([], |row| row.get::<_, f64>(0))
        .unwrap()
        .filter_map(Result::ok)
        .collect();

    assert_eq!(centralities.len(), 3);
    for c in &centralities {
        assert!(
            (0.0..=1.0).contains(c),
            "centrality should be in [0, 1], got {c}"
        );
    }
    // In a symmetric cycle, all nodes should have equal centrality (all 1.0 after normalization).
    let max_c = centralities
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    assert!(
        (max_c - 1.0).abs() < f64::EPSILON,
        "max centrality in cycle should be 1.0, got {max_c}"
    );
}

#[test]
fn graph_disconnected_components() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.db");
    let conn = ndxr::storage::db::open_or_create(&db_path).unwrap();

    // Two disconnected components: {A -> B} and {C -> D}.
    conn.execute_batch(
        "INSERT INTO files (path, language, blake3_hash, line_count, byte_size, indexed_at)
         VALUES ('test.ts', 'typescript', 'abc123', 40, 400, 1000);",
    )
    .unwrap();
    let file_id: i64 = conn.last_insert_rowid();

    let mut sym_ids = Vec::new();
    for (name, fqn, start, end) in [
        ("alpha", "comp1::alpha", 1, 10),
        ("beta", "comp1::beta", 11, 20),
        ("gamma", "comp2::gamma", 21, 30),
        ("delta", "comp2::delta", 31, 40),
    ] {
        conn.execute(
            "INSERT INTO symbols (file_id, name, kind, fqn, start_line, end_line, is_exported)
             VALUES (?1, ?2, 'function', ?3, ?4, ?5, 1)",
            rusqlite::params![file_id, name, fqn, start, end],
        )
        .unwrap();
        sym_ids.push(conn.last_insert_rowid());
    }

    // Component 1: A -> B.
    conn.execute(
        "INSERT INTO edges (from_id, to_id, kind) VALUES (?1, ?2, 'calls')",
        [sym_ids[0], sym_ids[1]],
    )
    .unwrap();
    // Component 2: C -> D.
    conn.execute(
        "INSERT INTO edges (from_id, to_id, kind) VALUES (?1, ?2, 'calls')",
        [sym_ids[2], sym_ids[3]],
    )
    .unwrap();

    let graph = ndxr::graph::builder::build_graph(&conn).unwrap();
    assert_eq!(graph.graph.node_count(), 4);
    assert_eq!(graph.graph.edge_count(), 2);

    let result = ndxr::graph::centrality::compute_and_store(&conn, &graph);
    assert!(
        result.is_ok(),
        "centrality should handle disconnected components"
    );

    // All four symbols should have centrality values in [0, 1].
    let mut stmt = conn
        .prepare("SELECT id, centrality FROM symbols ORDER BY id")
        .unwrap();
    let centralities: Vec<(i64, f64)> = stmt
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?)))
        .unwrap()
        .filter_map(Result::ok)
        .collect();

    assert_eq!(centralities.len(), 4);
    for (id, c) in &centralities {
        assert!(
            (0.0..=1.0).contains(c),
            "centrality for symbol {id} should be in [0, 1], got {c}"
        );
    }

    // At least one symbol should have centrality > 0 (the target of an edge).
    assert!(
        centralities.iter().any(|(_, c)| *c > 0.0),
        "at least one symbol should have nonzero centrality"
    );
    // The targets (B and D) should have higher centrality than the sources (A and C).
    let beta_centrality = centralities
        .iter()
        .find(|(id, _)| *id == sym_ids[1])
        .map(|(_, c)| *c)
        .unwrap();
    let alpha_centrality = centralities
        .iter()
        .find(|(id, _)| *id == sym_ids[0])
        .map(|(_, c)| *c)
        .unwrap();
    assert!(
        beta_centrality >= alpha_centrality,
        "beta (target) centrality {beta_centrality} should be >= alpha (source) {alpha_centrality}"
    );
}

// ---------------------------------------------------------------------------
// Scoring normalization tests
// ---------------------------------------------------------------------------

#[test]
fn scoring_normalize_identical_values() {
    let values = vec![5.0, 5.0, 5.0, 5.0, 5.0];
    let normalized = ndxr::graph::scoring::normalize_min_max(&values);
    assert_eq!(normalized.len(), 5);
    for (i, v) in normalized.iter().enumerate() {
        assert!(
            v.abs() < f64::EPSILON,
            "all identical values should normalize to 0.0, index {i} got {v}"
        );
    }
}

#[test]
fn scoring_normalize_single_value() {
    let normalized = ndxr::graph::scoring::normalize_min_max(&[42.0]);
    assert_eq!(normalized.len(), 1);
    assert!(
        normalized[0].abs() < f64::EPSILON,
        "single value should normalize to 0.0, got {}",
        normalized[0]
    );
}

#[test]
fn scoring_normalize_with_negative_bm25() {
    // BM25 raw scores are negative (more negative = more relevant).
    let raw_bm25 = vec![-10.0, -5.0, -1.0, 0.0, 3.0];
    let normalized = ndxr::graph::scoring::normalize_min_max(&raw_bm25);

    assert_eq!(normalized.len(), 5);
    for (i, v) in normalized.iter().enumerate() {
        assert!(
            (0.0..=1.0).contains(v),
            "normalized value at index {i} should be in [0, 1], got {v}"
        );
    }
    // The minimum input (-10.0) should map to 0.0.
    assert!(
        normalized[0].abs() < f64::EPSILON,
        "minimum value should normalize to 0.0, got {}",
        normalized[0]
    );
    // The maximum input (3.0) should map to 1.0.
    assert!(
        (normalized[4] - 1.0).abs() < f64::EPSILON,
        "maximum value should normalize to 1.0, got {}",
        normalized[4]
    );
    // Values should be monotonically increasing for monotonically increasing input.
    for i in 1..normalized.len() {
        assert!(
            normalized[i] >= normalized[i - 1],
            "normalized values should be monotonically increasing: index {} ({}) < index {} ({})",
            i,
            normalized[i],
            i - 1,
            normalized[i - 1]
        );
    }
}
