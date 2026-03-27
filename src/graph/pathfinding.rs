//! Pathfinding between symbols using Yen's K-shortest loopless paths algorithm.
//!
//! Finds up to K shortest loopless paths between two symbols in the call graph,
//! enabling logic-flow analysis across the dependency graph. All edges have
//! uniform weight (1 hop).

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use petgraph::Direction;
use petgraph::graph::{DiGraph, NodeIndex};
use rusqlite::Connection;
use serde::Serialize;

use crate::graph::builder::SymbolGraph;
use crate::storage::db::{BATCH_PARAM_LIMIT, build_batch_placeholders};

/// Default number of paths to return when the caller does not specify.
const DEFAULT_MAX_PATHS: usize = 3;

/// Absolute upper bound on the number of paths to search for.
const MAX_PATHS: usize = 5;

/// Timeout in milliseconds for the pathfinding algorithm.
const PATHFINDING_TIMEOUT_MS: u128 = 1000;

/// Metadata tuple for a flow node: (fqn, kind, `file_path`, signature).
type FlowMeta = (String, String, String, Option<String>);

/// Result of a logic flow search between two symbols.
#[derive(Debug, Clone, Serialize)]
pub struct LogicFlowResult {
    /// Fully qualified name of the source symbol.
    pub from: String,
    /// Fully qualified name of the target symbol.
    pub to: String,
    /// Number of paths found.
    pub paths_found: usize,
    /// The discovered execution paths, sorted by hops ascending then centrality descending.
    pub paths: Vec<FlowPath>,
    /// Symbols that connect source and target indirectly (only populated when `paths_found` == 0).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bridges: Vec<BridgeSymbol>,
}

/// A symbol that indirectly connects source and target (shared caller/callee/file).
#[derive(Debug, Clone, Serialize)]
pub struct BridgeSymbol {
    /// Fully-qualified symbol name.
    pub fqn: String,
    /// Symbol kind (function, class, etc.).
    pub kind: String,
    /// File path containing this symbol.
    pub file_path: String,
    /// How this symbol bridges the two: "calls both", "called by both", "shared file".
    pub relationship: String,
}

/// A single execution path between two symbols.
#[derive(Debug, Clone, Serialize)]
pub struct FlowPath {
    /// Number of edges in this path.
    pub hops: usize,
    /// Sum of centrality scores across all nodes in this path.
    pub centrality_score: f64,
    /// Ordered sequence of nodes from source to target.
    pub nodes: Vec<FlowNode>,
}

/// A node in an execution path.
#[derive(Debug, Clone, Serialize)]
pub struct FlowNode {
    /// Fully qualified name of the symbol.
    pub fqn: String,
    /// Symbol kind (function, method, class, etc.).
    pub kind: String,
    /// File path containing this symbol.
    pub file: String,
    /// Optional function signature.
    pub signature: Option<String>,
}

