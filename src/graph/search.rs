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
#[allow(clippy::cast_precision_loss)]
pub fn hybrid_search(
    conn: &Connection,
    graph: &SymbolGraph,
    query: &str,
    max_results: usize,
    intent_override: Option<Intent>,
) -> Result<Vec<SearchResult>> {
    let intent = intent_override.unwrap_or_else(|| intent::detect_intent(query));
    let weights = intent::get_weights(&intent);
    let intent_name = format!("{intent:?}").to_lowercase();

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
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(max_results);

    Ok(results)
}

/// Collects FTS5 candidates and enriches them with TF-IDF, centrality, and in-degree.
#[allow(clippy::cast_precision_loss)]
fn collect_fts_candidates(
    conn: &Connection,
    graph: &SymbolGraph,
    fts_query: &str,
    query_tf: &HashMap<String, f64>,
    idf_cache: &HashMap<String, f64>,
) -> Result<Vec<Candidate>> {
    // FTS5 BM25 query with column weights: name=10, fqn=5, docstring=1, signature=3.
    // BM25 scores are NEGATIVE (more negative = more relevant).
    let mut fts_stmt = conn
        .prepare(
            "SELECT rowid, bm25(symbols_fts, 10.0, 5.0, 1.0, 3.0) as score
             FROM symbols_fts
             WHERE symbols_fts MATCH ?1
             ORDER BY score
             LIMIT ?2",
        )
        .context("prepare FTS5 query")?;

    #[allow(clippy::cast_possible_wrap)]
    let limit = FTS_CANDIDATE_LIMIT as i64;

    let fts_rows: Vec<(i64, f64)> = fts_stmt
        .query_map(rusqlite::params![fts_query, limit], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
        })
        .context("execute FTS5 query")?
        .filter_map(Result::ok)
        .collect();

    if fts_rows.is_empty() {
        return Ok(vec![]);
    }

    // Prepare symbol data lookup.
    let mut sym_stmt = conn
        .prepare(
            "SELECT s.id, s.name, s.kind, s.fqn, f.path, s.start_line, s.end_line,
                    s.signature, s.is_exported, s.docstring, s.centrality
             FROM symbols s
             JOIN files f ON s.file_id = f.id
             WHERE s.id = ?1",
        )
        .context("prepare symbol lookup")?;

    let mut candidates = Vec::with_capacity(fts_rows.len());

    for (rowid, bm25_score) in &fts_rows {
        let row = sym_stmt.query_row([rowid], |row| {
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
        });

        let Ok((
            sym_id,
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
        )) = row
        else {
            continue;
        };

        // Compute TF-IDF cosine similarity.
        let tfidf = tfidf_cosine(conn, sym_id, query_tf, idf_cache);

        // Compute in-degree from graph.
        let in_degree = graph.id_to_node.get(&sym_id).map_or(0, |&node| {
            graph
                .graph
                .neighbors_directed(node, Direction::Incoming)
                .count()
        });

        // Determine matched terms.
        let matched_terms = find_matched_terms(query_tf, &name, &fqn, docstring.as_deref());

        // BM25 scores are negative; take abs for normalization.
        let bm25_abs = bm25_score.abs();

        candidates.push(Candidate {
            symbol_id: sym_id,
            fqn,
            name,
            kind,
            file_path,
            start_line,
            end_line,
            signature,
            is_exported,
            has_docstring: docstring.is_some(),
            bm25_raw: bm25_abs,
            tfidf,
            centrality,
            in_degree,
            matched_terms,
        });
    }

    Ok(candidates)
}

/// Preloads inverse document frequencies for all query terms in a single pass.
///
/// Avoids querying `doc_frequencies` per term per candidate, which is the main
/// bottleneck in TF-IDF cosine computation.
#[allow(clippy::cast_precision_loss)]
fn preload_idf(
    conn: &Connection,
    query_tf: &HashMap<String, f64>,
    total_docs: f64,
) -> HashMap<String, f64> {
    let mut cache = HashMap::with_capacity(query_tf.len());
    for term in query_tf.keys() {
        let df: f64 = conn
            .query_row(
                "SELECT df FROM doc_frequencies WHERE term = ?1",
                [term.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map(|d| d as f64)
            .unwrap_or(0.0);
        let idf = (total_docs / (1.0 + df)).ln();
        cache.insert(term.clone(), idf);
    }
    cache
}

/// Computes TF-IDF cosine similarity between a query and a symbol.
///
/// Loads term frequencies from the `term_frequencies` table and uses the
/// preloaded IDF cache. Returns 0.0 if no terms overlap.
fn tfidf_cosine(
    conn: &Connection,
    symbol_id: i64,
    query_tf: &HashMap<String, f64>,
    idf_cache: &HashMap<String, f64>,
) -> f64 {
    if query_tf.is_empty() {
        return 0.0;
    }

    // Load term frequencies for this symbol.
    let sym_tfs: HashMap<String, f64> = conn
        .prepare("SELECT term, tf FROM term_frequencies WHERE symbol_id = ?1")
        .and_then(|mut stmt| {
            let rows = stmt.query_map([symbol_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
            })?;
            let mut map = HashMap::new();
            for row in rows.flatten() {
                map.insert(row.0, row.1);
            }
            Ok(map)
        })
        .unwrap_or_default();

    if sym_tfs.is_empty() {
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
