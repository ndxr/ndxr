//! Smoke tests: indexer, storage, walker, workspace, and CLI edge cases.

use std::fs;

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Indexer edge cases
// ---------------------------------------------------------------------------

#[test]
fn index_whitespace_only_file() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::write(tmp.path().join("blank.ts"), "   \n\n  \t\n  ").unwrap();

    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    let stats = ndxr::indexer::index(&config, None).unwrap();

    assert_eq!(stats.files_indexed, 1);
    assert_eq!(stats.symbols_extracted, 0);
}

#[test]
fn index_comment_only_file() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::write(
        tmp.path().join("comments.ts"),
        "// This file has only comments\n// No actual code\n/* block comment */\n",
    )
    .unwrap();

    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    let stats = ndxr::indexer::index(&config, None).unwrap();

    assert_eq!(stats.files_indexed, 1);
    // Comment-only files should succeed regardless of symbol count.
}

#[test]
fn index_syntax_error_file() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    // Intentionally broken syntax: tree-sitter should handle this gracefully.
    fs::write(tmp.path().join("broken.ts"), "function { broken }").unwrap();

    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    let result = ndxr::indexer::index(&config, None);

    // Must not crash; tree-sitter produces partial ASTs for broken input.
    assert!(
        result.is_ok(),
        "indexing a syntax-error file must not crash"
    );
}

#[test]
fn index_mixed_languages() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();

    // 5 distinct languages
    fs::write(
        tmp.path().join("app.ts"),
        "export function tsFunc(): void {}",
    )
    .unwrap();
    fs::write(tmp.path().join("app.py"), "def py_func():\n    return 1\n").unwrap();
    fs::write(tmp.path().join("app.rs"), "pub fn rs_func() -> i32 { 42 }").unwrap();
    fs::write(
        tmp.path().join("app.go"),
        "package main\n\nfunc GoFunc() int { return 1 }",
    )
    .unwrap();
    fs::write(
        tmp.path().join("App.java"),
        "public class App { public void javaMethod() {} }",
    )
    .unwrap();

    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    let stats = ndxr::indexer::index(&config, None).unwrap();

    assert_eq!(
        stats.files_indexed, 5,
        "all 5 language files should be indexed"
    );

    // Verify the DB recorded all 5 files with correct languages.
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let file_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(file_count, 5);

    let mut stmt = conn
        .prepare("SELECT DISTINCT language FROM files ORDER BY language")
        .unwrap();
    let languages: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert!(
        languages.len() >= 5,
        "should have at least 5 distinct languages, got {languages:?}"
    );
}

#[test]
fn index_deeply_nested_directory() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    let deep_dir = tmp.path().join("src/a/b/c/d/e");
    fs::create_dir_all(&deep_dir).unwrap();
    fs::write(deep_dir.join("deep.ts"), "export function deepFunc() {}").unwrap();

    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    let stats = ndxr::indexer::index(&config, None).unwrap();

    assert_eq!(stats.files_indexed, 1);
    assert!(
        stats.symbols_extracted > 0,
        "deeply nested file should produce symbols"
    );

    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let path: String = conn
        .query_row("SELECT path FROM files LIMIT 1", [], |row| row.get(0))
        .unwrap();
    assert!(
        path.contains("src/a/b/c/d/e/deep.ts"),
        "stored path should reflect deep nesting, got: {path}"
    );
}

#[test]
fn index_file_with_no_extension() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::write(tmp.path().join("Makefile"), "all:\n\techo hello\n").unwrap();
    // Also include one supported file so we confirm only that one is indexed.
    fs::write(tmp.path().join("ok.ts"), "export function ok() {}").unwrap();

    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    let stats = ndxr::indexer::index(&config, None).unwrap();

    // Only the .ts file should be indexed; Makefile has no supported extension.
    assert_eq!(stats.files_indexed, 1);
}

#[test]
fn index_empty_workspace() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    // No source files at all.

    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    let stats = ndxr::indexer::index(&config, None).unwrap();

    assert_eq!(stats.files_indexed, 0);
    assert_eq!(stats.symbols_extracted, 0);
    assert_eq!(stats.skipped, 0);
}

