//! End-to-end tests that exercise the full CLI pipeline and verify database state.
//!
//! Unlike the unit/integration tests, these tests:
//! - Run the actual `ndxr` binary via CLI
//! - Create realistic multi-language projects with cross-file references
//! - Open the `SQLite` database after each CLI command and verify exact state
//! - Test the full lifecycle: index → search → modify → re-index → staleness

use std::fs;

use assert_cmd::Command;
use predicates::str::contains;
use rusqlite::params;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helper: create a realistic multi-language project
// ---------------------------------------------------------------------------

/// Creates a realistic TypeScript project with cross-file references, classes,
/// functions, imports, and docstrings.
#[allow(clippy::too_many_lines)] // fixture with 5 inline files; splitting would just hide the layout
fn create_typescript_project(tmp: &TempDir) {
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::create_dir_all(tmp.path().join("src/auth")).unwrap();
    fs::create_dir_all(tmp.path().join("src/db")).unwrap();
    fs::create_dir_all(tmp.path().join("src/api")).unwrap();

    fs::write(
        tmp.path().join("src/auth/token.ts"),
        r"
/** Validates JWT tokens against the signing key */
export async function validateToken(token: string): Promise<boolean> {
    const decoded = parseJwt(token);
    return decoded.exp > Date.now();
}

/** Parses a JWT token without verification */
export function parseJwt(token: string): JwtPayload {
    const parts = token.split('.');
    return JSON.parse(atob(parts[1]));
}

export interface JwtPayload {
    sub: string;
    exp: number;
    iat: number;
}
",
    )
    .unwrap();

    fs::write(
        tmp.path().join("src/auth/service.ts"),
        r"
import { validateToken } from './token';

/** Authentication service managing user sessions */
export class AuthService {
    constructor(private secret: string) {}

    /** Authenticates a request by validating its token */
    async authenticate(request: Request): Promise<User | null> {
        const token = this.extractToken(request);
        if (!token) return null;
        const valid = await validateToken(token);
        return valid ? this.getUser(token) : null;
    }

    private extractToken(request: Request): string | null {
        return null;
    }

    private getUser(token: string): User {
        return { id: '1', name: 'test' };
    }
}

export interface User {
    id: string;
    name: string;
}
",
    )
    .unwrap();

    fs::write(
        tmp.path().join("src/db/connection.ts"),
        r"
/** Database connection pool manager */
export class DatabasePool {
    private connections: Connection[] = [];

    constructor(private url: string, private maxSize: number) {}

    /** Gets a connection from the pool */
    async getConnection(): Promise<Connection> {
        return this.createConnection();
    }

    /** Returns a connection to the pool */
    release(conn: Connection): void {
        if (this.connections.length < this.maxSize) {
            this.connections.push(conn);
        }
    }

    private createConnection(): Connection {
        return { url: this.url, active: true };
    }
}

export interface Connection {
    url: string;
    active: boolean;
}
",
    )
    .unwrap();

    fs::write(
        tmp.path().join("src/db/queries.ts"),
        r"
import { DatabasePool } from './connection';

/** Executes a user lookup query */
export async function findUserById(pool: DatabasePool, id: string): Promise<any> {
    const conn = await pool.getConnection();
    try {
        return { id, name: 'found' };
    } finally {
        pool.release(conn);
    }
}
",
    )
    .unwrap();

    fs::write(
        tmp.path().join("src/api/routes.ts"),
        r"
import { AuthService } from '../auth/service';
import { findUserById } from '../db/queries';
import { DatabasePool } from '../db/connection';

/** Sets up API routes with authentication */
export function setupRoutes(auth: AuthService, db: DatabasePool): Route[] {
    return [
        { path: '/users/:id', handler: async (req) => {
            const user = await auth.authenticate(req);
            if (!user) return { status: 401 };
            return findUserById(db, req.params.id);
        }},
    ];
}

export interface Route {
    path: string;
    handler: (req: any) => Promise<any>;
}
",
    )
    .unwrap();
}

