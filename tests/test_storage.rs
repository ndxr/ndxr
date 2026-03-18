use tempfile::TempDir;

#[test]
fn creates_database_and_tables() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".ndxr").join("index.db");
    let conn = ndxr::storage::db::open_or_create(&db_path).unwrap();

    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .filter_map(Result::ok)
        .collect();

    assert!(tables.contains(&"files".to_string()));
    assert!(tables.contains(&"symbols".to_string()));
    assert!(tables.contains(&"edges".to_string()));
    assert!(tables.contains(&"sessions".to_string()));
    assert!(tables.contains(&"observations".to_string()));
    assert!(tables.contains(&"observation_links".to_string()));
    assert!(tables.contains(&"term_frequencies".to_string()));
    assert!(tables.contains(&"doc_frequencies".to_string()));
    assert!(tables.contains(&"schema_version".to_string()));
}

#[test]
fn sets_wal_mode() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".ndxr").join("index.db");
    let conn = ndxr::storage::db::open_or_create(&db_path).unwrap();

    let mode: String = conn
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
}

#[test]
fn fts5_symbols_table_exists() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".ndxr").join("index.db");
    let conn = ndxr::storage::db::open_or_create(&db_path).unwrap();

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols_fts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn fts5_observations_table_exists() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".ndxr").join("index.db");
    let conn = ndxr::storage::db::open_or_create(&db_path).unwrap();

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM observations_fts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn reset_code_tables_preserves_memory() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".ndxr").join("index.db");
    let conn = ndxr::storage::db::open_or_create(&db_path).unwrap();

    conn.execute(
        "INSERT INTO sessions (id, started_at, last_active) VALUES ('s1', 1000, 1000)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO files (path, language, blake3_hash, indexed_at) VALUES ('a.ts', 'typescript', 'abc', 1000)",
        [],
    )
    .unwrap();

    ndxr::storage::db::reset_code_tables(&conn).unwrap();

    let file_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(file_count, 0);

    let session_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(session_count, 1);
}

#[test]
fn schema_version_is_tracked() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".ndxr").join("index.db");
    let conn = ndxr::storage::db::open_or_create(&db_path).unwrap();

    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(version >= 1);
}

#[test]
fn cascade_delete_file_removes_symbols_edges_and_fts() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".ndxr").join("index.db");
    let conn = ndxr::storage::db::open_or_create(&db_path).unwrap();

    // Insert a file
    conn.execute(
        "INSERT INTO files (path, language, blake3_hash, line_count, byte_size, indexed_at) \
         VALUES ('a.ts', 'typescript', 'hash1', 10, 100, 1000)",
        [],
    )
    .unwrap();
    let file_id = conn.last_insert_rowid();

    // Insert two symbols
    conn.execute(
        "INSERT INTO symbols (file_id, name, kind, fqn, start_line, end_line) \
         VALUES (?1, 'foo', 'function', 'a.ts::foo', 1, 5)",
        [file_id],
    )
    .unwrap();
    let sym1 = conn.last_insert_rowid();

    conn.execute(
        "INSERT INTO symbols (file_id, name, kind, fqn, start_line, end_line) \
         VALUES (?1, 'bar', 'function', 'a.ts::bar', 6, 10)",
        [file_id],
    )
    .unwrap();
    let sym2 = conn.last_insert_rowid();

    // Insert edge between them
    conn.execute(
        "INSERT INTO edges (from_id, to_id, kind) VALUES (?1, ?2, 'calls')",
        rusqlite::params![sym1, sym2],
    )
    .unwrap();

    // Insert TF entry
    conn.execute(
        "INSERT INTO term_frequencies (term, symbol_id, tf) VALUES ('foo', ?1, 0.5)",
        [sym1],
    )
    .unwrap();

    // Verify FTS5 was populated by trigger
    let fts_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols_fts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(fts_count, 2);

    // Delete the file -- CASCADE should remove everything
    conn.execute("DELETE FROM files WHERE id = ?1", [file_id])
        .unwrap();

    let sym_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
        .unwrap();
    assert_eq!(sym_count, 0, "symbols should be deleted via CASCADE");

    let edge_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
        .unwrap();
    assert_eq!(edge_count, 0, "edges should be deleted via CASCADE");

    let tf_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM term_frequencies", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        tf_count, 0,
        "term_frequencies should be deleted via CASCADE"
    );

    let fts_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols_fts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(fts_count, 0, "FTS5 should be cleaned by delete trigger");
}

#[test]
fn cascade_delete_session_removes_observations_and_fts() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".ndxr").join("index.db");
    let conn = ndxr::storage::db::open_or_create(&db_path).unwrap();

    conn.execute(
        "INSERT INTO sessions (id, started_at, last_active) VALUES ('s1', 1000, 1000)",
        [],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO observations (session_id, kind, content, detail_level, created_at) \
         VALUES ('s1', 'manual', 'test obs', 2, 1000)",
        [],
    )
    .unwrap();
    let obs_id = conn.last_insert_rowid();

    conn.execute(
        "INSERT INTO observation_links (observation_id, symbol_fqn) VALUES (?1, 'a.ts::foo')",
        [obs_id],
    )
    .unwrap();

    // Verify FTS populated
    let fts_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM observations_fts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(fts_count, 1);

    // Delete session -- CASCADE
    conn.execute("DELETE FROM sessions WHERE id = 's1'", [])
        .unwrap();

    let obs_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM observations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(obs_count, 0);

    let link_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM observation_links", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(link_count, 0);

    let fts_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM observations_fts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        fts_count, 0,
        "observations FTS should be cleaned by delete trigger"
    );
}
