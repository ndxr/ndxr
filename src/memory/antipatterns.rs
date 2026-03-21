//! Anti-pattern detection framework for agent behavior analysis.
//!
//! Provides an extensible [`PatternRule`] trait and three built-in detectors
//! that identify unproductive patterns in agent sessions: dead-end symbol
//! churn, file thrashing, and circular search queries.

use std::collections::HashMap;

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;

use crate::indexer::tokenizer::{compute_tf, tokenize_text};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default detection window in seconds (5 minutes).
pub const DEFAULT_WINDOW_SECS: i64 = 300;

/// Minimum structural changes per file to flag as thrashing.
const FILE_THRASHING_THRESHOLD: i64 = 4;

/// Cosine similarity threshold for considering two queries "similar".
const CIRCULAR_SIMILARITY_THRESHOLD: f64 = 0.7;

/// Minimum similar consecutive queries to flag circular searching.
const CIRCULAR_MIN_OCCURRENCES: usize = 3;

/// Maximum recent observations to inspect for circular search detection.
const MAX_CIRCULAR_OBSERVATIONS: usize = 20;

/// Maximum dead-end query results.
const DEAD_END_QUERY_LIMIT: i64 = 100;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Extensible anti-pattern detection rule.
///
/// Implementations inspect recent session activity via the
/// [`DetectionContext`] and return zero or more detected [`AntiPattern`]s.
pub trait PatternRule: Send + Sync {
    /// Short identifier for this rule (e.g., `"dead_end"`).
    fn name(&self) -> &'static str;

    /// Human-readable description of what this rule detects.
    fn description(&self) -> &'static str;

    /// Runs the detection logic and returns any anti-patterns found.
    ///
    /// # Errors
    ///
    /// Returns an error if database queries fail.
    fn detect(&self, ctx: &DetectionContext<'_>) -> Result<Vec<AntiPattern>>;
}

/// Context passed to pattern detectors.
pub struct DetectionContext<'a> {
    /// Database connection for querying session history.
    pub conn: &'a Connection,
    /// Session to analyze.
    pub session_id: &'a str,
    /// How far back to look in seconds (duration, not a timestamp).
    pub window_secs: i64,
}

/// A detected anti-pattern.
#[derive(Debug, Clone, Serialize)]
pub struct AntiPattern {
    /// Name of the rule that triggered this detection.
    pub rule_name: String,
    /// Human-readable summary of the detected pattern.
    pub summary: String,
    /// Fully-qualified symbol names involved in the pattern.
    pub involved_fqns: Vec<String>,
    /// Severity classification.
    pub severity: Severity,
}

/// Severity classification for detected anti-patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Informational finding — may or may not indicate a problem.
    Info,
    /// Warning — likely indicates an unproductive pattern.
    Warning,
}

impl Severity {
    /// Returns the lowercase string representation (`"info"` or `"warning"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
        }
    }
}

/// Detects symbols that were added then removed within the detection window,
/// indicating dead-end exploratory work.
pub(crate) struct DeadEndDetector;

/// Detects files with excessive structural changes within the detection
/// window, indicating thrashing on the same files.
pub(crate) struct FileThrashingDetector;

/// Detects repeated similar search queries without a manual observation break,
/// indicating the agent is searching in circles.
pub(crate) struct CircularSearchDetector;

// ---------------------------------------------------------------------------
// Impls
// ---------------------------------------------------------------------------

