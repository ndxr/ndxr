//! Smoke tests: capsule, skeleton, and token budget edge cases.

mod helpers;

use std::collections::HashSet;

use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helper: build a capsule with given parameters.
// ---------------------------------------------------------------------------

fn build_capsule_with_budget(
    conn: &rusqlite::Connection,
    graph: &ndxr::graph::builder::SymbolGraph,
    results: &[ndxr::graph::search::SearchResult],
    query: &str,
    token_budget: usize,
    workspace_root: &std::path::Path,
) -> ndxr::capsule::Capsule {
    let estimator = ndxr::config::TokenEstimator::default();
    let (capsule, _memory_budget) =
        ndxr::capsule::builder::build_capsule(&ndxr::capsule::builder::CapsuleRequest {
            conn,
            graph,
            search_results: results,
            query,
            intent: &ndxr::graph::intent::Intent::Explore,
            token_budget,
            estimator: &estimator,
            workspace_root,
        })
        .unwrap();
    capsule
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn capsule_tiny_budget_no_overflow() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);
    let (config, conn, graph) = helpers::index_and_build(&tmp);

    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "auth", 10, None, None).unwrap();

    let capsule =
        build_capsule_with_budget(&conn, &graph, &results, "auth", 10, &config.workspace_root);

    assert!(
        capsule.stats.tokens_used <= capsule.stats.tokens_budget,
        "tokens_used ({}) must not exceed tokens_budget ({})",
        capsule.stats.tokens_used,
        capsule.stats.tokens_budget
    );
    assert_eq!(capsule.stats.tokens_budget, 10);
    // With a budget of 10, no file should fit as a pivot (files are larger).
    assert!(
        capsule.pivots.is_empty(),
        "budget=10 is too small to include any file as a pivot"
    );
}

#[test]
fn capsule_budget_one_still_respects_invariant() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);
    let (config, conn, graph) = helpers::index_and_build(&tmp);

    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "validate", 10, None, None).unwrap();

    let capsule = build_capsule_with_budget(
        &conn,
        &graph,
        &results,
        "validate",
        1,
        &config.workspace_root,
    );

    assert!(
        capsule.stats.tokens_used <= 1,
        "tokens_used ({}) must not exceed budget of 1",
        capsule.stats.tokens_used
    );
    assert!(capsule.pivots.is_empty());
    assert!(capsule.skeletons.is_empty());
}

#[test]
fn capsule_large_budget_fits_everything() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);
    let (config, conn, graph) = helpers::index_and_build(&tmp);

    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "auth", 10, None, None).unwrap();
    assert!(!results.is_empty(), "search should return results");

    let capsule = build_capsule_with_budget(
        &conn,
        &graph,
        &results,
        "auth",
        50_000,
        &config.workspace_root,
    );

    assert!(
        capsule.stats.tokens_used <= 50_000,
        "tokens_used ({}) must not exceed MAX_TOKEN_BUDGET",
        capsule.stats.tokens_used
    );
    // With 50k budget all matched files should fit as pivots.
    assert!(
        !capsule.pivots.is_empty(),
        "large budget should include at least one pivot"
    );
    // Verify every search result file appears somewhere in the capsule.
    let pivot_paths: HashSet<&str> = capsule.pivots.iter().map(|p| p.path.as_str()).collect();
    let skeleton_paths: HashSet<&str> = capsule.skeletons.iter().map(|s| s.path.as_str()).collect();
    for result in &results {
        assert!(
            pivot_paths.contains(result.file_path.as_str())
                || skeleton_paths.contains(result.file_path.as_str()),
            "file {} from search results should appear in capsule",
            result.file_path
        );
    }
}

#[test]
fn capsule_single_search_result() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);
    let (config, conn, graph) = helpers::index_and_build(&tmp);

    // Search for "setupRoutes" which should return exactly 1 result.
    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "setupRoutes", 1, None, None).unwrap();
    // Take at most 1 result to guarantee single-result scenario.
    let single = if results.is_empty() {
        vec![]
    } else {
        vec![results[0].clone()]
    };

    assert_eq!(single.len(), 1, "should have exactly 1 search result");

    let capsule = build_capsule_with_budget(
        &conn,
        &graph,
        &single,
        "setupRoutes",
        8000,
        &config.workspace_root,
    );

    assert_eq!(
        capsule.pivots.len(),
        1,
        "single search result should yield exactly 1 pivot file"
    );
    assert!(capsule.stats.tokens_used <= capsule.stats.tokens_budget);
}

