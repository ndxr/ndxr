//! `SQLite` storage for symbol embedding vectors.
//!
//! Embeddings are stored as BLOBs of `dimension * 4` bytes (f32 little-endian).
//! Uses `BATCH_PARAM_LIMIT` chunking for batch operations.

use std::collections::HashMap;

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::storage::db::{BATCH_PARAM_LIMIT, build_batch_placeholders};
use crate::util::unix_now;

/// Stores embedding vectors for a batch of symbols.
///
/// Each embedding is serialized as a BLOB of `dimension * 4` bytes (f32 little-endian).
/// Uses INSERT OR REPLACE to handle re-indexing.
///
/// # Errors
///
/// Returns an error if any database insert fails.
pub fn store_embeddings(
    conn: &Connection,
    entries: &[(i64, &[f32])],
    model_name: &str,
) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let now = unix_now();
    conn.execute_batch("BEGIN")
        .context("begin embedding transaction")?;
    let result = (|| -> Result<()> {
        let mut stmt = conn.prepare(
            "INSERT OR REPLACE INTO symbol_embeddings (symbol_id, embedding, model_name, created_at) \
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for (symbol_id, embedding) in entries {
            let blob = floats_to_blob(embedding);
            stmt.execute(rusqlite::params![symbol_id, blob, model_name, now])?;
        }
        Ok(())
    })();
    if result.is_ok() {
        conn.execute_batch("COMMIT")
            .context("commit embedding transaction")?;
    } else {
        let _ = conn.execute_batch("ROLLBACK");
    }
    result
}

/// Loads embedding vectors for the given symbol IDs.
///
/// Returns a map from `symbol_id` to embedding vector. Missing IDs are silently
/// skipped (not all symbols may have embeddings).
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn load_embeddings(conn: &Connection, symbol_ids: &[i64]) -> Result<HashMap<i64, Vec<f32>>> {
    if symbol_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut result = HashMap::with_capacity(symbol_ids.len());
    for chunk in symbol_ids.chunks(BATCH_PARAM_LIMIT) {
        let placeholders = build_batch_placeholders(chunk.len());
        let sql = format!(
            "SELECT symbol_id, embedding FROM symbol_embeddings WHERE symbol_id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<Box<dyn rusqlite::types::ToSql>> = chunk
            .iter()
            .map(|id| Box::new(*id) as Box<dyn rusqlite::types::ToSql>)
            .collect();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(std::convert::AsRef::as_ref).collect();
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            let id: i64 = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            Ok((id, blob))
        })?;
        for row in rows {
            match row {
                Ok((id, blob)) => {
                    let floats = blob_to_floats(&blob);
                    if floats.len() != crate::embeddings::model::EMBEDDING_DIMENSION {
                        tracing::warn!(
                            symbol_id = id,
                            actual = floats.len(),
                            expected = crate::embeddings::model::EMBEDDING_DIMENSION,
                            "skipping embedding with wrong dimension"
                        );
                        continue;
                    }
                    result.insert(id, floats);
                }
                Err(e) => tracing::warn!("skipping corrupt embedding row: {e}"),
            }
        }
    }
    Ok(result)
}

/// Returns the number of symbols with stored embeddings.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn embedding_count(conn: &Connection) -> Result<usize> {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbol_embeddings", [], |row| {
            row.get(0)
        })
        .context("count embeddings")?;
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    // count is non-negative, fits in usize
    Ok(count as usize)
}

/// Deletes all stored embeddings.
///
/// # Errors
///
/// Returns an error if the delete fails.
pub fn clear_embeddings(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM symbol_embeddings", [])
        .context("clear embeddings")?;
    Ok(())
}

/// Deletes embeddings for specific symbol IDs.
///
/// # Errors
///
/// Returns an error if the delete fails.
pub fn delete_embeddings(conn: &Connection, symbol_ids: &[i64]) -> Result<()> {
    if symbol_ids.is_empty() {
        return Ok(());
    }
    for chunk in symbol_ids.chunks(BATCH_PARAM_LIMIT) {
        let placeholders = build_batch_placeholders(chunk.len());
        let sql = format!("DELETE FROM symbol_embeddings WHERE symbol_id IN ({placeholders})");
        let params: Vec<Box<dyn rusqlite::types::ToSql>> = chunk
            .iter()
            .map(|id| Box::new(*id) as Box<dyn rusqlite::types::ToSql>)
            .collect();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(std::convert::AsRef::as_ref).collect();
        conn.execute(&sql, param_refs.as_slice())?;
    }
    Ok(())
}