/// Creates a multi-language project (TypeScript + Python + Rust + Go).
fn create_multilang_project(tmp: &TempDir) {
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::create_dir_all(tmp.path().join("src")).unwrap();

    fs::write(
        tmp.path().join("src/main.ts"),
        r"
/** Entry point for the application */
export function main(): void {
    const config = loadConfig();
    startServer(config);
}

function loadConfig(): Config {
    return { port: 3000, host: 'localhost' };
}

function startServer(config: Config): void {
    console.log(`Server on ${config.host}:${config.port}`);
}

interface Config {
    port: number;
    host: string;
}
",
    )
    .unwrap();

    fs::write(
        tmp.path().join("src/handler.py"),
        r#"
"""Request handler module."""

class RequestHandler:
    """Handles incoming HTTP requests."""

    def __init__(self, config: dict):
        self.config = config

    def handle_get(self, path: str) -> dict:
        """Process GET request."""
        return {"status": 200, "path": path}

    def handle_post(self, path: str, body: dict) -> dict:
        """Process POST request."""
        return {"status": 201, "path": path, "body": body}

def create_handler(config: dict) -> RequestHandler:
    """Factory function for handlers."""
    return RequestHandler(config)
"#,
    )
    .unwrap();

    fs::write(
        tmp.path().join("src/lib.rs"),
        r"
/// Configuration for the service.
pub struct ServiceConfig {
    pub port: u16,
    pub host: String,
}

/// Creates a new service configuration.
pub fn new_config(port: u16, host: &str) -> ServiceConfig {
    ServiceConfig {
        port,
        host: host.to_string(),
    }
}

/// Validates the configuration.
pub fn validate_config(config: &ServiceConfig) -> bool {
    config.port > 0 && !config.host.is_empty()
}
",
    )
    .unwrap();

    fs::write(
        tmp.path().join("src/server.go"),
        r"
package main

// Server represents the HTTP server.
type Server struct {
	Host string
	Port int
}

// NewServer creates a new server instance.
func NewServer(host string, port int) *Server {
	return &Server{Host: host, Port: port}
}

// Start begins listening for connections.
func (s *Server) Start() error {
	return nil
}

// Stop gracefully shuts down the server.
func (s *Server) Stop() error {
	return nil
}
",
    )
    .unwrap();
}

/// Opens the ndxr database for a workspace.
fn open_db(tmp: &TempDir) -> rusqlite::Connection {
    let db_path = tmp.path().join(".ndxr").join("index.db");
    ndxr::storage::db::open_or_create(&db_path).unwrap()
}

/// Runs `ndxr <args>` in the given workspace directory.
fn ndxr(tmp: &TempDir, args: &[&str]) -> assert_cmd::assert::Assert {
    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .args(args)
        .assert()
}

// ===========================================================================
// E2E: Index and verify database state
// ===========================================================================

#[test]
fn e2e_index_creates_correct_file_entries() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    let conn = open_db(&tmp);

    // Verify exact file count.
    let file_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(file_count, 5, "should index exactly 5 TypeScript files");

    // Verify each file path is stored correctly (relative, forward slashes).
    let mut paths: Vec<String> = conn
        .prepare("SELECT path FROM files ORDER BY path")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    paths.sort();

    assert_eq!(
        paths,
        vec![
            "src/api/routes.ts",
            "src/auth/service.ts",
            "src/auth/token.ts",
            "src/db/connection.ts",
            "src/db/queries.ts",
        ]
    );

    // Verify all files have non-empty hashes.
    let empty_hashes: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE blake3_hash = '' OR blake3_hash IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(empty_hashes, 0, "all files should have BLAKE3 hashes");

    // Verify all files have correct language.
    let languages: Vec<String> = conn
        .prepare("SELECT DISTINCT language FROM files")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert_eq!(languages, vec!["typescript"]);
}

