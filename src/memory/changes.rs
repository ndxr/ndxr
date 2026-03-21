//! AST structural diff detection and storage.
//!
//! Compares pre-index and post-index symbol snapshots to detect structural
//! changes (additions, removals, signature changes, visibility changes,
//! renames, body changes) and stores them in the `symbol_changes` table.

use std::collections::HashMap;

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;

use crate::storage::db::{BATCH_PARAM_LIMIT, build_batch_placeholders};
use crate::util::unix_now;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Time window (seconds) for correlating a change with the nearest
/// auto-observation.
const CORRELATION_WINDOW_SECS: i64 = 120;

/// Maximum Levenshtein distance for rename candidate pairing.
const RENAME_MAX_LEVENSHTEIN: usize = 3;

/// Maximum relative length difference for rename candidate pairing.
const RENAME_MAX_LENGTH_DIFF_RATIO: f64 = 0.20;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Type of structural change detected for a symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// Symbol was added to the codebase.
    Added,
    /// Symbol was removed from the codebase.
    Removed,
    /// Symbol's type signature changed.
    SignatureChanged,
    /// Symbol's visibility (exported/private) changed.
    VisibilityChanged,
    /// Symbol was renamed (paired addition + removal in the same file).
    Renamed,
    /// Symbol's implementation body changed.
    BodyChanged,
}

impl ChangeKind {
    /// Returns the `snake_case` string representation.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::SignatureChanged => "signature_changed",
            Self::VisibilityChanged => "visibility_changed",
            Self::Renamed => "renamed",
            Self::BodyChanged => "body_changed",
        }
    }
}

/// A detected structural change for a symbol.
#[derive(Debug, Clone)]
pub struct SymbolDiff {
    /// Fully-qualified name of the changed symbol.
    pub fqn: String,
    /// File path where the change occurred.
    pub file_path: String,
    /// The type of structural change.
    pub kind: ChangeKind,
    /// Previous value (signature, visibility, body hash, or old FQN for renames).
    pub old_value: Option<String>,
    /// New value (signature, visibility, body hash, or new FQN for renames).
    pub new_value: Option<String>,
}

/// Pre-index snapshot entry: `(signature, body_hash, is_exported, file_path)`.
pub(crate) type SnapshotEntry = (Option<String>, Option<String>, bool, String);

/// Pre-index snapshot keyed by FQN.
pub(crate) type SymbolSnapshot = HashMap<String, SnapshotEntry>;

/// A recent symbol change for capsule surfacing.
#[derive(Debug, Clone, Serialize)]
pub struct RecentChange {
    /// Fully-qualified name of the changed symbol.
    pub fqn: String,
    /// The type of change (`added`, `removed`, etc.).
    pub change_kind: String,
    /// Previous value, if applicable.
    pub old_value: Option<String>,
    /// New value, if applicable.
    pub new_value: Option<String>,
    /// Unix timestamp when the change was detected.
    pub detected_at: i64,
}

// ---------------------------------------------------------------------------
// Public functions
// ---------------------------------------------------------------------------

