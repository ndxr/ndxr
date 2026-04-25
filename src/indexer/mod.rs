//! Code indexing pipeline: file walking, parsing, symbol extraction, and database persistence.
//!
//! Provides [`index`] for incremental indexing and [`reindex`] for full re-indexing
//! of a workspace. The pipeline walks the filesystem, diffs against previously
//! indexed files, parses changed files in parallel via rayon, and writes results
//! to the `SQLite` database in a single transaction.
//!
//! After the main transaction commits, the pipeline builds the symbol dependency
//! graph, computes `PageRank` centrality scores, and detects observation staleness
//! for any symbols whose signatures or bodies changed.

pub mod edge_resolver;
pub mod manifest;
pub mod parser;
pub mod symbols;
pub mod tokenizer;
pub mod walker;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rayon::prelude::*;
use rusqlite::params;
use tracing::info;

use crate::config::NdxrConfig;
use crate::graph;
use crate::memory;
use crate::storage;

/// Statistics returned after an indexing operation.
#[derive(Debug, Default)]
pub struct IndexStats {
    /// Number of files that were parsed and indexed.
    pub files_indexed: usize,
    /// Number of files removed from the index (deleted from disk).
    pub files_deleted: usize,
    /// Number of unchanged files that were skipped.
    pub skipped: usize,
    /// Total symbols extracted across all indexed files.
    pub symbols_extracted: usize,
    /// Total edges extracted across all indexed files.
    pub edges_extracted: usize,
    /// Total time spent indexing in milliseconds.
    pub duration_ms: u128,
    /// Number of observations marked stale due to symbol changes.
    pub observations_marked_stale: usize,
    /// Number of symbols for which embedding vectors were computed.
    pub embeddings_computed: usize,
}

/// Performs incremental indexing of the workspace.
///
/// On first run, indexes all files. On subsequent runs, only processes
/// files that have been added, changed, or deleted since the last index.
///
/// The optional `on_progress` callback is invoked at each pipeline stage
/// boundary with a human-readable message. Pass `None` for silent operation.
///
/// # Pipeline
///
/// 1. Open/create database
/// 2. Walk filesystem for supported source files
/// 3. Compute BLAKE3 hashes in parallel and diff against indexed files
/// 4. Parse changed/new files in parallel (rayon)
/// 5. Snapshot existing symbol signatures/body hashes for staleness detection
/// 6. Write results to database in a single transaction
/// 7. Compute TF-IDF term frequencies
/// 8. Build dependency graph and compute `PageRank` centrality
/// 9. Compute embeddings for new/changed symbols (batch-by-batch)
/// 10. Detect observation staleness for changed symbols
/// 11. Return statistics
///
/// # Errors
///
/// Returns an error if the database cannot be opened, the filesystem walk
/// fails, or the database write fails.
pub fn index(config: &NdxrConfig, on_progress: Option<&dyn Fn(&str)>) -> Result<IndexStats> {
    index_inner(config, false, on_progress)
}

