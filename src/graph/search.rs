//! Hybrid search combining FTS5 BM25, TF-IDF cosine similarity, and `PageRank` centrality.
//!
//! Implements a six-stage search pipeline:
//! 1. Detect query intent (or accept an override)
//! 2. Collect FTS5 candidates with BM25 scores
//! 3. Compute TF-IDF cosine similarity for each candidate
//! 4. Look up centrality and in-degree from the graph
//! 5. Normalize all scores to \[0, 1\] and apply intent weights + boosts
//! 6. Sort by hybrid score descending and return the top N results

use std::collections::HashMap;

use anyhow::{Context, Result};
use petgraph::Direction;
use rusqlite::Connection;

use super::builder::SymbolGraph;
use super::intent::{self, Intent};
use super::scoring::{self, ScoreBreakdown};
use crate::indexer::tokenizer;

/// Maximum number of FTS5 candidates to evaluate before ranking.
const FTS_CANDIDATE_LIMIT: usize = 100;

/// A search result with score and breakdown.
///
/// Contains all metadata needed to display the result and explain its ranking.
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// Database ID of the matched symbol.
    pub symbol_id: i64,
    /// Fully-qualified name of the symbol.
    pub fqn: String,
    /// Short name of the symbol.
    pub name: String,
    /// Symbol kind (function, class, method, etc.).
    pub kind: String,
    /// Relative file path where the symbol is defined.
    pub file_path: String,
    /// First line of the symbol definition.
    pub start_line: i64,
    /// Last line of the symbol definition.
    pub end_line: i64,
    /// Type signature, if available.
    pub signature: Option<String>,
    /// Whether the symbol is exported (public API).
    pub is_exported: bool,
    /// Final hybrid score (higher = more relevant).
    pub score: f64,
    /// Detailed breakdown of how the score was computed.
    pub why: ScoreBreakdown,
}

/// Raw candidate collected from FTS5 before normalization.
struct Candidate {
    symbol_id: i64,
    fqn: String,
    name: String,
    kind: String,
    file_path: String,
    start_line: i64,
    end_line: i64,
    signature: Option<String>,
    is_exported: bool,
    has_docstring: bool,
    bm25_raw: f64,
    tfidf: f64,
    centrality: f64,
    in_degree: usize,
    matched_terms: Vec<String>,
}

/// Performs hybrid search over the indexed codebase.
///
/// # Pipeline
///
/// 1. Detect intent (or use override)
/// 2. FTS5 query for top 100 candidates with BM25 scores
/// 3. TF-IDF cosine similarity for each candidate
/// 4. Centrality lookup from the symbols table
/// 5. Normalize all scores to \[0, 1\]
/// 6. Apply intent weights + boosts, sort descending, return top N
///
/// # Errors
///
/// Returns an error if any database query fails.
#[allow(clippy::cast_precision_loss)] // usize->f64 loss negligible for counts
pub fn hybrid_search(
    conn: &Connection,
    graph: &SymbolGraph,
    query: &str,
    max_results: usize,
    intent_override: Option<Intent>,
) -> Result<Vec<SearchResult>> {
    let intent = intent_override.unwrap_or_else(|| intent::detect_intent(query));
    let weights = intent::get_weights(&intent);
    let intent_name = intent.name().to_owned();

    // 1. Tokenize query for TF-IDF.
    let query_tokens = tokenizer::tokenize_text(query);
    let query_tf = tokenizer::compute_tf(&query_tokens);

    // 2. Build FTS5 match expression: join terms with OR.
    let fts_query = tokenizer::build_fts_query(query);
    if fts_query.is_empty() {
        return Ok(vec![]);
    }

    // 3. Total document count for IDF.
    let total_docs: f64 = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |row| {
            row.get::<_, i64>(0)
        })
        .context("count total symbols")?
        .max(1) as f64;

    // 4. Preload IDF for all query terms (avoids per-candidate DB queries).
    let idf_cache = preload_idf(conn, &query_tf, total_docs);

    // 5. FTS5 candidate collection.
    let mut candidates = collect_fts_candidates(conn, graph, &fts_query, &query_tf, &idf_cache)?;

    if candidates.is_empty() {
        return Ok(vec![]);
    }

    // 6. Normalize scores.
    let bm25_raw: Vec<f64> = candidates.iter().map(|c| c.bm25_raw).collect();
    let bm25_norm = scoring::normalize_min_max(&bm25_raw);

    let centralities: Vec<f64> = candidates.iter().map(|c| c.centrality).collect();
    let centrality_norm = scoring::normalize_min_max(&centralities);

    // TF-IDF cosine is already in [0, 1].

    // 7. Compute hybrid scores with intent boosts.
    let mut results: Vec<SearchResult> = candidates
        .iter_mut()
        .enumerate()
        .map(|(i, c)| {
            let bm25_n = bm25_norm[i];
            let tfidf_n = c.tfidf;
            let cent_n = centrality_norm[i];

            // Evaluate intent boosts.
            let intent_boost: f64 = weights
                .boosts
                .iter()
                .filter(|b| (b.condition)(&c.kind, c.is_exported, c.has_docstring, c.in_degree))
                .map(|b| b.value)
                .sum();

            let hybrid =
                scoring::compute_hybrid_score(bm25_n, tfidf_n, cent_n, intent_boost, &weights);

            let breakdown = scoring::generate_breakdown(scoring::BreakdownParams {
                bm25: bm25_n,
                tfidf: tfidf_n,
                centrality: cent_n,
                intent_boost,
                intent: intent_name.clone(),
                matched_terms: std::mem::take(&mut c.matched_terms),
                in_degree: c.in_degree,
                has_docstring: c.has_docstring,
            });

            SearchResult {
                symbol_id: c.symbol_id,
                fqn: c.fqn.clone(),
                name: c.name.clone(),
                kind: c.kind.clone(),
                file_path: c.file_path.clone(),
                start_line: c.start_line,
                end_line: c.end_line,
                signature: c.signature.clone(),
                is_exported: c.is_exported,
                score: hybrid,
                why: breakdown,
            }
        })
        .collect();

    // 8. Sort descending by score, take top N.
    results.sort_by(|a, b| b.score.total_cmp(&a.score));
    results.truncate(max_results);

    Ok(results)
}