#[test]
fn index_paths_empty_list() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::write(tmp.path().join("a.ts"), "export function a() {}").unwrap();

    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    ndxr::indexer::index(&config, None).unwrap();

    // index_paths with an empty list should succeed immediately.
    let stats = ndxr::indexer::index_paths(&config, &[]).unwrap();
    assert_eq!(stats.files_indexed, 0);
    assert_eq!(stats.skipped, 0);
}

#[test]
fn index_paths_nonexistent_file() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::write(tmp.path().join("a.ts"), "export function a() {}").unwrap();

    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    ndxr::indexer::index(&config, None).unwrap();

    // Pass a nonexistent file path; should not crash.
    let bogus = vec![tmp.path().canonicalize().unwrap().join("does_not_exist.ts")];
    let result = ndxr::indexer::index_paths(&config, &bogus);
    assert!(
        result.is_ok(),
        "index_paths with nonexistent file must not crash"
    );
}

// ---------------------------------------------------------------------------
// Storage edge cases
// ---------------------------------------------------------------------------

#[test]
fn storage_reset_preserves_sessions() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".ndxr").join("index.db");
    let conn = ndxr::storage::db::open_or_create(&db_path).unwrap();

    conn.execute(
        "INSERT INTO sessions (id, started_at, last_active) VALUES ('sess1', 1000, 1000)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO observations (session_id, kind, content, detail_level, created_at) \
         VALUES ('sess1', 'manual', 'important observation', 2, 1001)",
        [],
    )
    .unwrap();

    ndxr::storage::db::reset_code_tables(&conn).unwrap();

    let session_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(session_count, 1, "session must survive reset_code_tables");

    let obs_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM observations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(obs_count, 1, "observation must survive reset_code_tables");
}

#[test]
fn storage_reset_clears_symbols_and_edges() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".ndxr").join("index.db");
    let conn = ndxr::storage::db::open_or_create(&db_path).unwrap();

    // Insert a file with a symbol and an edge.
    conn.execute(
        "INSERT INTO files (path, language, blake3_hash, line_count, byte_size, indexed_at) \
         VALUES ('x.ts', 'typescript', 'hash1', 5, 50, 1000)",
        [],
    )
    .unwrap();
    let fid = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO symbols (file_id, name, kind, fqn, start_line, end_line) \
         VALUES (?1, 'sym1', 'function', 'x.ts::sym1', 1, 5)",
        [fid],
    )
    .unwrap();
    let s1 = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO symbols (file_id, name, kind, fqn, start_line, end_line) \
         VALUES (?1, 'sym2', 'function', 'x.ts::sym2', 6, 10)",
        [fid],
    )
    .unwrap();
    let s2 = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO edges (from_id, to_id, kind) VALUES (?1, ?2, 'calls')",
        rusqlite::params![s1, s2],
    )
    .unwrap();

    ndxr::storage::db::reset_code_tables(&conn).unwrap();

    let file_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(file_count, 0, "files should be empty after reset");

    let sym_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
        .unwrap();
    assert_eq!(sym_count, 0, "symbols should be empty after reset");

    let edge_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
        .unwrap();
    assert_eq!(edge_count, 0, "edges should be empty after reset");
}

#[test]
fn storage_fts_trigger_insert() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".ndxr").join("index.db");
    let conn = ndxr::storage::db::open_or_create(&db_path).unwrap();

    conn.execute(
        "INSERT INTO files (path, language, blake3_hash, line_count, byte_size, indexed_at) \
         VALUES ('t.ts', 'typescript', 'h1', 3, 30, 1000)",
        [],
    )
    .unwrap();
    let fid = conn.last_insert_rowid();

    conn.execute(
        "INSERT INTO symbols (file_id, name, kind, fqn, start_line, end_line) \
         VALUES (?1, 'myFunction', 'function', 't.ts::myFunction', 1, 3)",
        [fid],
    )
    .unwrap();

    // The FTS trigger should have inserted a matching row automatically.
    let fts_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols_fts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        fts_count, 1,
        "FTS trigger should auto-insert on symbol insert"
    );

    // Verify the FTS content matches.
    let fts_name: String = conn
        .query_row(
            "SELECT name FROM symbols_fts WHERE symbols_fts MATCH '\"myFunction\"'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(fts_name, "myFunction");
}