#[test]
fn e2e_index_extracts_correct_symbols() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    let conn = open_db(&tmp);

    // Verify total symbol count is reasonable.
    let symbol_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
        .unwrap();
    assert!(
        symbol_count >= 10,
        "should extract at least 10 symbols, got {symbol_count}"
    );

    // Verify specific expected symbols exist with correct attributes.
    let validate_token: (String, String, bool) = conn
        .query_row(
            "SELECT s.kind, f.path, s.is_exported FROM symbols s \
             JOIN files f ON s.file_id = f.id \
             WHERE s.name = 'validateToken'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(validate_token.0, "function");
    assert_eq!(validate_token.1, "src/auth/token.ts");
    assert!(validate_token.2, "validateToken should be exported");

    let auth_service: (String, String, bool) = conn
        .query_row(
            "SELECT s.kind, f.path, s.is_exported FROM symbols s \
             JOIN files f ON s.file_id = f.id \
             WHERE s.name = 'AuthService'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(auth_service.0, "class");
    assert_eq!(auth_service.1, "src/auth/service.ts");
    assert!(auth_service.2, "AuthService should be exported");

    let database_pool: (String, String) = conn
        .query_row(
            "SELECT s.kind, f.path FROM symbols s \
             JOIN files f ON s.file_id = f.id \
             WHERE s.name = 'DatabasePool'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(database_pool.0, "class");
    assert_eq!(database_pool.1, "src/db/connection.ts");

    // Verify FQN format is correct (file::name pattern).
    let fqns: Vec<String> = conn
        .prepare("SELECT fqn FROM symbols WHERE name = 'validateToken'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert!(
        fqns.iter()
            .any(|f| f.contains("token.ts") && f.contains("validateToken")),
        "FQN should contain file and symbol name, got: {fqns:?}"
    );
}

#[test]
fn e2e_index_extracts_edges() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    let conn = open_db(&tmp);

    // Verify edges exist (imports and calls).
    let edge_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
        .unwrap();
    assert!(
        edge_count > 0,
        "should extract at least some edges, got {edge_count}"
    );

    // Verify edge kinds are valid.
    let edge_kinds: Vec<String> = conn
        .prepare("SELECT DISTINCT kind FROM edges")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    for kind in &edge_kinds {
        assert!(
            ["calls", "imports"].contains(&kind.as_str()),
            "unexpected edge kind: {kind}"
        );
    }
}

#[test]
fn e2e_index_populates_fts5() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    let conn = open_db(&tmp);

    // Verify FTS5 table is populated.
    let fts_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM symbols_fts WHERE symbols_fts MATCH '\"validate\"'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        fts_count > 0,
        "FTS5 should find 'validate' in indexed symbols"
    );

    // Verify FTS5 for docstrings.
    let doc_matches: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM symbols_fts WHERE symbols_fts MATCH '\"authentication\"'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        doc_matches > 0,
        "FTS5 should index docstrings containing 'authentication'"
    );
}

#[test]
fn e2e_index_populates_tfidf() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    let conn = open_db(&tmp);

    // Verify term_frequencies populated.
    let tf_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM term_frequencies", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(
        tf_count > 0,
        "term_frequencies should be populated, got {tf_count}"
    );

    // Verify doc_frequencies populated.
    let df_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM doc_frequencies", [], |row| row.get(0))
        .unwrap();
    assert!(
        df_count > 0,
        "doc_frequencies should be populated, got {df_count}"
    );

    // Verify all term frequencies are positive.
    let negative_tf: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM term_frequencies WHERE tf <= 0",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(negative_tf, 0, "all term frequencies should be positive");
}

// ===========================================================================
// E2E: Multi-language indexing
// ===========================================================================

#[test]
fn e2e_index_multilang_all_languages_detected() {
    let tmp = TempDir::new().unwrap();
    create_multilang_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    let conn = open_db(&tmp);

    let file_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(file_count, 4, "should index 4 files (ts, py, rs, go)");

    // Verify each language is detected correctly.
    let mut languages: Vec<(String, String)> = conn
        .prepare("SELECT path, language FROM files ORDER BY path")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    languages.sort_by(|a, b| a.0.cmp(&b.0));

    assert_eq!(languages[0], ("src/handler.py".into(), "python".into()));
    assert_eq!(languages[1], ("src/lib.rs".into(), "rust".into()));
    assert_eq!(languages[2], ("src/main.ts".into(), "typescript".into()));
    assert_eq!(languages[3], ("src/server.go".into(), "go".into()));
}

