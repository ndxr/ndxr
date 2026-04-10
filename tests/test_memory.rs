//! Integration tests for the session memory system.

mod helpers;

use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Store tests
// ---------------------------------------------------------------------------

#[test]
fn create_session_and_save_observation() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();

    let session_id = ndxr::memory::store::create_session(&conn).unwrap();
    assert!(!session_id.is_empty());

    let obs = ndxr::memory::store::NewObservation {
        session_id: session_id.clone(),
        kind: "manual".to_owned(),
        content: "Auth token validation uses JWT".to_owned(),
        headline: Some("JWT auth".to_owned()),
        detail_level: 2,
        linked_fqns: vec!["src/auth.ts::validateToken".to_owned()],
    };
    let obs_id = ndxr::memory::store::save_observation(&conn, &obs).unwrap();
    assert!(obs_id > 0);

    let observations = ndxr::memory::store::get_session_observations(&conn, &session_id).unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].content, "Auth token validation uses JWT");
}

#[test]
fn get_observation_links_returns_fqns() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();

    let session_id = ndxr::memory::store::create_session(&conn).unwrap();
    let obs_id = ndxr::memory::store::save_observation(
        &conn,
        &ndxr::memory::store::NewObservation {
            session_id,
            kind: "insight".to_owned(),
            content: "test".to_owned(),
            headline: None,
            detail_level: 2,
            linked_fqns: vec![
                "src/auth.ts::validateToken".to_owned(),
                "src/auth.ts::checkExpiry".to_owned(),
            ],
        },
    )
    .unwrap();

    let links = ndxr::memory::store::get_observation_links(&conn, obs_id).unwrap();
    assert_eq!(links.len(), 2);
    assert!(links.contains(&"src/auth.ts::validateToken".to_owned()));
    assert!(links.contains(&"src/auth.ts::checkExpiry".to_owned()));
}

#[test]
fn get_recent_sessions_returns_latest() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();

    ndxr::memory::store::create_session(&conn).unwrap();
    ndxr::memory::store::create_session(&conn).unwrap();

    let sessions = ndxr::memory::store::get_recent_sessions(&conn, 10, true).unwrap();
    assert_eq!(sessions.len(), 2);
}

#[test]
fn update_session_active_modifies_timestamp() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();

    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    // The update should succeed without error.
    ndxr::memory::store::update_session_active(&conn, &session_id).unwrap();

    let sessions = ndxr::memory::store::get_recent_sessions(&conn, 1, true).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, session_id);
}

// ---------------------------------------------------------------------------
// Search tests
// ---------------------------------------------------------------------------

#[test]
fn search_memory_finds_relevant() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    ndxr::memory::store::save_observation(
        &conn,
        &ndxr::memory::store::NewObservation {
            session_id: session_id.clone(),
            kind: "insight".to_owned(),
            content: "Authentication uses JWT tokens for validation".to_owned(),
            headline: Some("JWT auth tokens".to_owned()),
            detail_level: 2,
            linked_fqns: vec!["src/auth.ts::validateToken".to_owned()],
        },
    )
    .unwrap();

    ndxr::memory::store::save_observation(
        &conn,
        &ndxr::memory::store::NewObservation {
            session_id,
            kind: "insight".to_owned(),
            content: "Database uses connection pooling for performance".to_owned(),
            headline: Some("DB connection pooling".to_owned()),
            detail_level: 2,
            linked_fqns: vec![],
        },
    )
    .unwrap();

    let results = ndxr::memory::search::search_memories(
        &conn,
        &ndxr::memory::search::MemorySearchQuery {
            query: "JWT authentication",
            pivot_fqns: &[],
            limit: 10,
            include_stale: true,
            recency_half_life_days: 7.0,
            kind: None,
            exclude_auto: false,
        },
    )
    .unwrap();
    assert!(!results.is_empty());
    // The JWT-related observation should score higher.
    assert!(results[0].observation.content.contains("JWT"));
}

#[test]
fn search_memory_empty_query_returns_empty() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();

    let results = ndxr::memory::search::search_memories(
        &conn,
        &ndxr::memory::search::MemorySearchQuery {
            query: "",
            pivot_fqns: &[],
            limit: 10,
            include_stale: true,
            recency_half_life_days: 7.0,
            kind: None,
            exclude_auto: false,
        },
    )
    .unwrap();
    assert!(results.is_empty());
}

