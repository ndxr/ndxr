//! Integration tests for the capsule builder and auto-relaxation.

mod helpers;

use std::collections::HashSet;

use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn capsule_respects_token_budget() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);

    let (config, conn, graph) = helpers::index_and_build(&tmp);

    let results = ndxr::graph::search::hybrid_search(&conn, &graph, "auth", 10, None).unwrap();
    let estimator = ndxr::config::TokenEstimator::default();
    let capsule = ndxr::capsule::builder::build_capsule(&ndxr::capsule::builder::CapsuleRequest {
        conn: &conn,
        graph: &graph,
        search_results: &results,
        query: "auth",
        intent: &ndxr::graph::intent::Intent::Explore,
        token_budget: 8000,
        estimator: &estimator,
        workspace_root: &config.workspace_root,
    })
    .unwrap();

    assert!(capsule.stats.tokens_used <= capsule.stats.tokens_budget);
}

#[test]
fn capsule_no_file_in_both_pivots_and_skeletons() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);

    let (config, conn, graph) = helpers::index_and_build(&tmp);

    let results = ndxr::graph::search::hybrid_search(&conn, &graph, "validate", 10, None).unwrap();
    let estimator = ndxr::config::TokenEstimator::default();
    let capsule = ndxr::capsule::builder::build_capsule(&ndxr::capsule::builder::CapsuleRequest {
        conn: &conn,
        graph: &graph,
        search_results: &results,
        query: "validate",
        intent: &ndxr::graph::intent::Intent::Explore,
        token_budget: 8000,
        estimator: &estimator,
        workspace_root: &config.workspace_root,
    })
    .unwrap();

    let pivot_paths: HashSet<_> = capsule.pivots.iter().map(|p| &p.path).collect();
    for skel in &capsule.skeletons {
        assert!(
            !pivot_paths.contains(&skel.path),
            "File {} in both pivots and skeletons",
            skel.path
        );
    }
}

#[test]
fn capsule_pivots_contain_file_content() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);

    let (config, conn, graph) = helpers::index_and_build(&tmp);

    let results = ndxr::graph::search::hybrid_search(&conn, &graph, "auth", 10, None).unwrap();
    let estimator = ndxr::config::TokenEstimator::default();
    let capsule = ndxr::capsule::builder::build_capsule(&ndxr::capsule::builder::CapsuleRequest {
        conn: &conn,
        graph: &graph,
        search_results: &results,
        query: "auth",
        intent: &ndxr::graph::intent::Intent::Explore,
        token_budget: 8000,
        estimator: &estimator,
        workspace_root: &config.workspace_root,
    })
    .unwrap();

    for pivot in &capsule.pivots {
        assert!(
            !pivot.content.is_empty(),
            "Pivot {} should have content",
            pivot.path
        );
    }
}

#[test]
fn relaxation_returns_results_for_any_query() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);

    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    let outcome =
        ndxr::capsule::relaxation::search_with_relaxation(&conn, &graph, "validate", 5, None)
            .unwrap();
    assert!(!outcome.results.is_empty());
}

#[test]
fn impact_hints_generated() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);

    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    let results = ndxr::graph::search::hybrid_search(&conn, &graph, "validate", 5, None).unwrap();
    let hints = ndxr::capsule::builder::generate_impact_hints(&graph, &results);
    for hint in &hints {
        assert!(
            matches!(
                hint.blast_radius,
                ndxr::capsule::BlastRadius::Low
                    | ndxr::capsule::BlastRadius::Medium
                    | ndxr::capsule::BlastRadius::High
            ),
            "unexpected blast_radius: {}",
            hint.blast_radius
        );
    }
}

#[test]
fn capsule_with_empty_search_results() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);

    let (config, conn, graph) = helpers::index_and_build(&tmp);
    let estimator = ndxr::config::TokenEstimator::default();

    // Build capsule with empty search results -- should not crash
    let empty_results: Vec<ndxr::graph::search::SearchResult> = vec![];
    let capsule = ndxr::capsule::builder::build_capsule(&ndxr::capsule::builder::CapsuleRequest {
        conn: &conn,
        graph: &graph,
        search_results: &empty_results,
        query: "nothing",
        intent: &ndxr::graph::intent::Intent::Explore,
        token_budget: 8000,
        estimator: &estimator,
        workspace_root: &config.workspace_root,
    })
    .unwrap();

    assert!(capsule.pivots.is_empty());
    assert!(capsule.skeletons.is_empty());
    assert!(capsule.stats.tokens_used <= capsule.stats.tokens_budget);
}