#[test]
fn storage_fts_trigger_delete_cascade() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".ndxr").join("index.db");
    let conn = ndxr::storage::db::open_or_create(&db_path).unwrap();

    conn.execute(
        "INSERT INTO files (path, language, blake3_hash, line_count, byte_size, indexed_at) \
         VALUES ('del.ts', 'typescript', 'h2', 5, 50, 1000)",
        [],
    )
    .unwrap();
    let fid = conn.last_insert_rowid();

    conn.execute(
        "INSERT INTO symbols (file_id, name, kind, fqn, start_line, end_line) \
         VALUES (?1, 'alphaFunc', 'function', 'del.ts::alphaFunc', 1, 3)",
        [fid],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO symbols (file_id, name, kind, fqn, start_line, end_line) \
         VALUES (?1, 'betaFunc', 'function', 'del.ts::betaFunc', 4, 5)",
        [fid],
    )
    .unwrap();

    let pre_fts: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols_fts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(pre_fts, 2);

    // Delete the file; CASCADE should delete symbols, and the delete trigger
    // should clean up FTS.
    conn.execute("DELETE FROM files WHERE id = ?1", [fid])
        .unwrap();

    let post_fts: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols_fts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        post_fts, 0,
        "FTS entries must be cleaned when file is deleted"
    );
}

#[test]
fn storage_foreign_key_enforcement() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".ndxr").join("index.db");
    let conn = ndxr::storage::db::open_or_create(&db_path).unwrap();

    // Verify that foreign keys are enabled by checking the pragma.
    let fk_enabled: i64 = conn
        .pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .unwrap();
    assert_eq!(fk_enabled, 1, "foreign_keys pragma should be ON");

    // Attempt to insert an edge referencing nonexistent symbol IDs.
    let result = conn.execute(
        "INSERT INTO edges (from_id, to_id, kind) VALUES (999999, 999998, 'calls')",
        [],
    );
    assert!(
        result.is_err(),
        "inserting edge with nonexistent symbol IDs must fail"
    );
}

#[test]
fn storage_unique_constraint_files_path() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".ndxr").join("index.db");
    let conn = ndxr::storage::db::open_or_create(&db_path).unwrap();

    conn.execute(
        "INSERT INTO files (path, language, blake3_hash, line_count, byte_size, indexed_at) \
         VALUES ('dup.ts', 'typescript', 'aaa', 1, 10, 1000)",
        [],
    )
    .unwrap();

    // Inserting the same path again should violate the UNIQUE constraint.
    let result = conn.execute(
        "INSERT INTO files (path, language, blake3_hash, line_count, byte_size, indexed_at) \
         VALUES ('dup.ts', 'typescript', 'bbb', 2, 20, 1001)",
        [],
    );
    assert!(
        result.is_err(),
        "duplicate file path must violate UNIQUE constraint"
    );
}

// ---------------------------------------------------------------------------
// Reindex edge cases
// ---------------------------------------------------------------------------

#[test]
fn reindex_preserves_memory() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::write(tmp.path().join("a.ts"), "export function a() {}").unwrap();

    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    ndxr::indexer::index(&config, None).unwrap();

    // Insert session and observation into the DB.
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    conn.execute(
        "INSERT INTO sessions (id, started_at, last_active) VALUES ('mem_sess', 2000, 2000)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO observations (session_id, kind, content, detail_level, created_at) \
         VALUES ('mem_sess', 'manual', 'important decision', 2, 2001)",
        [],
    )
    .unwrap();
    drop(conn);

    // Reindex should preserve memory.
    ndxr::indexer::reindex(&config, None).unwrap();

    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(session_count, 1, "session must survive reindex");

    let obs_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM observations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(obs_count, 1, "observation must survive reindex");
}