#[test]
fn search_memory_excludes_stale_when_requested() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    ndxr::memory::store::save_observation(
        &conn,
        &ndxr::memory::store::NewObservation {
            session_id,
            kind: "insight".to_owned(),
            content: "Authentication uses JWT tokens for validation".to_owned(),
            headline: Some("JWT auth tokens".to_owned()),
            detail_level: 2,
            linked_fqns: vec!["src/auth.ts::validateToken".to_owned()],
        },
    )
    .unwrap();

    // Mark stale.
    let changed = vec![ndxr::memory::changes::SymbolDiff {
        fqn: "src/auth.ts::validateToken".to_owned(),
        file_path: String::new(),
        kind: ndxr::memory::changes::ChangeKind::BodyChanged,
        old_value: None,
        new_value: None,
    }];
    ndxr::memory::staleness::detect_staleness(&conn, &changed).unwrap();

    // Excluding stale should return nothing.
    let results = ndxr::memory::search::search_memories(
        &conn,
        &ndxr::memory::search::MemorySearchQuery {
            query: "JWT authentication",
            pivot_fqns: &[],
            limit: 10,
            include_stale: false,
            recency_half_life_days: 7.0,
            kind: None,
            exclude_auto: false,
        },
    )
    .unwrap();
    assert!(results.is_empty());

    // Including stale should find it.
    let results = ndxr::memory::search::search_memories(
        &conn,
        &ndxr::memory::search::MemorySearchQuery {
            query: "JWT authentication",
            pivot_fqns: &[],
            limit: 10,
            include_stale: true,
            recency_half_life_days: 7.0,
            kind: None,
            exclude_auto: false,
        },
    )
    .unwrap();
    assert!(!results.is_empty());
}

// ---------------------------------------------------------------------------
// Staleness tests
// ---------------------------------------------------------------------------

#[test]
fn staleness_marks_linked_observations() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    ndxr::memory::store::save_observation(
        &conn,
        &ndxr::memory::store::NewObservation {
            session_id,
            kind: "insight".to_owned(),
            content: "validateToken checks JWT expiry".to_owned(),
            headline: None,
            detail_level: 2,
            linked_fqns: vec!["src/auth.ts::validateToken".to_owned()],
        },
    )
    .unwrap();

    let changed = vec![ndxr::memory::changes::SymbolDiff {
        fqn: "src/auth.ts::validateToken".to_owned(),
        file_path: String::new(),
        kind: ndxr::memory::changes::ChangeKind::BodyChanged,
        old_value: None,
        new_value: None,
    }];
    let marked = ndxr::memory::staleness::detect_staleness(&conn, &changed).unwrap();
    assert_eq!(marked, 1);

    // Verify it is marked stale in the database.
    let is_stale: bool = conn
        .query_row(
            "SELECT is_stale FROM observations WHERE content = 'validateToken checks JWT expiry'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(is_stale);
}

#[test]
fn staleness_does_not_double_mark() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    ndxr::memory::store::save_observation(
        &conn,
        &ndxr::memory::store::NewObservation {
            session_id,
            kind: "insight".to_owned(),
            content: "validateToken checks JWT expiry".to_owned(),
            headline: None,
            detail_level: 2,
            linked_fqns: vec!["src/auth.ts::validateToken".to_owned()],
        },
    )
    .unwrap();

    let changed = vec![ndxr::memory::changes::SymbolDiff {
        fqn: "src/auth.ts::validateToken".to_owned(),
        file_path: String::new(),
        kind: ndxr::memory::changes::ChangeKind::SignatureChanged,
        old_value: None,
        new_value: None,
    }];

    let marked_first = ndxr::memory::staleness::detect_staleness(&conn, &changed).unwrap();
    assert_eq!(marked_first, 1);

    // Running again should not mark any more (already stale).
    let marked_second = ndxr::memory::staleness::detect_staleness(&conn, &changed).unwrap();
    assert_eq!(marked_second, 0);
}

// ---------------------------------------------------------------------------
// Compression tests
// ---------------------------------------------------------------------------

