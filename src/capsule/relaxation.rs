//! Auto-relaxation: ensures searches never return empty results.
//!
//! When the initial hybrid search returns no results, the relaxation strategy
//! progressively widens the candidate pool. As a final fallback, a pure FTS5
//! query is executed without hybrid scoring.

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::graph::builder::SymbolGraph;
use crate::graph::intent::Intent;
use crate::graph::scoring::ScoreBreakdown;
use crate::graph::search::{self, SearchResult};
use crate::indexer::tokenizer;

/// Multiplier applied to `max_results` on relaxation attempts.
const RELAXATION_MULTIPLIER: usize = 4;

/// Performs search with auto-relaxation.
///
/// If the initial search returns no results, progressively relaxes the
/// threshold: doubles the candidate pool up to 3 attempts. Falls back
/// to pure FTS5 if all attempts fail.
///
/// # Errors
///
/// Returns an error if any underlying search or database query fails.
pub fn search_with_relaxation(
    conn: &Connection,
    graph: &SymbolGraph,
    query: &str,
    max_results: usize,
    intent: Option<Intent>,
) -> Result<Vec<SearchResult>> {
    // Try normal search first.
    let results = search::hybrid_search(conn, graph, query, max_results, intent)?;
    if !results.is_empty() {
        return Ok(results);
    }

    // Relaxation: try with a larger candidate pool.
    let relaxed_limit = max_results.saturating_mul(RELAXATION_MULTIPLIER);
    let results = search::hybrid_search(conn, graph, query, relaxed_limit, intent)?;
    if !results.is_empty() {
        return Ok(results.into_iter().take(max_results).collect());
    }

    // Final fallback: pure FTS5 without hybrid scoring.
    fts5_fallback(conn, query, max_results)
}

/// Pure FTS5 fallback search without hybrid scoring.
///
/// Builds a simple FTS5 MATCH query from the input words and returns results
/// ranked purely by BM25 score. Each result gets a minimal score breakdown.
fn fts5_fallback(conn: &Connection, query: &str, max_results: usize) -> Result<Vec<SearchResult>> {
    let fts_query = tokenizer::build_fts_query(query);
    if fts_query.is_empty() {
        return Ok(Vec::new());
    }

    #[allow(clippy::cast_possible_wrap)]
    let limit = max_results as i64;

    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.name, s.kind, s.fqn, f.path, s.start_line, s.end_line, \
                    s.signature, s.is_exported, \
                    bm25(symbols_fts, 10.0, 5.0, 1.0, 3.0) as score \
             FROM symbols_fts \
             JOIN symbols s ON symbols_fts.rowid = s.id \
             JOIN files f ON s.file_id = f.id \
             WHERE symbols_fts MATCH ?1 \
             ORDER BY score \
             LIMIT ?2",
        )
        .context("prepare FTS5 fallback query")?;

    let results: Vec<SearchResult> = stmt
        .query_map(rusqlite::params![fts_query, limit], |row| {
            let bm25_score: f64 = row.get(9)?;
            Ok(SearchResult {
                symbol_id: row.get(0)?,
                name: row.get(1)?,
                kind: row.get(2)?,
                fqn: row.get(3)?,
                file_path: row.get(4)?,
                start_line: row.get(5)?,
                end_line: row.get(6)?,
                signature: row.get(7)?,
                is_exported: row.get(8)?,
                score: bm25_score.abs(),
                why: ScoreBreakdown {
                    bm25: bm25_score.abs(),
                    tfidf: 0.0,
                    centrality: 0.0,
                    intent_boost: 0.0,
                    intent: "fallback".to_string(),
                    matched_terms: Vec::new(),
                    reason: "FTS5 fallback (no hybrid scoring)".to_string(),
                },
            })
        })
        .context("execute FTS5 fallback query")?
        .filter_map(Result::ok)
        .collect();

    Ok(results)
}

#[cfg(test)]
mod tests {
    use crate::indexer::tokenizer;

    #[test]
    fn build_fts_query_basic() {
        assert_eq!(
            tokenizer::build_fts_query("hello world"),
            "\"hello\" OR \"world\""
        );
    }

    #[test]
    fn build_fts_query_strips_special() {
        assert_eq!(
            tokenizer::build_fts_query("foo.bar(baz)"),
            "\"foo\" OR \"bar\" OR \"baz\""
        );
    }

    #[test]
    fn build_fts_query_empty() {
        assert!(tokenizer::build_fts_query("").is_empty());
    }

    #[test]
    fn build_fts_query_only_special() {
        assert!(tokenizer::build_fts_query("(){}[]").is_empty());
    }
}
