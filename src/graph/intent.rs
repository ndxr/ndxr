//! Intent detection from natural language queries.
//!
//! Classifies a search query into one of six intents (Debug, Test, Refactor,
//! Modify, Understand, Explore) using keyword matching with priority-based
//! tiebreaking. Each intent carries tuned scoring weights and boost rules
//! that adjust how search results are ranked.

/// The detected purpose of a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Debugging: finding bugs, errors, crashes.
    Debug,
    /// Refactoring: restructuring, renaming, reorganizing code.
    Refactor,
    /// Modifying: adding features, extending functionality.
    Modify,
    /// Exploring: browsing, discovering code structure.
    Explore,
    /// Understanding: learning how code works, tracing logic.
    Understand,
    /// Testing: finding test files, verifying coverage.
    Test,
}

/// Boost rule for intent-specific scoring adjustments.
#[derive(Debug, Clone)]
pub struct BoostRule {
    /// Human-readable description of what this boost rewards.
    pub description: &'static str,
    /// Boost value to add to the score.
    pub value: f64,
    /// Condition function: takes (kind, `is_exported`, `has_docstring`, `in_degree`, `file_path`)
    /// and returns `true` if the boost applies.
    pub condition: fn(&str, bool, bool, usize, &str) -> bool,
}

/// Capsule construction hints derived from the detected intent.
///
/// Controls how the capsule builder allocates its token budget and
/// how deeply it expands context around the pivot symbols.
#[derive(Debug, Clone, Copy)]
#[must_use]
pub struct CapsuleHints {
    /// BFS expansion depth for skeleton neighbors (default: 2).
    pub bfs_depth: usize,
    /// Fraction of remaining budget allocated to pivots vs skeletons (default: 0.85).
    pub pivot_fraction: f64,
    /// Whether to include docstrings in skeleton output.
    pub include_skeleton_docs: bool,
}

impl Default for CapsuleHints {
    fn default() -> Self {
        Self {
            bfs_depth: 2,
            pivot_fraction: 0.85,
            include_skeleton_docs: false,
        }
    }
}

/// Intent-specific scoring weights and capsule construction hints.
///
/// Controls the relative importance of BM25 text matching, TF-IDF cosine
/// similarity, `PageRank` centrality, character n-gram similarity, and
/// semantic embedding similarity in the hybrid score computation.
/// Also provides hints that shape capsule construction behavior.
pub struct IntentWeights {
    /// Weight for BM25 full-text search score.
    pub w_bm25: f64,
    /// Weight for TF-IDF cosine similarity score.
    pub w_tfidf: f64,
    /// Weight for `PageRank` centrality score.
    pub w_centrality: f64,
    /// Weight for character trigram n-gram similarity score.
    pub w_ngram: f64,
    /// Weight for semantic embedding cosine similarity score.
    pub w_semantic: f64,
    /// Additional boost rules that apply conditional score bonuses.
    pub boosts: Vec<BoostRule>,
    /// Hints that shape how the capsule builder allocates budget and context.
    pub capsule_hints: CapsuleHints,
}

impl Intent {
    /// Returns the lowercase name of the intent variant.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Refactor => "refactor",
            Self::Modify => "modify",
            Self::Explore => "explore",
            Self::Understand => "understand",
            Self::Test => "test",
        }
    }
}

/// Keywords for each intent category, used for keyword-match scoring.
const DEBUG_KEYWORDS: &[&str] = &[
    "fix",
    "bug",
    "error",
    "crash",
    "broken",
    "fail",
    "panic",
    "exception",
    "issue",
    "wrong",
];
const REFACTOR_KEYWORDS: &[&str] = &[
    "refactor",
    "rename",
    "move",
    "extract",
    "reorganize",
    "split",
    "merge",
    "restructure",
];
const MODIFY_KEYWORDS: &[&str] = &[
    "add",
    "implement",
    "create",
    "build",
    "extend",
    "integrate",
    "new",
    "feature",
];
const UNDERSTAND_KEYWORDS: &[&str] = &[
    "explain",
    "how does",
    "what is",
    "trace",
    "understand",
    "why does",
    "walk through",
];
const TEST_KEYWORDS: &[&str] = &[
    "test",
    "spec",
    "coverage",
    "assert",
    "mock",
    "verify",
    "fixture",
    "unit test",
];