impl PatternRule for DeadEndDetector {
    fn name(&self) -> &'static str {
        "dead_end"
    }

    fn description(&self) -> &'static str {
        "Symbols added then removed within the detection window"
    }

    fn detect(&self, ctx: &DetectionContext<'_>) -> Result<Vec<AntiPattern>> {
        let cutoff = crate::util::unix_now() - ctx.window_secs;
        let mut stmt = ctx
            .conn
            .prepare(
                "SELECT DISTINCT a.symbol_fqn FROM symbol_changes a \
                 JOIN symbol_changes b ON a.symbol_fqn = b.symbol_fqn \
                 WHERE a.change_kind = 'added' AND b.change_kind = 'removed' \
                   AND b.detected_at > a.detected_at \
                   AND b.detected_at - a.detected_at < ?1 \
                   AND a.session_id = ?2 AND a.detected_at > ?3 \
                 LIMIT ?4",
            )
            .context("prepare dead-end detector query")?;

        let rows = stmt
            .query_map(
                rusqlite::params![
                    ctx.window_secs,
                    ctx.session_id,
                    cutoff,
                    DEAD_END_QUERY_LIMIT,
                ],
                |row| row.get::<_, String>(0),
            )
            .context("execute dead-end detector query")?;

        let mut fqns = Vec::new();
        for row in rows {
            fqns.push(row.context("read dead-end row")?);
        }

        if fqns.is_empty() {
            return Ok(Vec::new());
        }

        let summary = format!(
            "{} symbol(s) added then removed within the detection window: {}",
            fqns.len(),
            fqns.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
        );

        Ok(vec![AntiPattern {
            rule_name: "dead_end".to_owned(),
            summary,
            involved_fqns: fqns,
            severity: Severity::Warning,
        }])
    }
}

impl PatternRule for FileThrashingDetector {
    fn name(&self) -> &'static str {
        "file_thrashing"
    }

    fn description(&self) -> &'static str {
        "Files with excessive structural changes within the detection window"
    }

    fn detect(&self, ctx: &DetectionContext<'_>) -> Result<Vec<AntiPattern>> {
        let cutoff = crate::util::unix_now() - ctx.window_secs;
        let mut stmt = ctx
            .conn
            .prepare(
                "SELECT file_path, COUNT(*) as change_count FROM symbol_changes \
                 WHERE detected_at > ?1 AND session_id = ?2 \
                 GROUP BY file_path HAVING COUNT(*) >= ?3",
            )
            .context("prepare file-thrashing detector query")?;

        let rows = stmt
            .query_map(
                rusqlite::params![cutoff, ctx.session_id, FILE_THRASHING_THRESHOLD],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .context("execute file-thrashing detector query")?;

        let mut patterns = Vec::new();
        for row in rows {
            let (file_path, change_count) = row.context("read file-thrashing row")?;
            patterns.push(AntiPattern {
                rule_name: "file_thrashing".to_owned(),
                summary: format!(
                    "File '{file_path}' has {change_count} structural changes within the detection window"
                ),
                involved_fqns: vec![file_path],
                severity: Severity::Warning,
            });
        }

        Ok(patterns)
    }
}

impl PatternRule for CircularSearchDetector {
    fn name(&self) -> &'static str {
        "circular_search"
    }

    fn description(&self) -> &'static str {
        "Repeated similar search queries without a manual observation break"
    }

    fn detect(&self, ctx: &DetectionContext<'_>) -> Result<Vec<AntiPattern>> {
        let cutoff = crate::util::unix_now() - ctx.window_secs;
        let mut stmt = ctx
            .conn
            .prepare(
                "SELECT kind, content FROM observations \
                 WHERE session_id = ?1 AND created_at > ?2 \
                 ORDER BY created_at DESC \
                 LIMIT ?3",
            )
            .context("prepare circular-search detector query")?;

        #[allow(clippy::cast_possible_wrap)] // MAX_CIRCULAR_OBSERVATIONS is small
        let rows = stmt
            .query_map(
                rusqlite::params![ctx.session_id, cutoff, MAX_CIRCULAR_OBSERVATIONS as i64,],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .context("execute circular-search detector query")?;

        let mut queries: Vec<String> = Vec::new();
        for row in rows {
            let (kind, content) = row.context("read circular-search row")?;
            // Manual observations break the chain.
            if kind == "manual" {
                break;
            }
            if let Some(query) = extract_query_from_content(&content) {
                queries.push(query);
            }
        }

        // Results come newest-first; reverse so we walk oldest-to-newest.
        queries.reverse();

        if queries.len() < CIRCULAR_MIN_OCCURRENCES {
            return Ok(Vec::new());
        }

        // Build TF vectors for each query.
        let tf_vectors: Vec<HashMap<String, f64>> = queries
            .iter()
            .map(|q| compute_tf(&tokenize_text(q)))
            .collect();

        // Find runs of consecutive similar queries.
        let mut run_start = 0;
        let mut best_run_start = 0;
        let mut best_run_len: usize = 1;
        let mut current_run_len: usize = 1;

        for i in 1..tf_vectors.len() {
            if cosine_similarity(&tf_vectors[i - 1], &tf_vectors[i])
                >= CIRCULAR_SIMILARITY_THRESHOLD
            {
                current_run_len += 1;
            } else {
                if current_run_len > best_run_len {
                    best_run_len = current_run_len;
                    best_run_start = run_start;
                }
                run_start = i;
                current_run_len = 1;
            }
        }
        if current_run_len > best_run_len {
            best_run_len = current_run_len;
            best_run_start = run_start;
        }

        if best_run_len < CIRCULAR_MIN_OCCURRENCES {
            return Ok(Vec::new());
        }

        let representative = &queries[best_run_start];
        Ok(vec![AntiPattern {
            rule_name: "circular_search".to_owned(),
            summary: format!(
                "{best_run_len} similar consecutive searches detected (e.g., '{representative}')"
            ),
            involved_fqns: Vec::new(),
            severity: Severity::Info,
        }])
    }
}

