//! Compresses inactive sessions to save space and improve search quality.
//!
//! Compression extracts key terms and files from observation content, generates
//! a summary from observation headlines, deletes noisy `auto` observations, and
//! preserves valuable observations (`insight`, `decision`, `error`, `manual`).

use std::collections::HashSet;

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

use crate::util::unix_now;

use crate::indexer::tokenizer;

/// Compresses sessions that have been inactive longer than `max_age_secs`.
///
/// Compression steps for each qualifying session:
/// 1. Extract key terms (top 20 TF terms from observation contents).
/// 2. Extract key files (unique file paths from linked FQNs).
/// 3. Generate summary (concatenated and deduplicated observation headlines).
/// 4. Delete `auto` observations (noisy, low-value).
/// 5. Preserve valuable observations (`insight`, `decision`, `error`, `manual`).
/// 6. Mark session as `is_compressed = 1`.
///
/// Returns the number of sessions compressed.
///
/// # Errors
///
/// Returns an error if any database operation fails.
#[allow(clippy::cast_possible_wrap)]
pub fn compress_inactive_sessions(conn: &Connection, max_age_secs: u64) -> Result<usize> {
    let now = unix_now();
    let cutoff = now - max_age_secs as i64;

    let mut stmt = conn
        .prepare("SELECT id FROM sessions WHERE last_active < ?1 AND is_compressed = 0")
        .context("prepare find inactive sessions")?;

    let session_ids: Vec<String> = stmt
        .query_map(params![cutoff], |row| row.get(0))
        .context("query inactive sessions")?
        .filter_map(Result::ok)
        .collect();

    for session_id in &session_ids {
        compress_session(conn, session_id)?;
    }

    Ok(session_ids.len())
}

/// Compresses a single session.
fn compress_session(conn: &Connection, session_id: &str) -> Result<()> {
    // Load all observations for this session.
    let mut stmt = conn
        .prepare("SELECT content, headline FROM observations WHERE session_id = ?1")
        .context("prepare load observations for compression")?;

    let rows: Vec<(String, Option<String>)> = stmt
        .query_map(params![session_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .context("query observations for compression")?
        .filter_map(Result::ok)
        .collect();

    // Extract key terms: tokenize all contents, compute TF, take top 20.
    let mut all_tokens = Vec::new();
    for (content, _) in &rows {
        all_tokens.extend(tokenizer::tokenize_text(content));
    }
    let tf = tokenizer::compute_tf(&all_tokens);
    let mut terms: Vec<(String, f64)> = tf.into_iter().collect();
    terms.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let key_terms: Vec<String> = terms.into_iter().take(20).map(|(t, _)| t).collect();
    let key_terms_str = key_terms.join(",");

    // Extract key files: unique file paths from linked FQNs.
    let mut fqn_stmt = conn
        .prepare(
            "SELECT DISTINCT ol.symbol_fqn FROM observation_links ol \
             JOIN observations o ON ol.observation_id = o.id \
             WHERE o.session_id = ?1",
        )
        .context("prepare load linked FQNs for compression")?;

    let fqns: Vec<String> = fqn_stmt
        .query_map(params![session_id], |row| row.get(0))
        .context("query linked FQNs for compression")?
        .filter_map(Result::ok)
        .collect();

    let key_files: HashSet<String> = fqns
        .iter()
        .filter_map(|fqn| fqn.split("::").next().map(String::from))
        .collect();
    let mut key_files_sorted: Vec<String> = key_files.into_iter().collect();
    key_files_sorted.sort();
    let key_files_str = key_files_sorted.join(",");

    // Summary from headlines (deduplicated, preserving order).
    let mut seen = HashSet::new();
    let mut unique_headlines = Vec::new();
    for (_, headline) in &rows {
        if let Some(h) = headline
            && seen.insert(h.clone())
        {
            unique_headlines.push(h.clone());
        }
    }
    let summary = unique_headlines.join("; ");

    // Delete auto observations (noisy, low-value).
    conn.execute(
        "DELETE FROM observations WHERE session_id = ?1 AND kind = 'auto'",
        params![session_id],
    )
    .context("delete auto observations during compression")?;

    // Mark session as compressed with extracted metadata.
    conn.execute(
        "UPDATE sessions SET is_compressed = 1, summary = ?1, key_terms = ?2, key_files = ?3 \
         WHERE id = ?4",
        params![summary, key_terms_str, key_files_str, session_id],
    )
    .context("mark session as compressed")?;

    Ok(())
}