/// Shared implementation for [`index`] and [`reindex`].
///
/// When `skip_changes` is `true`, the change detection pipeline (snapshot,
/// diff, store, staleness) is skipped entirely. This avoids the noise of
/// marking every symbol as "added" after a full `reset_code_tables()`.
fn index_inner(
    config: &NdxrConfig,
    skip_changes: bool,
    on_progress: Option<&dyn Fn(&str)>,
) -> Result<IndexStats> {
    let start = std::time::Instant::now();
    let mut stats = IndexStats::default();

    let emit = |msg: &str| {
        if let Some(cb) = on_progress {
            cb(msg);
        }
    };

    // 1. Open/create DB.
    let conn = storage::db::open_or_create(&config.db_path)?;

    // 2. Walk filesystem.
    emit("Walking filesystem...");
    let files = walker::walk_workspace(&config.workspace_root)?;
    emit(&format!("Walking filesystem... {} files", files.len()));

    // 3. Read and hash all files in parallel, then diff against DB.
    let current_files = read_and_hash_files_parallel(&config.workspace_root, &files);

    let manifest_entries: Vec<(PathBuf, String)> = current_files
        .iter()
        .map(|(path, _, hash)| (path.clone(), hash.clone()))
        .collect();
    let diff = manifest::diff_files(&conn, &manifest_entries)?;

    let (changed_count, deleted_count, unchanged_count) = count_file_statuses(&diff);
    emit(&format!(
        "Hashing files... {} files ({changed_count} changed, {deleted_count} deleted, {unchanged_count} unchanged)",
        files.len()
    ));

    // 4. Collect files to process, retaining their pre-read content.
    let changed_paths: std::collections::HashSet<PathBuf> = diff
        .iter()
        .filter(|(_, status)| {
            matches!(
                status,
                manifest::FileStatus::Added | manifest::FileStatus::Changed { .. }
            )
        })
        .map(|(path, _)| path.clone())
        .collect();

    let to_parse: Vec<parser::PreReadFile> = current_files
        .into_iter()
        .filter(|(path, _, _)| changed_paths.contains(path))
        .map(|(rel_path, source, hash)| parser::PreReadFile {
            abs_path: config.workspace_root.join(&rel_path),
            source,
            blake3_hash: hash,
        })
        .collect();

    let deleted: Vec<PathBuf> = diff
        .iter()
        .filter(|(_, status)| matches!(status, manifest::FileStatus::Deleted))
        .map(|(path, _)| path.clone())
        .collect();

    stats.skipped = unchanged_count;

    // 5. Parse files in parallel using pre-read content.
    emit(&format!("Parsing {} files...", to_parse.len()));
    let results = parser::parse_files_parallel_from_content(&config.workspace_root, to_parse);
    stats.files_indexed = results.len();

    // 5b. Snapshot existing symbol signatures/body hashes before the write
    //     transaction so we can detect what changed. Skipped during reindex
    //     because reset_code_tables() clears all symbols, making every symbol
    //     appear as "added" (useless noise).
    let pre_index_symbols = if skip_changes {
        HashMap::new()
    } else {
        snapshot_pre_index(&conn, &results, &deleted)?
    };

    // 6. Write to DB in a single transaction.
    emit("Writing to database...");
    let fqn_to_id = write_index_results(&conn, &results, &deleted, &diff, &mut stats)?;

    // 7. Post-index: build graph and compute PageRank.
    //    These run AFTER the transaction commits since PageRank reads from DB.
    emit("Building graph...");
    let graph = graph::builder::build_graph(&conn)?;
    emit(&format!(
        "Building graph ({} nodes, {} edges)...",
        graph.graph.node_count(),
        graph.graph.edge_count()
    ));

    emit("Computing PageRank...");
    graph::centrality::compute_and_store(&conn, &graph)?;
    info!(
        nodes = graph.graph.node_count(),
        edges = graph.graph.edge_count(),
        "graph built and PageRank computed"
    );

    // 8. Compute embeddings for indexed symbols (skipped if model absent).
    let models_dir = config.workspace_root.join(".ndxr").join("models");
    let embeddings_computed =
        compute_embeddings(&conn, &results, &fqn_to_id, &models_dir, on_progress)?;
    if embeddings_computed > 0 {
        info!(count = embeddings_computed, "embeddings computed");
    }
    stats.embeddings_computed = embeddings_computed;

    // 9. Detect observation staleness for changed symbols.
    if !skip_changes {
        emit("Detecting staleness...");
        stats.observations_marked_stale =
            detect_and_mark_stale(&conn, &results, &pre_index_symbols)?;
    }

    stats.duration_ms = start.elapsed().as_millis();
    Ok(stats)
}

/// Detects which symbols changed between the pre-index snapshot and the
/// current DB state, stores the diffs, and marks any observations that now
/// reference stale symbol bodies. Returns the number of observations marked.
fn detect_and_mark_stale(
    conn: &rusqlite::Connection,
    results: &[parser::ParseResult],
    pre_index_symbols: &memory::changes::SymbolSnapshot,
) -> Result<usize> {
    let reindexed_paths: Vec<String> = results
        .iter()
        .map(|r| crate::util::normalize_path(&r.path))
        .collect();
    let symbol_diffs =
        memory::changes::detect_symbol_diffs(conn, pre_index_symbols, &reindexed_paths)?;
    if symbol_diffs.is_empty() {
        return Ok(0);
    }
    memory::changes::store_symbol_changes(conn, &symbol_diffs, None)?;
    let marked = memory::staleness::detect_staleness(conn, &symbol_diffs)?;
    if marked > 0 {
        info!(
            marked,
            changed = symbol_diffs.len(),
            "observations marked stale"
        );
    }
    Ok(marked)
}

