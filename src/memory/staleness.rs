//! Marks observations as stale when linked symbols change.
//!
//! When symbols are deleted, have their signature changed, or have their body
//! changed, all observations linked to those symbols via `observation_links`
//! are marked `is_stale = 1`.

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

/// A symbol that changed during re-indexing.
pub struct ChangedSymbol {
    /// Fully-qualified name of the changed symbol.
    pub fqn: String,
    /// Type of change detected.
    pub change_type: SymbolChange,
}

/// Type of change detected for a symbol.
pub enum SymbolChange {
    /// The symbol was removed from the codebase.
    Deleted,
    /// The symbol's type signature changed.
    SignatureChanged,
    /// The symbol's implementation body changed.
    BodyChanged,
}

/// Detects and marks stale observations for changed symbols.
///
/// For each changed or deleted symbol, finds all observations linked to that
/// FQN via `observation_links` and sets `is_stale = 1`.
///
/// Returns the number of observations marked stale.
///
/// # Errors
///
/// Returns an error if any database update fails.
pub fn detect_staleness(conn: &Connection, changed_symbols: &[ChangedSymbol]) -> Result<usize> {
    let mut total_marked = 0;

    for sym in changed_symbols {
        let count = conn
            .execute(
                "UPDATE observations SET is_stale = 1 WHERE id IN (\
                     SELECT observation_id FROM observation_links WHERE symbol_fqn = ?1\
                 ) AND is_stale = 0",
                params![sym.fqn],
            )
            .with_context(|| format!("mark stale observations for {}", sym.fqn))?;
        total_marked += count;
    }

    Ok(total_marked)
}
