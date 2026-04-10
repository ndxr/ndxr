//! `SQLite` database initialization, migration, and schema management.
//!
//! Provides [`open_or_create`] to open (or create) the ndxr index database with
//! all required pragmas and schema migrations applied, and [`reset_code_tables`]
//! to drop and recreate code-related tables while preserving memory tables.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::util::unix_now;

/// Maximum number of SQL parameters per batch query.
///
/// Set to 900 to stay safely below `SQLite`'s default `SQLITE_LIMIT_VARIABLE_NUMBER` (999).
pub(crate) const BATCH_PARAM_LIMIT: usize = 900;

/// Builds a comma-separated SQL placeholder string `?1, ?2, ..., ?N`.
///
/// Used with `WHERE id IN (...)` batch queries chunked by [`BATCH_PARAM_LIMIT`].
#[must_use]
pub(crate) fn build_batch_placeholders(count: usize) -> String {
    let parts: Vec<String> = (1..=count).map(|i| format!("?{i}")).collect();
    parts.join(", ")
}

/// Opens an existing ndxr database or creates a new one at `path`.
///
/// Parent directories are created if they do not exist. Connection pragmas
/// (WAL mode, cache size, memory-mapped I/O, etc.) are set on every open.
/// Any pending schema migrations are applied automatically.
///
/// # Errors
///
/// Returns an error if the path cannot be created, the database cannot be
/// opened, pragmas fail to apply, or a migration fails.
pub fn open_or_create(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create directory: {}", parent.display()))?;
    }

    let conn = Connection::open(path)
        .with_context(|| format!("cannot open database: {}", path.display()))?;

    apply_pragmas(&conn)?;
    run_migrations(&conn)?;

    Ok(conn)
}

/// Drops and recreates all code-related tables (files, symbols, edges,
/// `symbols_fts`, `term_frequencies`, `doc_frequencies`) and their triggers.
///
/// Memory tables (`sessions`, `observations`, `observation_links`,
/// `observations_fts`) are **not** touched.
///
/// # Errors
///
/// Returns an error if any SQL statement fails.
pub fn reset_code_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        -- Drop FTS table and triggers first (they reference symbols)
        DROP TRIGGER IF EXISTS symbols_au;
        DROP TRIGGER IF EXISTS symbols_ad;
        DROP TRIGGER IF EXISTS symbols_ai;
        DROP TABLE IF EXISTS symbols_fts;

        -- Drop dependent tables in correct order
        DROP TABLE IF EXISTS term_frequencies;
        DROP TABLE IF EXISTS doc_frequencies;
        DROP TABLE IF EXISTS edges;
        DROP TABLE IF EXISTS symbols;
        DROP TABLE IF EXISTS files;
        ",
    )
    .context("failed to drop code tables")?;

    conn.execute_batch(CREATE_CODE_TABLES)
        .context("failed to recreate code tables")?;
    conn.execute_batch(CREATE_SYMBOLS_FTS)
        .context("failed to recreate symbols FTS")?;
    conn.execute_batch(CREATE_CODE_INDEXES)
        .context("failed to recreate code indexes")?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Pragmas
// ---------------------------------------------------------------------------

fn apply_pragmas(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA cache_size = -64000;
        PRAGMA temp_store = MEMORY;
        PRAGMA mmap_size = 268435456;
        PRAGMA foreign_keys = ON;
        ",
    )
    .context("failed to set connection pragmas")
}

// ---------------------------------------------------------------------------
// Schema DDL — split into composable fragments
// ---------------------------------------------------------------------------