/// Returns the model name used for stored embeddings, if any.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn embedding_model_name(conn: &Connection) -> Result<Option<String>> {
    match conn.query_row(
        "SELECT DISTINCT model_name FROM symbol_embeddings LIMIT 1",
        [],
        |row| row.get::<_, String>(0),
    ) {
        Ok(name) => Ok(Some(name)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e).context("query embedding model name"),
    }
}

/// Converts f32 slice to little-endian byte vec.
fn floats_to_blob(floats: &[f32]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(floats.len() * 4);
    for f in floats {
        blob.extend_from_slice(&f.to_le_bytes());
    }
    blob
}

/// Converts little-endian byte vec back to f32 vec.
fn blob_to_floats(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE symbols (id INTEGER PRIMARY KEY, fqn TEXT NOT NULL);
             CREATE TABLE symbol_embeddings (
                 symbol_id INTEGER PRIMARY KEY REFERENCES symbols(id) ON DELETE CASCADE,
                 embedding BLOB NOT NULL,
                 model_name TEXT NOT NULL,
                 created_at INTEGER NOT NULL
             );",
        )
        .unwrap();
        conn
    }

    #[test]
    fn store_and_load_roundtrip() {
        let conn = setup_db();
        conn.execute("INSERT INTO symbols (id, fqn) VALUES (1, 'test::sym')", [])
            .unwrap();
        #[allow(clippy::cast_precision_loss)]
        // test data; i32 → f32 precision loss irrelevant for 0..384
        let emb: Vec<f32> = (0..384_i32).map(|i| i as f32 * 0.01).collect();
        store_embeddings(&conn, &[(1, &emb)], "test-model").unwrap();
        let loaded = load_embeddings(&conn, &[1]).unwrap();
        assert_eq!(loaded.len(), 1);
        let loaded_emb = &loaded[&1];
        assert_eq!(loaded_emb.len(), 384);
        for (a, b) in emb.iter().zip(loaded_emb.iter()) {
            assert!((a - b).abs() < f32::EPSILON, "mismatch at value {a} vs {b}");
        }
    }

    #[test]
    fn store_batch_with_chunking() {
        let conn = setup_db();
        let emb: Vec<f32> = vec![0.1; 384];
        for i in 1..=1000 {
            conn.execute(
                "INSERT INTO symbols (id, fqn) VALUES (?1, ?2)",
                rusqlite::params![i, format!("sym{i}")],
            )
            .unwrap();
        }
        let entries: Vec<(i64, Vec<f32>)> = (1..=1000).map(|i| (i, emb.clone())).collect();
        let refs: Vec<(i64, &[f32])> = entries.iter().map(|(id, e)| (*id, e.as_slice())).collect();
        store_embeddings(&conn, &refs, "test-model").unwrap();
        assert_eq!(embedding_count(&conn).unwrap(), 1000);
    }

    #[test]
    fn load_missing_returns_empty() {
        let conn = setup_db();
        let loaded = load_embeddings(&conn, &[999]).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn clear_embeddings_removes_all() {
        let conn = setup_db();
        conn.execute("INSERT INTO symbols (id, fqn) VALUES (1, 'a')", [])
            .unwrap();
        let emb = vec![0.5_f32; 384];
        store_embeddings(&conn, &[(1, &emb)], "m").unwrap();
        assert_eq!(embedding_count(&conn).unwrap(), 1);
        clear_embeddings(&conn).unwrap();
        assert_eq!(embedding_count(&conn).unwrap(), 0);
    }

    #[test]
    fn cascade_delete_on_symbol_removal() {
        let conn = setup_db();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute("INSERT INTO symbols (id, fqn) VALUES (1, 'a')", [])
            .unwrap();
        let emb = vec![0.5_f32; 384];
        store_embeddings(&conn, &[(1, &emb)], "m").unwrap();
        assert_eq!(embedding_count(&conn).unwrap(), 1);
        conn.execute("DELETE FROM symbols WHERE id = 1", [])
            .unwrap();
        assert_eq!(embedding_count(&conn).unwrap(), 0);
    }
}
