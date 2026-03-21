//! Builds context capsules from search results with BFS expansion.
//!
//! The capsule builder takes ranked search results and packs relevant code
//! context into a token-budgeted capsule. Pivot files contain full source
//! code for the highest-scoring results, while skeleton files provide
//! signature-only context for adjacent symbols discovered via BFS.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::{Context, Result};
use petgraph::Direction;
use petgraph::graph::NodeIndex;
use rusqlite::Connection;

use super::{BlastRadius, Capsule, CapsuleStats, ImpactHint, PivotFile, PivotSymbol, SkeletonFile};
use crate::config::TokenEstimator;
use crate::graph::builder::SymbolGraph;
use crate::graph::intent::{Intent, get_capsule_hints};
use crate::graph::search::SearchResult;
use crate::skeleton::reducer;
use crate::storage::db::{BATCH_PARAM_LIMIT, build_batch_placeholders};

/// Maximum memory token budget (hard cap).
const MAX_MEMORY_TOKENS: f64 = 500.0;

/// Fraction of total budget reserved for memory.
const MEMORY_FRACTION: f64 = 0.10;

/// Groups all parameters needed to build a capsule.
///
/// Reduces the argument count of [`build_capsule`] to satisfy the
/// `too_many_arguments` lint while keeping a clean public API.
pub struct CapsuleRequest<'a> {
    /// Database connection.
    pub conn: &'a Connection,
    /// In-memory symbol graph.
    pub graph: &'a SymbolGraph,
    /// Ranked search results.
    pub search_results: &'a [SearchResult],
    /// Original search query.
    pub query: &'a str,
    /// Detected or overridden intent.
    pub intent: &'a Intent,
    /// Maximum token budget for the capsule.
    pub token_budget: usize,
    /// Token count estimator.
    pub estimator: &'a TokenEstimator,
    /// Absolute path to the workspace root.
    pub workspace_root: &'a Path,
}

/// Builds a context capsule from search results.
///
/// # Pipeline
///
/// 1. Fetch intent-specific capsule hints (BFS depth, pivot fraction, skeleton docs)
/// 2. Deduplicate pivots by file
/// 3. BFS expand from pivot symbols (depth from intent hints)
/// 4. Reserve memory budget: `min(total * 0.10, 500)`
/// 5. Fill pivots (pivot fraction from intent hints)
/// 6. Fill skeletons (remaining budget, plus overflow from pivots)
/// 7. Assemble capsule
///
/// # Invariants
///
/// - No file appears in both `pivots` and `skeletons`
/// - `tokens_used <= tokens_budget`
///
/// # Errors
///
/// Returns an error if file reading or database queries fail.
///
/// The return tuple is `(capsule, memory_budget_tokens)`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
pub fn build_capsule(req: &CapsuleRequest<'_>) -> Result<(Capsule, usize)> {
    let intent_name = req.intent.name().to_owned();
    let hints = get_capsule_hints(req.intent);

    // 1. Budget allocation.
    let memory_budget =
        ((req.token_budget as f64) * MEMORY_FRACTION).min(MAX_MEMORY_TOKENS) as usize;
    let remaining = req.token_budget.saturating_sub(memory_budget);
    let pivot_budget = (remaining as f64 * hints.pivot_fraction) as usize;
    let skeleton_budget = remaining.saturating_sub(pivot_budget);

    // 2. Cache the canonical workspace root (avoids re-canonicalizing per file).
    let canonical_root = req.workspace_root.canonicalize().with_context(|| {
        format!(
            "cannot resolve workspace root: {}",
            req.workspace_root.display()
        )
    })?;

    // 3. Build pivot files.
    let (pivots, pivot_files_set, tokens_pivots) = build_pivots(req, pivot_budget, &canonical_root);

    // 4. BFS expansion and skeleton construction.
    let (skeletons, tokens_skeletons) = build_skeletons(
        req,
        &pivot_files_set,
        skeleton_budget + pivot_budget.saturating_sub(tokens_pivots),
        hints.bfs_depth,
        hints.include_skeleton_docs,
    )?;

    // 5. Assemble capsule.
    let stats = CapsuleStats {
        tokens_used: tokens_pivots + tokens_skeletons,
        tokens_budget: req.token_budget,
        tokens_pivots,
        tokens_skeletons,
        tokens_memories: 0,
        pivot_count: pivots.iter().map(|p| p.symbols.len()).sum(),
        pivot_files: pivots.len(),
        skeleton_count: skeletons.iter().map(|s| s.symbols.len()).sum(),
        skeleton_files: skeletons.len(),
        memory_count: 0,
        candidates_evaluated: req.search_results.len(),
        search_time_ms: 0,
        intent: intent_name,
        relaxation_applied: false,
    };

    Ok((
        Capsule {
            intent: stats.intent.clone(),
            query: req.query.to_string(),
            pivots,
            skeletons,
            memories: Vec::new(),
            impact_hints: Vec::new(),
            recent_changes: Vec::new(),
            warnings: Vec::new(),
            stats,
        },
        memory_budget,
    ))
}