/// Finds up to `max_paths` shortest loopless paths between two symbols.
///
/// Resolves symbols by FQN or name, runs Yen's K-shortest paths algorithm on
/// the call graph, and returns paths annotated with metadata and centrality
/// scores.
///
/// # Errors
///
/// Returns an error if either symbol cannot be resolved, if database queries
/// fail, or if the symbols are not present in the graph.
pub fn find_paths(
    conn: &Connection,
    graph: &SymbolGraph,
    from_fqn: &str,
    to_fqn: &str,
    max_paths: Option<usize>,
) -> Result<LogicFlowResult> {
    if from_fqn == to_fqn {
        bail!("source and target symbols are the same: '{from_fqn}'");
    }

    let k = max_paths.unwrap_or(DEFAULT_MAX_PATHS).min(MAX_PATHS);

    // 1. Resolve symbols to DB IDs and then to graph NodeIndex.
    let from_id = resolve_symbol(conn, from_fqn)?;
    let to_id = resolve_symbol(conn, to_fqn)?;

    let from_node = graph
        .id_to_node
        .get(&from_id)
        .with_context(|| format!("symbol '{from_fqn}' (id={from_id}) not in graph"))?;
    let to_node = graph
        .id_to_node
        .get(&to_id)
        .with_context(|| format!("symbol '{to_fqn}' (id={to_id}) not in graph"))?;

    // 2. Run Yen's algorithm.
    let start = Instant::now();
    let raw_paths = yens_k_shortest(&graph.graph, *from_node, *to_node, k, start);

    // 3. Collect all unique symbol IDs from the discovered paths.
    let mut all_ids: Vec<i64> = raw_paths
        .iter()
        .flat_map(|path| {
            path.iter()
                .filter_map(|node| graph.node_to_id.get(node).copied())
        })
        .collect();
    all_ids.sort_unstable();
    all_ids.dedup();

    // 4. Batch-load metadata and centrality.
    let metadata = batch_load_flow_metadata(conn, &all_ids)?;
    let centralities = batch_load_centrality(conn, &all_ids)?;

    // 5. Build FlowPath results.
    let mut paths: Vec<FlowPath> = Vec::with_capacity(raw_paths.len());
    for raw_path in &raw_paths {
        let mut nodes = Vec::with_capacity(raw_path.len());
        let mut centrality_score = 0.0;

        for node_idx in raw_path {
            let db_id = graph.node_to_id.get(node_idx).copied().unwrap_or_default();

            if let Some((fqn, kind, file, signature)) = metadata.get(&db_id) {
                nodes.push(FlowNode {
                    fqn: fqn.clone(),
                    kind: kind.clone(),
                    file: file.clone(),
                    signature: signature.clone(),
                });
            }

            centrality_score += centralities.get(&db_id).copied().unwrap_or(0.0);
        }

        let hops = raw_path.len().saturating_sub(1);
        paths.push(FlowPath {
            hops,
            centrality_score,
            nodes,
        });
    }

    // 6. Sort by hops ascending, then centrality descending.
    paths.sort_by(|a, b| {
        a.hops
            .cmp(&b.hops)
            .then_with(|| b.centrality_score.total_cmp(&a.centrality_score))
    });

    // 7. If no direct paths found, search for bridge symbols.
    let bridges = if paths.is_empty() {
        find_bridge_symbols(conn, graph, *from_node, *to_node)?
    } else {
        Vec::new()
    };

    Ok(LogicFlowResult {
        from: from_fqn.to_owned(),
        to: to_fqn.to_owned(),
        paths_found: paths.len(),
        paths,
        bridges,
    })
}