/// Performs a targeted incremental index on a specific set of changed paths.
///
/// Unlike [`index`], this skips the full workspace walk and only processes
/// the given absolute file paths. Files that no longer exist are treated as
/// deletions. Files that exist are hashed, compared against the manifest, and
/// re-parsed if changed.
///
/// This is used by the file watcher for efficient re-indexing of only the
/// files that were actually modified.
///
/// # Errors
///
/// Returns an error if the database cannot be opened, parsing fails, or the
/// database write fails.
pub fn index_paths(config: &NdxrConfig, changed_paths: &[PathBuf]) -> Result<IndexStats> {
    let start = std::time::Instant::now();
    let mut stats = IndexStats::default();

    if changed_paths.is_empty() {
        return Ok(stats);
    }

    let conn = storage::db::open_or_create(&config.db_path)?;

    // Partition into existing files (with content) and deleted files.
    let mut existing: Vec<(PathBuf, String, String)> = Vec::new();
    let mut deleted_rel: Vec<PathBuf> = Vec::new();

    for abs_path in changed_paths {
        let rel_path = match abs_path.strip_prefix(&config.workspace_root) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue, // outside workspace, skip
        };

        if abs_path.is_file() {
            if let Ok(bytes) = std::fs::read(abs_path) {
                let hash = blake3::hash(&bytes).to_hex().to_string();
                if let Ok(source) = String::from_utf8(bytes) {
                    existing.push((rel_path, source, hash));
                }
            }
        } else {
            // File no longer exists — treat as deletion.
            deleted_rel.push(rel_path);
        }
    }

    // Diff only these files against the manifest.
    let manifest_entries: Vec<(PathBuf, String)> = existing
        .iter()
        .map(|(path, _, hash)| (path.clone(), hash.clone()))
        .collect();
    let diff = manifest::diff_files(&conn, &manifest_entries)?;

    let changed_paths_set: HashSet<PathBuf> = diff
        .iter()
        .filter(|(_, status)| {
            matches!(
                status,
                manifest::FileStatus::Added | manifest::FileStatus::Changed { .. }
            )
        })
        .map(|(path, _)| path.clone())
        .collect();

    let to_parse: Vec<parser::PreReadFile> = existing
        .into_iter()
        .filter(|(path, _, _)| changed_paths_set.contains(path))
        .map(|(rel_path, source, hash)| parser::PreReadFile {
            abs_path: config.workspace_root.join(&rel_path),
            source,
            blake3_hash: hash,
        })
        .collect();

    // diff_files marks every indexed file absent from `current_files` as
    // Deleted.  Since index_paths receives only a targeted subset, those
    // diff-sourced deletions are false positives.  Use only the explicit
    // deletion list (files passed to us that no longer exist on disk).
    let all_deleted: Vec<PathBuf> = deleted_rel;

    stats.skipped = diff
        .iter()
        .filter(|(_, s)| matches!(s, manifest::FileStatus::Unchanged))
        .count();

    let results = parser::parse_files_parallel_from_content(&config.workspace_root, to_parse);
    stats.files_indexed = results.len();

    let fqn_set: std::collections::HashSet<&str> = results
        .iter()
        .flat_map(|r| r.symbols.iter().map(|s| s.fqn.as_str()))
        .collect();
    let fqns: Vec<&str> = fqn_set.into_iter().collect();
    let deleted_norm: Vec<String> = all_deleted
        .iter()
        .map(|p| crate::util::normalize_path(p))
        .collect();
    let pre_index_symbols = memory::changes::snapshot_symbol_state(&conn, &fqns, &deleted_norm)?;

    let _fqn_to_id = write_index_results(&conn, &results, &all_deleted, &diff, &mut stats)?;

    // NOTE: graph + PageRank rebuild is intentionally skipped here.
    // The file watcher (the primary caller of `index_paths`) rebuilds the
    // graph on its own connection and stores it in the shared `CoreEngine`,
    // so building it here would be redundant work that is immediately
    // discarded.

    // Staleness detection.
    let reindexed_paths: Vec<String> = results
        .iter()
        .map(|r| crate::util::normalize_path(&r.path))
        .collect();
    let symbol_diffs =
        memory::changes::detect_symbol_diffs(&conn, &pre_index_symbols, &reindexed_paths)?;
    if !symbol_diffs.is_empty() {
        memory::changes::store_symbol_changes(&conn, &symbol_diffs, None)?;
        let marked = memory::staleness::detect_staleness(&conn, &symbol_diffs)?;
        stats.observations_marked_stale = marked;
    }

    stats.duration_ms = start.elapsed().as_millis();
    Ok(stats)
}

