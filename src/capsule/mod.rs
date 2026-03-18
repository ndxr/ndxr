//! Context capsule construction: packs relevant code context within a token budget.

pub mod builder;
pub mod relaxation;

use serde::Serialize;

use crate::graph::scoring::ScoreBreakdown;

/// A pivot file containing full source code.
#[derive(Debug, Clone, Serialize)]
pub struct PivotFile {
    /// Relative path to the source file.
    pub path: String,
    /// Full file content.
    pub content: String,
    /// Symbols in this file that matched the search.
    pub symbols: Vec<PivotSymbol>,
}

/// A symbol within a pivot file.
#[derive(Debug, Clone, Serialize)]
pub struct PivotSymbol {
    /// Fully-qualified name of the symbol.
    pub fqn: String,
    /// Symbol kind (function, class, method, etc.).
    pub kind: String,
    /// Final hybrid score.
    pub score: f64,
    /// Score breakdown explaining the ranking.
    pub why: ScoreBreakdown,
}

/// A skeleton file containing signature-only representations.
#[derive(Debug, Clone, Serialize)]
pub struct SkeletonFile {
    /// Relative path to the source file.
    pub path: String,
    /// Skeleton content (signatures only).
    pub content: String,
    /// Names of symbols included in this skeleton.
    pub symbols: Vec<String>,
    /// BFS depth at which this file was discovered.
    pub expansion_depth: usize,
}

/// A memory entry included in the capsule.
#[derive(Debug, Clone, Serialize)]
pub struct MemoryEntry {
    /// Database ID of the observation.
    pub id: i64,
    /// Observation content text.
    pub content: String,
    /// Observation kind (e.g., "decision", "context").
    pub kind: String,
    /// Session ID that created this observation.
    pub session_id: String,
    /// Unix timestamp when the observation was created.
    pub created_at: i64,
    /// Relevance score for this memory entry.
    pub memory_score: f64,
    /// Whether the observation has been marked stale.
    pub is_stale: bool,
}

/// An impact hint showing blast radius for a pivot symbol.
#[derive(Debug, Clone, Serialize)]
pub struct ImpactHint {
    /// Fully-qualified name of the symbol.
    pub fqn: String,
    /// Number of direct callers (in-degree).
    pub callers: usize,
    /// Number of direct callees (out-degree).
    pub callees: usize,
    /// Blast radius category: "low", "medium", or "high".
    pub blast_radius: String,
}

/// Statistics about the capsule construction.
#[derive(Debug, Clone, Serialize)]
pub struct CapsuleStats {
    /// Total tokens consumed by the capsule.
    pub tokens_used: usize,
    /// Maximum token budget allowed.
    pub tokens_budget: usize,
    /// Tokens consumed by pivot files.
    pub tokens_pivots: usize,
    /// Tokens consumed by skeleton files.
    pub tokens_skeletons: usize,
    /// Tokens consumed by memory entries.
    pub tokens_memories: usize,
    /// Number of pivot symbols across all pivot files.
    pub pivot_count: usize,
    /// Number of pivot files.
    pub pivot_files: usize,
    /// Number of skeleton symbols across all skeleton files.
    pub skeleton_count: usize,
    /// Number of skeleton files.
    pub skeleton_files: usize,
    /// Number of memory entries included.
    pub memory_count: usize,
    /// Number of search candidates evaluated.
    pub candidates_evaluated: usize,
    /// Time spent on the search phase in milliseconds.
    pub search_time_ms: u128,
    /// Detected or overridden intent name.
    pub intent: String,
    /// Whether auto-relaxation was applied during search.
    pub relaxation_applied: bool,
}

/// A complete context capsule.
#[derive(Debug, Clone, Serialize)]
pub struct Capsule {
    /// Detected or overridden intent name.
    pub intent: String,
    /// Original search query.
    pub query: String,
    /// Pivot files with full source code.
    pub pivots: Vec<PivotFile>,
    /// Skeleton files with signature-only representations.
    pub skeletons: Vec<SkeletonFile>,
    /// Memory entries relevant to the query.
    pub memories: Vec<MemoryEntry>,
    /// Impact hints for pivot symbols.
    pub impact_hints: Vec<ImpactHint>,
    /// Construction statistics.
    pub stats: CapsuleStats,
}