/// Counts keyword matches in the lowercased query.
fn count_matches(query: &str, keywords: &[&str]) -> usize {
    keywords.iter().filter(|kw| query.contains(**kw)).count()
}

/// Detects the intent from a query string.
///
/// Uses keyword matching with priority tiebreaker:
/// Debug > Test > Refactor > Modify > Understand > Explore.
///
/// The query is lowercased before matching. Multi-word keywords (e.g.,
/// "how does", "unit test") are matched as substrings. If no keywords
/// match, the default intent is [`Intent::Explore`].
#[must_use]
pub fn detect_intent(query: &str) -> Intent {
    let lower = query.to_lowercase();

    let debug_count = count_matches(&lower, DEBUG_KEYWORDS);
    let test_count = count_matches(&lower, TEST_KEYWORDS);
    let refactor_count = count_matches(&lower, REFACTOR_KEYWORDS);
    let modify_count = count_matches(&lower, MODIFY_KEYWORDS);
    let understand_count = count_matches(&lower, UNDERSTAND_KEYWORDS);

    // Priority-ordered pairs: higher-priority intents come first.
    // On equal counts the earlier (higher-priority) intent wins.
    let candidates = [
        (debug_count, Intent::Debug),
        (test_count, Intent::Test),
        (refactor_count, Intent::Refactor),
        (modify_count, Intent::Modify),
        (understand_count, Intent::Understand),
    ];

    let max_count = candidates.iter().map(|(c, _)| *c).max().unwrap_or(0);

    if max_count == 0 {
        return Intent::Explore;
    }

    // Return the first (highest-priority) intent that achieved max_count.
    candidates
        .iter()
        .find(|(c, _)| *c == max_count)
        .map_or(Intent::Explore, |(_, intent)| *intent)
}

/// Returns the scoring weights for a given intent.
///
/// When `has_embeddings` is true, allocates weight to the semantic signal
/// by redistributing from other weights. When false, semantic weight is
/// zero and other weights use the non-embedding distribution.
#[must_use]
pub fn get_weights(intent: &Intent, has_embeddings: bool) -> IntentWeights {
    match intent {
        Intent::Debug => debug_weights(has_embeddings),
        Intent::Refactor => refactor_weights(has_embeddings),
        Intent::Modify => modify_weights(has_embeddings),
        Intent::Explore => explore_weights(has_embeddings),
        Intent::Understand => understand_weights(has_embeddings),
        Intent::Test => test_weights(has_embeddings),
    }
}

/// Returns the capsule construction hints for a given intent.
///
/// Lightweight alternative to [`get_weights`] when only the capsule hints are
/// needed — avoids allocating the `Vec<BoostRule>` scoring weights.
pub fn get_capsule_hints(intent: &Intent) -> CapsuleHints {
    match intent {
        Intent::Debug => CapsuleHints {
            bfs_depth: 3,
            pivot_fraction: 0.85,
            include_skeleton_docs: false,
        },
        Intent::Refactor => CapsuleHints {
            bfs_depth: 3,
            pivot_fraction: 0.70,
            include_skeleton_docs: false,
        },
        Intent::Understand => CapsuleHints {
            bfs_depth: 2,
            pivot_fraction: 0.85,
            include_skeleton_docs: true,
        },
        Intent::Modify | Intent::Explore | Intent::Test => CapsuleHints::default(),
    }
}

