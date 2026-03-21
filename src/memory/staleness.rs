//! Marks observations as stale when linked symbols change.
//!
//! When symbols are deleted, have their signature changed, or have their body
//! changed, all observations linked to those symbols via `observation_links`
//! are marked `is_stale = 1`.

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::memory::changes::SymbolDiff;
use crate::storage::db::{BATCH_PARAM_LIMIT, build_batch_placeholders};

/// Detects and marks stale observations for changed symbols.
///
/// For each batch of changed or deleted symbols, finds all observations linked
/// to those FQNs via `observation_links` and sets `is_stale = 1`.
/// Uses `WHERE symbol_fqn IN (...)` with chunking for efficiency and wraps
/// all updates in a single transaction.
///
/// Returns the number of observations marked stale.
///
/// # Errors
///
/// Returns an error if any database update fails.
pub fn detect_staleness(conn: &Connection, diffs: &[SymbolDiff]) -> Result<usize> {
    if diffs.is_empty() {
        return Ok(0);
    }

    let tx = conn
        .unchecked_transaction()
        .context("begin staleness transaction")?;
    let mut total_marked = 0;

    let fqns: Vec<&str> = diffs.iter().map(|d| d.fqn.as_str()).collect();
    for chunk in fqns.chunks(BATCH_PARAM_LIMIT) {
        let placeholders = build_batch_placeholders(chunk.len());
        let sql = format!(
            "UPDATE observations SET is_stale = 1 WHERE id IN (\
                 SELECT observation_id FROM observation_links WHERE symbol_fqn IN ({placeholders})\
             ) AND is_stale = 0"
        );
        let params: Vec<&dyn rusqlite::types::ToSql> = chunk
            .iter()
            .map(|fqn| fqn as &dyn rusqlite::types::ToSql)
            .collect();
        let count = tx
            .execute(&sql, params.as_slice())
            .context("mark stale observations batch")?;
        total_marked += count;
    }

    tx.commit().context("commit staleness transaction")?;
    Ok(total_marked)
}