/// Queries FTS5 for the top candidates ranked by BM25.
///
/// Returns `(rowid, bm25_score)` pairs. BM25 scores are negative (more
/// negative = more relevant).
fn query_fts_rows(conn: &Connection, fts_query: &str) -> Result<Vec<(i64, f64)>> {
    let mut fts_stmt = conn
        .prepare(
            "SELECT rowid, bm25(symbols_fts, 10.0, 5.0, 1.0, 3.0) as score
             FROM symbols_fts
             WHERE symbols_fts MATCH ?1
             ORDER BY score
             LIMIT ?2",
        )
        .context("prepare FTS5 query")?;

    #[allow(clippy::cast_possible_wrap)] // small usize fits in i64
    let limit = FTS_CANDIDATE_LIMIT as i64;

    let rows: Vec<(i64, f64)> = fts_stmt
        .query_map(rusqlite::params![fts_query, limit], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
        })
        .context("execute FTS5 query")?
        .filter_map(|r| match r {
            Ok(val) => Some(val),
            Err(e) => {
                tracing::warn!("skipping corrupt FTS5 row: {e}");
                None
            }
        })
        .collect();

    Ok(rows)
}

/// Collects FTS5 candidates and enriches them with TF-IDF, centrality, and in-degree.
fn collect_fts_candidates(
    conn: &Connection,
    graph: &SymbolGraph,
    fts_query: &str,
    query_tf: &HashMap<String, f64>,
    idf_cache: &HashMap<String, f64>,
) -> Result<Vec<Candidate>> {
    let fts_rows = query_fts_rows(conn, fts_query)?;
    if fts_rows.is_empty() {
        return Ok(vec![]);
    }

    // Pre-load all data for candidate symbols in batch.
    let candidate_ids: Vec<i64> = fts_rows.iter().map(|(id, _)| *id).collect();
    let all_tfs = batch_load_term_frequencies(conn, &candidate_ids);
    let sym_metadata = batch_load_symbol_metadata(conn, &candidate_ids);

    let empty_tfs = HashMap::new();
    let mut candidates = Vec::with_capacity(fts_rows.len());

    for (rowid, bm25_score) in &fts_rows {
        let Some(meta) = sym_metadata.get(rowid) else {
            tracing::warn!("skipping FTS candidate rowid={rowid}: not found in metadata batch");
            continue;
        };

        let sym_tfs = all_tfs.get(rowid).unwrap_or(&empty_tfs);
        let tfidf = tfidf_cosine(sym_tfs, query_tf, idf_cache);
        let in_degree = graph.id_to_node.get(rowid).map_or(0, |&node| {
            graph
                .graph
                .neighbors_directed(node, Direction::Incoming)
                .count()
        });
        let matched_terms =
            find_matched_terms(query_tf, &meta.name, &meta.fqn, meta.docstring.as_deref());

        candidates.push(Candidate {
            symbol_id: *rowid,
            fqn: meta.fqn.clone(),
            name: meta.name.clone(),
            kind: meta.kind.clone(),
            file_path: meta.file_path.clone(),
            start_line: meta.start_line,
            end_line: meta.end_line,
            signature: meta.signature.clone(),
            is_exported: meta.is_exported,
            has_docstring: meta.docstring.is_some(),
            bm25_raw: bm25_score.abs(),
            tfidf,
            centrality: meta.centrality,
            in_degree,
            matched_terms,
        });
    }

    Ok(candidates)
}