/// Weights for [`Intent::Debug`]: prioritize text match, boost error-related symbols.
fn debug_weights(has_embeddings: bool) -> IntentWeights {
    let (w_bm25, w_tfidf, w_centrality, w_ngram, w_semantic) = if has_embeddings {
        (0.30, 0.25, 0.20, 0.10, 0.15)
    } else {
        (0.35, 0.30, 0.25, 0.10, 0.00)
    };
    IntentWeights {
        w_bm25,
        w_tfidf,
        w_centrality,
        w_ngram,
        w_semantic,
        boosts: vec![
            BoostRule {
                description: "Symbols with error/exception/panic in name or kind",
                value: 0.20,
                condition: |kind, _, _, _, _| {
                    let k = kind.to_lowercase();
                    k.contains("error") || k.contains("exception") || k.contains("panic")
                },
            },
            BoostRule {
                description: "Symbols near error-handling paths (high in-degree)",
                value: 0.10,
                condition: |_, _, _, in_degree, _| in_degree >= 3,
            },
        ],
        capsule_hints: CapsuleHints {
            bfs_depth: 3,
            pivot_fraction: 0.85,
            include_skeleton_docs: false,
        },
    }
}

/// Weights for [`Intent::Modify`]: balanced, boost registry/entry-point files and extension points.
fn modify_weights(has_embeddings: bool) -> IntentWeights {
    let (w_bm25, w_tfidf, w_centrality, w_ngram, w_semantic) = if has_embeddings {
        (0.25, 0.25, 0.25, 0.10, 0.15)
    } else {
        (0.30, 0.30, 0.30, 0.10, 0.00)
    };
    IntentWeights {
        w_bm25,
        w_tfidf,
        w_centrality,
        w_ngram,
        w_semantic,
        boosts: vec![
            BoostRule {
                description: "Registry/entry-point files (mod.rs, index.ts, __init__.py)",
                value: 0.15,
                condition: |_, _, _, _, file_path| {
                    file_path.ends_with("mod.rs")
                        || file_path.ends_with("index.ts")
                        || file_path.ends_with("index.js")
                        || file_path.ends_with("__init__.py")
                },
            },
            BoostRule {
                description: "Exported symbols with high in-degree (extension points)",
                value: 0.10,
                condition: |_, is_exported, _, in_degree, _| is_exported && in_degree >= 5,
            },
        ],
        capsule_hints: CapsuleHints::default(),
    }
}

/// Weights for [`Intent::Refactor`]: prioritize centrality, boost exported/high-degree symbols.
fn refactor_weights(has_embeddings: bool) -> IntentWeights {
    let (w_bm25, w_tfidf, w_centrality, w_ngram, w_semantic) = if has_embeddings {
        (0.20, 0.20, 0.30, 0.15, 0.15)
    } else {
        (0.25, 0.25, 0.35, 0.15, 0.00)
    };
    IntentWeights {
        w_bm25,
        w_tfidf,
        w_centrality,
        w_ngram,
        w_semantic,
        boosts: vec![
            BoostRule {
                description: "Exported symbols (public API surface)",
                value: 0.25,
                condition: |_, is_exported, _, _, _| is_exported,
            },
            BoostRule {
                description: "High in-degree symbols (many callers)",
                value: 0.15,
                condition: |_, _, _, in_degree, _| in_degree >= 5,
            },
        ],
        capsule_hints: CapsuleHints {
            bfs_depth: 3,
            pivot_fraction: 0.70,
            include_skeleton_docs: false,
        },
    }
}

/// Weights for [`Intent::Explore`]: balanced, boost documented and central symbols.
fn explore_weights(has_embeddings: bool) -> IntentWeights {
    let (w_bm25, w_tfidf, w_centrality, w_ngram, w_semantic) = if has_embeddings {
        (0.30, 0.25, 0.20, 0.10, 0.15)
    } else {
        (0.35, 0.30, 0.25, 0.10, 0.00)
    };
    IntentWeights {
        w_bm25,
        w_tfidf,
        w_centrality,
        w_ngram,
        w_semantic,
        boosts: vec![
            BoostRule {
                description: "Symbols with docstrings",
                value: 0.10,
                condition: |_, _, has_docstring, _, _| has_docstring,
            },
            BoostRule {
                description: "High centrality symbols",
                value: 0.05,
                condition: |_, _, _, in_degree, _| in_degree >= 3,
            },
        ],
        capsule_hints: CapsuleHints::default(),
    }
}

