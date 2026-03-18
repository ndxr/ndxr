//! Integration tests for symbol and edge extraction, file parsing, and the full indexing pipeline.

use std::fs;

use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helper: create a minimal TypeScript project for pipeline tests.
// ---------------------------------------------------------------------------

fn create_ts_project(tmp: &TempDir) {
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::create_dir_all(tmp.path().join("src")).unwrap();
    fs::write(
        tmp.path().join("src/main.ts"),
        "
import { greet } from './greet';
export function main() {
    greet(\"world\");
}
",
    )
    .unwrap();
    fs::write(
        tmp.path().join("src/greet.ts"),
        "
/** Greets someone by name */
export function greet(name: string): string {
    return `Hello, ${name}!`;
}
",
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// Full indexing pipeline tests
// ---------------------------------------------------------------------------

#[test]
fn full_index_creates_symbols_and_edges() {
    let tmp = TempDir::new().unwrap();
    create_ts_project(&tmp);

    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    let stats = ndxr::indexer::index(&config).unwrap();

    assert_eq!(stats.files_indexed, 2);
    assert!(stats.symbols_extracted > 0);

    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let file_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(file_count, 2);

    let symbol_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
        .unwrap();
    assert!(symbol_count >= 2, "at least main + greet expected");
}

#[test]
fn incremental_index_skips_unchanged() {
    let tmp = TempDir::new().unwrap();
    create_ts_project(&tmp);

    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    ndxr::indexer::index(&config).unwrap();

    let stats = ndxr::indexer::index(&config).unwrap();
    assert_eq!(stats.files_indexed, 0);
    assert_eq!(stats.skipped, 2);
}

#[test]
fn incremental_index_detects_changes() {
    let tmp = TempDir::new().unwrap();
    create_ts_project(&tmp);

    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    ndxr::indexer::index(&config).unwrap();

    fs::write(
        tmp.path().join("src/greet.ts"),
        "
export function greet(name: string): string {
    return `Hi, ${name}!`;
}
",
    )
    .unwrap();

    let stats = ndxr::indexer::index(&config).unwrap();
    assert_eq!(stats.files_indexed, 1);
    assert_eq!(stats.skipped, 1);
}

#[test]
fn reindex_clears_and_rebuilds() {
    let tmp = TempDir::new().unwrap();
    create_ts_project(&tmp);

    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    ndxr::indexer::index(&config).unwrap();
    let stats = ndxr::indexer::reindex(&config).unwrap();

    assert_eq!(stats.files_indexed, 2);
}

#[test]
fn tfidf_tables_populated_after_index() {
    let tmp = TempDir::new().unwrap();
    create_ts_project(&tmp);

    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    ndxr::indexer::index(&config).unwrap();

    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let tf_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM term_frequencies", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(tf_count > 0, "term_frequencies should have entries");

    let df_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM doc_frequencies", [], |row| row.get(0))
        .unwrap();
    assert!(df_count > 0, "doc_frequencies should have entries");
}

#[test]
fn fts5_populated_after_index() {
    let tmp = TempDir::new().unwrap();
    create_ts_project(&tmp);

    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    ndxr::indexer::index(&config).unwrap();

    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let fts_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols_fts", [], |row| row.get(0))
        .unwrap();
    assert!(
        fts_count > 0,
        "symbols_fts should have entries after indexing"
    );
}

// ---------------------------------------------------------------------------
// Symbol extraction tests
// ---------------------------------------------------------------------------

#[test]
fn extracts_typescript_function_symbol() {
    let source = "\n/** Validates a JWT token */\nexport async function validateToken(token: string): Promise<User> {\n    const decoded = jwt.verify(token);\n    return decoded;\n}\n";
    let config = ndxr::languages::get_language_config(".ts").unwrap();
    let symbols = ndxr::indexer::symbols::extract_symbols("src/auth.ts", source, config).unwrap();
    assert!(!symbols.is_empty(), "should extract at least one symbol");
    let sym = symbols
        .iter()
        .find(|s| s.name == "validateToken")
        .expect("should find validateToken");
    assert_eq!(sym.kind, "function");
    assert!(sym.fqn.contains("validateToken"));
    assert!(sym.body_hash.is_some());
}

#[test]
fn extracts_typescript_class_and_method() {
    let source = "\nclass AuthService {\n    validate(token: string): boolean {\n        return true;\n    }\n}\n";
    let config = ndxr::languages::get_language_config(".ts").unwrap();
    let symbols = ndxr::indexer::symbols::extract_symbols("src/auth.ts", source, config).unwrap();

    let class = symbols.iter().find(|s| s.name == "AuthService");
    assert!(class.is_some(), "should find AuthService class");
    assert_eq!(class.unwrap().kind, "class");

    let method = symbols.iter().find(|s| s.name == "validate");
    assert!(method.is_some(), "should find validate method");
    assert_eq!(method.unwrap().kind, "method");
    // Method FQN should include the class.
    assert!(
        method.unwrap().fqn.contains("AuthService"),
        "method FQN should contain class name"
    );
}

#[test]
fn extracts_import_edges() {
    let source = "\nimport { User } from '../models/user';\nexport function validateToken(token: string): User { return verify(token) as User; }\n";
    let config = ndxr::languages::get_language_config(".ts").unwrap();
    let edges = ndxr::indexer::symbols::extract_edges("src/auth.ts", source, config).unwrap();
    assert!(
        edges.iter().any(|e| e.kind == "imports"),
        "should find at least one import edge"
    );
}

#[test]
fn extracts_call_edges() {
    let source = "function foo() { bar(); baz(); }";
    let config = ndxr::languages::get_language_config(".ts").unwrap();
    let edges = ndxr::indexer::symbols::extract_edges("test.ts", source, config).unwrap();
    let call_edges: Vec<_> = edges.iter().filter(|e| e.kind == "calls").collect();
    assert!(
        call_edges.len() >= 2,
        "should find at least 2 call edges, found {}",
        call_edges.len()
    );
}

#[test]
fn extracts_python_function() {
    let source = r#"
def hello(name: str) -> str:
    """Greet the user."""
    return f"Hello, {name}!"
"#;
    let config = ndxr::languages::get_language_config(".py").unwrap();
    let symbols = ndxr::indexer::symbols::extract_symbols("app/greet.py", source, config).unwrap();
    let sym = symbols
        .iter()
        .find(|s| s.name == "hello")
        .expect("should find hello function");
    assert_eq!(sym.kind, "function");
}

#[test]
fn extracts_rust_function() {
    let source = "pub fn process(data: &[u8]) -> Vec<u8> { data.to_vec() }";
    let config = ndxr::languages::get_language_config(".rs").unwrap();
    let symbols = ndxr::indexer::symbols::extract_symbols("src/lib.rs", source, config).unwrap();
    let sym = symbols
        .iter()
        .find(|s| s.name == "process")
        .expect("should find process function");
    assert_eq!(sym.kind, "function");
    assert!(sym.is_exported, "pub fn should be exported");
}

#[test]
fn extracts_go_exported_function() {
    let source = "package main\n\nfunc ProcessData(data []byte) []byte { return data }";
    let config = ndxr::languages::get_language_config(".go").unwrap();
    let symbols = ndxr::indexer::symbols::extract_symbols("main.go", source, config).unwrap();
    let sym = symbols
        .iter()
        .find(|s| s.name == "ProcessData")
        .expect("should find ProcessData");
    assert_eq!(sym.kind, "function");
    assert!(sym.is_exported, "Go uppercase name should be exported");
}

#[test]
fn go_lowercase_not_exported() {
    let source = "package main\n\nfunc helper() {}";
    let config = ndxr::languages::get_language_config(".go").unwrap();
    let symbols = ndxr::indexer::symbols::extract_symbols("main.go", source, config).unwrap();
    let sym = symbols
        .iter()
        .find(|s| s.name == "helper")
        .expect("should find helper");
    assert!(!sym.is_exported, "Go lowercase name should not be exported");
}

// ---------------------------------------------------------------------------
// Parser dispatch tests
// ---------------------------------------------------------------------------

#[test]
fn parse_file_produces_result() {
    let tmp = TempDir::new().unwrap();
    let src_dir = tmp.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let file = src_dir.join("hello.ts");
    fs::write(&file, "export function hello() { return 42; }").unwrap();

    let result = ndxr::indexer::parser::parse_file(tmp.path(), &file).unwrap();
    assert_eq!(result.language, "typescript");
    assert!(!result.blake3_hash.is_empty());
    assert!(result.line_count > 0);
    assert!(result.byte_size > 0);
    assert!(!result.symbols.is_empty());
}

#[test]
fn parse_files_parallel_works() {
    let tmp = TempDir::new().unwrap();
    let src_dir = tmp.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(
        src_dir.join("a.ts"),
        "export function aFunc() { return 1; }",
    )
    .unwrap();
    fs::write(src_dir.join("b.py"), "def b_func():\n    return 2\n").unwrap();
    fs::write(src_dir.join("c.rs"), "pub fn c_func() -> i32 { 3 }").unwrap();

    let files = vec![
        src_dir.join("a.ts"),
        src_dir.join("b.py"),
        src_dir.join("c.rs"),
    ];

    let results = ndxr::indexer::parser::parse_files_parallel(tmp.path(), &files);
    assert_eq!(results.len(), 3);
}

#[test]
fn parse_files_parallel_skips_bad_files() {
    let tmp = TempDir::new().unwrap();
    let src_dir = tmp.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(src_dir.join("good.ts"), "function good() {}").unwrap();

    let files = vec![
        src_dir.join("good.ts"),
        src_dir.join("nonexistent.ts"), // Does not exist.
    ];

    let results = ndxr::indexer::parser::parse_files_parallel(tmp.path(), &files);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].language, "typescript");
}

// ---------------------------------------------------------------------------
// Edge case: empty file and file deletion
// ---------------------------------------------------------------------------

#[test]
fn index_empty_file_produces_no_symbols() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    // Create an empty .ts file (0 bytes)
    fs::write(tmp.path().join("empty.ts"), "").unwrap();

    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    let stats = ndxr::indexer::index(&config).unwrap();

    assert_eq!(stats.files_indexed, 1);
    assert_eq!(stats.symbols_extracted, 0);
}