#[test]
fn compression_removes_auto_preserves_insights() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();

    // Create session manually with an old timestamp so it qualifies for compression.
    let session_id = uuid::Uuid::new_v4().to_string();
    let old_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .cast_signed()
        - 10_000;
    conn.execute(
        "INSERT INTO sessions (id, started_at, last_active) VALUES (?1, ?2, ?3)",
        rusqlite::params![session_id, old_ts, old_ts],
    )
    .unwrap();

    // Add auto and insight observations.
    conn.execute(
        "INSERT INTO observations (session_id, kind, content, headline, detail_level, created_at) \
         VALUES (?1, 'auto', 'auto obs', 'auto', 2, ?2)",
        rusqlite::params![session_id, old_ts],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO observations (session_id, kind, content, headline, detail_level, created_at) \
         VALUES (?1, 'insight', 'important insight', 'insight', 2, ?2)",
        rusqlite::params![session_id, old_ts],
    )
    .unwrap();

    let compressed = ndxr::memory::compression::compress_inactive_sessions(&conn, 7200).unwrap();
    assert_eq!(compressed, 1);

    // Auto should be deleted, insight preserved.
    let auto_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM observations WHERE session_id = ?1 AND kind = 'auto'",
            rusqlite::params![session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(auto_count, 0);

    let insight_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM observations WHERE session_id = ?1 AND kind = 'insight'",
            rusqlite::params![session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(insight_count, 1);

    // Session should be marked compressed.
    let is_compressed: bool = conn
        .query_row(
            "SELECT is_compressed FROM sessions WHERE id = ?1",
            rusqlite::params![session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(is_compressed);
}

#[test]
fn compression_skips_already_compressed() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();

    let session_id = uuid::Uuid::new_v4().to_string();
    let old_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .cast_signed()
        - 10_000;
    conn.execute(
        "INSERT INTO sessions (id, started_at, last_active, is_compressed) VALUES (?1, ?2, ?3, 1)",
        rusqlite::params![session_id, old_ts, old_ts],
    )
    .unwrap();

    let compressed = ndxr::memory::compression::compress_inactive_sessions(&conn, 7200).unwrap();
    assert_eq!(compressed, 0);
}

#[test]
fn compression_populates_session_metadata() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();

    let session_id = uuid::Uuid::new_v4().to_string();
    let old_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .cast_signed()
        - 10_000;
    conn.execute(
        "INSERT INTO sessions (id, started_at, last_active) VALUES (?1, ?2, ?3)",
        rusqlite::params![session_id, old_ts, old_ts],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO observations (session_id, kind, content, headline, detail_level, created_at) \
         VALUES (?1, 'insight', 'JWT tokens validated here', 'JWT validation', 2, ?2)",
        rusqlite::params![session_id, old_ts],
    )
    .unwrap();

    ndxr::memory::compression::compress_inactive_sessions(&conn, 7200).unwrap();

    let (summary, key_terms): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT summary, key_terms FROM sessions WHERE id = ?1",
            rusqlite::params![session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();

    assert!(summary.is_some());
    assert!(key_terms.is_some());
    assert!(!summary.unwrap().is_empty());
    assert!(!key_terms.unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Auto-capture tests
// ---------------------------------------------------------------------------

#[test]
fn auto_capture_generates_observation() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    let record = ndxr::memory::capture::ToolCallRecord {
        tool_name: "run_pipeline".to_owned(),
        intent: Some("debug".to_owned()),
        query: Some("fix auth bug".to_owned()),
        pivot_fqns: vec!["src/auth.ts::validateToken".to_owned()],
        result_summary: "2p 3s".to_owned(),
    };

    ndxr::memory::capture::auto_capture(&conn, &session_id, &record).unwrap();

    let observations = ndxr::memory::store::get_session_observations(&conn, &session_id).unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].kind, "auto");
    assert!(observations[0].content.contains("run_pipeline"));
}

#[test]
fn auto_capture_skips_excluded_tools() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    let record = ndxr::memory::capture::ToolCallRecord {
        tool_name: "search_memory".to_owned(),
        intent: None,
        query: Some("test".to_owned()),
        pivot_fqns: vec![],
        result_summary: "3 results".to_owned(),
    };

    ndxr::memory::capture::auto_capture(&conn, &session_id, &record).unwrap();

    let observations = ndxr::memory::store::get_session_observations(&conn, &session_id).unwrap();
    assert_eq!(observations.len(), 0);
}

#[test]
fn auto_capture_records_linked_fqns() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    let record = ndxr::memory::capture::ToolCallRecord {
        tool_name: "get_context_capsule".to_owned(),
        intent: None,
        query: Some("auth flow".to_owned()),
        pivot_fqns: vec![
            "src/auth.ts::validateToken".to_owned(),
            "src/auth.ts::refreshToken".to_owned(),
        ],
        result_summary: "2 pivots".to_owned(),
    };

    ndxr::memory::capture::auto_capture(&conn, &session_id, &record).unwrap();

    let observations = ndxr::memory::store::get_session_observations(&conn, &session_id).unwrap();
    assert_eq!(observations.len(), 1);

    let links = ndxr::memory::store::get_observation_links(&conn, observations[0].id).unwrap();
    assert_eq!(links.len(), 2);
}