#[test]
fn e2e_index_multilang_symbols_per_language() {
    let tmp = TempDir::new().unwrap();
    create_multilang_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    let conn = open_db(&tmp);

    // Verify Python symbols.
    let py_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM symbols s JOIN files f ON s.file_id = f.id \
             WHERE f.language = 'python'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        py_count >= 3,
        "Python should have at least 3 symbols (class + 2 methods + factory), got {py_count}"
    );

    // Verify Rust symbols.
    let rs_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM symbols s JOIN files f ON s.file_id = f.id \
             WHERE f.language = 'rust'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        rs_count >= 2,
        "Rust should have at least 2 symbols (struct + functions), got {rs_count}"
    );

    // Verify Go symbols.
    let go_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM symbols s JOIN files f ON s.file_id = f.id \
             WHERE f.language = 'go'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        go_count >= 3,
        "Go should have at least 3 symbols (struct + methods), got {go_count}"
    );

    // Verify TypeScript symbols.
    let ts_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM symbols s JOIN files f ON s.file_id = f.id \
             WHERE f.language = 'typescript'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        ts_count >= 3,
        "TypeScript should have at least 3 symbols, got {ts_count}"
    );
}

// ===========================================================================
// E2E: Incremental indexing via CLI
// ===========================================================================

#[test]
fn e2e_incremental_index_skips_unchanged_files() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    // First index.
    ndxr(&tmp, &["index"]).success();

    let conn = open_db(&tmp);
    let first_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
        .unwrap();
    drop(conn);

    // Second index without changes — output should say "0 new, 0 deleted, 5 skipped".
    ndxr(&tmp, &["index"])
        .success()
        .stdout(contains("0 new"))
        .stdout(contains("5 skipped"));

    // Symbol count should be unchanged.
    let conn = open_db(&tmp);
    let second_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
        .unwrap();
    assert_eq!(first_count, second_count, "symbol count should not change");
}

#[test]
fn e2e_incremental_index_detects_modified_file() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    let conn = open_db(&tmp);
    let orig_sym_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
        .unwrap();
    drop(conn);

    // Modify a file — add a new function.
    fs::write(
        tmp.path().join("src/auth/token.ts"),
        r#"
/** Validates JWT tokens against the signing key */
export async function validateToken(token: string): Promise<boolean> {
    const decoded = parseJwt(token);
    return decoded.exp > Date.now();
}

export function parseJwt(token: string): JwtPayload {
    const parts = token.split('.');
    return JSON.parse(atob(parts[1]));
}

/** NEW: Refreshes an expired token */
export function refreshToken(oldToken: string): string {
    return "new-token-" + oldToken;
}

export interface JwtPayload {
    sub: string;
    exp: number;
    iat: number;
}
"#,
    )
    .unwrap();

    // Re-index — should detect 1 changed file.
    ndxr(&tmp, &["index"])
        .success()
        .stdout(contains("1 new"))
        .stdout(contains("4 skipped"));

    // Verify the new symbol exists in DB.
    let conn = open_db(&tmp);
    let refresh_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM symbols WHERE name = 'refreshToken'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(refresh_exists, "refreshToken should exist after re-index");

    // Symbol count should have increased.
    let new_sym_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
        .unwrap();
    assert!(
        new_sym_count > orig_sym_count,
        "symbol count should increase ({orig_sym_count} -> {new_sym_count})"
    );
}

#[test]
fn e2e_incremental_index_detects_deleted_file() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    // Delete a file.
    fs::remove_file(tmp.path().join("src/db/queries.ts")).unwrap();

    // Re-index — should detect deletion.
    ndxr(&tmp, &["index"])
        .success()
        .stdout(contains("1 deleted"));

    // Verify the file is gone from DB.
    let conn = open_db(&tmp);
    let file_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM files WHERE path = 'src/db/queries.ts'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!file_exists, "deleted file should not be in DB");

    // Verify CASCADE removed its symbols.
    let orphan_syms: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM symbols WHERE fqn LIKE '%queries.ts%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        orphan_syms, 0,
        "symbols from deleted file should be removed"
    );
}

// ===========================================================================
// E2E: Reindex via CLI
// ===========================================================================

#[test]
fn e2e_reindex_preserves_memory_in_db() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    // Save a session and observation directly in DB.
    let conn = open_db(&tmp);
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();
    ndxr::memory::store::save_observation(
        &conn,
        &ndxr::memory::store::NewObservation {
            session_id: session_id.clone(),
            kind: "decision".into(),
            content: "JWT auth uses RS256 signing".into(),
            headline: Some("JWT signing algorithm".into()),
            detail_level: 2,
            linked_fqns: vec!["src/auth/token.ts::validateToken".into()],
        },
    )
    .unwrap();
    drop(conn);

    // Reindex — should clear code tables but preserve memory.
    ndxr(&tmp, &["reindex"])
        .success()
        .stdout(contains("Re-indexed"));

    let conn = open_db(&tmp);

    // Memory should be preserved.
    let session_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(session_exists, "session should survive reindex");

    let obs_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM observations WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(obs_count, 1, "observation should survive reindex");

    // Code tables should be fully repopulated.
    let file_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(file_count, 5, "all 5 files should be re-indexed");
}

