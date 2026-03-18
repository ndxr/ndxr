//! Constructs a `petgraph` `DiGraph` from `SQLite` edge data.
//!
//! Loads all symbol IDs and edges from the database, producing an in-memory
//! directed graph suitable for centrality computation and graph traversal.

use std::collections::HashMap;

use anyhow::{Context, Result};
use petgraph::graph::{DiGraph, NodeIndex};
use rusqlite::Connection;

/// In-memory symbol dependency graph.
///
/// Maps between database symbol IDs and `petgraph` node indices in both
/// directions, enabling efficient lookups from either representation.
pub struct SymbolGraph {
    /// Directed graph where nodes carry symbol IDs and edges carry relationship kinds.
    pub graph: DiGraph<i64, String>,
    /// Maps symbol database ID to graph node index.
    pub id_to_node: HashMap<i64, NodeIndex>,
    /// Maps graph node index to symbol database ID.
    pub node_to_id: HashMap<NodeIndex, i64>,
}

/// Builds the dependency graph from all edges in the database.
///
/// Creates a node for every symbol and a directed edge for every row in the
/// `edges` table. Both forward and reverse ID mappings are built so callers
/// can translate freely between database IDs and `petgraph` indices.
///
/// # Errors
///
/// Returns an error if any database query fails.
pub fn build_graph(conn: &Connection) -> Result<SymbolGraph> {
    let mut graph = DiGraph::new();
    let mut id_to_node: HashMap<i64, NodeIndex> = HashMap::new();
    let mut node_to_id: HashMap<NodeIndex, i64> = HashMap::new();

    // 1. Load all symbol IDs and create nodes.
    let mut stmt = conn
        .prepare("SELECT id FROM symbols")
        .context("prepare symbol ID query")?;

    let symbol_ids: Vec<i64> = stmt
        .query_map([], |row| row.get::<_, i64>(0))
        .context("query symbol IDs")?
        .filter_map(Result::ok)
        .collect();

    for sym_id in symbol_ids {
        let node = graph.add_node(sym_id);
        id_to_node.insert(sym_id, node);
        node_to_id.insert(node, sym_id);
    }

    // 2. Load all edges and create directed edges.
    let mut edge_stmt = conn
        .prepare("SELECT from_id, to_id, kind FROM edges")
        .context("prepare edge query")?;

    let edges: Vec<(i64, i64, String)> = edge_stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .context("query edges")?
        .filter_map(Result::ok)
        .collect();

    for (from_id, to_id, kind) in edges {
        if let (Some(&from_node), Some(&to_node)) =
            (id_to_node.get(&from_id), id_to_node.get(&to_id))
        {
            graph.add_edge(from_node, to_node, kind);
        }
    }

    Ok(SymbolGraph {
        graph,
        id_to_node,
        node_to_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db;
    use tempfile::TempDir;

    fn test_db() -> (TempDir, Connection) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.db");
        let conn = db::open_or_create(&db_path).unwrap();
        (tmp, conn)
    }

    #[test]
    fn empty_db_produces_empty_graph() {
        let (_tmp, conn) = test_db();
        let graph = build_graph(&conn).unwrap();
        assert_eq!(graph.graph.node_count(), 0);
        assert_eq!(graph.graph.edge_count(), 0);
        assert!(graph.id_to_node.is_empty());
        assert!(graph.node_to_id.is_empty());
    }

    #[test]
    fn graph_with_edges_has_correct_counts() {
        let (_tmp, conn) = test_db();

        // Insert a file, two symbols, and one edge.
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
            "INSERT INTO edges (from_id, to_id, kind) VALUES (?1, ?2, 'calls')",
            [sym1, sym2],
        )
        .unwrap();

        let graph = build_graph(&conn).unwrap();
        assert_eq!(graph.graph.node_count(), 2);
        assert_eq!(graph.graph.edge_count(), 1);
        assert!(graph.id_to_node.contains_key(&sym1));
        assert!(graph.id_to_node.contains_key(&sym2));
        assert_eq!(graph.node_to_id.len(), 2);
    }
}