/// Selects and reads pivot files from the highest-scoring search results.
///
/// Returns the list of pivot files, the set of their paths, and the total
/// token count consumed.
fn build_pivots(
    req: &CapsuleRequest<'_>,
    pivot_budget: usize,
    canonical_root: &Path,
) -> (Vec<PivotFile>, HashSet<String>, usize) {
    // Group search results by file path.
    let mut file_symbols: HashMap<String, Vec<&SearchResult>> = HashMap::new();
    for result in req.search_results {
        file_symbols
            .entry(result.file_path.clone())
            .or_default()
            .push(result);
    }

    // Sort files by best symbol score descending.
    let mut files_by_score: Vec<(&String, &Vec<&SearchResult>)> = file_symbols.iter().collect();
    files_by_score.sort_by(|a, b| {
        let score_a =
            a.1.iter()
                .map(|s| s.score)
                .fold(f64::NEG_INFINITY, f64::max);
        let score_b =
            b.1.iter()
                .map(|s| s.score)
                .fold(f64::NEG_INFINITY, f64::max);
        score_b.total_cmp(&score_a)
    });

    let mut pivots = Vec::new();
    let mut pivot_paths: HashSet<String> = HashSet::new();
    let mut tokens_used = 0;

    for (file_path, syms) in &files_by_score {
        let Ok(content) = read_file_content(req.workspace_root, canonical_root, file_path) else {
            continue;
        };
        let file_tokens = req.estimator.estimate(&content);

        if tokens_used + file_tokens > pivot_budget {
            continue;
        }

        let pivot_symbols: Vec<PivotSymbol> = syms
            .iter()
            .map(|s| PivotSymbol {
                fqn: s.fqn.clone(),
                kind: s.kind.clone(),
                score: s.score,
                why: s.why.clone(),
            })
            .collect();

        pivots.push(PivotFile {
            path: (*file_path).clone(),
            content,
            symbols: pivot_symbols,
        });
        pivot_paths.insert((*file_path).clone());
        tokens_used += file_tokens;
    }

    (pivots, pivot_paths, tokens_used)
}