/// Weights for [`Intent::Understand`]: prioritize TF-IDF, boost docs/modules/entry points.
fn understand_weights(has_embeddings: bool) -> IntentWeights {
    let (w_bm25, w_tfidf, w_centrality, w_ngram, w_semantic) = if has_embeddings {
        (0.25, 0.25, 0.20, 0.10, 0.20)
    } else {
        (0.30, 0.30, 0.25, 0.15, 0.00)
    };
    IntentWeights {
        w_bm25,
        w_tfidf,
        w_centrality,
        w_ngram,
        w_semantic,
        boosts: vec![
            BoostRule {
                description: "Symbols with docstrings",
                value: 0.20,
                condition: |_, _, has_docstring, _, _| has_docstring,
            },
            BoostRule {
                description: "Module/class/trait/interface symbols",
                value: 0.15,
                condition: |kind, _, _, _, _| {
                    matches!(
                        kind,
                        "module" | "class" | "trait" | "interface" | "namespace"
                    )
                },
            },
            BoostRule {
                description: "Entry points (no callers)",
                value: 0.10,
                condition: |_, _, _, in_degree, _| in_degree == 0,
            },
        ],
        capsule_hints: CapsuleHints {
            bfs_depth: 2,
            pivot_fraction: 0.85,
            include_skeleton_docs: true,
        },
    }
}

/// Weights for [`Intent::Test`]: balanced with centrality, boost test-related symbols.
fn test_weights(has_embeddings: bool) -> IntentWeights {
    let (w_bm25, w_tfidf, w_centrality, w_ngram, w_semantic) = if has_embeddings {
        (0.30, 0.25, 0.20, 0.10, 0.15)
    } else {
        (0.35, 0.30, 0.25, 0.10, 0.00)
    };
    IntentWeights {
        w_bm25,
        w_tfidf,
        w_centrality,
        w_ngram,
        w_semantic,
        boosts: vec![
            BoostRule {
                description: "Test files (*_test.*, *_spec.*, test_*.*)",
                value: 0.20,
                condition: |kind, _, _, _, _| {
                    let k = kind.to_lowercase();
                    k.contains("test") || k.contains("spec")
                },
            },
            BoostRule {
                description: "Symbols imported by tests (high in-degree)",
                value: 0.15,
                condition: |_, _, _, in_degree, _| in_degree >= 2,
            },
        ],
        capsule_hints: CapsuleHints::default(),
    }
}

