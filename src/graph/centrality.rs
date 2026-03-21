//! `PageRank` centrality computation using `petgraph`.
//!
//! Computes `PageRank` scores for all symbols in the dependency graph and
//! persists the normalized scores to the `centrality` column in the `symbols`
//! table.

use anyhow::{Context, Result};
use petgraph::algo::page_rank;
use rusqlite::Connection;

use super::builder::SymbolGraph;

/// Damping factor for `PageRank` computation.
///
/// The standard value of 0.85 models the probability that a "random surfer"
/// follows an edge rather than jumping to a random node.
const DAMPING_FACTOR: f64 = 0.85;

/// Number of `PageRank` iterations.
///
/// 100 iterations is sufficient for convergence on typical codebases.
const ITERATIONS: usize = 100;

/// Computes `PageRank` centrality and writes scores to the `symbols` table.
///
/// Uses damping factor 0.85 with 100 iterations. Scores are normalized
/// to \[0, 1\] by dividing by the maximum score. Empty graphs are handled
/// gracefully with no database writes.
///
/// # Errors
///
/// Returns an error if the database update fails.
pub fn compute_and_store(conn: &Connection, graph: &SymbolGraph) -> Result<()> {
    if graph.graph.node_count() == 0 {
        return Ok(());
    }

    // Compute raw PageRank scores (indexed by NodeIndex order).
    let scores = page_rank(&graph.graph, DAMPING_FACTOR, ITERATIONS);

    // Find max for normalization.
    let max_score = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);

    // Batch update inside a transaction.
    let tx = conn
        .unchecked_transaction()
        .context("begin centrality transaction")?;

    {
        let mut stmt = tx
            .prepare("UPDATE symbols SET centrality = ?1 WHERE id = ?2")
            .context("prepare centrality update")?;

        for (node_idx, &raw_score) in scores.iter().enumerate() {
            let node = petgraph::graph::NodeIndex::new(node_idx);
            if let Some(&sym_id) = graph.node_to_id.get(&node) {
                let normalized = if max_score > f64::EPSILON {
                    raw_score / max_score
                } else {
                    0.0
                };
                stmt.execute(rusqlite::params![normalized, sym_id])
                    .with_context(|| format!("update centrality for symbol {sym_id}"))?;
            }
        }
    }

    tx.commit().context("commit centrality transaction")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::builder;
    use crate::storage::db;
    use tempfile::TempDir;

    fn test_db() -> (TempDir, Connection) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.db");
        let conn = db::open_or_create(&db_path).unwrap();
        (tmp, conn)
    }

    #[test]
    fn empty_graph_no_error() {
        let (_tmp, conn) = test_db();
        let graph = builder::build_graph(&conn).unwrap();
        assert!(compute_and_store(&conn, &graph).is_ok());
    }

    #[test]
    fn graph_with_edges_produces_nonzero_centrality() {
        let (_tmp, conn) = test_db();

        conn.execute_batch(
            "INSERT INTO files (path, language, blake3_hash, line_count, byte_size, indexed_at)
             VALUES ('test.ts', 'typescript', 'abc123', 10, 100, 1000);",
        )
        .unwrap();
        let file_id: i64 = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO symbols (file_id, name, kind, fqn, start_line, end_line, is_exported)
             VALUES (?1, 'foo', 'function', 'test::foo', 1, 5, 1)",
            [file_id],
        )
        .unwrap();
        let sym1 = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO symbols (file_id, name, kind, fqn, start_line, end_line, is_exported)
             VALUES (?1, 'bar', 'function', 'test::bar', 6, 10, 0)",
            [file_id],
        )
        .unwrap();
        let sym2 = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO symbols (file_id, name, kind, fqn, start_line, end_line, is_exported)
             VALUES (?1, 'baz', 'function', 'test::baz', 11, 15, 0)",
            [file_id],
        )
        .unwrap();
        let sym3 = conn.last_insert_rowid();

        // foo -> bar, foo -> baz, bar -> baz  (baz is the most linked-to)
        conn.execute(
            "INSERT INTO edges (from_id, to_id, kind) VALUES (?1, ?2, 'calls')",
            [sym1, sym2],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edges (from_id, to_id, kind) VALUES (?1, ?2, 'calls')",
            [sym1, sym3],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edges (from_id, to_id, kind) VALUES (?1, ?2, 'calls')",
            [sym2, sym3],
        )
        .unwrap();

        let graph = builder::build_graph(&conn).unwrap();
        compute_and_store(&conn, &graph).unwrap();

        // All centrality values should be in [0, 1].
        let mut stmt = conn.prepare("SELECT centrality FROM symbols").unwrap();
        let centralities: Vec<f64> = stmt
            .query_map([], |row| row.get::<_, f64>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(centralities.len(), 3);
        for &c in &centralities {
            assert!(c >= 0.0, "centrality should be >= 0, got {c}");
            assert!(c <= 1.0, "centrality should be <= 1, got {c}");
        }

        // At least one should be non-zero (the max will be 1.0 after normalization).
        assert!(
            centralities.iter().any(|&c| c > 0.0),
            "at least one centrality should be non-zero"
        );
        // Max should be exactly 1.0.
        let max_c = centralities
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            (max_c - 1.0).abs() < f64::EPSILON,
            "max centrality should be 1.0, got {max_c}"
        );
    }
}