#[test]
fn incremental_index_detects_file_deletion() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::write(tmp.path().join("a.ts"), "export function a() {}").unwrap();
    fs::write(tmp.path().join("b.ts"), "export function b() {}").unwrap();

    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    ndxr::indexer::index(&config).unwrap();

    // Delete one file
    fs::remove_file(tmp.path().join("b.ts")).unwrap();

    let stats = ndxr::indexer::index(&config).unwrap();
    assert_eq!(stats.files_deleted, 1);
    assert_eq!(stats.skipped, 1); // a.ts unchanged

    // Verify DB: only 1 file remains
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let file_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(file_count, 1);

    // Verify symbols from deleted file are gone
    let sym_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM symbols WHERE fqn LIKE '%b.ts%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sym_count, 0);
}

#[test]
fn index_paths_targeted_reindex() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::write(tmp.path().join("a.ts"), "export function a() {}").unwrap();
    fs::write(tmp.path().join("b.ts"), "export function b() {}").unwrap();

    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    ndxr::indexer::index(&config).unwrap();

    // Modify only b.ts
    fs::write(tmp.path().join("b.ts"), "export function b_new() {}").unwrap();

    // Use index_paths with only the changed file
    let changed = vec![tmp.path().canonicalize().unwrap().join("b.ts")];
    let stats = ndxr::indexer::index_paths(&config, &changed).unwrap();
    assert_eq!(stats.files_indexed, 1);
    // a.ts was not even checked since it wasn't in the changed paths
}