/// Symbol metadata loaded in batch for FTS candidate enrichment.
struct SymbolMeta {
    name: String,
    kind: String,
    fqn: String,
    file_path: String,
    start_line: i64,
    end_line: i64,
    signature: Option<String>,
    is_exported: bool,
    docstring: Option<String>,
    centrality: f64,
}

/// Batch-loads symbol metadata for a set of symbol IDs.
///
/// Uses `WHERE s.id IN (...)` with chunking to load all metadata at once
/// instead of one query per candidate.
fn batch_load_symbol_metadata(conn: &Connection, sym_ids: &[i64]) -> HashMap<i64, SymbolMeta> {
    let mut result = HashMap::with_capacity(sym_ids.len());
    for chunk in sym_ids.chunks(crate::storage::db::BATCH_PARAM_LIMIT) {
        let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "SELECT s.id, s.name, s.kind, s.fqn, f.path, s.start_line, s.end_line, \
                    s.signature, s.is_exported, s.docstring, s.centrality \
             FROM symbols s \
             JOIN files f ON s.file_id = f.id \
             WHERE s.id IN ({})",
            placeholders.join(", ")
        );
        let params: Vec<&dyn rusqlite::types::ToSql> = chunk
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();

        let Ok(mut stmt) = conn.prepare(&sql) else {
            tracing::warn!("failed to prepare batch symbol metadata query");
            break;
        };
        let rows = match stmt.query_map(params.as_slice(), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, bool>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, f64>(10)?,
            ))
        }) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("batch symbol metadata query failed: {e}");
                break;
            }
        };
        for row in rows {
            match row {
                Ok((
                    id,
                    name,
                    kind,
                    fqn,
                    file_path,
                    start_line,
                    end_line,
                    signature,
                    is_exported,
                    docstring,
                    centrality,
                )) => {
                    result.insert(
                        id,
                        SymbolMeta {
                            name,
                            kind,
                            fqn,
                            file_path,
                            start_line,
                            end_line,
                            signature,
                            is_exported,
                            docstring,
                            centrality,
                        },
                    );
                }
                Err(e) => {
                    tracing::warn!("skipping corrupt symbol metadata row: {e}");
                }
            }
        }
    }
    result
}

/// Preloads inverse document frequencies for all query terms in a single batch.
///
/// Uses `WHERE term IN (...)` with chunking to avoid per-term DB queries.
/// Terms not found in the database receive `ln(total_docs)` as a fallback IDF.
#[allow(clippy::cast_precision_loss)] // usize->f64 loss negligible for counts
fn preload_idf(
    conn: &Connection,
    query_tf: &HashMap<String, f64>,
    total_docs: f64,
) -> HashMap<String, f64> {
    let mut cache = HashMap::with_capacity(query_tf.len());
    let terms: Vec<&String> = query_tf.keys().collect();
    // Terms absent from doc_frequencies are unknown — they provide no signal.
    // Using 0.0 ensures they don't inflate TF-IDF scores.
    let fallback_idf = 0.0;

    for chunk in terms.chunks(crate::storage::db::BATCH_PARAM_LIMIT) {
        let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "SELECT term, df FROM doc_frequencies WHERE term IN ({})",
            placeholders.join(", ")
        );
        let params: Vec<&dyn rusqlite::types::ToSql> = chunk
            .iter()
            .map(|t| *t as &dyn rusqlite::types::ToSql)
            .collect();

        let Ok(mut stmt) = conn.prepare(&sql) else {
            tracing::warn!("failed to prepare IDF batch query");
            break;
        };
        let rows = match stmt.query_map(params.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        }) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("IDF batch query failed: {e}");
                break;
            }
        };
        for row in rows {
            match row {
                Ok((term, df)) => {
                    let idf = (total_docs / (1.0 + df as f64)).ln();
                    cache.insert(term, idf);
                }
                Err(e) => {
                    tracing::warn!("skipping corrupt IDF row: {e}");
                }
            }
        }
    }

    // Assign fallback IDF for terms not found in the database.
    for term in query_tf.keys() {
        cache.entry(term.clone()).or_insert(fallback_idf);
    }

    cache
}