/// Captures a pre-index snapshot of symbol state for the given FQNs and
/// deleted file paths.
///
/// Batch-queries the database for existing symbols matching `fqns` and
/// symbols in `deleted_paths`, returning `(signature, body_hash, is_exported,
/// file_path)` keyed by FQN.
///
/// # Errors
///
/// Returns an error if any database query fails.
pub fn snapshot_symbol_state(
    conn: &Connection,
    fqns: &[&str],
    deleted_paths: &[String],
) -> Result<SymbolSnapshot> {
    let mut snapshot = SymbolSnapshot::new();

    // Batch-query by FQN.
    for chunk in fqns.chunks(BATCH_PARAM_LIMIT) {
        let placeholders = build_batch_placeholders(chunk.len());
        let sql = format!(
            "SELECT s.fqn, s.signature, s.body_hash, s.is_exported, f.path \
             FROM symbols s JOIN files f ON s.file_id = f.id \
             WHERE s.fqn IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql).context("prepare snapshot by FQN")?;
        let params: Vec<&dyn rusqlite::types::ToSql> = chunk
            .iter()
            .map(|fqn| fqn as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt
            .query_map(params.as_slice(), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .context("query snapshot by FQN")?;
        for row in rows {
            let (fqn, sig, body_hash, exported, path) = row.context("read snapshot FQN row")?;
            snapshot.insert(fqn, (sig, body_hash, exported, path));
        }
    }

    // Batch-query symbols in deleted file paths.
    for chunk in deleted_paths.chunks(BATCH_PARAM_LIMIT) {
        let placeholders = build_batch_placeholders(chunk.len());
        let sql = format!(
            "SELECT s.fqn, s.signature, s.body_hash, s.is_exported, f.path \
             FROM symbols s JOIN files f ON s.file_id = f.id \
             WHERE f.path IN ({placeholders})"
        );
        let mut stmt = conn
            .prepare(&sql)
            .context("prepare snapshot by deleted paths")?;
        let params: Vec<&dyn rusqlite::types::ToSql> = chunk
            .iter()
            .map(|p| p as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt
            .query_map(params.as_slice(), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .context("query snapshot by deleted paths")?;
        for row in rows {
            let (fqn, sig, body_hash, exported, path) =
                row.context("read snapshot deleted-path row")?;
            snapshot
                .entry(fqn)
                .or_insert((sig, body_hash, exported, path));
        }
    }

    Ok(snapshot)
}

/// Compares a pre-index snapshot against post-index DB state and detects
/// structural diffs.
///
/// Detects: `Added`, `Removed`, `SignatureChanged`, `VisibilityChanged`,
/// `BodyChanged`, and `Renamed` (paired removal + addition in the same file
/// with similar names). When multiple changes occur for a symbol, only the
/// highest-priority change is reported: signature > visibility > body.
///
/// # Errors
///
/// Returns an error if any database query fails.
pub fn detect_symbol_diffs(
    conn: &Connection,
    pre_snapshot: &SymbolSnapshot,
    reindexed_paths: &[String],
) -> Result<Vec<SymbolDiff>> {
    if pre_snapshot.is_empty() && reindexed_paths.is_empty() {
        return Ok(Vec::new());
    }

    let post_state = load_post_index_state(conn, reindexed_paths)?;

    let mut diffs = Vec::new();
    let mut removed = Vec::new();
    let mut added = Vec::new();

    compare_snapshots(
        pre_snapshot,
        &post_state,
        &mut diffs,
        &mut removed,
        &mut added,
    );
    resolve_renames(&mut diffs, removed, added);

    Ok(diffs)
}

/// Inserts detected diffs into the `symbol_changes` table.
///
/// Correlates each change with the nearest auto-observation within a
/// [`CORRELATION_WINDOW_SECS`] window. Returns the count of stored changes.
///
/// # Errors
///
/// Returns an error if any database insert fails.
pub fn store_symbol_changes(
    conn: &Connection,
    diffs: &[SymbolDiff],
    session_id: Option<&str>,
) -> Result<usize> {
    if diffs.is_empty() {
        return Ok(0);
    }

    let now = unix_now();
    let window_start = now - CORRELATION_WINDOW_SECS;

    // Find the nearest auto-observation within the correlation window.
    let correlated_obs_id: Option<i64> = session_id.and_then(|sid| {
        match conn.query_row(
            "SELECT id FROM observations \
             WHERE session_id = ?1 AND kind = 'auto' AND created_at >= ?2 \
             ORDER BY created_at DESC LIMIT 1",
            rusqlite::params![sid, window_start],
            |row| row.get(0),
        ) {
            Ok(id) => Some(id),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => {
                tracing::warn!("failed to query correlated observation: {e}");
                None
            }
        }
    });

    let tx = conn
        .unchecked_transaction()
        .context("begin store_symbol_changes transaction")?;

    for diff in diffs {
        tx.execute(
            "INSERT INTO symbol_changes \
             (symbol_fqn, file_path, change_kind, old_value, new_value, \
              session_id, correlated_observation_id, detected_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                diff.fqn,
                diff.file_path,
                diff.kind.as_str(),
                diff.old_value,
                diff.new_value,
                session_id,
                correlated_obs_id,
                now,
            ],
        )
        .context("insert symbol change")?;
    }

    tx.commit().context("commit store_symbol_changes")?;
    Ok(diffs.len())
}

/// Queries recent changes for given FQNs since a timestamp.
///
/// Results are sorted by `detected_at` descending and truncated to `limit`.
///
/// # Errors
///
/// Returns an error if any database query fails.
#[allow(clippy::cast_possible_wrap)] // small usize limit fits in i64
pub fn query_recent_changes(
    conn: &Connection,
    fqns: &[String],
    since: i64,
    limit: usize,
) -> Result<Vec<RecentChange>> {
    if fqns.is_empty() {
        return Ok(Vec::new());
    }

    let mut all_changes = Vec::new();

    for chunk in fqns.chunks(BATCH_PARAM_LIMIT) {
        let placeholders = build_batch_placeholders(chunk.len());
        let sql = format!(
            "SELECT symbol_fqn, change_kind, old_value, new_value, detected_at \
             FROM symbol_changes \
             WHERE symbol_fqn IN ({placeholders}) AND detected_at >= ?{} \
             ORDER BY detected_at DESC",
            chunk.len() + 1
        );
        let mut stmt = conn.prepare(&sql).context("prepare query_recent_changes")?;

        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = chunk
            .iter()
            .map(|fqn| Box::new(fqn.clone()) as Box<dyn rusqlite::types::ToSql>)
            .collect();
        params.push(Box::new(since));

        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|p| p.as_ref() as &dyn rusqlite::types::ToSql)
            .collect();

        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok(RecentChange {
                    fqn: row.get(0)?,
                    change_kind: row.get(1)?,
                    old_value: row.get(2)?,
                    new_value: row.get(3)?,
                    detected_at: row.get(4)?,
                })
            })
            .context("query recent changes")?;

        for row in rows {
            all_changes.push(row.context("read recent change row")?);
        }
    }

    // Sort all collected changes by detected_at DESC and truncate.
    all_changes.sort_by(|a, b| b.detected_at.cmp(&a.detected_at));
    all_changes.truncate(limit);
    Ok(all_changes)
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Loads the post-index symbol state for the given file paths.
fn load_post_index_state(conn: &Connection, reindexed_paths: &[String]) -> Result<SymbolSnapshot> {
    let mut post_state = SymbolSnapshot::new();
    for chunk in reindexed_paths.chunks(BATCH_PARAM_LIMIT) {
        let placeholders = build_batch_placeholders(chunk.len());
        let sql = format!(
            "SELECT s.fqn, s.signature, s.body_hash, s.is_exported, f.path \
             FROM symbols s JOIN files f ON s.file_id = f.id \
             WHERE f.path IN ({placeholders})"
        );
        let mut stmt = conn
            .prepare(&sql)
            .context("prepare post-index state query")?;
        let params: Vec<&dyn rusqlite::types::ToSql> = chunk
            .iter()
            .map(|p| p as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt
            .query_map(params.as_slice(), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .context("query post-index state")?;
        for row in rows {
            let (fqn, sig, body_hash, exported, path) = row.context("read post-index row")?;
            post_state.insert(fqn, (sig, body_hash, exported, path));
        }
    }
    Ok(post_state)
}

/// Compares pre and post snapshots, partitioning into modified, removed, and
/// added diffs.
fn compare_snapshots(
    pre_snapshot: &SymbolSnapshot,
    post_state: &SymbolSnapshot,
    diffs: &mut Vec<SymbolDiff>,
    removed: &mut Vec<SymbolDiff>,
    added: &mut Vec<SymbolDiff>,
) {
    for (fqn, (pre_sig, pre_body, pre_exported, pre_path)) in pre_snapshot {
        if let Some((post_sig, post_body, post_exported, _)) = post_state.get(fqn) {
            // Priority: signature > visibility > body (only highest reported).
            if pre_sig != post_sig {
                diffs.push(SymbolDiff {
                    fqn: fqn.clone(),
                    file_path: pre_path.clone(),
                    kind: ChangeKind::SignatureChanged,
                    old_value: pre_sig.clone(),
                    new_value: post_sig.clone(),
                });
            } else if pre_exported != post_exported {
                diffs.push(SymbolDiff {
                    fqn: fqn.clone(),
                    file_path: pre_path.clone(),
                    kind: ChangeKind::VisibilityChanged,
                    old_value: Some(visibility_label(*pre_exported)),
                    new_value: Some(visibility_label(*post_exported)),
                });
            } else if pre_body != post_body {
                diffs.push(SymbolDiff {
                    fqn: fqn.clone(),
                    file_path: pre_path.clone(),
                    kind: ChangeKind::BodyChanged,
                    old_value: pre_body.clone(),
                    new_value: post_body.clone(),
                });
            }
        } else {
            removed.push(SymbolDiff {
                fqn: fqn.clone(),
                file_path: pre_path.clone(),
                kind: ChangeKind::Removed,
                old_value: pre_sig.clone(),
                new_value: None,
            });
        }
    }

    for (fqn, (post_sig, _, _, post_path)) in post_state {
        if !pre_snapshot.contains_key(fqn) {
            added.push(SymbolDiff {
                fqn: fqn.clone(),
                file_path: post_path.clone(),
                kind: ChangeKind::Added,
                old_value: None,
                new_value: post_sig.clone(),
            });
        }
    }
}

/// Maximum candidates for rename detection to avoid quadratic blowup.
const MAX_RENAME_CANDIDATES: usize = 200;

/// Pairs removed+added symbols in the same file with similar names as renames,
/// then appends unmatched removals and additions to `diffs`.
fn resolve_renames(diffs: &mut Vec<SymbolDiff>, removed: Vec<SymbolDiff>, added: Vec<SymbolDiff>) {
    // Guard against quadratic blowup on large refactors.
    if removed.len() > MAX_RENAME_CANDIDATES || added.len() > MAX_RENAME_CANDIDATES {
        diffs.extend(removed);
        diffs.extend(added);
        return;
    }

    let mut matched_removed = vec![false; removed.len()];
    let mut matched_added = vec![false; added.len()];

    for (ri, rem) in removed.iter().enumerate() {
        let rem_name = extract_short_name(&rem.fqn);
        for (ai, add) in added.iter().enumerate() {
            if matched_added[ai] || rem.file_path != add.file_path {
                continue;
            }
            let add_name = extract_short_name(&add.fqn);
            if is_rename_candidate(rem_name, add_name) {
                diffs.push(SymbolDiff {
                    fqn: add.fqn.clone(),
                    file_path: rem.file_path.clone(),
                    kind: ChangeKind::Renamed,
                    old_value: Some(rem.fqn.clone()),
                    new_value: Some(add.fqn.clone()),
                });
                matched_removed[ri] = true;
                matched_added[ai] = true;
                break;
            }
        }
    }

    for (i, rem) in removed.into_iter().enumerate() {
        if !matched_removed[i] {
            diffs.push(rem);
        }
    }
    for (i, add) in added.into_iter().enumerate() {
        if !matched_added[i] {
            diffs.push(add);
        }
    }
}

fn visibility_label(exported: bool) -> String {
    if exported {
        "exported".to_owned()
    } else {
        "private".to_owned()
    }
}

/// Extracts the short name from a fully-qualified name (last segment after `::`).
fn extract_short_name(fqn: &str) -> &str {
    fqn.rsplit_once("::").map_or(fqn, |(_, name)| name)
}

/// Checks whether two symbol names are similar enough to be a rename candidate.
///
/// Requires: length difference <= 20% of the longer name, and Levenshtein
/// distance <= 3.
fn is_rename_candidate(old_name: &str, new_name: &str) -> bool {
    let max_len = old_name.len().max(new_name.len());
    if max_len == 0 {
        return false;
    }

    let diff = old_name.len().abs_diff(new_name.len());
    // Allow up to ceil(max_len * ratio) characters of length difference.
    #[allow(clippy::cast_precision_loss)] // usize->f64 loss negligible for name lengths
    #[allow(clippy::cast_possible_truncation)] // result is a small positive count
    #[allow(clippy::cast_sign_loss)] // ceil of a positive product is non-negative
    let max_diff = (max_len as f64 * RENAME_MAX_LENGTH_DIFF_RATIO).ceil() as usize;
    if diff > max_diff {
        return false;
    }

    levenshtein(old_name, new_name) <= RENAME_MAX_LEVENSHTEIN
}

/// Computes the Levenshtein (edit) distance between two strings.
fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let a_len = a_chars.len();
    let b_len = b_chars.len();

    if a_len == 0 {
        return b_len;
    }
    if b_len == 0 {
        return a_len;
    }

    // Use single-row rolling DP for space efficiency.
    let mut prev: Vec<usize> = (0..=b_len).collect();
    let mut curr = vec![0; b_len + 1];

    for (i, a_ch) in a_chars.iter().enumerate() {
        curr[0] = i + 1;
        for (j, b_ch) in b_chars.iter().enumerate() {
            let cost = usize::from(a_ch != b_ch);
            curr[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(curr[j] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[b_len]
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
    fn levenshtein_identical() {
        assert_eq!(levenshtein("foo", "foo"), 0);
    }

    #[test]
    fn levenshtein_one_edit() {
        assert_eq!(levenshtein("foo", "foos"), 1);
        assert_eq!(levenshtein("foo", "bar"), 3);
    }

    #[test]
    fn rename_candidate_similar_names() {
        assert!(is_rename_candidate("foo", "foos"));
        assert!(is_rename_candidate("validate", "validata"));
    }

    #[test]
    fn rename_candidate_rejects_dissimilar() {
        assert!(!is_rename_candidate("foo", "completely_different"));
    }

    #[test]
    fn change_kind_serializes_snake_case() {
        let json = serde_json::to_string(&ChangeKind::SignatureChanged).unwrap();
        assert_eq!(json, "\"signature_changed\"");

        let json = serde_json::to_string(&ChangeKind::VisibilityChanged).unwrap();
        assert_eq!(json, "\"visibility_changed\"");

        let json = serde_json::to_string(&ChangeKind::BodyChanged).unwrap();
        assert_eq!(json, "\"body_changed\"");
    }

    #[test]
    fn store_and_query_changes() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.db");
        let conn = open_or_create(&db_path).unwrap();

        let diffs = vec![
            SymbolDiff {
                fqn: "crate::foo::bar".to_owned(),
                file_path: "src/foo.rs".to_owned(),
                kind: ChangeKind::SignatureChanged,
                old_value: Some("fn bar(x: i32)".to_owned()),
                new_value: Some("fn bar(x: i64)".to_owned()),
            },
            SymbolDiff {
                fqn: "crate::foo::baz".to_owned(),
                file_path: "src/foo.rs".to_owned(),
                kind: ChangeKind::Added,
                old_value: None,
                new_value: Some("fn baz()".to_owned()),
            },
        ];

        let count = store_symbol_changes(&conn, &diffs, None).unwrap();
        assert_eq!(count, 2);

        let fqns = vec!["crate::foo::bar".to_owned(), "crate::foo::baz".to_owned()];
        let results = query_recent_changes(&conn, &fqns, 0, 10).unwrap();
        assert_eq!(results.len(), 2);

        // Verify content — results are sorted by detected_at DESC, both have same
        // timestamp so check both are present.
        let kinds: Vec<&str> = results.iter().map(|r| r.change_kind.as_str()).collect();
        assert!(kinds.contains(&"signature_changed"));
        assert!(kinds.contains(&"added"));

        let sig_change = results
            .iter()
            .find(|r| r.change_kind == "signature_changed")
            .unwrap();
        assert_eq!(sig_change.fqn, "crate::foo::bar");
        assert_eq!(sig_change.old_value.as_deref(), Some("fn bar(x: i32)"));
        assert_eq!(sig_change.new_value.as_deref(), Some("fn bar(x: i64)"));
    }

    #[test]
    fn detect_diffs_finds_signature_change() {
        let tmp = TempDir::new().unwrap();
        let conn = open_or_create(&tmp.path().join("test.db")).unwrap();

        // Insert a file and symbol.
        conn.execute_batch(
            "INSERT INTO files (path, language, blake3_hash, line_count, byte_size, indexed_at)
             VALUES ('src/test.rs', 'rust', 'hash1', 10, 100, 1000);",
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO symbols (file_id, name, kind, fqn, start_line, end_line, is_exported, signature, body_hash)
             VALUES (?1, 'foo', 'function', 'test::foo', 1, 5, 1, 'fn foo(x: i32)', 'body1')",
            [file_id],
        )
        .unwrap();

        // Build pre-snapshot.
        let pre = snapshot_symbol_state(&conn, &["test::foo"], &[]).unwrap();
        assert_eq!(pre.len(), 1);

        // Simulate post-index: change the signature.
        conn.execute(
            "UPDATE symbols SET signature = 'fn foo(x: i64)' WHERE fqn = 'test::foo'",
            [],
        )
        .unwrap();

        let diffs = detect_symbol_diffs(&conn, &pre, &["src/test.rs".to_owned()]).unwrap();
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].kind, ChangeKind::SignatureChanged);
        assert_eq!(diffs[0].old_value, Some("fn foo(x: i32)".to_owned()));
        assert_eq!(diffs[0].new_value, Some("fn foo(x: i64)".to_owned()));
    }

    #[test]
    fn detect_diffs_finds_removed_symbol() {
        let tmp = TempDir::new().unwrap();
        let conn = open_or_create(&tmp.path().join("test.db")).unwrap();

        conn.execute_batch(
            "INSERT INTO files (path, language, blake3_hash, line_count, byte_size, indexed_at)
             VALUES ('src/test.rs', 'rust', 'hash1', 10, 100, 1000);",
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO symbols (file_id, name, kind, fqn, start_line, end_line, is_exported, signature)
             VALUES (?1, 'gone', 'function', 'test::gone', 1, 5, 0, 'fn gone()')",
            [file_id],
        )
        .unwrap();

        let pre = snapshot_symbol_state(&conn, &["test::gone"], &[]).unwrap();

        // Simulate post-index: symbol removed.
        conn.execute("DELETE FROM symbols WHERE fqn = 'test::gone'", [])
            .unwrap();

        let diffs = detect_symbol_diffs(&conn, &pre, &["src/test.rs".to_owned()]).unwrap();
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].kind, ChangeKind::Removed);
    }

    #[test]
    fn detect_diffs_visibility_change() {
        let tmp = TempDir::new().unwrap();
        let conn = open_or_create(&tmp.path().join("test.db")).unwrap();

        conn.execute_batch(
            "INSERT INTO files (path, language, blake3_hash, line_count, byte_size, indexed_at)
             VALUES ('src/test.rs', 'rust', 'hash1', 10, 100, 1000);",
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO symbols (file_id, name, kind, fqn, start_line, end_line, is_exported, signature, body_hash)
             VALUES (?1, 'bar', 'function', 'test::bar', 1, 5, 0, 'fn bar()', 'body1')",
            [file_id],
        )
        .unwrap();

        let pre = snapshot_symbol_state(&conn, &["test::bar"], &[]).unwrap();

        // Change visibility only (same signature, same body).
        conn.execute(
            "UPDATE symbols SET is_exported = 1 WHERE fqn = 'test::bar'",
            [],
        )
        .unwrap();

        let diffs = detect_symbol_diffs(&conn, &pre, &["src/test.rs".to_owned()]).unwrap();
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].kind, ChangeKind::VisibilityChanged);
    }

    #[test]
    fn store_symbol_changes_correlates_with_observation() {
        let tmp = TempDir::new().unwrap();
        let conn = open_or_create(&tmp.path().join("test.db")).unwrap();

        // Create a session and an auto observation.
        let session_id = crate::memory::store::create_session(&conn).unwrap();
        let obs = crate::memory::store::NewObservation {
            session_id: session_id.clone(),
            kind: "auto".to_owned(),
            content: "Tool: run_pipeline. Query: auth".to_owned(),
            headline: Some("Pipeline: auth".to_owned()),
            detail_level: 2,
            linked_fqns: vec![],
        };
        crate::memory::store::save_observation(&conn, &obs).unwrap();

        let diffs = vec![SymbolDiff {
            fqn: "test::correlated".to_owned(),
            file_path: "src/test.rs".to_owned(),
            kind: ChangeKind::Added,
            old_value: None,
            new_value: Some("fn correlated()".to_owned()),
        }];

        store_symbol_changes(&conn, &diffs, Some(&session_id)).unwrap();

        // Verify correlated_observation_id is set.
        let correlated: Option<i64> = conn
            .query_row(
                "SELECT correlated_observation_id FROM symbol_changes WHERE symbol_fqn = 'test::correlated'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(correlated.is_some(), "should have correlated observation");
    }

    #[test]
    fn query_recent_changes_respects_limit() {
        let tmp = TempDir::new().unwrap();
        let conn = open_or_create(&tmp.path().join("test.db")).unwrap();
        let now = unix_now();

        for i in 0..5 {
            conn.execute(
                "INSERT INTO symbol_changes (symbol_fqn, file_path, change_kind, detected_at)
                 VALUES ('test::foo', 'src/test.rs', 'body_changed', ?1)",
                [now - 100 + i],
            )
            .unwrap();
        }

        let results = query_recent_changes(&conn, &["test::foo".to_owned()], 0, 2).unwrap();
        assert_eq!(results.len(), 2, "limit=2 should return exactly 2");
    }

    #[test]
    fn query_recent_changes_filters_by_since() {
        let tmp = TempDir::new().unwrap();
        let conn = open_or_create(&tmp.path().join("test.db")).unwrap();
        let now = unix_now();

        // Old change (before cutoff).
        conn.execute(
            "INSERT INTO symbol_changes (symbol_fqn, file_path, change_kind, detected_at)
             VALUES ('test::foo', 'src/test.rs', 'added', ?1)",
            [now - 1000],
        )
        .unwrap();
        // Recent change (after cutoff).
        conn.execute(
            "INSERT INTO symbol_changes (symbol_fqn, file_path, change_kind, detected_at)
             VALUES ('test::foo', 'src/test.rs', 'body_changed', ?1)",
            [now - 10],
        )
        .unwrap();

        let results =
            query_recent_changes(&conn, &["test::foo".to_owned()], now - 500, 10).unwrap();
        assert_eq!(
            results.len(),
            1,
            "only the recent change should be returned"
        );
        assert_eq!(results[0].change_kind, "body_changed");
    }

    #[test]
    fn rename_candidate_rejects_empty_names() {
        assert!(!is_rename_candidate("", ""));
    }
}