// ===========================================================================
// E2E: Search via CLI verifies output
// ===========================================================================

#[test]
fn e2e_search_finds_specific_symbols() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    // Search for "validateToken" — should find it.
    ndxr(&tmp, &["search", "validateToken"])
        .success()
        .stdout(contains("validateToken"))
        .stdout(contains("src/auth/token.ts"));

    // Search for "DatabasePool" — should find the class.
    ndxr(&tmp, &["search", "DatabasePool"])
        .success()
        .stdout(contains("DatabasePool"))
        .stdout(contains("src/db/connection.ts"));

    // Search for "setupRoutes" — should find it in api/routes.ts.
    ndxr(&tmp, &["search", "setupRoutes"])
        .success()
        .stdout(contains("setupRoutes"))
        .stdout(contains("src/api/routes.ts"));
}

#[test]
fn e2e_search_explain_shows_score_components() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    let output = ndxr(&tmp, &["search", "authentication", "--explain"]).success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    // Verify score breakdown components are present.
    assert!(stdout.contains("bm25="), "should show BM25 score");
    assert!(stdout.contains("tfidf="), "should show TF-IDF score");
    assert!(
        stdout.contains("centrality="),
        "should show centrality score"
    );
    assert!(stdout.contains("intent="), "should show detected intent");
}

#[test]
fn e2e_search_intent_override_reflected_in_output() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    // Debug intent.
    ndxr(
        &tmp,
        &["search", "validate", "--intent", "debug", "--explain"],
    )
    .success()
    .stdout(contains("intent=debug"));

    // Refactor intent.
    ndxr(
        &tmp,
        &["search", "validate", "--intent", "refactor", "--explain"],
    )
    .success()
    .stdout(contains("intent=refactor"));
}

#[test]
fn e2e_search_limit_restricts_output() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    let output = ndxr(&tmp, &["search", "function", "-n", "2"]).success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    // Count result lines (each result starts with "N. ").
    let result_lines = stdout
        .lines()
        .filter(|l| l.starts_with("1.") || l.starts_with("2.") || l.starts_with("3."))
        .count();
    assert!(
        result_lines <= 2,
        "should have at most 2 results with -n 2, got {result_lines}"
    );
}

// ===========================================================================
// E2E: Status via CLI matches DB state
// ===========================================================================

#[test]
fn e2e_status_matches_db_counts() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    // Get counts from DB.
    let conn = open_db(&tmp);
    let db_files: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
        .unwrap();
    let db_symbols: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
        .unwrap();
    let db_edges: i64 = conn
        .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
        .unwrap();
    drop(conn);

    // Verify JSON status output matches.
    let output = ndxr(&tmp, &["status", "--json"]).success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(json["files"], db_files, "status files should match DB");
    assert_eq!(
        json["symbols"], db_symbols,
        "status symbols should match DB"
    );
    assert_eq!(json["edges"], db_edges, "status edges should match DB");
    assert!(
        json["db_size_bytes"].as_u64().unwrap() > 0,
        "DB should have non-zero size"
    );
}

#[test]
fn e2e_status_text_shows_all_fields() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    ndxr(&tmp, &["status"])
        .success()
        .stdout(contains("Files:"))
        .stdout(contains("Symbols:"))
        .stdout(contains("Edges:"))
        .stdout(contains("Languages:"))
        .stdout(contains("supported"))
        .stdout(contains("Sessions:"))
        .stdout(contains("Observations:"))
        .stdout(contains("DB size:"))
        .stdout(contains("Last indexed:"));
}

// ===========================================================================
// E2E: Skeleton via CLI
// ===========================================================================

#[test]
fn e2e_skeleton_shows_class_structure() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    let output = ndxr(&tmp, &["skeleton", "src/auth/service.ts"]).success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    // Should show class and its methods as signatures.
    assert!(stdout.contains("AuthService"), "should show AuthService");
    assert!(
        stdout.contains("authenticate"),
        "should show authenticate method"
    );
}