/// Performs BFS expansion from pivot symbols and renders skeleton files.
///
/// Returns the list of skeleton files and total token count consumed.
fn build_skeletons(
    req: &CapsuleRequest<'_>,
    pivot_paths: &HashSet<String>,
    budget: usize,
    bfs_depth: usize,
    include_docs: bool,
) -> Result<(Vec<SkeletonFile>, usize)> {
    let pivot_nodes: Vec<NodeIndex> = req
        .search_results
        .iter()
        .filter(|r| pivot_paths.contains(&r.file_path))
        .filter_map(|r| req.graph.id_to_node.get(&r.symbol_id).copied())
        .collect();

    let adjacent = bfs_expand(req.graph, &pivot_nodes, bfs_depth);

    // Collect all symbol IDs from BFS results for a single batch query.
    let sym_entries: Vec<(i64, usize)> = adjacent
        .iter()
        .filter_map(|(node_idx, depth)| req.graph.node_to_id.get(node_idx).map(|&id| (id, *depth)))
        .collect();
    let sym_ids: Vec<i64> = sym_entries.iter().map(|(id, _)| *id).collect();
    let id_to_depth: HashMap<i64, usize> = sym_entries.into_iter().collect();

    // Batch-query file path and symbol name for all BFS neighbors.
    let id_to_file_name = batch_load_symbol_file_names(req.conn, &sym_ids)?;

    // Group adjacent symbols by file.
    let mut adjacent_by_file: HashMap<String, (Vec<String>, usize)> = HashMap::new();
    let mut file_order: Vec<String> = Vec::new();
    let mut file_order_set: HashSet<String> = HashSet::new();

    for &sym_id in &sym_ids {
        if let Some((fp, name)) = id_to_file_name.get(&sym_id)
            && !pivot_paths.contains(fp)
        {
            let depth = id_to_depth.get(&sym_id).copied().unwrap_or(0);
            let entry = adjacent_by_file
                .entry(fp.clone())
                .or_insert_with(|| (Vec::new(), depth));
            entry.0.push(name.clone());
            if depth < entry.1 {
                entry.1 = depth;
            }
            if file_order_set.insert(fp.clone()) {
                file_order.push(fp.clone());
            }
        }
    }

    let mut skeletons = Vec::new();
    let mut tokens_used = 0;

    // Re-sort skeleton files by BFS depth (shallowest first) so the budget
    // loop prioritizes the most closely related neighbors.
    let mut skeleton_data = reducer::render_skeletons(req.conn, &file_order, include_docs)?;
    skeleton_data.sort_by_key(|skel| {
        adjacent_by_file
            .get(&skel.path)
            .map_or(usize::MAX, |(_, depth)| *depth)
    });
    for skel in skeleton_data {
        let skel_tokens = req.estimator.estimate(&skel.content);
        if tokens_used + skel_tokens > budget {
            continue;
        }

        let (sym_names, depth) = adjacent_by_file
            .get(&skel.path)
            .cloned()
            .unwrap_or_else(|| (Vec::new(), 0));

        skeletons.push(SkeletonFile {
            path: skel.path,
            content: skel.content,
            symbols: sym_names,
            expansion_depth: depth,
        });
        tokens_used += skel_tokens;
    }

    Ok((skeletons, tokens_used))
}

/// Batch-loads `(file_path, symbol_name)` for a set of symbol IDs.
///
/// Chunks IDs into groups of `BATCH_PARAM_LIMIT` to stay within the `SQLite`
/// parameter limit, querying `WHERE s.id IN (...)` per chunk.
fn batch_load_symbol_file_names(
    conn: &Connection,
    ids: &[i64],
) -> Result<HashMap<i64, (String, String)>> {
    let mut result = HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(BATCH_PARAM_LIMIT) {
        let placeholders = build_batch_placeholders(chunk.len());
        let sql = format!(
            "SELECT s.id, f.path, s.name FROM symbols s \
             JOIN files f ON s.file_id = f.id \
             WHERE s.id IN ({placeholders})"
        );
        let mut stmt = conn
            .prepare(&sql)
            .context("prepare batch_load_symbol_file_names")?;
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
                ))
            })
            .context("query batch symbol file names")?;
        for row in rows {
            let (id, path, name) = row.context("read symbol file name row")?;
            result.insert(id, (path, name));
        }
    }
    Ok(result)
}

