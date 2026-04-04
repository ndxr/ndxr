//! Full pipeline integration test.
//!
//! Exercises: index -> search -> capsule -> memory -> incremental reindex -> staleness

use std::collections::HashSet;
use std::fs;

use tempfile::TempDir;

/// Creates a realistic multi-file TypeScript project.
#[allow(clippy::too_many_lines)]
fn create_project(tmp: &TempDir) {
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
        const header = request.headers.get('authorization');
        return header?.replace('Bearer ', '') ?? null;
    }

    private getUser(token: string): User {
        return { id: '1', name: 'test' };
    }
}

export interface User {
    id: string;
    name: string;
}

export interface Request {
    headers: Map<string, string>;
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

    /** Creates a new connection pool */
    constructor(private url: string, private maxSize: number) {}

    /** Gets a connection from the pool */
    async getConnection(): Promise<Connection> {
        if (this.connections.length > 0) {
            return this.connections.pop()!;
        }
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

/** Executes a batch insert */
export async function insertBatch(pool: DatabasePool, records: any[]): Promise<number> {
    const conn = await pool.getConnection();
    try {
        return records.length;
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

/** Sets up API routes */
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

#[test]
#[allow(clippy::too_many_lines)]
fn full_pipeline_integration() {
    let tmp = TempDir::new().unwrap();
    create_project(&tmp);

    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());

    // 1. Index the project
    let stats = ndxr::indexer::index(&config).unwrap();
    assert_eq!(stats.files_indexed, 5);
    assert!(
        stats.symbols_extracted > 10,
        "should extract many symbols, got {}",
        stats.symbols_extracted
    );
    assert!(
        stats.edges_extracted > 0,
        "should extract edges, got {}",
        stats.edges_extracted
    );

    // 2. Open DB and verify counts
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let file_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(file_count, 5);

    // 3. Build graph
    let graph = ndxr::graph::builder::build_graph(&conn).unwrap();
    assert!(graph.graph.node_count() > 0);
    ndxr::graph::centrality::compute_and_store(&conn, &graph).unwrap();

    // 4. Search for "authentication" — verify auth symbols found
    let results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "authentication", 10, None, None)
            .unwrap();
    assert!(!results.is_empty(), "search should find results");
    assert!(
        results.iter().any(|r| r.fqn.contains("auth")
            || r.fqn.contains("Auth")
            || r.name.contains("auth")
            || r.name.contains("Auth")),
        "should find auth-related symbols"
    );

    // 5. Build capsule — verify token budget respected
    let estimator = ndxr::config::TokenEstimator::default();
    let (capsule, _memory_budget) =
        ndxr::capsule::builder::build_capsule(&ndxr::capsule::builder::CapsuleRequest {
            conn: &conn,
            graph: &graph,
            search_results: &results,
            query: "authentication",
            intent: &ndxr::graph::intent::Intent::Explore,
            token_budget: 10_000,
            estimator: &estimator,
            workspace_root: &config.workspace_root,
        })
        .unwrap();
    assert!(capsule.stats.tokens_used <= capsule.stats.tokens_budget);
    // No file in both pivots and skeletons
    let pivot_paths: HashSet<_> = capsule.pivots.iter().map(|p| &p.path).collect();
    for skel in &capsule.skeletons {
        assert!(
            !pivot_paths.contains(&skel.path),
            "File {} appears in both pivots and skeletons",
            skel.path
        );
    }

    // 6. Save an observation
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();
    ndxr::memory::store::save_observation(
        &conn,
        &ndxr::memory::store::NewObservation {
            session_id: session_id.clone(),
            kind: "insight".to_string(),
            content: "Auth token validation uses JWT with expiry check".to_string(),
            headline: Some("JWT auth with expiry".to_string()),
            detail_level: 2,
            linked_fqns: vec!["src/auth/token.ts::validateToken".to_string()],
        },
    )
    .unwrap();

    // 7. Modify a file
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

    // 8. Re-index incrementally
    let stats2 = ndxr::indexer::index(&config).unwrap();
    assert_eq!(
        stats2.files_indexed, 1,
        "only changed file should be re-indexed"
    );
    assert_eq!(stats2.skipped, 4, "unchanged files should be skipped");

    // 9. Verify observation retrieval
    let obs = ndxr::memory::store::get_session_observations(&conn, &session_id).unwrap();
    assert!(
        !obs.is_empty(),
        "session should have at least one observation"
    );

    // 10. Search memory — verify observation surfaces
    let mem_results = ndxr::memory::search::search_memories(
        &conn,
        "JWT authentication",
        &[],
        10,
        true,
        7.0,
        None,
    )
    .unwrap();
    assert!(
        !mem_results.is_empty(),
        "memory search should find the observation"
    );

    // 11. Auto-relaxation test: query that uses relaxation path
    let relaxed = ndxr::capsule::relaxation::search_with_relaxation(
        &conn,
        &graph,
        "authentication",
        5,
        None,
        None,
    )
    .unwrap();
    assert!(
        !relaxed.results.is_empty(),
        "relaxation should return at least one result"
    );

    // 12. Search for database — verify DB symbols found
    let db_results =
        ndxr::graph::search::hybrid_search(&conn, &graph, "database connection", 10, None, None)
            .unwrap();
    assert!(!db_results.is_empty(), "should find database symbols");
}

#[test]
fn concurrent_search_during_reindex() {
    let tmp = TempDir::new().unwrap();
    create_project(&tmp);

    let config = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());
    ndxr::indexer::index(&config).unwrap();

    // Search and reindex concurrently should both succeed (WAL mode)
    let config2 = ndxr::config::NdxrConfig::from_workspace(tmp.path().canonicalize().unwrap());

    let search_handle = std::thread::spawn(move || {
        let conn = ndxr::storage::db::open_or_create(&config2.db_path).unwrap();
        let graph = ndxr::graph::builder::build_graph(&conn).unwrap();
        ndxr::graph::search::hybrid_search(&conn, &graph, "auth", 5, None, None).unwrap()
    });

    // Trigger incremental reindex in main thread
    let stats = ndxr::indexer::index(&config).unwrap();
    assert_eq!(stats.skipped, 5); // nothing changed

    let results = search_handle.join().unwrap();
    // Both should complete without errors
    assert!(!results.is_empty());
}