/// Core code tables: files, symbols, edges, `term_frequencies`, `doc_frequencies`.
const CREATE_CODE_TABLES: &str = "
CREATE TABLE IF NOT EXISTS files (
    id          INTEGER PRIMARY KEY,
    path        TEXT NOT NULL UNIQUE,
    language    TEXT NOT NULL,
    blake3_hash TEXT NOT NULL,
    line_count  INTEGER NOT NULL DEFAULT 0,
    byte_size   INTEGER NOT NULL DEFAULT 0,
    indexed_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS symbols (
    id          INTEGER PRIMARY KEY,
    file_id     INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    kind        TEXT NOT NULL,
    fqn         TEXT NOT NULL,
    signature   TEXT,
    docstring   TEXT,
    start_line  INTEGER NOT NULL,
    end_line    INTEGER NOT NULL,
    is_exported INTEGER NOT NULL DEFAULT 0,
    body_hash   TEXT,
    centrality  REAL NOT NULL DEFAULT 0.0,
    UNIQUE(fqn, start_line)
);

CREATE TABLE IF NOT EXISTS edges (
    id          INTEGER PRIMARY KEY,
    from_id     INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
    to_id       INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,
    UNIQUE(from_id, to_id, kind)
);

CREATE TABLE IF NOT EXISTS term_frequencies (
    term        TEXT NOT NULL,
    symbol_id   INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
    tf          REAL NOT NULL,
    PRIMARY KEY (term, symbol_id)
);

CREATE TABLE IF NOT EXISTS doc_frequencies (
    term        TEXT NOT NULL PRIMARY KEY,
    df          INTEGER NOT NULL
);
";

/// FTS5 virtual table and sync triggers for symbols.
const CREATE_SYMBOLS_FTS: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(
    name, fqn, docstring, signature,
    content='symbols', content_rowid='id',
    tokenize='porter unicode61'
);

CREATE TRIGGER IF NOT EXISTS symbols_ai AFTER INSERT ON symbols BEGIN
    INSERT INTO symbols_fts(rowid, name, fqn, docstring, signature)
    VALUES (new.id, new.name, new.fqn, new.docstring, new.signature);
END;

CREATE TRIGGER IF NOT EXISTS symbols_ad AFTER DELETE ON symbols BEGIN
    INSERT INTO symbols_fts(symbols_fts, rowid, name, fqn, docstring, signature)
    VALUES ('delete', old.id, old.name, old.fqn, old.docstring, old.signature);
END;

CREATE TRIGGER IF NOT EXISTS symbols_au AFTER UPDATE ON symbols BEGIN
    INSERT INTO symbols_fts(symbols_fts, rowid, name, fqn, docstring, signature)
    VALUES ('delete', old.id, old.name, old.fqn, old.docstring, old.signature);
    INSERT INTO symbols_fts(rowid, name, fqn, docstring, signature)
    VALUES (new.id, new.name, new.fqn, new.docstring, new.signature);
END;
";

/// Memory tables: sessions, observations, `observation_links`, `observations_fts`.
const CREATE_MEMORY_TABLES: &str = "
CREATE TABLE IF NOT EXISTS sessions (
    id           TEXT PRIMARY KEY,
    started_at   INTEGER NOT NULL,
    last_active  INTEGER NOT NULL,
    is_compressed INTEGER NOT NULL DEFAULT 0,
    summary      TEXT,
    key_terms    TEXT,
    key_files    TEXT
);

CREATE TABLE IF NOT EXISTS observations (
    id           INTEGER PRIMARY KEY,
    session_id   TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    kind         TEXT NOT NULL,
    content      TEXT NOT NULL,
    headline     TEXT,
    detail_level INTEGER NOT NULL DEFAULT 2,
    is_stale     INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL,
    score        REAL
);

CREATE TABLE IF NOT EXISTS observation_links (
    observation_id INTEGER NOT NULL REFERENCES observations(id) ON DELETE CASCADE,
    symbol_fqn     TEXT NOT NULL,
    PRIMARY KEY (observation_id, symbol_fqn)
);
";

/// FTS5 virtual table and sync triggers for observations.
const CREATE_OBSERVATIONS_FTS: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS observations_fts USING fts5(
    content, headline,
    content='observations', content_rowid='id',
    tokenize='porter unicode61'
);

CREATE TRIGGER IF NOT EXISTS observations_ai AFTER INSERT ON observations BEGIN
    INSERT INTO observations_fts(rowid, content, headline)
    VALUES (new.id, new.content, new.headline);
END;

CREATE TRIGGER IF NOT EXISTS observations_ad AFTER DELETE ON observations BEGIN
    INSERT INTO observations_fts(observations_fts, rowid, content, headline)
    VALUES ('delete', old.id, old.content, old.headline);
END;

CREATE TRIGGER IF NOT EXISTS observations_au AFTER UPDATE ON observations BEGIN
    INSERT INTO observations_fts(observations_fts, rowid, content, headline)
    VALUES ('delete', old.id, old.content, old.headline);
    INSERT INTO observations_fts(rowid, content, headline)
    VALUES (new.id, new.content, new.headline);
END;
";

/// Schema version tracking table.
const CREATE_SCHEMA_VERSION: &str = "
CREATE TABLE IF NOT EXISTS schema_version (
    version     INTEGER NOT NULL,
    migrated_at INTEGER NOT NULL
);
";

/// Indexes on code tables.
const CREATE_CODE_INDEXES: &str = "
CREATE INDEX IF NOT EXISTS idx_symbols_file_id ON symbols(file_id);
CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
CREATE INDEX IF NOT EXISTS idx_symbols_kind ON symbols(kind);
CREATE INDEX IF NOT EXISTS idx_symbols_exported ON symbols(is_exported) WHERE is_exported = 1;
CREATE INDEX IF NOT EXISTS idx_edges_from ON edges(from_id);
CREATE INDEX IF NOT EXISTS idx_edges_to ON edges(to_id);
CREATE INDEX IF NOT EXISTS idx_edges_kind ON edges(kind);
CREATE INDEX IF NOT EXISTS idx_tf_symbol ON term_frequencies(symbol_id);
CREATE INDEX IF NOT EXISTS idx_symbols_fqn ON symbols(fqn);
";

/// Indexes on memory tables.
const CREATE_MEMORY_INDEXES: &str = "
CREATE INDEX IF NOT EXISTS idx_observations_session ON observations(session_id);
CREATE INDEX IF NOT EXISTS idx_observations_created ON observations(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_observations_stale ON observations(is_stale);
CREATE INDEX IF NOT EXISTS idx_obs_links_fqn ON observation_links(symbol_fqn);
";

/// Symbol changes table for AST structural diff tracking.
const CREATE_SYMBOL_CHANGES: &str = "
CREATE TABLE IF NOT EXISTS symbol_changes (
    id                        INTEGER PRIMARY KEY AUTOINCREMENT,
    symbol_fqn                TEXT    NOT NULL,
    file_path                 TEXT    NOT NULL,
    change_kind               TEXT    NOT NULL,
    old_value                 TEXT,
    new_value                 TEXT,
    session_id                TEXT,
    correlated_observation_id INTEGER,
    detected_at               INTEGER NOT NULL,
    FOREIGN KEY (correlated_observation_id) REFERENCES observations(id) ON DELETE SET NULL
);
";

/// Indexes on the `symbol_changes` table.
const CREATE_SYMBOL_CHANGES_INDEXES: &str = "
CREATE INDEX IF NOT EXISTS idx_changes_fqn_detected ON symbol_changes(symbol_fqn, detected_at);
CREATE INDEX IF NOT EXISTS idx_changes_detected ON symbol_changes(detected_at DESC);
CREATE INDEX IF NOT EXISTS idx_changes_file ON symbol_changes(file_path);
CREATE INDEX IF NOT EXISTS idx_changes_kind ON symbol_changes(change_kind);
CREATE INDEX IF NOT EXISTS idx_changes_session ON symbol_changes(session_id);
";

/// Embeddings table for semantic search vectors.
const CREATE_SYMBOL_EMBEDDINGS: &str = "
CREATE TABLE IF NOT EXISTS symbol_embeddings (
    symbol_id   INTEGER PRIMARY KEY REFERENCES symbols(id) ON DELETE CASCADE,
    embedding   BLOB NOT NULL,
    model_name  TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);
";

// ---------------------------------------------------------------------------
// Migrations
// ---------------------------------------------------------------------------

/// Each migration function receives a transaction and applies one schema step.
/// Migrations are cumulative and never removed.
const MIGRATIONS: &[fn(&rusqlite::Transaction<'_>) -> Result<()>] =
    &[migrate_v1, migrate_v2, migrate_v3];

/// V1: create the full initial schema.
fn migrate_v1(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(CREATE_CODE_TABLES)
        .context("v1: create code tables")?;
    tx.execute_batch(CREATE_SYMBOLS_FTS)
        .context("v1: create symbols FTS")?;
    tx.execute_batch(CREATE_MEMORY_TABLES)
        .context("v1: create memory tables")?;
    tx.execute_batch(CREATE_OBSERVATIONS_FTS)
        .context("v1: create observations FTS")?;
    tx.execute_batch(CREATE_CODE_INDEXES)
        .context("v1: create code indexes")?;
    tx.execute_batch(CREATE_MEMORY_INDEXES)
        .context("v1: create memory indexes")?;
    Ok(())
}

/// V2: add `symbol_changes` table for AST structural diff tracking.
fn migrate_v2(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(CREATE_SYMBOL_CHANGES)
        .context("v2: create symbol_changes table")?;
    tx.execute_batch(CREATE_SYMBOL_CHANGES_INDEXES)
        .context("v2: create symbol_changes indexes")?;
    Ok(())
}

/// V3: add `symbol_embeddings` table for semantic search vectors.
fn migrate_v3(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(CREATE_SYMBOL_EMBEDDINGS)
        .context("v3: create symbol_embeddings table")?;
    Ok(())
}

/// Runs all pending migrations inside individual transactions.
fn run_migrations(conn: &Connection) -> Result<()> {
    // Ensure the schema_version table exists before querying it.
    conn.execute_batch(CREATE_SCHEMA_VERSION)
        .context("create schema_version table")?;

    let current_version: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |row| row.get(0),
        )
        .context("read current schema version")?;

    for (i, migration) in MIGRATIONS.iter().enumerate() {
        #[allow(clippy::cast_possible_wrap)] // MIGRATIONS array has ≤10 entries
        let version = (i + 1) as i64;
        if version <= current_version {
            continue;
        }

        let tx = conn
            .unchecked_transaction()
            .context("begin migration transaction")?;
        migration(&tx).with_context(|| format!("migration v{version}"))?;
        tx.execute(
            "INSERT INTO schema_version (version, migrated_at) VALUES (?1, ?2)",
            rusqlite::params![version, unix_now()],
        )
        .with_context(|| format!("record migration v{version}"))?;
        tx.commit()
            .with_context(|| format!("commit migration v{version}"))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn build_batch_placeholders_typical() {
        assert_eq!(build_batch_placeholders(3), "?1, ?2, ?3");
    }

    #[test]
    fn build_batch_placeholders_single() {
        assert_eq!(build_batch_placeholders(1), "?1");
    }

    #[test]
    fn build_batch_placeholders_empty() {
        assert_eq!(build_batch_placeholders(0), "");
    }

    #[test]
    fn v2_migration_creates_symbol_changes_table() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.db");
        let conn = open_or_create(&db_path).unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='symbol_changes'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "symbol_changes table should exist after migration"
        );

        conn.execute(
            "INSERT INTO symbol_changes (symbol_fqn, file_path, change_kind, old_value, new_value, session_id, detected_at)
             VALUES ('test::foo', 'src/test.rs', 'added', NULL, 'pub fn foo()', NULL, 1000)",
            [],
        )
        .unwrap();

        let idx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name LIKE 'idx_changes_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            idx_count, 5,
            "expected 5 indexes on symbol_changes, got {idx_count}"
        );
    }

    #[test]
    fn migrations_are_idempotent() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.db");
        let conn1 = open_or_create(&db_path).unwrap();
        drop(conn1);
        let conn2 = open_or_create(&db_path).unwrap();
        let version: i64 = conn2
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        #[allow(clippy::cast_possible_wrap)] // MIGRATIONS.len() is tiny, never near i64::MAX
        let expected = MIGRATIONS.len() as i64;
        assert_eq!(version, expected);
    }

    #[test]
    fn reset_code_tables_preserves_symbol_changes() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.db");
        let conn = open_or_create(&db_path).unwrap();

        conn.execute(
            "INSERT INTO symbol_changes (symbol_fqn, file_path, change_kind, detected_at)
             VALUES ('test::foo', 'src/test.rs', 'added', 1000)",
            [],
        )
        .unwrap();

        reset_code_tables(&conn).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbol_changes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "symbol_changes should survive reset_code_tables");
    }
}