/// BFS expansion from pivot symbols, following edges in both directions.
///
/// Returns `(NodeIndex, depth)` pairs for all reachable symbols up to `max_depth`.
/// Start nodes themselves are excluded from the result.
fn bfs_expand(
    graph: &SymbolGraph,
    start_nodes: &[NodeIndex],
    max_depth: usize,
) -> Vec<(NodeIndex, usize)> {
    let mut visited: HashSet<NodeIndex> = start_nodes.iter().copied().collect();
    let mut queue: VecDeque<(NodeIndex, usize)> = VecDeque::new();
    let mut result = Vec::new();

    for &node in start_nodes {
        queue.push_back((node, 0));
    }

    while let Some((node, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }

        for direction in [Direction::Outgoing, Direction::Incoming] {
            for neighbor in graph.graph.neighbors_directed(node, direction) {
                if visited.insert(neighbor) {
                    let next_depth = depth + 1;
                    result.push((neighbor, next_depth));
                    queue.push_back((neighbor, next_depth));
                }
            }
        }
    }

    result.sort_by_key(|(_, d)| *d);
    result
}

/// Reads file content from the workspace by resolving a relative path.
///
/// Canonicalizes the resolved path and validates that it remains under the
/// pre-computed `canonical_root`, preventing path-traversal attacks via
/// `../` segments.
fn read_file_content(
    workspace_root: &Path,
    canonical_root: &Path,
    rel_path: &str,
) -> Result<String> {
    let abs_path = workspace_root.join(rel_path);
    // Prevent path traversal — verify resolved path is under workspace root.
    let canonical = abs_path
        .canonicalize()
        .with_context(|| format!("cannot resolve file: {}", abs_path.display()))?;
    anyhow::ensure!(
        canonical.starts_with(canonical_root),
        "path traversal detected: {rel_path} escapes workspace root"
    );
    std::fs::read_to_string(&canonical)
        .with_context(|| format!("cannot read file: {}", canonical.display()))
}

/// Generates impact hints for pivot symbols.
///
/// Each hint includes the direct caller/callee counts and a blast radius
/// classification based on the transitive caller count.
#[must_use]
#[allow(clippy::similar_names)] // callers/callees are distinct domain concepts
pub fn generate_impact_hints(
    graph: &SymbolGraph,
    pivot_results: &[SearchResult],
) -> Vec<ImpactHint> {
    pivot_results
        .iter()
        .filter_map(|result| {
            let node = graph.id_to_node.get(&result.symbol_id)?;
            let callers = graph
                .graph
                .neighbors_directed(*node, Direction::Incoming)
                .count();
            let callees = graph
                .graph
                .neighbors_directed(*node, Direction::Outgoing)
                .count();

            let transitive = count_transitive_callers(graph, *node);
            let blast_radius = BlastRadius::from_caller_count(transitive);

            Some(ImpactHint {
                fqn: result.fqn.clone(),
                callers,
                callees,
                blast_radius,
            })
        })
        .collect()
}

