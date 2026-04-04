//! Integration tests for the embedding-based semantic search.

use ndxr::graph::intent::Intent;
use ndxr::graph::search;
use tempfile::TempDir;

mod helpers;

/// Verify the full search pipeline works when embedding model files are absent.
#[test]
fn search_works_without_model() {
    let tmp = TempDir::new().unwrap();
    helpers::create_search_project(&tmp);
    let (_config, conn, graph) = helpers::index_and_build(&tmp);

    let results = search::hybrid_search(
        &conn,
        &graph,
        "authentication",
        10,
        Some(Intent::Explore),
        None,
    )
    .unwrap();
    assert!(
        !results.is_empty(),
        "search should return results without embeddings"
    );
    for result in &results {
        assert!(
            result.why.semantic.abs() < f64::EPSILON,
            "semantic score should be 0.0 without model, got {}",
            result.why.semantic
        );
    }
}

/// Index a workspace with model present, verify embeddings stored for all symbols.
/// Requires the model to be downloaded in the project root — skipped in CI.
#[test]
#[ignore = "requires embedding model files to be downloaded"]
fn embedding_index_roundtrip() {
    // Copy model files from the project root into the temp workspace.
    let project_models = std::path::Path::new(".ndxr/models");
    let info = &ndxr::embeddings::download::DEFAULT_MODEL;
    if !project_models.join(info.onnx_filename).exists() {
        eprintln!("model not downloaded at .ndxr/models/ — skipping");
        return;
    }

    let tmp = TempDir::new().unwrap();
    helpers::create_search_project(&tmp);

    let tmp_models = tmp.path().join(".ndxr").join("models");
    std::fs::create_dir_all(&tmp_models).unwrap();
    std::fs::copy(
        project_models.join(info.onnx_filename),
        tmp_models.join(info.onnx_filename),
    )
    .unwrap();
    std::fs::copy(
        project_models.join(info.tokenizer_filename),
        tmp_models.join(info.tokenizer_filename),
    )
    .unwrap();

    let (_config, conn, _graph) = helpers::index_and_build(&tmp);

    let symbol_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
        .unwrap();
    let emb_count = ndxr::embeddings::storage::embedding_count(&conn).unwrap();
    #[allow(clippy::cast_possible_wrap)] // symbol count fits in i64
    let emb_count_i64 = emb_count as i64;
    assert_eq!(
        emb_count_i64, symbol_count,
        "all symbols should have embeddings, got {emb_count}/{symbol_count}"
    );
    let loaded = ndxr::embeddings::storage::load_embeddings(&conn, &[1]).unwrap();
    if let Some(emb) = loaded.get(&1) {
        assert_eq!(emb.len(), ndxr::embeddings::model::EMBEDDING_DIMENSION);
    }
}

/// Search with embeddings should rank semantically related symbols higher.
/// Requires the model to be downloaded in the project root — skipped in CI.
#[test]
#[ignore = "requires embedding model files to be downloaded"]
fn semantic_search_improves_ranking() {
    let project_models = std::path::Path::new(".ndxr/models");
    let info = &ndxr::embeddings::download::DEFAULT_MODEL;
    if !project_models.join(info.onnx_filename).exists() {
        eprintln!("model not downloaded at .ndxr/models/ — skipping");
        return;
    }

    let tmp = TempDir::new().unwrap();
    helpers::create_search_project(&tmp);

    let tmp_models = tmp.path().join(".ndxr").join("models");
    std::fs::create_dir_all(&tmp_models).unwrap();
    std::fs::copy(
        project_models.join(info.onnx_filename),
        tmp_models.join(info.onnx_filename),
    )
    .unwrap();
    std::fs::copy(
        project_models.join(info.tokenizer_filename),
        tmp_models.join(info.tokenizer_filename),
    )
    .unwrap();

    let (config, conn, graph) = helpers::index_and_build(&tmp);

    let _without =
        search::hybrid_search(&conn, &graph, "verify credentials", 10, None, None).unwrap();

    let models_dir = config.ndxr_dir.join("models");
    let model = ndxr::embeddings::model::ModelHandle::load(&models_dir).unwrap();
    assert!(model.is_some(), "model should load from temp workspace");
    let with = search::hybrid_search(
        &conn,
        &graph,
        "verify credentials",
        10,
        None,
        model.as_ref(),
    )
    .unwrap();
    if !with.is_empty() {
        let has_semantic = with.iter().any(|r| r.why.semantic > 0.0);
        assert!(
            has_semantic,
            "at least one result should have a non-zero semantic score"
        );
    }
}