/// Finds symbols that indirectly connect source and target via shared neighbors.
///
/// Does a 1-hop BFS from both source and target (both incoming and outgoing
/// directions), then finds nodes that appear in both neighborhoods. Limited
/// to 5 results to keep the response compact.
fn find_bridge_symbols(
    conn: &Connection,
    graph: &SymbolGraph,
    source_node: NodeIndex,
    target_node: NodeIndex,
) -> Result<Vec<BridgeSymbol>> {
    // Collect 1-hop neighbors of source.
    let source_out: HashSet<NodeIndex> = graph
        .graph
        .neighbors_directed(source_node, Direction::Outgoing)
        .collect();
    let source_in: HashSet<NodeIndex> = graph
        .graph
        .neighbors_directed(source_node, Direction::Incoming)
        .collect();

    // Collect 1-hop neighbors of target.
    let target_out: HashSet<NodeIndex> = graph
        .graph
        .neighbors_directed(target_node, Direction::Outgoing)
        .collect();
    let target_in: HashSet<NodeIndex> = graph
        .graph
        .neighbors_directed(target_node, Direction::Incoming)
        .collect();

    // Find shared neighbors with classified relationships.
    let mut bridge_ids: Vec<(i64, &str)> = Vec::new();

    // Nodes called by both source and target ("called by both").
    for node in source_out.intersection(&target_out) {
        if let Some(&id) = graph.node_to_id.get(node) {
            bridge_ids.push((id, "called by both"));
        }
    }

    // Nodes that call both source and target ("calls both").
    for node in source_in.intersection(&target_in) {
        if let Some(&id) = graph.node_to_id.get(node) {
            bridge_ids.push((id, "calls both"));
        }
    }

    // Deduplicate by id and cap at 5.
    bridge_ids.sort_by_key(|(id, _)| *id);
    bridge_ids.dedup_by_key(|(id, _)| *id);
    bridge_ids.truncate(5);

    if bridge_ids.is_empty() {
        // Fall back to shared-file: symbols in the same file as source/target.
        let src_db_id = graph.node_to_id.get(&source_node).copied().unwrap_or(0);
        let tgt_db_id = graph.node_to_id.get(&target_node).copied().unwrap_or(0);

        let shared: Vec<(i64, String, String, String)> = conn
            .prepare(
                "SELECT s.id, s.fqn, s.kind, f.path FROM symbols s \
                 JOIN files f ON s.file_id = f.id \
                 WHERE f.id IN ( \
                     SELECT file_id FROM symbols WHERE id = ?1 \
                     INTERSECT \
                     SELECT file_id FROM symbols WHERE id = ?2 \
                 ) AND s.id NOT IN (?1, ?2) \
                 LIMIT 5",
            )
            .context("prepare shared-file bridge query")?
            .query_map(rusqlite::params![src_db_id, tgt_db_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .context("query shared-file bridges")?
            .filter_map(|r| match r {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!("skipping corrupt bridge row: {e}");
                    None
                }
            })
            .collect();

        return Ok(shared
            .into_iter()
            .map(|(_, fqn, kind, file_path)| BridgeSymbol {
                fqn,
                kind,
                file_path,
                relationship: "shared file".to_owned(),
            })
            .collect());
    }

    // Load metadata for bridge symbols.
    let all_ids: Vec<i64> = bridge_ids.iter().map(|(id, _)| *id).collect();
    let metadata = batch_load_flow_metadata(conn, &all_ids)?;

    Ok(bridge_ids
        .into_iter()
        .filter_map(|(id, relationship)| {
            let (fqn, kind, file_path, _) = metadata.get(&id)?;
            Some(BridgeSymbol {
                fqn: fqn.clone(),
                kind: kind.clone(),
                file_path: file_path.clone(),
                relationship: relationship.to_owned(),
            })
        })
        .collect())
}

/// Resolves a symbol identifier to its database ID.
///
/// Tries exact FQN match first, then falls back to name match. Returns an
/// error if the symbol is not found or if the name is ambiguous (matches
/// multiple symbols).
fn resolve_symbol(conn: &Connection, fqn_or_name: &str) -> Result<i64> {
    // Try exact FQN match first.
    match conn.query_row(
        "SELECT id FROM symbols WHERE fqn = ?1",
        [fqn_or_name],
        |row| row.get::<_, i64>(0),
    ) {
        Ok(id) => return Ok(id),
        Err(rusqlite::Error::QueryReturnedNoRows) => {} // fall through to name match
        Err(e) => {
            return Err(e).with_context(|| format!("FQN lookup failed for '{fqn_or_name}'"));
        }
    }

    // Fall back to name match.
    let mut stmt = conn
        .prepare("SELECT id FROM symbols WHERE name = ?1")
        .context("prepare name lookup")?;
    let ids: Vec<i64> = stmt
        .query_map([fqn_or_name], |row| row.get::<_, i64>(0))
        .context("query name lookup")?
        .filter_map(|r| match r {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::warn!("skipping corrupt row in resolve_symbol name lookup: {e}");
                None
            }
        })
        .collect();

    match ids.len() {
        0 => bail!("symbol not found: '{fqn_or_name}'"),
        1 => Ok(ids[0]),
        n => bail!(
            "ambiguous symbol name '{fqn_or_name}': matches {n} symbols — use a fully qualified name"
        ),
    }
}