// ---------------------------------------------------------------------------
// Public functions
// ---------------------------------------------------------------------------

/// Runs all pattern detectors and collects results.
///
/// Individual detector failures are logged via `tracing::warn!` and do not
/// prevent other detectors from running.
///
/// # Errors
///
/// Returns an error only if result aggregation itself fails (currently
/// infallible beyond detector errors, which are logged and skipped).
pub fn run_all_detectors(
    ctx: &DetectionContext<'_>,
    rules: &[Box<dyn PatternRule>],
) -> Result<Vec<AntiPattern>> {
    let mut results = Vec::new();
    for rule in rules {
        match rule.detect(ctx) {
            Ok(patterns) => results.extend(patterns),
            Err(e) => {
                tracing::warn!(rule = rule.name(), "anti-pattern detector failed: {e:#}");
            }
        }
    }
    Ok(results)
}

/// Returns the default set of built-in detectors.
#[must_use]
pub fn default_detectors() -> Vec<Box<dyn PatternRule>> {
    vec![
        Box::new(DeadEndDetector),
        Box::new(FileThrashingDetector),
        Box::new(CircularSearchDetector),
    ]
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Extracts the query string from an auto-observation's content.
///
/// Looks for a `"Query: "` prefix and extracts text until the next `". "`
/// separator or end of string.
fn extract_query_from_content(content: &str) -> Option<String> {
    let prefix = "Query: ";
    let start = content.find(prefix)? + prefix.len();
    let rest = &content[start..];
    let end = rest.find(". ").unwrap_or(rest.len());
    let query = rest[..end].trim();
    if query.is_empty() {
        return None;
    }
    Some(query.to_owned())
}

/// Computes cosine similarity between two TF vectors.
///
/// Returns 0.0 if either vector is empty or both have zero magnitude.
fn cosine_similarity(a: &HashMap<String, f64>, b: &HashMap<String, f64>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0;
    for (term, a_val) in a {
        if let Some(b_val) = b.get(term) {
            dot += a_val * b_val;
        }
    }

    let mag_a: f64 = a.values().map(|v| v * v).sum::<f64>().sqrt();
    let mag_b: f64 = b.values().map(|v| v * v).sum::<f64>().sqrt();

    if mag_a == 0.0 || mag_b == 0.0 {
        return 0.0;
    }

    dot / (mag_a * mag_b)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db::open_or_create;
    use tempfile::TempDir;

    #[test]
    fn extract_query_from_auto_content() {
        let content = "Tool: run_pipeline. Intent: debug. Query: fix auth bug. Result: 5 symbols";
        let query = extract_query_from_content(content).unwrap();
        assert_eq!(query, "fix auth bug");
    }

    #[test]
    fn extract_query_missing() {
        let content = "Tool: get_skeleton. Result: rendered 3 files";
        assert!(extract_query_from_content(content).is_none());
    }

    #[test]
    fn cosine_similarity_identical() {
        let tokens = tokenize_text("fix authentication bug");
        let tf = compute_tf(&tokens);
        let sim = cosine_similarity(&tf, &tf);
        assert!((sim - 1.0).abs() < 1e-9, "expected ~1.0, got {sim}");
    }

    #[test]
    fn cosine_similarity_orthogonal() {
        let tf_a = compute_tf(&tokenize_text("authentication login"));
        let tf_b = compute_tf(&tokenize_text("rendering graphics"));
        let sim = cosine_similarity(&tf_a, &tf_b);
        assert!(sim.abs() < 1e-9, "expected ~0.0, got {sim}");
    }

    #[test]
    fn dead_end_detector_no_data() {
        let tmp = TempDir::new().unwrap();
        let conn = open_or_create(&tmp.path().join("test.db")).unwrap();
        let ctx = DetectionContext {
            conn: &conn,
            session_id: "test-session",
            window_secs: 0,
        };
        let detector = DeadEndDetector;
        let results = detector.detect(&ctx).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn file_thrashing_detector_no_data() {
        let tmp = TempDir::new().unwrap();
        let conn = open_or_create(&tmp.path().join("test.db")).unwrap();
        let ctx = DetectionContext {
            conn: &conn,
            session_id: "test-session",
            window_secs: 0,
        };
        let detector = FileThrashingDetector;
        let results = detector.detect(&ctx).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn circular_search_detector_no_data() {
        let tmp = TempDir::new().unwrap();
        let conn = open_or_create(&tmp.path().join("test.db")).unwrap();
        let ctx = DetectionContext {
            conn: &conn,
            session_id: "test-session",
            window_secs: 0,
        };
        let detector = CircularSearchDetector;
        let results = detector.detect(&ctx).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn default_detectors_returns_three() {
        let detectors = default_detectors();
        assert_eq!(detectors.len(), 3);
        assert_eq!(detectors[0].name(), "dead_end");
        assert_eq!(detectors[1].name(), "file_thrashing");
        assert_eq!(detectors[2].name(), "circular_search");
    }

    #[test]
    fn dead_end_detector_finds_added_then_removed() {
        let tmp = TempDir::new().unwrap();
        let conn = open_or_create(&tmp.path().join("test.db")).unwrap();
        let session = "sess-1";
        let now = crate::util::unix_now();

        conn.execute(
            "INSERT INTO symbol_changes (symbol_fqn, file_path, change_kind, session_id, detected_at)
             VALUES ('test::foo', 'src/test.rs', 'added', ?1, ?2)",
            rusqlite::params![session, now - 200],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO symbol_changes (symbol_fqn, file_path, change_kind, session_id, detected_at)
             VALUES ('test::foo', 'src/test.rs', 'removed', ?1, ?2)",
            rusqlite::params![session, now - 100],
        )
        .unwrap();

        let ctx = DetectionContext {
            conn: &conn,
            session_id: session,
            window_secs: DEFAULT_WINDOW_SECS,
        };
        let results = DeadEndDetector.detect(&ctx).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].rule_name, "dead_end");
        assert_eq!(results[0].severity, Severity::Warning);
        assert!(results[0].involved_fqns.contains(&"test::foo".to_owned()));
    }

    #[test]
    fn file_thrashing_detector_triggers_at_threshold() {
        let tmp = TempDir::new().unwrap();
        let conn = open_or_create(&tmp.path().join("test.db")).unwrap();
        let session = "sess-1";
        let now = crate::util::unix_now();

        // Insert FILE_THRASHING_THRESHOLD (4) changes to same file.
        for i in 0..FILE_THRASHING_THRESHOLD {
            conn.execute(
                "INSERT INTO symbol_changes (symbol_fqn, file_path, change_kind, session_id, detected_at)
                 VALUES (?1, 'src/hot.rs', 'body_changed', ?2, ?3)",
                rusqlite::params![format!("hot::fn{i}"), session, now - 100 + i],
            )
            .unwrap();
        }

        let ctx = DetectionContext {
            conn: &conn,
            session_id: session,
            window_secs: DEFAULT_WINDOW_SECS,
        };
        let results = FileThrashingDetector.detect(&ctx).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].rule_name, "file_thrashing");
        assert_eq!(results[0].severity, Severity::Warning);
    }

    #[test]
    fn file_thrashing_below_threshold_no_match() {
        let tmp = TempDir::new().unwrap();
        let conn = open_or_create(&tmp.path().join("test.db")).unwrap();
        let session = "sess-1";
        let now = crate::util::unix_now();

        // Insert FILE_THRASHING_THRESHOLD - 1 changes — should NOT trigger.
        for i in 0..(FILE_THRASHING_THRESHOLD - 1) {
            conn.execute(
                "INSERT INTO symbol_changes (symbol_fqn, file_path, change_kind, session_id, detected_at)
                 VALUES (?1, 'src/ok.rs', 'body_changed', ?2, ?3)",
                rusqlite::params![format!("ok::fn{i}"), session, now - 100 + i],
            )
            .unwrap();
        }

        let ctx = DetectionContext {
            conn: &conn,
            session_id: session,
            window_secs: DEFAULT_WINDOW_SECS,
        };
        let results = FileThrashingDetector.detect(&ctx).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn circular_search_detector_finds_repeated_queries() {
        let tmp = TempDir::new().unwrap();
        let conn = open_or_create(&tmp.path().join("test.db")).unwrap();
        let session_id = crate::memory::store::create_session(&conn).unwrap();
        let now = crate::util::unix_now();

        // Insert CIRCULAR_MIN_OCCURRENCES (3) auto observations with similar queries.
        for i in 0..CIRCULAR_MIN_OCCURRENCES {
            #[allow(clippy::cast_possible_wrap)]
            let ts = now - (CIRCULAR_MIN_OCCURRENCES as i64 - i as i64);
            conn.execute(
                "INSERT INTO observations (session_id, kind, content, detail_level, is_stale, created_at)
                 VALUES (?1, 'auto', ?2, 2, 0, ?3)",
                rusqlite::params![
                    session_id,
                    format!("Tool: run_pipeline. Intent: debug. Query: fix the auth bug. Result: 3 pivots"),
                    ts,
                ],
            )
            .unwrap();
        }

        let ctx = DetectionContext {
            conn: &conn,
            session_id: &session_id,
            window_secs: DEFAULT_WINDOW_SECS,
        };
        let results = CircularSearchDetector.detect(&ctx).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].rule_name, "circular_search");
        assert_eq!(results[0].severity, Severity::Info);
        assert!(
            results[0].summary.contains("similar consecutive searches"),
            "summary should mention similar searches: {}",
            results[0].summary
        );
    }

    #[test]
    fn circular_search_broken_by_manual_observation() {
        let tmp = TempDir::new().unwrap();
        let conn = open_or_create(&tmp.path().join("test.db")).unwrap();
        let session_id = crate::memory::store::create_session(&conn).unwrap();
        let now = crate::util::unix_now();

        // Two similar auto observations, then a manual break, then two more.
        // Neither run reaches CIRCULAR_MIN_OCCURRENCES (3).
        for i in 0..2 {
            conn.execute(
                "INSERT INTO observations (session_id, kind, content, detail_level, is_stale, created_at)
                 VALUES (?1, 'auto', 'Tool: run_pipeline. Query: fix auth bug. Result: ok', 2, 0, ?2)",
                rusqlite::params![session_id, now - 10 + i],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO observations (session_id, kind, content, detail_level, is_stale, created_at)
             VALUES (?1, 'manual', 'User saved an insight', 2, 0, ?2)",
            rusqlite::params![session_id, now - 5],
        )
        .unwrap();
        for i in 0..2 {
            conn.execute(
                "INSERT INTO observations (session_id, kind, content, detail_level, is_stale, created_at)
                 VALUES (?1, 'auto', 'Tool: run_pipeline. Query: fix auth bug. Result: ok', 2, 0, ?2)",
                rusqlite::params![session_id, now - 2 + i],
            )
            .unwrap();
        }

        let ctx = DetectionContext {
            conn: &conn,
            session_id: &session_id,
            window_secs: DEFAULT_WINDOW_SECS,
        };
        let results = CircularSearchDetector.detect(&ctx).unwrap();
        assert!(
            results.is_empty(),
            "manual observation should break the chain"
        );
    }

    #[test]
    fn cosine_similarity_partial_overlap() {
        let a = HashMap::from([("fix".to_owned(), 0.5), ("auth".to_owned(), 0.5)]);
        let b = HashMap::from([("fix".to_owned(), 0.5), ("login".to_owned(), 0.5)]);
        let sim = cosine_similarity(&a, &b);
        assert!(
            sim > 0.0 && sim < 1.0,
            "partial overlap should be between 0 and 1, got {sim}"
        );
    }
}