/// Forces a complete re-index by clearing code tables first.
///
/// Preserves session memory (sessions, observations, `observation_links`).
/// Equivalent to dropping the code index and running [`index`] from scratch.
///
/// The optional `on_progress` callback is invoked at each pipeline stage
/// boundary with a human-readable message. Pass `None` for silent operation.
///
/// # Errors
///
/// Returns an error if the database cannot be opened, tables cannot be
/// reset, or the subsequent indexing fails.
pub fn reindex(config: &NdxrConfig, on_progress: Option<&dyn Fn(&str)>) -> Result<IndexStats> {
    let conn = storage::db::open_or_create(&config.db_path)?;
    storage::db::reset_code_tables(&conn)?;
    crate::embeddings::storage::clear_embeddings(&conn)?;
    drop(conn);
    index_inner(config, true, on_progress)
}

/// Reads and hashes every walker-discovered file in parallel, returning the
/// `(relative_path, source, blake3_hash)` tuple for each successful read.
///
/// Unreadable files are logged via `tracing::warn!` and skipped so that a
/// single bad file cannot abort an indexing run.
fn read_and_hash_files_parallel(
    workspace_root: &std::path::Path,
    files: &[PathBuf],
) -> Vec<(PathBuf, String, String)> {
    files
        .par_iter()
        .filter_map(|abs_path| {
            let rel_path = abs_path.strip_prefix(workspace_root).ok()?;
            let bytes = match std::fs::read(abs_path) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!("skipping unreadable file {}: {e}", abs_path.display());
                    return None;
                }
            };
            let hash = blake3::hash(&bytes).to_hex().to_string();
            let source = String::from_utf8(bytes).ok()?;
            Some((rel_path.to_path_buf(), source, hash))
        })
        .collect()
}

/// Single-pass tally of file status counts — returns `(changed, deleted, unchanged)`.
///
/// Avoids iterating `diff` three separate times just to build a progress string.
fn count_file_statuses(diff: &[(PathBuf, manifest::FileStatus)]) -> (usize, usize, usize) {
    diff.iter().fold(
        (0_usize, 0_usize, 0_usize),
        |(c, d, u), (_, status)| match status {
            manifest::FileStatus::Added | manifest::FileStatus::Changed { .. } => (c + 1, d, u),
            manifest::FileStatus::Deleted => (c, d + 1, u),
            manifest::FileStatus::Unchanged => (c, d, u + 1),
        },
    )
}

/// Captures the pre-index symbol state so `detect_symbol_diffs` can diff it
/// after the write transaction commits.
fn snapshot_pre_index(
    conn: &rusqlite::Connection,
    results: &[parser::ParseResult],
    deleted: &[PathBuf],
) -> Result<memory::changes::SymbolSnapshot> {
    let fqn_set: HashSet<&str> = results
        .iter()
        .flat_map(|r| r.symbols.iter().map(|s| s.fqn.as_str()))
        .collect();
    let fqns: Vec<&str> = fqn_set.into_iter().collect();
    let deleted_paths: Vec<String> = deleted
        .iter()
        .map(|p| crate::util::normalize_path(p))
        .collect();
    memory::changes::snapshot_symbol_state(conn, &fqns, &deleted_paths)
}

