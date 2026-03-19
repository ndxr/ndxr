//! Shared test helpers for integration tests.
//!
//! Provides common project scaffolding and indexing utilities used across
//! multiple test files, avoiding byte-for-byte duplication.

// Each integration test binary compiles this module independently, so not every
// binary uses every helper. Suppress the resulting dead_code warnings.
#![allow(dead_code)]

use std::fs;

use tempfile::TempDir;

/// Creates and indexes a minimal TypeScript project with a single `auth.ts` file.
///
/// Returns the temp directory (kept alive by the caller) and the `NdxrConfig`.
pub fn setup_indexed_workspace() -> (TempDir, ndxr::config::NdxrConfig) {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::create_dir_all(tmp.path().join("src")).unwrap();
    fs::write(
        tmp.path().join("src/auth.ts"),
        r"
export function validateToken(token: string): boolean { return true; }
",
    )
    .unwrap();
    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    ndxr::indexer::index(&config).unwrap();
    (tmp, config)
}

/// Creates a multi-file TypeScript project for search tests.
///
/// Writes `.git/`, `src/auth.ts`, `src/database.ts`, and `src/middleware.ts`.
pub fn create_search_project(tmp: &TempDir) {
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::create_dir_all(tmp.path().join("src")).unwrap();

    fs::write(
        tmp.path().join("src/auth.ts"),
        r"
/** Validates authentication tokens */
export function validateToken(token: string): boolean {
    return token.length > 0;
}

/** Handles authentication errors */
export function handleAuthError(error: Error): void {
    console.error(error);
}

export class AuthService {
    validate(token: string): boolean {
        return validateToken(token);
    }
}
",
    )
    .unwrap();

    fs::write(
        tmp.path().join("src/database.ts"),
        r"
/** Database connection manager */
export class DatabaseConnection {
    connect(url: string): void {}
    query(sql: string): any[] { return []; }
    disconnect(): void {}
}

export function createConnection(url: string): DatabaseConnection {
    return new DatabaseConnection();
}
",
    )
    .unwrap();

    fs::write(
        tmp.path().join("src/middleware.ts"),
        r"
import { validateToken } from './auth';
import { DatabaseConnection } from './database';

export function authMiddleware(req: any): boolean {
    return validateToken(req.token);
}
",
    )
    .unwrap();
}

/// Creates a multi-file TypeScript project for capsule tests.
///
/// Writes `.git/`, `src/auth.ts` (with `AuthService`), `src/middleware.ts`,
/// and `src/routes.ts`.
pub fn create_capsule_project(tmp: &TempDir) {
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::create_dir_all(tmp.path().join("src")).unwrap();

    fs::write(
        tmp.path().join("src/auth.ts"),
        r"
/** Validates authentication tokens */
export function validateToken(token: string): boolean {
    return token.length > 0;
}

export class AuthService {
    validate(token: string): boolean {
        return validateToken(token);
    }
}
",
    )
    .unwrap();

    fs::write(
        tmp.path().join("src/middleware.ts"),
        r"
import { validateToken } from './auth';

export function authMiddleware(req: any): boolean {
    return validateToken(req.token);
}
",
    )
    .unwrap();

    fs::write(
        tmp.path().join("src/routes.ts"),
        r"
import { authMiddleware } from './middleware';

export function setupRoutes(app: any): void {
    app.use(authMiddleware);
}
",
    )
    .unwrap();
}

/// Indexes the project in `tmp`, opens the database, builds the symbol graph,
/// and computes centrality scores.
///
/// Returns `(config, connection, graph)`.
pub fn index_and_build(
    tmp: &TempDir,
) -> (
    ndxr::config::NdxrConfig,
    rusqlite::Connection,
    ndxr::graph::builder::SymbolGraph,
) {
    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    ndxr::indexer::index(&config).unwrap();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let graph = ndxr::graph::builder::build_graph(&conn).unwrap();
    ndxr::graph::centrality::compute_and_store(&conn, &graph).unwrap();
    (config, conn, graph)
}