#[test]
fn capsule_stats_consistency() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);
    let (config, conn, graph) = helpers::index_and_build(&tmp);

    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "auth", 10, None, None).unwrap();

    let capsule = build_capsule_with_budget(
        &conn,
        &graph,
        &results,
        "auth",
        8000,
        &config.workspace_root,
    );

    // build_capsule sets tokens_memories to 0 (memories are attached later by the MCP layer).
    assert_eq!(
        capsule.stats.tokens_memories, 0,
        "capsule builder does not include memories"
    );
    assert_eq!(
        capsule.stats.tokens_used,
        capsule.stats.tokens_pivots + capsule.stats.tokens_skeletons,
        "tokens_used ({}) must equal tokens_pivots ({}) + tokens_skeletons ({})",
        capsule.stats.tokens_used,
        capsule.stats.tokens_pivots,
        capsule.stats.tokens_skeletons
    );
}

#[test]
fn capsule_no_duplicate_files_across_pivots() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);
    let (config, conn, graph) = helpers::index_and_build(&tmp);

    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "validate", 10, None, None).unwrap();

    let capsule = build_capsule_with_budget(
        &conn,
        &graph,
        &results,
        "validate",
        8000,
        &config.workspace_root,
    );

    let mut seen_paths: HashSet<&str> = HashSet::new();
    for pivot in &capsule.pivots {
        assert!(
            seen_paths.insert(&pivot.path),
            "duplicate pivot file detected: {}",
            pivot.path
        );
    }
}

#[test]
fn capsule_pivot_file_paths_are_relative() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);
    let (config, conn, graph) = helpers::index_and_build(&tmp);

    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "auth", 10, None, None).unwrap();

    let capsule = build_capsule_with_budget(
        &conn,
        &graph,
        &results,
        "auth",
        8000,
        &config.workspace_root,
    );

    for pivot in &capsule.pivots {
        assert!(
            !pivot.path.starts_with('/'),
            "pivot path should be relative, got: {}",
            pivot.path
        );
    }
    for skeleton in &capsule.skeletons {
        assert!(
            !skeleton.path.starts_with('/'),
            "skeleton path should be relative, got: {}",
            skeleton.path
        );
    }
}

#[test]
fn skeleton_nonexistent_file_returns_empty() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);
    let (_config, conn, _graph) = helpers::index_and_build(&tmp);

    let file_paths = vec!["nonexistent.ts".to_string()];
    let skeletons = ndxr::skeleton::reducer::render_skeletons(&conn, &file_paths, false).unwrap();

    assert!(
        skeletons.is_empty(),
        "render_skeletons for a nonexistent file should return empty, got {} entries",
        skeletons.len()
    );
}

#[test]
fn skeleton_empty_file_list() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);
    let (_config, conn, _graph) = helpers::index_and_build(&tmp);

    let file_paths: Vec<String> = vec![];
    let skeletons = ndxr::skeleton::reducer::render_skeletons(&conn, &file_paths, false).unwrap();

    assert!(
        skeletons.is_empty(),
        "render_skeletons for empty file list should return empty"
    );
}

#[test]
fn skeleton_with_docs_contains_docstrings() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);
    let (_config, conn, _graph) = helpers::index_and_build(&tmp);

    let file_paths = vec!["src/auth.ts".to_string()];
    let skeletons = ndxr::skeleton::reducer::render_skeletons(&conn, &file_paths, true).unwrap();

    assert!(!skeletons.is_empty(), "auth.ts should have symbols");

    let combined: String = skeletons.iter().map(|s| s.content.as_str()).collect();
    assert!(
        combined.contains("///"),
        "skeleton with include_docs=true should contain doc comment lines (///)"
    );
}