/// Writes parse results and deletions to the database in a single transaction.
///
/// Returns the combined FQN-to-ID map for all inserted symbols, used by the
/// embedding computation step that runs after the transaction commits.
///
/// Within the transaction:
/// 1. Deletes rows for changed files (CASCADE handles symbols/edges/TF)
/// 2. Deletes rows for removed files
/// 3. Inserts new file rows
/// 4. Inserts symbols and builds FQN-to-ID maps
/// 5. Resolves and inserts edges
/// 6. Computes and inserts TF-IDF term frequencies
/// 7. Recomputes global document frequencies
fn write_index_results(
    conn: &rusqlite::Connection,
    results: &[parser::ParseResult],
    deleted: &[PathBuf],
    diff: &[(PathBuf, manifest::FileStatus)],
    stats: &mut IndexStats,
) -> Result<HashMap<String, i64>> {
    let tx = conn
        .unchecked_transaction()
        .context("begin index transaction")?;

    // Delete changed files (their old data; CASCADE removes symbols/edges/TF).
    for (path, status) in diff {
        if matches!(status, manifest::FileStatus::Changed { .. }) {
            tx.execute(
                "DELETE FROM files WHERE path = ?1",
                [crate::util::normalize_path(path)],
            )
            .with_context(|| format!("delete changed file: {}", path.display()))?;
        }
    }

    // Delete removed files.
    for path in deleted {
        tx.execute(
            "DELETE FROM files WHERE path = ?1",
            [crate::util::normalize_path(path)],
        )
        .with_context(|| format!("delete removed file: {}", path.display()))?;
        stats.files_deleted += 1;
    }

    // Compute the current Unix timestamp once for all inserts.
    let now: i64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before UNIX epoch")?
        .as_secs()
        .try_into()
        .context("Unix timestamp exceeds i64 range")?;

    // Combined FQN->ID map across all files for TF-IDF computation.
    let mut all_fqn_to_id: HashMap<String, i64> = HashMap::new();

    // Insert new/changed files and their symbols/edges.
    for result in results {
        let rel_path = crate::util::normalize_path(&result.path);

        // Insert file row.
        tx.execute(
            "INSERT INTO files (path, language, blake3_hash, line_count, byte_size, indexed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                rel_path,
                result.language,
                result.blake3_hash,
                i64::try_from(result.line_count).unwrap_or(i64::MAX),
                i64::try_from(result.byte_size).unwrap_or(i64::MAX),
                now
            ],
        )
        .with_context(|| format!("insert file: {rel_path}"))?;

        let file_id = tx.last_insert_rowid();

        // Insert symbols and build FQN->ID map for edge resolution.
        let mut fqn_to_id: HashMap<String, i64> = HashMap::new();
        for sym in &result.symbols {
            tx.execute(
                "INSERT OR IGNORE INTO symbols \
                 (file_id, name, kind, fqn, signature, docstring, start_line, end_line, is_exported, body_hash) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    file_id,
                    sym.name,
                    sym.kind,
                    sym.fqn,
                    sym.signature,
                    sym.docstring,
                    i64::try_from(sym.start_line).unwrap_or(i64::MAX),
                    i64::try_from(sym.end_line).unwrap_or(i64::MAX),
                    i32::from(sym.is_exported),
                    sym.body_hash
                ],
            )
            .with_context(|| format!("insert symbol: {}", sym.fqn))?;

            let sym_id = if tx.changes() == 0 {
                // INSERT OR IGNORE was a no-op (duplicate fqn+start_line).
                // Look up the existing symbol's ID.
                tx.query_row(
                    "SELECT id FROM symbols WHERE fqn = ?1 AND start_line = ?2",
                    params![sym.fqn, i64::try_from(sym.start_line).unwrap_or(i64::MAX)],
                    |row| row.get::<_, i64>(0),
                )
                .with_context(|| format!("lookup existing symbol: {}", sym.fqn))?
            } else {
                stats.symbols_extracted += 1;
                tx.last_insert_rowid()
            };
            fqn_to_id.insert(sym.fqn.clone(), sym_id);
        }

        // Resolve and insert edges.
        let resolved = edge_resolver::resolve_edges(&tx, &rel_path, &fqn_to_id, &result.edges)
            .with_context(|| format!("resolve edges for: {rel_path}"))?;

        for edge in &resolved {
            tx.execute(
                "INSERT OR IGNORE INTO edges (from_id, to_id, kind) VALUES (?1, ?2, ?3)",
                params![edge.from_id, edge.to_id, edge.kind],
            )
            .context("insert edge")?;
            stats.edges_extracted += 1;
        }

        all_fqn_to_id.extend(fqn_to_id.drain());
    }

    // Compute TF-IDF term frequencies for newly inserted symbols.
    compute_tfidf(&tx, results, &all_fqn_to_id)?;

    tx.commit().context("commit index transaction")?;
    Ok(all_fqn_to_id)
}