#[test]
fn reindex_idempotent_symbol_count() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::write(
        tmp.path().join("lib.ts"),
        "export function alpha() {}\nexport function beta() {}\n",
    )
    .unwrap();

    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    let first = ndxr::indexer::reindex(&config, None).unwrap();
    let second = ndxr::indexer::reindex(&config, None).unwrap();

    assert_eq!(
        first.symbols_extracted, second.symbols_extracted,
        "reindex should produce the same symbol count each time"
    );
    assert_eq!(
        first.files_indexed, second.files_indexed,
        "reindex should index the same file count each time"
    );
}

// ---------------------------------------------------------------------------
// Walker edge cases
// ---------------------------------------------------------------------------

#[test]
fn walker_respects_ndxrignore_patterns() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::write(tmp.path().join("keep.ts"), "export function keep() {}").unwrap();
    fs::write(tmp.path().join("skip.test.ts"), "export function skip() {}").unwrap();
    fs::write(
        tmp.path().join("skip2.test.ts"),
        "export function skip2() {}",
    )
    .unwrap();

    // .ndxrignore with a glob pattern.
    fs::write(tmp.path().join(".ndxrignore"), "*.test.ts\n").unwrap();

    let files = ndxr::indexer::walker::walk_workspace(tmp.path()).unwrap();
    let names: Vec<String> = files
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();

    assert!(
        names.contains(&"keep.ts".to_string()),
        "keep.ts should be included"
    );
    assert!(
        !names.iter().any(|n| n.ends_with(".test.ts")),
        "*.test.ts files should be excluded by .ndxrignore, but found: {names:?}"
    );
}

#[test]
fn walker_skips_dot_directories() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::create_dir_all(tmp.path().join(".hidden")).unwrap();
    fs::write(
        tmp.path().join(".hidden/secret.ts"),
        "export function secret() {}",
    )
    .unwrap();
    fs::write(
        tmp.path().join("visible.ts"),
        "export function visible() {}",
    )
    .unwrap();

    let files = ndxr::indexer::walker::walk_workspace(tmp.path()).unwrap();
    let has_hidden = files
        .iter()
        .any(|p| p.display().to_string().contains(".hidden"));
    assert!(
        !has_hidden,
        "files in dot-prefixed directories should be skipped"
    );

    let has_visible = files
        .iter()
        .any(|p| p.display().to_string().contains("visible.ts"));
    assert!(has_visible, "visible.ts should be found");
}

// ---------------------------------------------------------------------------
// CLI edge cases
// ---------------------------------------------------------------------------

#[test]
fn cli_search_empty_query() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::write(tmp.path().join("a.ts"), "export function a() {}").unwrap();

    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .arg("index")
        .assert()
        .success();

    // Search with an empty string should not crash.
    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .args(["search", ""])
        .assert()
        .success();
}

#[test]
fn cli_search_before_index() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    // No index performed; search creates the DB on the fly and finds nothing.
    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .args(["search", "test"])
        .assert()
        .success()
        .stdout(contains("No results"));
}

#[test]
fn cli_skeleton_nonexistent_file() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::write(tmp.path().join("a.ts"), "export function a() {}").unwrap();

    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .arg("index")
        .assert()
        .success();

    // Skeleton for a file that does not exist in the index.
    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .args(["skeleton", "nonexistent.ts"])
        .assert()
        .success()
        .stdout(contains("No symbols"));
}

#[test]
fn cli_status_empty_workspace() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    // No files, no index — status should still work.

    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .arg("status")
        .assert()
        .success()
        .stdout(contains("Files:"));
}

#[test]
fn cli_index_creates_ndxr_dir() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::write(tmp.path().join("a.ts"), "export function a() {}").unwrap();

    assert!(
        !tmp.path().join(".ndxr").exists(),
        ".ndxr should not exist before index"
    );

    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .arg("index")
        .assert()
        .success();

    assert!(
        tmp.path().join(".ndxr").is_dir(),
        ".ndxr directory should be created"
    );
    assert!(
        tmp.path().join(".ndxr/index.db").is_file(),
        ".ndxr/index.db should exist after index"
    );
}

#[test]
fn cli_help_for_each_subcommand() {
    let subcommands = [
        "index", "reindex", "search", "status", "skeleton", "setup", "mcp",
    ];
    for sub in &subcommands {
        Command::cargo_bin("ndxr")
            .unwrap()
            .args([*sub, "--help"])
            .assert()
            .success();
    }
}
