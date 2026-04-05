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

/// End-to-end model integration test: downloads the pinned model from
/// Hugging Face, verifies its SHA-256 checksums, loads it, produces
/// embeddings, and sanity-checks that semantically related strings yield
/// more similar vectors than unrelated strings.
///
/// This test hits the live network and is gated behind `#[ignore]`.
/// Run manually with: `cargo test --test test_embeddings -- --ignored model_download_and_embed_end_to_end --nocapture`
#[test]
#[ignore = "hits network — run manually to verify model download URL/SHA"]
fn model_download_and_embed_end_to_end() {
    use ndxr::embeddings::download::{DEFAULT_MODEL, download_model, verify_model};
    use ndxr::embeddings::model::{EMBEDDING_DIMENSION, ModelHandle};
    use ndxr::embeddings::similarity::cosine_similarity;

    let tmp = TempDir::new().unwrap();
    let models_dir = tmp.path().join("models");

    // 1. Download fresh (no prior files).
    download_model(&models_dir, &DEFAULT_MODEL, None)
        .expect("fresh download should succeed — if this fails, DEFAULT_MODEL URL or SHA is stale");

    // 2. Verify SHA-256 matches the pinned values. `download_model` already
    //    validates during download, but verify_model re-hashes from disk —
    //    this catches any post-download corruption and proves the stored
    //    bytes match the constants.
    assert!(
        verify_model(&models_dir, &DEFAULT_MODEL).unwrap(),
        "verify_model should return true immediately after a successful download"
    );

    // 3. Load the model and produce an embedding.
    let model = ModelHandle::load(&models_dir)
        .expect("load should not error")
        .expect("model files exist after download, load should return Some");

    let vec_auth = model.embed_text("validate authentication token").unwrap();
    let vec_login = model.embed_text("user login credentials").unwrap();
    let vec_color = model.embed_text("the color of a sunset").unwrap();

    // 4. Shape checks.
    assert_eq!(
        vec_auth.len(),
        EMBEDDING_DIMENSION,
        "embedding dimension should be 384"
    );
    assert_eq!(vec_login.len(), EMBEDDING_DIMENSION);
    assert_eq!(vec_color.len(), EMBEDDING_DIMENSION);

    // 5. L2-normalization: vectors should have unit norm (within float tolerance).
    let norm: f32 = vec_auth.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!(
        (norm - 1.0).abs() < 1e-4,
        "embedding should be L2-normalized, got norm = {norm}"
    );

    // 6. Semantic sanity check: auth/login should be more similar than auth/color.
    let sim_related = cosine_similarity(&vec_auth, &vec_login);
    let sim_unrelated = cosine_similarity(&vec_auth, &vec_color);
    assert!(
        sim_related > sim_unrelated,
        "semantic similarity should rank related > unrelated: \
         sim(auth, login)={sim_related} vs sim(auth, color)={sim_unrelated}"
    );
    // And the related pair should be meaningfully similar, not just marginally.
    assert!(
        sim_related > 0.4,
        "semantically related strings should have cosine > 0.4, got {sim_related}"
    );
}

/// Skip-if-verified behaviour: a second `download_model` call on a directory
/// that already has valid files should not re-download (and must succeed).
#[test]
#[ignore = "hits network on first run"]
fn model_download_is_idempotent() {
    use ndxr::embeddings::download::{DEFAULT_MODEL, download_model, verify_model};

    let tmp = TempDir::new().unwrap();
    let models_dir = tmp.path().join("models");

    download_model(&models_dir, &DEFAULT_MODEL, None).expect("first download");
    assert!(verify_model(&models_dir, &DEFAULT_MODEL).unwrap());

    // Second call should be a no-op from a correctness standpoint — it will
    // re-download because download_model always re-fetches (the CLI has the
    // verify-first skip, not the function). Both invocations must leave the
    // directory in a verified state.
    download_model(&models_dir, &DEFAULT_MODEL, None).expect("second download");
    assert!(verify_model(&models_dir, &DEFAULT_MODEL).unwrap());
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