#[test]
fn e2e_skeleton_shows_function_signatures() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    let output = ndxr(&tmp, &["skeleton", "src/auth/token.ts"]).success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    assert!(
        stdout.contains("validateToken"),
        "should show validateToken signature"
    );
    assert!(
        stdout.contains("parseJwt"),
        "should show parseJwt signature"
    );
}

#[test]
fn e2e_skeleton_multiple_files() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    let output = ndxr(
        &tmp,
        &["skeleton", "src/auth/token.ts", "src/db/connection.ts"],
    )
    .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    // Both files should appear.
    assert!(
        stdout.contains("src/auth/token.ts"),
        "should show token.ts header"
    );
    assert!(
        stdout.contains("src/db/connection.ts"),
        "should show connection.ts header"
    );
}

// ===========================================================================
// E2E: Full lifecycle — index → search → modify → re-index → staleness
// ===========================================================================

#[test]
fn e2e_full_lifecycle_with_staleness_detection() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    // 1. Index.
    ndxr(&tmp, &["index"]).success();

    // 2. Save an observation linked to validateToken.
    let conn = open_db(&tmp);
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();
    ndxr::memory::store::save_observation(
        &conn,
        &ndxr::memory::store::NewObservation {
            session_id,
            kind: "insight".into(),
            content: "validateToken checks JWT expiry field".into(),
            headline: Some("JWT expiry check".into()),
            detail_level: 2,
            linked_fqns: vec!["src/auth/token.ts::validateToken".into()],
        },
    )
    .unwrap();

    // Verify observation is NOT stale.
    let is_stale: bool = conn
        .query_row(
            "SELECT is_stale FROM observations WHERE content LIKE '%JWT expiry%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!is_stale, "observation should not be stale initially");
    drop(conn);

    // 3. Modify the file containing validateToken (change signature).
    fs::write(
        tmp.path().join("src/auth/token.ts"),
        r"
/** Validates JWT tokens with enhanced security */
export async function validateToken(token: string, secret: string): Promise<boolean> {
    const decoded = parseJwt(token);
    return decoded.exp > Date.now() && decoded.iss === 'ndxr';
}

export function parseJwt(token: string): JwtPayload {
    const parts = token.split('.');
    return JSON.parse(atob(parts[1]));
}

export interface JwtPayload {
    sub: string;
    exp: number;
    iat: number;
    iss: string;
}
",
    )
    .unwrap();

    // 4. Re-index — should detect change and mark observation stale.
    let output = ndxr(&tmp, &["index"]).success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("1 new"),
        "should show 1 re-indexed file, got: {stdout}"
    );

    // 5. Verify observation is now marked stale.
    let conn = open_db(&tmp);
    let is_stale: bool = conn
        .query_row(
            "SELECT is_stale FROM observations WHERE content LIKE '%JWT expiry%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        is_stale,
        "observation should be marked stale after linked symbol changed"
    );

    // 6. Verify the modified symbol has new signature in DB.
    let new_sig: Option<String> = conn
        .query_row(
            "SELECT signature FROM symbols WHERE name = 'validateToken'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        new_sig.as_ref().is_some_and(|s| s.contains("secret")),
        "validateToken signature should now include 'secret' param, got: {new_sig:?}"
    );
}

// ===========================================================================
// E2E: PageRank centrality in DB
// ===========================================================================

#[test]
fn e2e_index_computes_pagerank() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    let conn = open_db(&tmp);

    // Verify at least some symbols have non-zero centrality.
    let nonzero_centrality: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM symbols WHERE centrality > 0.0",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        nonzero_centrality > 0,
        "at least some symbols should have non-zero centrality"
    );

    // All centrality values should be in [0, 1].
    let out_of_range: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM symbols WHERE centrality < 0.0 OR centrality > 1.0",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(out_of_range, 0, "all centrality values should be in [0, 1]");
}

// ===========================================================================
// E2E: Status shows indexed languages
// ===========================================================================