/// Computes and stores embedding vectors for indexed symbols.
///
/// Loads the embedding model from `.ndxr/models/`. If the model is not
/// present, returns 0 (silently skips). Uses batch inference for efficiency.
/// When `on_progress` is `Some`, emits a message for each batch of
/// `EMBEDDING_BATCH_SIZE` symbols.
fn compute_embeddings(
    conn: &rusqlite::Connection,
    results: &[parser::ParseResult],
    fqn_to_id: &HashMap<String, i64>,
    models_dir: &std::path::Path,
    on_progress: Option<&dyn Fn(&str)>,
) -> Result<usize> {
    let Some(model) = crate::embeddings::model::ModelHandle::load(models_dir)? else {
        return Ok(0);
    };

    let mut items: Vec<(i64, String)> = Vec::new();
    for result in results {
        for symbol in &result.symbols {
            if let Some(&id) = fqn_to_id.get(&symbol.fqn) {
                let text = crate::embeddings::model::symbol_to_embedding_text(
                    &symbol.name,
                    symbol.signature.as_deref(),
                    symbol.docstring.as_deref(),
                );
                items.push((id, text));
            }
        }
    }

    if items.is_empty() {
        return Ok(0);
    }

    // Chunk items into EMBEDDING_BATCH_SIZE groups for per-batch progress.
    let batch_size = crate::embeddings::model::EMBEDDING_BATCH_SIZE;
    let total_batches = items.len().div_ceil(batch_size);
    let mut all_entries: Vec<(i64, Vec<f32>)> = Vec::with_capacity(items.len());

    for (batch_idx, chunk) in items.chunks(batch_size).enumerate() {
        if let Some(cb) = on_progress {
            cb(&format!(
                "Computing embeddings (batch {}/{total_batches})...",
                batch_idx + 1
            ));
        }
        let texts: Vec<&str> = chunk.iter().map(|(_, t)| t.as_str()).collect();
        let embeddings = model.embed_batch(&texts)?;
        for ((id, _), emb) in chunk.iter().zip(embeddings) {
            all_entries.push((*id, emb));
        }
    }

    let entries: Vec<(i64, &[f32])> = all_entries
        .iter()
        .map(|(id, emb)| (*id, emb.as_slice()))
        .collect();

    crate::embeddings::storage::store_embeddings(
        conn,
        &entries,
        crate::embeddings::download::DEFAULT_MODEL.name,
    )?;

    Ok(entries.len())
}

/// Computes TF-IDF term frequencies for all symbols in the given parse results.
///
/// For each symbol:
/// 1. Resolves the symbol ID from the pre-built `fqn_to_id` map
/// 2. Tokenizes the symbol (name + docstring + FQN)
/// 3. Computes the TF vector
/// 4. Inserts into `term_frequencies`
///
/// After all symbols are processed, recomputes the global `doc_frequencies`
/// table from the full `term_frequencies` table.
fn compute_tfidf(
    tx: &rusqlite::Transaction<'_>,
    results: &[parser::ParseResult],
    fqn_to_id: &HashMap<String, i64>,
) -> Result<()> {
    // Delete old doc_frequencies (will recompute from scratch).
    tx.execute("DELETE FROM doc_frequencies", [])
        .context("clear doc_frequencies")?;

    // For each result's symbols, insert term frequencies.
    for result in results {
        for sym in &result.symbols {
            let Some(&sym_id) = fqn_to_id.get(&sym.fqn) else {
                continue;
            };

            let tokens = tokenizer::tokenize_symbol(&sym.name, sym.docstring.as_deref(), &sym.fqn);
            let tf = tokenizer::compute_tf(&tokens);
            for (term, freq) in &tf {
                tx.execute(
                    "INSERT OR REPLACE INTO term_frequencies (term, symbol_id, tf) \
                     VALUES (?1, ?2, ?3)",
                    params![term, sym_id, freq],
                )
                .context("insert term frequency")?;
            }
        }
    }

    // Recompute doc_frequencies from the full term_frequencies table.
    tx.execute_batch(
        "INSERT INTO doc_frequencies (term, df) \
         SELECT term, COUNT(DISTINCT symbol_id) FROM term_frequencies GROUP BY term;",
    )
    .context("recompute doc_frequencies")?;

    Ok(())
}
