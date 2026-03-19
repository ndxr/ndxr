//! Shared index status collection.
//!
//! Provides a single function to gather all index health statistics from the
//! database, used by both the CLI `status` command and the MCP `index_status`
//! tool.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;

/// Aggregate health statistics for the ndxr index.
#[derive(Debug, Serialize)]
pub struct IndexStatus {
    /// Number of indexed source files.
    pub file_count: i64,
    /// Number of extracted symbols.
    pub symbol_count: i64,
    /// Number of resolved dependency edges.
    pub edge_count: i64,
    /// Number of stored observations.
    pub observation_count: i64,
    /// Number of recorded sessions.
    pub session_count: i64,
    /// Unix timestamp of the earliest indexed file, if any.
    pub oldest_indexed_at: Option<i64>,
    /// Unix timestamp of the most recently indexed file, if any.
    pub newest_indexed_at: Option<i64>,
    /// Database file size in bytes.
    pub db_size_bytes: u64,
}

/// Collects index health statistics from the database.
///
/// Queries aggregate counts from every core table and reads the database
/// file size from disk.
///
/// # Errors
///
/// Returns an error if any SQL query fails or the database file metadata
/// cannot be read.
pub fn collect_index_status(conn: &Connection, db_path: &Path) -> Result<IndexStatus> {
    let file_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
        .context("failed to count files")?;
    let symbol_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
        .context("failed to count symbols")?;
    let edge_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
        .context("failed to count edges")?;
    let observation_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM observations", [], |row| row.get(0))
        .context("failed to count observations")?;
    let session_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
        .context("failed to count sessions")?;
    let oldest_indexed_at: Option<i64> = conn
        .query_row("SELECT MIN(indexed_at) FROM files", [], |row| row.get(0))
        .context("failed to query oldest indexed_at")?;
    let newest_indexed_at: Option<i64> = conn
        .query_row("SELECT MAX(indexed_at) FROM files", [], |row| row.get(0))
        .context("failed to query newest indexed_at")?;

    let db_size_bytes = std::fs::metadata(db_path)
        .with_context(|| format!("cannot read database metadata: {}", db_path.display()))?
        .len();

    Ok(IndexStatus {
        file_count,
        symbol_count,
        edge_count,
        observation_count,
        session_count,
        oldest_indexed_at,
        newest_indexed_at,
        db_size_bytes,
    })
}
