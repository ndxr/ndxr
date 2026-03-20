//! Resolves edge target names to symbol database IDs.
//!
//! After symbol extraction, edges reference symbols by name only. This module
//! resolves those names to database row IDs using a prioritized lookup:
//! same-file first, then globally exported symbols, then any symbol by name.

use std::collections::HashMap;
use std::hash::BuildHasher;

use anyhow::{Context, Result};
use rusqlite::Connection;
use tracing::trace;

use super::symbols::ExtractedEdge;

/// An edge with resolved database IDs.
#[derive(Debug)]
pub struct ResolvedEdge {
    /// Database ID of the source symbol.
    pub from_id: i64,
    /// Database ID of the target symbol.
    pub to_id: i64,
    /// Edge kind (`imports`, `calls`, etc.).
    pub kind: String,
}

/// Resolves extracted edges to database symbol IDs.
///
/// Resolution priority:
/// 1. **Same file**: symbol with matching name in the same file.
/// 2. **Global exported**: any exported symbol with a matching name.
/// 3. **Global any**: any symbol with a matching name.
/// 4. **Unresolved**: silently skipped (logged at trace level).
///
/// # Errors
///
/// Returns an error if a database query fails unexpectedly.
pub fn resolve_edges<S: BuildHasher>(
    conn: &Connection,
    file_path: &str,
    from_fqn_to_id: &HashMap<String, i64, S>,
    edges: &[ExtractedEdge],
) -> Result<Vec<ResolvedEdge>> {
    // Prepare statements for lookup.
    let mut stmt_same_file = conn
        .prepare(
            "SELECT s.id FROM symbols s
             JOIN files f ON s.file_id = f.id
             WHERE s.name = ?1 AND f.path = ?2
             LIMIT 1",
        )
        .context("prepare same-file symbol lookup")?;

    let mut stmt_exported = conn
        .prepare(
            "SELECT id FROM symbols
             WHERE name = ?1 AND is_exported = 1
             LIMIT 1",
        )
        .context("prepare exported symbol lookup")?;

    let mut stmt_any = conn
        .prepare(
            "SELECT id FROM symbols
             WHERE name = ?1
             LIMIT 1",
        )
        .context("prepare any symbol lookup")?;

    let mut resolved = Vec::new();

    for edge in edges {
        // Look up from_id via the FQN map.
        let Some(&from_id) = from_fqn_to_id.get(&edge.from_fqn) else {
            trace!(
                "skipping edge: from_fqn {:?} not found in FQN map",
                edge.from_fqn
            );
            continue;
        };

        // Resolve to_id by name.
        let to_id = resolve_target_id(
            &mut stmt_same_file,
            &mut stmt_exported,
            &mut stmt_any,
            &edge.to_name,
            file_path,
        );

        match to_id {
            Some(id) => {
                resolved.push(ResolvedEdge {
                    from_id,
                    to_id: id,
                    kind: edge.kind.clone(),
                });
            }
            None => {
                trace!(
                    "unresolved edge: {} -> {} (kind: {})",
                    edge.from_fqn, edge.to_name, edge.kind
                );
            }
        }
    }

    Ok(resolved)
}

/// Attempts to resolve a symbol name to an ID using the priority cascade.
fn resolve_target_id(
    stmt_same_file: &mut rusqlite::Statement<'_>,
    stmt_exported: &mut rusqlite::Statement<'_>,
    stmt_any: &mut rusqlite::Statement<'_>,
    name: &str,
    file_path: &str,
) -> Option<i64> {
    // 1. Same file.
    match stmt_same_file.query_row(rusqlite::params![name, file_path], |row| {
        row.get::<_, i64>(0)
    }) {
        Ok(id) => return Some(id),
        Err(rusqlite::Error::QueryReturnedNoRows) => {}
        Err(e) => tracing::warn!("edge resolution same-file query failed for {name}: {e}"),
    }

    // 2. Global exported.
    match stmt_exported.query_row(rusqlite::params![name], |row| row.get::<_, i64>(0)) {
        Ok(id) => return Some(id),
        Err(rusqlite::Error::QueryReturnedNoRows) => {}
        Err(e) => tracing::warn!("edge resolution exported query failed for {name}: {e}"),
    }

    // 3. Global any.
    match stmt_any.query_row(rusqlite::params![name], |row| row.get::<_, i64>(0)) {
        Ok(id) => return Some(id),
        Err(rusqlite::Error::QueryReturnedNoRows) => {}
        Err(e) => tracing::warn!("edge resolution any query failed for {name}: {e}"),
    }

    None
}
