//! Score normalization and hybrid score computation.
//!
//! Provides min-max normalization for raw score vectors and a hybrid scoring
//! function that combines BM25, TF-IDF, centrality, character n-gram similarity,
//! and semantic embedding similarity with intent-specific weights.

use std::borrow::Cow;

use super::intent::IntentWeights;

/// Breakdown of how a search result's score was computed.
///
/// Serializable to JSON for inclusion in MCP tool responses, giving the
/// caller full transparency into why a result was ranked where it was.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScoreBreakdown {
    /// Normalized BM25 full-text search score in \[0, 1\].
    pub bm25: f64,
    /// TF-IDF cosine similarity in \[0, 1\].
    pub tfidf: f64,
    /// Normalized `PageRank` centrality in \[0, 1\].
    pub centrality: f64,
    /// Trigram Jaccard similarity in \[0, 1\].
    pub ngram: f64,
    /// Semantic embedding cosine similarity in \[0, 1\].
    pub semantic: f64,
    /// Cumulative intent-specific boost applied.
    pub intent_boost: f64,
    /// Name of the detected (or overridden) intent.
    pub intent: String,
    /// Query terms that matched this symbol.
    pub matched_terms: Vec<String>,
    /// Human-readable explanation of the score components.
    pub reason: String,
}

/// Normalizes a slice of scores to \[0, 1\] using min-max normalization.
///
/// If all values are equal (or the slice contains a single value), returns
/// all 0.0 — no differentiation signal exists. Handles negative values
/// (like BM25 raw scores) correctly by shifting the minimum to zero.
#[must_use]
pub fn normalize_min_max(scores: &[f64]) -> Vec<f64> {
    if scores.is_empty() {
        return vec![];
    }
    let min = scores.iter().copied().fold(f64::INFINITY, f64::min);
    let max = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let range = max - min;
    if range < f64::EPSILON {
        return vec![0.0; scores.len()];
    }
    scores.iter().map(|&s| (s - min) / range).collect()
}

/// Computes the hybrid score from normalized components.
///
/// Combines the five signal dimensions using the intent-specific weights
/// and adds any intent boost on top.
#[must_use]
pub const fn compute_hybrid_score(
    bm25: f64,
    tfidf: f64,
    centrality: f64,
    ngram: f64,
    semantic: f64,
    intent_boost: f64,
    weights: &IntentWeights,
) -> f64 {
    weights.w_bm25 * bm25
        + weights.w_tfidf * tfidf
        + weights.w_centrality * centrality
        + weights.w_ngram * ngram
        + weights.w_semantic * semantic
        + intent_boost
}

/// Parameters for generating a [`ScoreBreakdown`].
///
/// Groups the many inputs to [`generate_breakdown`] into a single struct
/// to keep the function signature manageable.
pub struct BreakdownParams {
    /// Normalized BM25 score.
    pub bm25: f64,
    /// TF-IDF cosine similarity.
    pub tfidf: f64,
    /// Normalized centrality.
    pub centrality: f64,
    /// Trigram n-gram similarity.
    pub ngram: f64,
    /// Semantic embedding similarity.
    pub semantic: f64,
    /// Cumulative intent boost.
    pub intent_boost: f64,
    /// Intent name string.
    pub intent: String,
    /// Query terms that matched.
    pub matched_terms: Vec<String>,
    /// Number of incoming edges in the graph.
    pub in_degree: usize,
    /// Whether the symbol has a docstring.
    pub has_docstring: bool,
}