/// Runs Yen's K-shortest loopless paths algorithm.
///
/// Finds up to `k` shortest paths from `source` to `target` in the directed
/// graph. All edges have uniform weight (1). Aborts early if the timeout is
/// exceeded.
fn yens_k_shortest(
    graph: &DiGraph<i64, String>,
    source: NodeIndex,
    target: NodeIndex,
    k: usize,
    start_time: Instant,
) -> Vec<Vec<NodeIndex>> {
    let mut a: Vec<Vec<NodeIndex>> = Vec::with_capacity(k);
    let mut b: Vec<Vec<NodeIndex>> = Vec::new();

    // Find initial shortest path via BFS.
    let first =
        bfs_shortest_path_excluding(graph, source, target, &HashSet::new(), &HashSet::new());
    let Some(first_path) = first else {
        return Vec::new();
    };
    a.push(first_path);

    for _ki in 1..k {
        if start_time.elapsed().as_millis() >= PATHFINDING_TIMEOUT_MS {
            break;
        }

        let prev_path = a[a.len() - 1].clone();
        let spur_limit = prev_path.len().saturating_sub(1);

        for i in 0..spur_limit {
            if start_time.elapsed().as_millis() >= PATHFINDING_TIMEOUT_MS {
                break;
            }

            let spur_node = prev_path[i];
            let root_path = &prev_path[..=i];

            // Exclude edges from existing A paths that share this root prefix.
            let mut excluded_edges: HashSet<(NodeIndex, NodeIndex)> = HashSet::new();
            for existing_path in &a {
                if existing_path.len() > i && existing_path[..=i] == *root_path {
                    excluded_edges.insert((existing_path[i], existing_path[i + 1]));
                }
            }

            // Exclude root path nodes (except the spur node) to ensure loopless paths.
            let mut excluded_nodes: HashSet<NodeIndex> = HashSet::new();
            for node in &root_path[..root_path.len() - 1] {
                excluded_nodes.insert(*node);
            }

            // Find spur path from spur_node to target.
            if let Some(spur_path) = bfs_shortest_path_excluding(
                graph,
                spur_node,
                target,
                &excluded_nodes,
                &excluded_edges,
            ) {
                // Combine root path (without spur node) and spur path.
                let mut total = Vec::with_capacity(root_path.len() - 1 + spur_path.len());
                total.extend_from_slice(&root_path[..root_path.len() - 1]);
                total.extend_from_slice(&spur_path);

                // Only add if this path is not already in A or B.
                if !a.contains(&total) && !b.contains(&total) {
                    b.push(total);
                }
            }
        }

        if b.is_empty() {
            break;
        }

        // Select the shortest candidate path.
        b.sort_by_key(Vec::len);
        a.push(b.remove(0));
    }

    a
}

/// BFS shortest path with node and edge exclusions.
///
/// Returns the shortest path from `source` to `target` in the directed graph,
/// skipping any nodes in `excluded_nodes` and edges in `excluded_edges`.
/// Returns `None` if no path exists.
fn bfs_shortest_path_excluding(
    graph: &DiGraph<i64, String>,
    source: NodeIndex,
    target: NodeIndex,
    excluded_nodes: &HashSet<NodeIndex>,
    excluded_edges: &HashSet<(NodeIndex, NodeIndex)>,
) -> Option<Vec<NodeIndex>> {
    if excluded_nodes.contains(&source) || excluded_nodes.contains(&target) {
        return None;
    }

    let mut visited: HashSet<NodeIndex> = HashSet::new();
    visited.insert(source);

    let mut queue: VecDeque<Vec<NodeIndex>> = VecDeque::new();
    queue.push_back(vec![source]);

    while let Some(path) = queue.pop_front() {
        let current = *path.last()?;

        if current == target {
            return Some(path);
        }

        for neighbor in graph.neighbors_directed(current, Direction::Outgoing) {
            if visited.contains(&neighbor)
                || excluded_nodes.contains(&neighbor)
                || excluded_edges.contains(&(current, neighbor))
            {
                continue;
            }
            visited.insert(neighbor);
            let mut new_path = path.clone();
            new_path.push(neighbor);
            queue.push_back(new_path);
        }
    }

    None
}