/// Parses a string into an [`Intent`] variant (case-insensitive).
///
/// Returns `None` for unrecognized strings.
#[must_use]
pub fn parse_intent(s: &str) -> Option<Intent> {
    match s.to_lowercase().as_str() {
        "debug" => Some(Intent::Debug),
        "test" => Some(Intent::Test),
        "refactor" => Some(Intent::Refactor),
        "modify" => Some(Intent::Modify),
        "understand" => Some(Intent::Understand),
        "explore" => Some(Intent::Explore),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fix_auth_bug_is_debug() {
        assert_eq!(detect_intent("fix the auth bug"), Intent::Debug);
    }

    #[test]
    fn refactor_middleware_is_refactor() {
        assert_eq!(detect_intent("refactor the middleware"), Intent::Refactor);
    }

    #[test]
    fn add_rate_limiting_is_modify() {
        assert_eq!(detect_intent("add rate limiting"), Intent::Modify);
    }

    #[test]
    fn how_does_auth_work_is_understand() {
        assert_eq!(detect_intent("how does auth work"), Intent::Understand);
    }

    #[test]
    fn test_the_validator_is_test() {
        assert_eq!(detect_intent("test the validator"), Intent::Test);
    }

    #[test]
    fn find_the_config_is_explore() {
        assert_eq!(detect_intent("find the config"), Intent::Explore);
    }

    #[test]
    fn fix_the_tests_is_debug_tiebreak() {
        // "fix" matches Debug, "test" matches Test; Debug has higher priority.
        assert_eq!(detect_intent("fix the tests"), Intent::Debug);
    }

    #[test]
    fn just_looking_around_is_explore() {
        assert_eq!(detect_intent("just looking around"), Intent::Explore);
    }

    #[test]
    fn empty_query_is_explore() {
        assert_eq!(detect_intent(""), Intent::Explore);
    }

    #[test]
    fn intent_name_returns_lowercase() {
        assert_eq!(Intent::Debug.name(), "debug");
        assert_eq!(Intent::Refactor.name(), "refactor");
        assert_eq!(Intent::Modify.name(), "modify");
        assert_eq!(Intent::Explore.name(), "explore");
        assert_eq!(Intent::Understand.name(), "understand");
        assert_eq!(Intent::Test.name(), "test");
    }

    #[test]
    fn parse_intent_all_variants() {
        assert_eq!(parse_intent("debug"), Some(Intent::Debug));
        assert_eq!(parse_intent("test"), Some(Intent::Test));
        assert_eq!(parse_intent("refactor"), Some(Intent::Refactor));
        assert_eq!(parse_intent("modify"), Some(Intent::Modify));
        assert_eq!(parse_intent("understand"), Some(Intent::Understand));
        assert_eq!(parse_intent("explore"), Some(Intent::Explore));
    }

    #[test]
    fn parse_intent_case_insensitive() {
        assert_eq!(parse_intent("DEBUG"), Some(Intent::Debug));
        assert_eq!(parse_intent("Refactor"), Some(Intent::Refactor));
        assert_eq!(parse_intent("EXPLORE"), Some(Intent::Explore));
    }

    #[test]
    fn parse_intent_invalid_returns_none() {
        assert_eq!(parse_intent(""), None);
        assert_eq!(parse_intent("unknown"), None);
        assert_eq!(parse_intent("debugging"), None);
    }

    #[test]
    fn capsule_hints_valid_ranges() {
        let intents = [
            Intent::Debug,
            Intent::Refactor,
            Intent::Modify,
            Intent::Explore,
            Intent::Understand,
            Intent::Test,
        ];
        for intent in &intents {
            let hints = get_capsule_hints(intent);
            assert!(
                hints.bfs_depth >= 1 && hints.bfs_depth <= 5,
                "bfs_depth for {intent:?} out of range: {}",
                hints.bfs_depth,
            );
            assert!(
                hints.pivot_fraction > 0.0 && hints.pivot_fraction < 1.0,
                "pivot_fraction for {intent:?} out of range: {}",
                hints.pivot_fraction,
            );
        }
    }

    #[test]
    fn capsule_hints_per_intent_contract() {
        // Debug and Refactor use deeper BFS.
        assert_eq!(get_capsule_hints(&Intent::Debug).bfs_depth, 3);
        assert_eq!(get_capsule_hints(&Intent::Refactor).bfs_depth, 3);

        // Others use default depth 2.
        assert_eq!(get_capsule_hints(&Intent::Explore).bfs_depth, 2);
        assert_eq!(get_capsule_hints(&Intent::Modify).bfs_depth, 2);
        assert_eq!(get_capsule_hints(&Intent::Test).bfs_depth, 2);
        assert_eq!(get_capsule_hints(&Intent::Understand).bfs_depth, 2);

        // Refactor gives more budget to skeletons.
        assert!(
            get_capsule_hints(&Intent::Refactor).pivot_fraction
                < get_capsule_hints(&Intent::Explore).pivot_fraction,
        );

        // Only Understand includes skeleton docs.
        assert!(get_capsule_hints(&Intent::Understand).include_skeleton_docs);
        assert!(!get_capsule_hints(&Intent::Debug).include_skeleton_docs);
        assert!(!get_capsule_hints(&Intent::Explore).include_skeleton_docs);
    }

    #[test]
    fn capsule_hints_consistent_with_weights() {
        // get_capsule_hints must return the same values as the capsule_hints
        // field in get_weights, since both are the source of truth.
        let intents = [
            Intent::Debug,
            Intent::Refactor,
            Intent::Modify,
            Intent::Explore,
            Intent::Understand,
            Intent::Test,
        ];
        for intent in &intents {
            let from_weights = get_weights(intent, false).capsule_hints;
            let standalone = get_capsule_hints(intent);
            assert_eq!(
                from_weights.bfs_depth, standalone.bfs_depth,
                "bfs_depth mismatch for {intent:?}"
            );
            assert!(
                (from_weights.pivot_fraction - standalone.pivot_fraction).abs() < f64::EPSILON,
                "pivot_fraction mismatch for {intent:?}"
            );
            assert_eq!(
                from_weights.include_skeleton_docs, standalone.include_skeleton_docs,
                "include_skeleton_docs mismatch for {intent:?}"
            );
        }
    }

    #[test]
    fn modify_boosts_registry_files() {
        let w = get_weights(&Intent::Modify, false);
        let registry_boost = &w.boosts[0];
        // Registry files should trigger the boost.
        assert!((registry_boost.condition)(
            "function",
            true,
            false,
            0,
            "src/languages/mod.rs"
        ));
        assert!((registry_boost.condition)(
            "function",
            false,
            false,
            0,
            "src/index.ts"
        ));
        assert!((registry_boost.condition)(
            "function",
            false,
            false,
            0,
            "src/index.js"
        ));
        assert!((registry_boost.condition)(
            "class",
            false,
            false,
            0,
            "pkg/__init__.py"
        ));
        // Regular files should NOT trigger the boost.
        assert!(!(registry_boost.condition)(
            "function",
            true,
            false,
            0,
            "src/auth/service.ts"
        ));
        assert!(!(registry_boost.condition)(
            "function",
            true,
            false,
            0,
            "src/lib.rs"
        ));
    }

    #[test]
    fn modify_boosts_exported_extension_points() {
        let w = get_weights(&Intent::Modify, false);
        let extension_boost = &w.boosts[1];
        // Exported + high in-degree should trigger.
        assert!((extension_boost.condition)(
            "function",
            true,
            false,
            5,
            "src/lib.rs"
        ));
        assert!((extension_boost.condition)(
            "function",
            true,
            false,
            10,
            "src/auth.ts"
        ));
        // Not exported → no boost.
        assert!(!(extension_boost.condition)(
            "function",
            false,
            false,
            5,
            "src/lib.rs"
        ));
        // Low in-degree → no boost.
        assert!(!(extension_boost.condition)(
            "function",
            true,
            false,
            4,
            "src/lib.rs"
        ));
    }

    #[test]
    fn existing_boosts_unaffected_by_file_path_param() {
        // Debug boosts should work the same regardless of file_path.
        let w = get_weights(&Intent::Debug, false);
        assert!((w.boosts[0].condition)(
            "error_handler",
            false,
            false,
            0,
            "any.rs"
        ));
        assert!((w.boosts[1].condition)(
            "function", false, false, 3, "any.rs"
        ));

        // Refactor boosts.
        let w = get_weights(&Intent::Refactor, false);
        assert!((w.boosts[0].condition)(
            "function", true, false, 0, "any.rs"
        ));
        assert!((w.boosts[1].condition)(
            "function", false, false, 5, "any.rs"
        ));
    }

    #[test]
    fn weights_sum_to_one() {
        let intents = [
            Intent::Debug,
            Intent::Refactor,
            Intent::Modify,
            Intent::Explore,
            Intent::Understand,
            Intent::Test,
        ];
        for intent in &intents {
            let w = get_weights(intent, false);
            let sum = w.w_bm25 + w.w_tfidf + w.w_centrality + w.w_ngram + w.w_semantic;
            assert!(
                (sum - 1.0).abs() < 1e-10,
                "weights (no embeddings) for {intent:?} sum to {sum}, expected 1.0"
            );
        }
        for intent in &intents {
            let w = get_weights(intent, true);
            let sum = w.w_bm25 + w.w_tfidf + w.w_centrality + w.w_ngram + w.w_semantic;
            assert!(
                (sum - 1.0).abs() < 1e-10,
                "weights (with embeddings) for {intent:?} sum to {sum}, expected 1.0"
            );
        }
    }
}