#[test]
fn skeleton_without_docs_excludes_docstrings() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);
    let (_config, conn, _graph) = helpers::index_and_build(&tmp);

    let file_paths = vec!["src/auth.ts".to_string()];
    let skeletons = ndxr::skeleton::reducer::render_skeletons(&conn, &file_paths, false).unwrap();

    assert!(!skeletons.is_empty(), "auth.ts should have symbols");

    for skel in &skeletons {
        for line in skel.content.lines() {
            assert!(
                !line.trim_start().starts_with("///"),
                "skeleton with include_docs=false should not contain doc lines, \
                 but found '{}' in {}",
                line,
                skel.path
            );
        }
    }
}

#[test]
fn relaxation_empty_query_returns_empty() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    let outcome =
        ndxr::capsule::relaxation::search_with_relaxation(&conn, &graph, "", 10, None, None)
            .unwrap();

    assert!(
        outcome.results.is_empty(),
        "empty query should return no results even with relaxation"
    );
}

#[test]
fn relaxation_special_chars_only_returns_results() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    // All special characters are stripped by build_fts_query, resulting in empty FTS query.
    let outcome =
        ndxr::capsule::relaxation::search_with_relaxation(&conn, &graph, "(){}[]", 10, None, None)
            .unwrap();

    // After stripping special chars the query is empty, so relaxation cannot find anything.
    // The important thing is that it does not crash or panic.
    assert!(
        outcome.results.is_empty(),
        "query with only special chars should return empty after sanitization"
    );
}

#[test]
fn impact_hints_isolated_symbol_is_low() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    // Search for setupRoutes which has few callers/callees in this small project.
    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "setupRoutes", 5, None, None).unwrap();
    assert!(!results.is_empty(), "should find setupRoutes");

    let hints = ndxr::capsule::builder::generate_impact_hints(&graph, &results);
    assert!(!hints.is_empty(), "should produce at least one hint");

    for hint in &hints {
        // In this tiny project, setupRoutes has very few connections.
        // Transitive callers <= 4 maps to "low".
        assert_eq!(
            hint.blast_radius,
            ndxr::capsule::BlastRadius::Low,
            "isolated symbol should have low blast_radius, got: {} (callers={}, callees={})",
            hint.blast_radius,
            hint.callers,
            hint.callees
        );
    }
}

#[test]
fn impact_hints_symbol_not_in_graph() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);
    let (_config, _conn, graph) = helpers::index_and_build(&tmp);

    // Create a fake search result with a symbol_id that does not exist in the graph.
    let fake_results = vec![ndxr::graph::search::SearchResult {
        symbol_id: 999_999,
        fqn: "fake::nonexistent".to_string(),
        name: "nonexistent".to_string(),
        kind: "function".to_string(),
        file_path: "fake.ts".to_string(),
        start_line: 1,
        end_line: 3,
        signature: None,
        is_exported: true,
        score: 1.0,
        why: ndxr::graph::scoring::ScoreBreakdown {
            bm25: 0.5,
            tfidf: 0.5,
            centrality: 0.0,
            ngram: 0.0,
            semantic: 0.0,
            intent_boost: 0.0,
            intent: "explore".to_string(),
            matched_terms: vec![],
            reason: "test".to_string(),
        },
    }];

    let hints = ndxr::capsule::builder::generate_impact_hints(&graph, &fake_results);
    assert!(
        hints.is_empty(),
        "symbol_id not in graph should produce no hints, got {}",
        hints.len()
    );
}

#[test]
fn token_estimator_empty_string_is_zero() {
    let estimator = ndxr::config::TokenEstimator::default();
    assert_eq!(
        estimator.estimate(""),
        0,
        "empty string should estimate to 0 tokens"
    );
}

#[test]
fn token_estimator_consistent() {
    let estimator = ndxr::config::TokenEstimator::default();
    let first = estimator.estimate("hello world");
    let second = estimator.estimate("hello world");
    let third = estimator.estimate("hello world");

    assert_eq!(first, second, "token estimates must be deterministic");
    assert_eq!(second, third, "token estimates must be deterministic");
    assert!(
        first > 0,
        "non-empty string should have positive token count"
    );
}