// ---------------------------------------------------------------------------
// Edge case tests
// ---------------------------------------------------------------------------

#[test]
fn search_memory_with_empty_database() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".ndxr").join("index.db");
    let conn = ndxr::storage::db::open_or_create(&db_path).unwrap();

    // No sessions, no observations -- search should return empty
    let results = ndxr::memory::search::search_memories(
        &conn,
        &ndxr::memory::search::MemorySearchQuery {
            query: "anything",
            pivot_fqns: &[],
            limit: 10,
            include_stale: true,
            recency_half_life_days: 7.0,
            kind: None,
            exclude_auto: false,
        },
    )
    .unwrap();
    assert!(results.is_empty());
}

#[test]
fn staleness_with_unlinked_observation() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".ndxr").join("index.db");
    let conn = ndxr::storage::db::open_or_create(&db_path).unwrap();

    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    // Create observation with NO linked FQNs
    ndxr::memory::store::save_observation(
        &conn,
        &ndxr::memory::store::NewObservation {
            session_id,
            kind: "manual".into(),
            content: "General note with no symbol links".into(),
            headline: None,
            detail_level: 2,
            linked_fqns: vec![],
        },
    )
    .unwrap();

    // Run staleness detection -- should NOT mark the unlinked observation
    let changed = vec![ndxr::memory::changes::SymbolDiff {
        fqn: "any::symbol".into(),
        file_path: String::new(),
        kind: ndxr::memory::changes::ChangeKind::BodyChanged,
        old_value: None,
        new_value: None,
    }];
    let marked = ndxr::memory::staleness::detect_staleness(&conn, &changed).unwrap();
    assert_eq!(marked, 0, "unlinked observation should not be marked stale");
}

#[test]
fn search_memories_excludes_auto_by_default() {
    use ndxr::memory::store::{NewObservation, create_session, save_observation};

    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join("test.db");
    let conn = ndxr::storage::db::open_or_create(&db_path).unwrap();
    let session_id = create_session(&conn).unwrap();

    // Save one auto observation and one insight, both mentioning "validateToken"
    save_observation(
        &conn,
        &NewObservation {
            session_id: session_id.clone(),
            kind: "auto".to_owned(),
            content: "Tool: run_pipeline about validateToken".to_owned(),
            headline: None,
            detail_level: 3,
            linked_fqns: vec![],
        },
    )
    .unwrap();

    save_observation(
        &conn,
        &NewObservation {
            session_id,
            kind: "insight".to_owned(),
            content: "Important insight about validateToken and JWT".to_owned(),
            headline: None,
            detail_level: 3,
            linked_fqns: vec![],
        },
    )
    .unwrap();

    // Default (exclude_auto=true) should only surface the insight.
    let results = ndxr::memory::search::search_memories(
        &conn,
        &ndxr::memory::search::MemorySearchQuery {
            query: "validateToken",
            pivot_fqns: &[],
            limit: 10,
            include_stale: false,
            recency_half_life_days: 7.0,
            kind: None,
            exclude_auto: true,
        },
    )
    .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].observation.kind, "insight");

    // exclude_auto=false should surface both.
    let results_all = ndxr::memory::search::search_memories(
        &conn,
        &ndxr::memory::search::MemorySearchQuery {
            query: "validateToken",
            pivot_fqns: &[],
            limit: 10,
            include_stale: false,
            recency_half_life_days: 7.0,
            kind: None,
            exclude_auto: false,
        },
    )
    .unwrap();
    assert_eq!(results_all.len(), 2);

    // kind='auto' with exclude_auto=false should surface only auto.
    let results_auto = ndxr::memory::search::search_memories(
        &conn,
        &ndxr::memory::search::MemorySearchQuery {
            query: "validateToken",
            pivot_fqns: &[],
            limit: 10,
            include_stale: false,
            recency_half_life_days: 7.0,
            kind: Some("auto"),
            exclude_auto: false,
        },
    )
    .unwrap();
    assert_eq!(results_auto.len(), 1);
    assert_eq!(results_auto[0].observation.kind, "auto");
}