/// Counts transitive callers via BFS over incoming edges.
///
/// The start node itself is excluded from the count.
fn count_transitive_callers(graph: &SymbolGraph, start: NodeIndex) -> usize {
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    queue.push_back(start);
    visited.insert(start);

    while let Some(node) = queue.pop_front() {
        for caller in graph.graph.neighbors_directed(node, Direction::Incoming) {
            if visited.insert(caller) {
                queue.push_back(caller);
            }
        }
    }

    visited.len().saturating_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use petgraph::graph::DiGraph;

    fn empty_graph() -> SymbolGraph {
        SymbolGraph {
            graph: DiGraph::new(),
            id_to_node: HashMap::new(),
            node_to_id: HashMap::new(),
        }
    }

    #[test]
    fn bfs_expand_empty_start() {
        let graph = empty_graph();
        let result = bfs_expand(&graph, &[], 2);
        assert!(result.is_empty());
    }

    #[test]
    #[allow(clippy::many_single_char_names)]
    fn bfs_expand_respects_max_depth() {
        let mut g = DiGraph::new();
        let a = g.add_node(1_i64);
        let b = g.add_node(2_i64);
        let c = g.add_node(3_i64);
        let d = g.add_node(4_i64);
        g.add_edge(a, b, "calls".to_string());
        g.add_edge(b, c, "calls".to_string());
        g.add_edge(c, d, "calls".to_string());

        let mut id_to_node = HashMap::new();
        let mut node_to_id = HashMap::new();
        for node in [a, b, c, d] {
            let id = g[node];
            id_to_node.insert(id, node);
            node_to_id.insert(node, id);
        }

        let graph = SymbolGraph {
            graph: g,
            id_to_node,
            node_to_id,
        };

        // Depth 1 from node a: should reach b only (outgoing).
        let result = bfs_expand(&graph, &[a], 1);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, b);

        // Depth 2 from node a: should reach b and c.
        let result = bfs_expand(&graph, &[a], 2);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn count_transitive_callers_no_callers() {
        let mut g = DiGraph::new();
        let a = g.add_node(1_i64);
        let graph = SymbolGraph {
            graph: g,
            id_to_node: HashMap::from([(1, a)]),
            node_to_id: HashMap::from([(a, 1)]),
        };
        assert_eq!(count_transitive_callers(&graph, a), 0);
    }

    #[test]
    fn count_transitive_callers_chain() {
        let mut g = DiGraph::new();
        let a = g.add_node(1_i64);
        let b = g.add_node(2_i64);
        let c = g.add_node(3_i64);
        // c -> b -> a (a is called by b, b is called by c)
        g.add_edge(c, b, "calls".to_string());
        g.add_edge(b, a, "calls".to_string());

        let graph = SymbolGraph {
            graph: g,
            id_to_node: HashMap::from([(1, a), (2, b), (3, c)]),
            node_to_id: HashMap::from([(a, 1), (b, 2), (c, 3)]),
        };
        // Transitive callers of a: b and c = 2
        assert_eq!(count_transitive_callers(&graph, a), 2);
    }

    #[test]
    fn read_file_content_rejects_path_traversal() {
        let workspace = tempfile::TempDir::new().unwrap();
        let canonical_root = workspace.path().canonicalize().unwrap();

        // Create a real file OUTSIDE the workspace so canonicalize() succeeds
        // and the starts_with() guard is what actually catches the traversal.
        let outside = workspace.path().parent().unwrap();
        let target = outside.join("traversal_target.txt");
        std::fs::write(&target, "secret").unwrap();

        let result =
            read_file_content(workspace.path(), &canonical_root, "../traversal_target.txt");

        // Clean up the outside file regardless of test outcome.
        let _ = std::fs::remove_file(&target);

        assert!(
            result.is_err(),
            "path traversal should be rejected, but got: {result:?}"
        );
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("path traversal"),
            "expected 'path traversal detected' error, got: {err_msg}"
        );
    }

    #[test]
    fn read_file_content_allows_valid_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = tmp.path();
        let canonical_root = workspace.canonicalize().unwrap();

        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::write(workspace.join("src/main.ts"), "export function main() {}").unwrap();

        let content = read_file_content(workspace, &canonical_root, "src/main.ts").unwrap();
        assert_eq!(content, "export function main() {}");
    }

    #[test]
    fn impact_hints_blast_radius_categories() {
        let mut g = DiGraph::new();
        let a = g.add_node(1_i64);
        let graph = SymbolGraph {
            graph: g,
            id_to_node: HashMap::from([(1, a)]),
            node_to_id: HashMap::from([(a, 1)]),
        };

        let results = vec![SearchResult {
            symbol_id: 1,
            fqn: "test::a".to_string(),
            name: "a".to_string(),
            kind: "function".to_string(),
            file_path: "test.ts".to_string(),
            start_line: 1,
            end_line: 3,
            signature: None,
            is_exported: true,
            score: 1.0,
            why: crate::graph::scoring::ScoreBreakdown {
                bm25: 0.5,
                tfidf: 0.5,
                centrality: 0.5,
                intent_boost: 0.0,
                intent: "explore".to_string(),
                matched_terms: vec![],
                reason: "test".to_string(),
            },
        }];

        let hints = generate_impact_hints(&graph, &results);
        assert_eq!(hints.len(), 1);
        assert!(matches!(
            hints[0].blast_radius,
            BlastRadius::Low | BlastRadius::Medium | BlastRadius::High
        ));
    }
}