/// Batch-loads metadata for symbols by their database IDs.
///
/// Returns a map from symbol ID to [`FlowMeta`] (fqn, kind, `file_path`, signature).
/// Chunks queries by `BATCH_PARAM_LIMIT`.
fn batch_load_flow_metadata(conn: &Connection, ids: &[i64]) -> Result<HashMap<i64, FlowMeta>> {
    let mut result = HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(BATCH_PARAM_LIMIT) {
        let placeholders = build_batch_placeholders(chunk.len());
        let sql = format!(
            "SELECT s.id, s.fqn, s.kind, f.path, s.signature FROM symbols s \
             JOIN files f ON s.file_id = f.id \
             WHERE s.id IN ({placeholders})"
        );
        let mut stmt = conn
            .prepare(&sql)
            .context("prepare batch_load_flow_metadata")?;
        let params: Vec<&dyn rusqlite::types::ToSql> = chunk
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt
            .query_map(params.as_slice(), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })
            .context("query batch flow metadata")?;
        for row in rows {
            let (id, fqn, kind, path, signature) = row.context("read flow metadata row")?;
            result.insert(id, (fqn, kind, path, signature));
        }
    }
    Ok(result)
}

/// Batch-loads centrality scores for symbols by their database IDs.
///
/// Returns a map from symbol ID to centrality score. Chunks queries by
/// `BATCH_PARAM_LIMIT`.
fn batch_load_centrality(conn: &Connection, ids: &[i64]) -> Result<HashMap<i64, f64>> {
    let mut result = HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(BATCH_PARAM_LIMIT) {
        let placeholders = build_batch_placeholders(chunk.len());
        let sql = format!("SELECT id, centrality FROM symbols WHERE id IN ({placeholders})");
        let mut stmt = conn
            .prepare(&sql)
            .context("prepare batch_load_centrality")?;
        let params: Vec<&dyn rusqlite::types::ToSql> = chunk
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt
            .query_map(params.as_slice(), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
            })
            .context("query batch centrality")?;
        for row in rows {
            let (id, centrality) = row.context("read centrality row")?;
            result.insert(id, centrality);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db;
    use petgraph::graph::DiGraph;
    use tempfile::TempDir;

    fn build_test_graph() -> SymbolGraph {
        let mut g = DiGraph::new();
        let a = g.add_node(1_i64);
        let b = g.add_node(2_i64);
        let c = g.add_node(3_i64);
        let d = g.add_node(4_i64);
        g.add_edge(a, b, "calls".to_owned());
        g.add_edge(b, c, "calls".to_owned());
        g.add_edge(c, d, "calls".to_owned());
        g.add_edge(a, c, "calls".to_owned());

        let mut id_to_node = HashMap::new();
        id_to_node.insert(1_i64, a);
        id_to_node.insert(2_i64, b);
        id_to_node.insert(3_i64, c);
        id_to_node.insert(4_i64, d);

        let mut node_to_id = HashMap::new();
        node_to_id.insert(a, 1_i64);
        node_to_id.insert(b, 2_i64);
        node_to_id.insert(c, 3_i64);
        node_to_id.insert(d, 4_i64);

        SymbolGraph {
            graph: g,
            id_to_node,
            node_to_id,
        }
    }

    fn test_db() -> (TempDir, Connection) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.db");
        let conn = db::open_or_create(&db_path).unwrap();
        (tmp, conn)
    }

    fn insert_test_symbols(conn: &Connection) -> (i64, i64) {
        conn.execute_batch(
            "INSERT INTO files (path, language, blake3_hash, line_count, byte_size, indexed_at)
             VALUES ('test.ts', 'typescript', 'abc', 10, 100, 1000);",
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();

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

        (sym1, sym2)
    }

    #[test]
    fn bfs_finds_shortest_path() {
        let sg = build_test_graph();
        let a = sg.id_to_node[&1];
        let d = sg.id_to_node[&4];

        let path = bfs_shortest_path_excluding(&sg.graph, a, d, &HashSet::new(), &HashSet::new());
        let path = path.expect("should find a path");

        // Shortest: a -> c -> d (2 hops, 3 nodes)
        assert_eq!(path.len(), 3);
        assert_eq!(sg.node_to_id[&path[0]], 1); // a
        assert_eq!(sg.node_to_id[&path[1]], 3); // c
        assert_eq!(sg.node_to_id[&path[2]], 4); // d
    }

    #[test]
    fn bfs_no_path() {
        let sg = build_test_graph();
        let d = sg.id_to_node[&4];
        let a = sg.id_to_node[&1];

        // d -> a has no path in this directed graph.
        let path = bfs_shortest_path_excluding(&sg.graph, d, a, &HashSet::new(), &HashSet::new());
        assert!(path.is_none());
    }

    #[test]
    fn yens_finds_multiple_paths() {
        let sg = build_test_graph();
        let a = sg.id_to_node[&1];
        let d = sg.id_to_node[&4];

        let paths = yens_k_shortest(&sg.graph, a, d, 3, Instant::now());

        // Should find 2 paths: a->c->d and a->b->c->d
        assert_eq!(paths.len(), 2);

        // First (shortest): a -> c -> d
        let p0: Vec<i64> = paths[0].iter().map(|n| sg.node_to_id[n]).collect();
        assert_eq!(p0, vec![1, 3, 4]);

        // Second: a -> b -> c -> d
        let p1: Vec<i64> = paths[1].iter().map(|n| sg.node_to_id[n]).collect();
        assert_eq!(p1, vec![1, 2, 3, 4]);
    }

    #[test]
    fn resolve_symbol_exact_fqn() {
        let (_tmp, conn) = test_db();
        let (sym1, _) = insert_test_symbols(&conn);

        let id = resolve_symbol(&conn, "test::foo").unwrap();
        assert_eq!(id, sym1);
    }

    #[test]
    fn resolve_symbol_by_name() {
        let (_tmp, conn) = test_db();
        let (sym1, _) = insert_test_symbols(&conn);

        // "foo" is unique, so name lookup should work.
        let id = resolve_symbol(&conn, "foo").unwrap();
        assert_eq!(id, sym1);
    }

    #[test]
    fn resolve_symbol_ambiguous() {
        let (_tmp, conn) = test_db();
        insert_test_symbols(&conn);

        // Insert another symbol with name "foo" but different FQN.
        let file_id: i64 = conn
            .query_row("SELECT id FROM files LIMIT 1", [], |row| row.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO symbols (file_id, name, kind, fqn, start_line, end_line, is_exported)
             VALUES (?1, 'foo', 'function', 'other::foo', 11, 15, 1)",
            [file_id],
        )
        .unwrap();

        let result = resolve_symbol(&conn, "foo");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("ambiguous"),
            "expected 'ambiguous' in error: {err_msg}"
        );
    }

    #[test]
    fn source_equals_target_returns_error() {
        let (_tmp, conn) = test_db();
        let graph = build_test_graph();
        let result = super::find_paths(&conn, &graph, "anything", "anything", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("same"));
    }

    #[test]
    fn bridge_symbols_found_for_disconnected_nodes() {
        // Build graph: A -> C, B -> C (C is called by both A and B).
        let mut g = DiGraph::new();
        let a = g.add_node(1_i64);
        let b = g.add_node(2_i64);
        let c = g.add_node(3_i64);
        g.add_edge(a, c, "calls".to_owned());
        g.add_edge(b, c, "calls".to_owned());
        // No edge A->B or B->A.

        let sg = SymbolGraph {
            graph: g,
            id_to_node: [(1, a), (2, b), (3, c)].into_iter().collect(),
            node_to_id: [(a, 1), (b, 2), (c, 3)].into_iter().collect(),
        };

        let (_tmp, conn) = test_db();
        conn.execute_batch(
            "INSERT INTO files (path, language, blake3_hash, line_count, byte_size, indexed_at)
             VALUES ('test.ts', 'typescript', 'abc', 10, 100, 1000);",
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        for (id, name, fqn) in [
            (1, "a", "test::a"),
            (2, "b", "test::b"),
            (3, "c", "test::c"),
        ] {
            conn.execute(
                "INSERT INTO symbols (id, file_id, name, kind, fqn, start_line, end_line, is_exported)
                 VALUES (?1, ?2, ?3, 'function', ?4, 1, 5, 1)",
                rusqlite::params![id, file_id, name, fqn],
            )
            .unwrap();
        }

        let bridges = find_bridge_symbols(&conn, &sg, a, b).unwrap();
        assert!(!bridges.is_empty(), "should find bridge symbol C");
        assert_eq!(bridges[0].fqn, "test::c");
        assert_eq!(bridges[0].relationship, "called by both");
    }

    #[test]
    fn bridge_symbols_empty_for_completely_disconnected() {
        // Build graph: A and B with no shared neighbors.
        let mut g = DiGraph::new();
        let a = g.add_node(1_i64);
        let b = g.add_node(2_i64);

        let sg = SymbolGraph {
            graph: g,
            id_to_node: [(1, a), (2, b)].into_iter().collect(),
            node_to_id: [(a, 1), (b, 2)].into_iter().collect(),
        };

        let (_tmp, conn) = test_db();
        // Insert symbols in DIFFERENT files so shared-file fallback doesn't trigger.
        conn.execute_batch(
            "INSERT INTO files (id, path, language, blake3_hash, line_count, byte_size, indexed_at)
             VALUES (1, 'a.ts', 'typescript', 'abc', 10, 100, 1000);
             INSERT INTO files (id, path, language, blake3_hash, line_count, byte_size, indexed_at)
             VALUES (2, 'b.ts', 'typescript', 'def', 10, 100, 1000);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO symbols (id, file_id, name, kind, fqn, start_line, end_line, is_exported)
             VALUES (1, 1, 'a', 'function', 'a::a', 1, 5, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO symbols (id, file_id, name, kind, fqn, start_line, end_line, is_exported)
             VALUES (2, 2, 'b', 'function', 'b::b', 1, 5, 1)",
            [],
        )
        .unwrap();

        let bridges = find_bridge_symbols(&conn, &sg, a, b).unwrap();
        assert!(
            bridges.is_empty(),
            "should find no bridges between completely disconnected symbols"
        );
    }

    #[test]
    fn bridges_empty_when_direct_paths_exist() {
        let (_tmp, conn) = test_db();
        let (sym1, sym2) = insert_test_symbols(&conn);

        // Build connected graph: sym1 -> sym2.
        let mut g = DiGraph::new();
        let n1 = g.add_node(sym1);
        let n2 = g.add_node(sym2);
        g.add_edge(n1, n2, "calls".to_owned());

        let sg = SymbolGraph {
            graph: g,
            id_to_node: [(sym1, n1), (sym2, n2)].into_iter().collect(),
            node_to_id: [(n1, sym1), (n2, sym2)].into_iter().collect(),
        };

        let result = find_paths(&conn, &sg, "test::foo", "test::bar", None).unwrap();
        assert!(result.paths_found > 0, "should find direct paths");
        assert!(
            result.bridges.is_empty(),
            "bridges should be empty when paths exist"
        );
    }
}