#[test]
fn e2e_status_json_includes_languages() {
    let tmp = TempDir::new().unwrap();
    create_multilang_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    let output = ndxr(&tmp, &["status", "--json"]).success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    let languages = json["languages"].as_array().unwrap();
    assert!(
        languages.len() >= 3,
        "should detect at least 3 languages, got {languages:?}"
    );

    let supported = json["languages_supported"].as_u64().unwrap();
    assert!(
        supported >= 10,
        "should report at least 10 supported languages"
    );
}

#[test]
fn e2e_status_text_shows_indexed_languages() {
    let tmp = TempDir::new().unwrap();
    create_multilang_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    ndxr(&tmp, &["status"])
        .success()
        .stdout(contains("typescript"))
        .stdout(contains("python"))
        .stdout(contains("supported"));
}

// ===========================================================================
// E2E: Memory search persists scores
// ===========================================================================

#[test]
fn e2e_memory_search_persists_score_to_db() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    let conn = open_db(&tmp);
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    // Save an observation.
    let obs_id = ndxr::memory::store::save_observation(
        &conn,
        &ndxr::memory::store::NewObservation {
            session_id,
            kind: "insight".into(),
            content: "JWT tokens are validated using RS256 algorithm".into(),
            headline: Some("JWT RS256 validation".into()),
            detail_level: 2,
            linked_fqns: vec!["src/auth/token.ts::validateToken".into()],
        },
    )
    .unwrap();

    // Score should be NULL before any search.
    let score_before: Option<f64> = conn
        .query_row(
            "SELECT score FROM observations WHERE id = ?1",
            params![obs_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        score_before.is_none(),
        "score should be NULL before search, got {score_before:?}"
    );

    // Search for the observation.
    let results = ndxr::memory::search::search_memories(
        &conn,
        &ndxr::memory::search::MemorySearchQuery {
            query: "JWT validation",
            pivot_fqns: &[],
            limit: 10,
            include_stale: true,
            recency_half_life_days: 7.0,
            kind: None,
            exclude_auto: false,
        },
    )
    .unwrap();
    assert!(!results.is_empty(), "should find the observation");

    // Score should now be persisted in the DB.
    let score_after: Option<f64> = conn
        .query_row(
            "SELECT score FROM observations WHERE id = ?1",
            params![obs_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        score_after.is_some(),
        "score should be persisted after search"
    );
    assert!(
        score_after.unwrap() > 0.0,
        "persisted score should be positive, got {score_after:?}"
    );

    // Persisted score should match the search result score.
    let search_score = results[0].memory_score;
    let db_score = score_after.unwrap();
    assert!(
        (search_score - db_score).abs() < f64::EPSILON,
        "DB score ({db_score}) should match search score ({search_score})"
    );
}

// ===========================================================================
// E2E: Search results are ranked correctly
// ===========================================================================

#[test]
fn e2e_search_ranks_exact_match_highest() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    let conn = open_db(&tmp);
    let graph = ndxr::graph::builder::build_graph(&conn).unwrap();
    ndxr::graph::centrality::compute_and_store(&conn, &graph).unwrap();

    // Search for exact symbol name — it should be the top result.
    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "validateToken", 5, None, None).unwrap();

    assert!(!results.is_empty(), "should find results");
    assert_eq!(
        results[0].name, "validateToken",
        "exact name match should be top result, got: {}",
        results[0].name
    );
    assert!(
        results[0].score > 0.0,
        "top result should have positive score"
    );
}

#[test]
fn e2e_search_docstring_matches_work() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    let conn = open_db(&tmp);
    let graph = ndxr::graph::builder::build_graph(&conn).unwrap();
    ndxr::graph::centrality::compute_and_store(&conn, &graph).unwrap();

    // Search by docstring content — "connection pool manager".
    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "connection pool manager", 5, None, None)
            .unwrap();

    assert!(
        !results.is_empty(),
        "should find results via docstring match"
    );
    assert!(
        results
            .iter()
            .any(|r| r.name == "DatabasePool" || r.fqn.contains("DatabasePool")),
        "should find DatabasePool via docstring, results: {:?}",
        results.iter().map(|r| &r.name).collect::<Vec<_>>()
    );
}

// ===========================================================================
// E2E: Database integrity constraints
// ===========================================================================