/// Batch-loads term frequencies for a set of symbol IDs.
///
/// Uses `WHERE symbol_id IN (...)` with chunking to load all TFs at once
/// instead of one query per candidate.
fn batch_load_term_frequencies(
    conn: &Connection,
    sym_ids: &[i64],
) -> HashMap<i64, HashMap<String, f64>> {
    let mut result: HashMap<i64, HashMap<String, f64>> = HashMap::with_capacity(sym_ids.len());
    for chunk in sym_ids.chunks(crate::storage::db::BATCH_PARAM_LIMIT) {
        let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "SELECT symbol_id, term, tf FROM term_frequencies WHERE symbol_id IN ({})",
            placeholders.join(", ")
        );
        let params: Vec<&dyn rusqlite::types::ToSql> = chunk
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();

        let Ok(mut stmt) = conn.prepare(&sql) else {
            tracing::warn!("failed to prepare batch TF query");
            break;
        };
        let rows = match stmt.query_map(params.as_slice(), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
            ))
        }) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("batch TF query failed: {e}");
                break;
            }
        };
        for row in rows {
            match row {
                Ok((sym_id, term, tf)) => {
                    result.entry(sym_id).or_default().insert(term, tf);
                }
                Err(e) => {
                    tracing::warn!("skipping corrupt TF row: {e}");
                }
            }
        }
    }
    result
}

/// Computes TF-IDF cosine similarity between a query and a symbol.
///
/// Uses pre-loaded term frequencies and the preloaded IDF cache.
/// Returns 0.0 if no terms overlap.
fn tfidf_cosine(
    sym_tfs: &HashMap<String, f64>,
    query_tf: &HashMap<String, f64>,
    idf_cache: &HashMap<String, f64>,
) -> f64 {
    if query_tf.is_empty() || sym_tfs.is_empty() {
        return 0.0;
    }

    // Compute cosine similarity with IDF weighting.
    let mut dot_product = 0.0;
    let mut query_magnitude = 0.0;
    let mut sym_magnitude = 0.0;

    // Collect all unique terms from both vectors.
    let mut all_terms: Vec<&String> = query_tf.keys().collect();
    for k in sym_tfs.keys() {
        if !query_tf.contains_key(k) {
            all_terms.push(k);
        }
    }

    for term in &all_terms {
        let idf = idf_cache.get(term.as_str()).copied().unwrap_or(0.0);
        let q_val = query_tf.get(*term).copied().unwrap_or(0.0) * idf;
        let s_val = sym_tfs.get(*term).copied().unwrap_or(0.0) * idf;

        dot_product += q_val * s_val;
        query_magnitude += q_val * q_val;
        sym_magnitude += s_val * s_val;
    }

    let magnitude = query_magnitude.sqrt() * sym_magnitude.sqrt();
    if magnitude < f64::EPSILON {
        0.0
    } else {
        (dot_product / magnitude).clamp(0.0, 1.0)
    }
}

/// Finds which query terms matched the symbol.
fn find_matched_terms(
    query_tf: &HashMap<String, f64>,
    name: &str,
    fqn: &str,
    docstring: Option<&str>,
) -> Vec<String> {
    let name_lower = name.to_lowercase();
    let fqn_lower = fqn.to_lowercase();
    let doc_lower = docstring.map(str::to_lowercase);

    query_tf
        .keys()
        .filter(|term| {
            name_lower.contains(term.as_str())
                || fqn_lower.contains(term.as_str())
                || doc_lower
                    .as_ref()
                    .is_some_and(|d| d.contains(term.as_str()))
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::indexer::tokenizer;

    #[test]
    fn build_fts_query_joins_with_or() {
        assert_eq!(
            tokenizer::build_fts_query("hello world"),
            "\"hello\" OR \"world\""
        );
    }

    #[test]
    fn build_fts_query_strips_special_chars() {
        assert_eq!(
            tokenizer::build_fts_query("foo.bar(baz)"),
            "\"foo\" OR \"bar\" OR \"baz\""
        );
    }

    #[test]
    fn build_fts_query_empty_input() {
        assert!(tokenizer::build_fts_query("").is_empty());
    }

    #[test]
    fn build_fts_query_only_special_chars() {
        assert!(tokenizer::build_fts_query("(){}[]").is_empty());
    }
}