/// Generates a human-readable score breakdown.
///
/// Inspects each score component and builds a comma-separated reason string
/// highlighting the dominant factors that contributed to the result's ranking.
#[must_use]
pub fn generate_breakdown(params: BreakdownParams) -> ScoreBreakdown {
    let BreakdownParams {
        bm25,
        tfidf,
        centrality,
        ngram,
        semantic,
        intent_boost,
        intent,
        matched_terms,
        in_degree,
        has_docstring,
    } = params;
    let mut parts: Vec<Cow<'static, str>> = Vec::new();
    if centrality > 0.7 {
        parts.push(format!("High centrality (called by {in_degree} symbols)").into());
    }
    if bm25 > 0.7 {
        parts.push("Strong term match".into());
    }
    if tfidf > 0.7 {
        parts.push("High TF-IDF similarity".into());
    }
    if intent_boost > 0.0 {
        parts.push(format!("{intent}-boosted").into());
    }
    if ngram > 0.3 {
        parts.push("Partial name match".into());
    }
    if semantic > 0.5 {
        parts.push("Semantic match".into());
    }
    if has_docstring {
        parts.push("Has documentation".into());
    }
    let reason = if parts.is_empty() {
        "General relevance".to_string()
    } else {
        parts.join(", ")
    };

    ScoreBreakdown {
        bm25,
        tfidf,
        centrality,
        ngram,
        semantic,
        intent_boost,
        intent,
        matched_terms,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_single_value() {
        assert_eq!(normalize_min_max(&[5.0]), vec![0.0]);
    }

    #[test]
    fn normalize_range() {
        let result = normalize_min_max(&[0.0, 5.0, 10.0]);
        assert!((result[0] - 0.0).abs() < f64::EPSILON);
        assert!((result[1] - 0.5).abs() < f64::EPSILON);
        assert!((result[2] - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn normalize_negative_values() {
        // BM25 scores are negative (more negative = more relevant).
        let result = normalize_min_max(&[-10.0, -5.0, -1.0]);
        assert!((result[0] - 0.0).abs() < f64::EPSILON);
        assert!((result[2] - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn normalize_empty() {
        assert!(normalize_min_max(&[]).is_empty());
    }

    #[test]
    fn normalize_all_equal() {
        let result = normalize_min_max(&[3.0, 3.0, 3.0]);
        assert!(result.iter().all(|&v| v.abs() < f64::EPSILON));
    }

    #[test]
    fn compute_hybrid_score_basic() {
        let weights = crate::graph::intent::IntentWeights {
            w_bm25: 0.35,
            w_tfidf: 0.30,
            w_centrality: 0.25,
            w_ngram: 0.10,
            w_semantic: 0.00,
            boosts: vec![],
            capsule_hints: crate::graph::intent::CapsuleHints::default(),
        };
        let score = compute_hybrid_score(1.0, 1.0, 1.0, 1.0, 0.0, 0.0, &weights);
        assert!((score - 1.0).abs() < 1e-10);
    }

    #[test]
    fn compute_hybrid_score_with_boost() {
        let weights = crate::graph::intent::IntentWeights {
            w_bm25: 0.35,
            w_tfidf: 0.30,
            w_centrality: 0.25,
            w_ngram: 0.10,
            w_semantic: 0.00,
            boosts: vec![],
            capsule_hints: crate::graph::intent::CapsuleHints::default(),
        };
        let score = compute_hybrid_score(1.0, 1.0, 1.0, 1.0, 0.0, 0.5, &weights);
        assert!((score - 1.5).abs() < 1e-10);
    }

    #[test]
    fn compute_hybrid_score_five_signals() {
        let weights = crate::graph::intent::IntentWeights {
            w_bm25: 0.30,
            w_tfidf: 0.25,
            w_centrality: 0.20,
            w_ngram: 0.10,
            w_semantic: 0.15,
            boosts: vec![],
            capsule_hints: crate::graph::intent::CapsuleHints::default(),
        };
        let score = compute_hybrid_score(1.0, 1.0, 1.0, 1.0, 1.0, 0.0, &weights);
        assert!((score - 1.0).abs() < 1e-10);
    }

    #[test]
    fn generate_breakdown_general_relevance() {
        let bd = generate_breakdown(BreakdownParams {
            bm25: 0.3,
            tfidf: 0.3,
            centrality: 0.3,
            ngram: 0.0,
            semantic: 0.0,
            intent_boost: 0.0,
            intent: "explore".to_string(),
            matched_terms: vec![],
            in_degree: 0,
            has_docstring: false,
        });
        assert_eq!(bd.reason, "General relevance");
    }

    #[test]
    fn generate_breakdown_with_high_components() {
        let bd = generate_breakdown(BreakdownParams {
            bm25: 0.9,
            tfidf: 0.8,
            centrality: 0.9,
            ngram: 0.0,
            semantic: 0.0,
            intent_boost: 0.2,
            intent: "debug".to_string(),
            matched_terms: vec!["auth".to_string()],
            in_degree: 5,
            has_docstring: true,
        });
        assert!(bd.reason.contains("Strong term match"));
        assert!(bd.reason.contains("High centrality"));
        assert!(bd.reason.contains("High TF-IDF similarity"));
        assert!(bd.reason.contains("debug-boosted"));
        assert!(bd.reason.contains("Has documentation"));
    }

    #[test]
    fn generate_breakdown_partial_name_match() {
        let bd = generate_breakdown(BreakdownParams {
            bm25: 0.3,
            tfidf: 0.3,
            centrality: 0.3,
            ngram: 0.5,
            semantic: 0.0,
            intent_boost: 0.0,
            intent: "explore".to_string(),
            matched_terms: vec![],
            in_degree: 0,
            has_docstring: false,
        });
        assert!(bd.reason.contains("Partial name match"));
    }

    #[test]
    fn generate_breakdown_semantic_match() {
        let bd = generate_breakdown(BreakdownParams {
            bm25: 0.3,
            tfidf: 0.3,
            centrality: 0.3,
            ngram: 0.0,
            semantic: 0.7,
            intent_boost: 0.0,
            intent: "explore".to_string(),
            matched_terms: vec![],
            in_degree: 0,
            has_docstring: false,
        });
        assert!(bd.reason.contains("Semantic match"));
    }
}