#[test]
fn e2e_cascade_delete_keeps_db_consistent() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    let conn = open_db(&tmp);

    // Count symbols in token.ts.
    let token_syms: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM symbols s JOIN files f ON s.file_id = f.id \
             WHERE f.path = 'src/auth/token.ts'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(token_syms > 0, "token.ts should have symbols");

    // Delete the file row — CASCADE should remove symbols, edges, tf.
    conn.execute("DELETE FROM files WHERE path = 'src/auth/token.ts'", [])
        .unwrap();

    // Verify no orphaned symbols.
    let orphans: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM symbols WHERE file_id NOT IN (SELECT id FROM files)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(orphans, 0, "no orphaned symbols should exist after CASCADE");

    // Verify no orphaned edges.
    let orphan_edges: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM edges WHERE from_id NOT IN (SELECT id FROM symbols) \
             OR to_id NOT IN (SELECT id FROM symbols)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(orphan_edges, 0, "no orphaned edges after CASCADE");

    // Verify FTS5 cleaned up.
    let fts_orphans: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM symbols_fts WHERE symbols_fts MATCH '\"validateToken\"'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        fts_orphans, 0,
        "FTS5 entries should be cleaned up after CASCADE"
    );
}

// ===========================================================================
// E2E: Schema version tracking
// ===========================================================================

#[test]
fn e2e_schema_version_recorded() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    let conn = open_db(&tmp);

    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(version >= 1, "schema version should be at least 1");

    let migrated_at: i64 = conn
        .query_row(
            "SELECT migrated_at FROM schema_version WHERE version = ?1",
            params![version],
            |row| row.get(0),
        )
        .unwrap();
    assert!(migrated_at > 0, "migration timestamp should be positive");
}

// ===========================================================================
// E2E: Activity command
// ===========================================================================

#[test]
fn e2e_activity_shows_observations() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    // Insert a session and observation directly in DB.
    let conn = open_db(&tmp);
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();
    ndxr::memory::store::save_observation(
        &conn,
        &ndxr::memory::store::NewObservation {
            session_id: session_id.clone(),
            kind: "auto".into(),
            content: "run_pipeline called for auth flow".into(),
            headline: Some("run_pipeline: auth flow query".into()),
            detail_level: 1,
            linked_fqns: vec![],
        },
    )
    .unwrap();
    ndxr::memory::store::save_observation(
        &conn,
        &ndxr::memory::store::NewObservation {
            session_id,
            kind: "warning".into(),
            content: "dead-end exploration detected".into(),
            headline: Some("dead-end: repeated search".into()),
            detail_level: 1,
            linked_fqns: vec![],
        },
    )
    .unwrap();
    drop(conn);

    // Run activity command and verify output.
    let output = ndxr(&tmp, &["activity", "--limit", "5"])
        .success()
        .stdout(contains("[  tool  ]"))
        .stdout(contains("run_pipeline: auth flow query"))
        .stdout(contains("[  warn  ]"))
        .stdout(contains("dead-end: repeated search"))
        .get_output()
        .stdout
        .clone();

    // Verify chronological ordering (oldest first): tool observation before warn.
    let stdout = std::str::from_utf8(&output).unwrap();
    let tool_pos = stdout
        .find("run_pipeline: auth flow query")
        .expect("tool observation should be in output");
    let warn_pos = stdout
        .find("dead-end: repeated search")
        .expect("warn observation should be in output");
    assert!(
        tool_pos < warn_pos,
        "tool observation should appear before warn (chronological order)"
    );
}

#[test]
fn e2e_reindex_does_not_add_symbol_changes() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    // First index populates symbol_changes via change detection.
    ndxr(&tmp, &["index"]).success();

    let conn = open_db(&tmp);
    let changes_before: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbol_changes", [], |row| row.get(0))
        .unwrap();
    drop(conn);

    // Reindex should skip change detection entirely — no new rows added.
    ndxr(&tmp, &["reindex"]).success();

    let conn = open_db(&tmp);
    let changes_after: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbol_changes", [], |row| row.get(0))
        .unwrap();

    assert_eq!(
        changes_after, changes_before,
        "reindex should not add new symbol_changes rows (before={changes_before}, after={changes_after})"
    );
}

#[test]
fn e2e_activity_empty_shows_no_activity() {
    let tmp = TempDir::new().unwrap();
    create_typescript_project(&tmp);

    ndxr(&tmp, &["index"]).success();

    ndxr(&tmp, &["activity"])
        .success()
        .stdout(contains("No activity recorded yet."));
}
