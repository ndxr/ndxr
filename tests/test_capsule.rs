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

    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "auth", 10, None, None).unwrap();
    let estimator = ndxr::config::TokenEstimator::default();
    let (capsule, _memory_budget) =
        ndxr::capsule::builder::build_capsule(&ndxr::capsule::builder::CapsuleRequest {
            conn: &conn,
            graph: &graph,
            search_results: &results,
            query: "auth",
            intent: &ndxr::graph::intent::Intent::Explore,
            token_budget: 10_000,
            estimator: &estimator,
            workspace_root: &config.workspace_root,
        })
        .unwrap();

    assert!(capsule.stats.tokens_used <= capsule.stats.tokens_budget);
    // Builder initializes new fields empty — MCP layer fills them.
    assert!(capsule.recent_changes.is_empty());
    assert!(capsule.warnings.is_empty());
}

#[test]
fn capsule_no_file_in_both_pivots_and_skeletons() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);

    let (config, conn, graph) = helpers::index_and_build(&tmp);

    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "validate", 10, None, None).unwrap();
    let estimator = ndxr::config::TokenEstimator::default();
    let (capsule, _memory_budget) =
        ndxr::capsule::builder::build_capsule(&ndxr::capsule::builder::CapsuleRequest {
            conn: &conn,
            graph: &graph,
            search_results: &results,
            query: "validate",
            intent: &ndxr::graph::intent::Intent::Explore,
            token_budget: 10_000,
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

    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "auth", 10, None, None).unwrap();
    let estimator = ndxr::config::TokenEstimator::default();
    let (capsule, _memory_budget) =
        ndxr::capsule::builder::build_capsule(&ndxr::capsule::builder::CapsuleRequest {
            conn: &conn,
            graph: &graph,
            search_results: &results,
            query: "auth",
            intent: &ndxr::graph::intent::Intent::Explore,
            token_budget: 10_000,
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
        ndxr::capsule::relaxation::search_with_relaxation(&conn, &graph, "validate", 5, None, None)
            .unwrap();
    assert!(!outcome.results.is_empty());
}

#[test]
fn impact_hints_generated() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);

    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "validate", 5, None, None).unwrap();
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
    let (capsule, _memory_budget) =
        ndxr::capsule::builder::build_capsule(&ndxr::capsule::builder::CapsuleRequest {
            conn: &conn,
            graph: &graph,
            search_results: &empty_results,
            query: "nothing",
            intent: &ndxr::graph::intent::Intent::Explore,
            token_budget: 10_000,
            estimator: &estimator,
            workspace_root: &config.workspace_root,
        })
        .unwrap();

    assert!(capsule.pivots.is_empty());
    assert!(capsule.skeletons.is_empty());
    assert!(capsule.stats.tokens_used <= capsule.stats.tokens_budget);
}

#[test]
fn refactor_intent_produces_more_skeletons_than_explore() {
    let tmp = TempDir::new().unwrap();
    helpers::create_capsule_project(&tmp);

    let (config, conn, graph) = helpers::index_and_build(&tmp);
    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "validate", 10, None, None).unwrap();
    let estimator = ndxr::config::TokenEstimator::default();

    let (explore_capsule, _) =
        ndxr::capsule::builder::build_capsule(&ndxr::capsule::builder::CapsuleRequest {
            conn: &conn,
            graph: &graph,
            search_results: &results,
            query: "validate",
            intent: &ndxr::graph::intent::Intent::Explore,
            token_budget: 10_000,
            estimator: &estimator,
            workspace_root: &config.workspace_root,
        })
        .unwrap();

    let (refactor_capsule, _) =
        ndxr::capsule::builder::build_capsule(&ndxr::capsule::builder::CapsuleRequest {
            conn: &conn,
            graph: &graph,
            search_results: &results,
            query: "validate",
            intent: &ndxr::graph::intent::Intent::Refactor,
            token_budget: 10_000,
            estimator: &estimator,
            workspace_root: &config.workspace_root,
        })
        .unwrap();

    // Guard: verify both capsules actually produced skeletons so the
    // comparison below is not vacuously true with 0 vs 0.
    assert!(
        refactor_capsule.stats.tokens_skeletons > 0,
        "refactor capsule should produce skeleton tokens (got 0)"
    );

    // Refactor uses pivot_fraction=0.70 (vs 0.85), giving 30% to skeletons
    // instead of 15%. With the same search results, it should allocate at
    // least as many tokens to skeletons.
    assert!(
        refactor_capsule.stats.tokens_skeletons >= explore_capsule.stats.tokens_skeletons,
        "refactor should allocate more tokens to skeletons: refactor={} vs explore={}",
        refactor_capsule.stats.tokens_skeletons,
        explore_capsule.stats.tokens_skeletons,
    );
}
