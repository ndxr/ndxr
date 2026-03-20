//! Smoke tests: memory module edge cases (store, search, staleness, compression, capture).

mod helpers;

/// Creates an old session with the given timestamp offset (seconds before now).
fn create_old_session(conn: &rusqlite::Connection, age_secs: i64) -> String {
    let session_id = uuid::Uuid::new_v4().to_string();
    let old_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .cast_signed()
        - age_secs;
    conn.execute(
        "INSERT INTO sessions (id, started_at, last_active) VALUES (?1, ?2, ?3)",
        rusqlite::params![session_id, old_ts, old_ts],
    )
    .unwrap();
    session_id
}

/// Inserts an observation directly with an old timestamp.
fn insert_observation_raw(
    conn: &rusqlite::Connection,
    session_id: &str,
    kind: &str,
    content: &str,
    headline: Option<&str>,
    ts: i64,
) {
    conn.execute(
        "INSERT INTO observations (session_id, kind, content, headline, detail_level, created_at) \
         VALUES (?1, ?2, ?3, ?4, 2, ?5)",
        rusqlite::params![session_id, kind, content, headline, ts],
    )
    .unwrap();
}

// ===========================================================================
// Store edge cases
// ===========================================================================

#[test]
fn save_observation_unicode_content() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    let unicode_content = "Caf\u{00e9} authentication \u{8ba4}\u{8bc1} \u{1f512}";
    let obs_id = ndxr::memory::store::save_observation(
        &conn,
        &ndxr::memory::store::NewObservation {
            session_id: session_id.clone(),
            kind: "manual".to_owned(),
            content: unicode_content.to_owned(),
            headline: Some("Unicode test".to_owned()),
            detail_level: 2,
            linked_fqns: vec![],
        },
    )
    .unwrap();
    assert!(obs_id > 0);

    let observations = ndxr::memory::store::get_session_observations(&conn, &session_id).unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].content, unicode_content);
}

#[test]
fn save_observation_very_long_content() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    let long_content = "x".repeat(50_000);
    let obs_id = ndxr::memory::store::save_observation(
        &conn,
        &ndxr::memory::store::NewObservation {
            session_id: session_id.clone(),
            kind: "manual".to_owned(),
            content: long_content.clone(),
            headline: Some("Long content test".to_owned()),
            detail_level: 2,
            linked_fqns: vec![],
        },
    )
    .unwrap();
    assert!(obs_id > 0);

    let observations = ndxr::memory::store::get_session_observations(&conn, &session_id).unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].content.len(), 50_000);
    assert_eq!(observations[0].content, long_content);
}

#[test]
fn save_observation_empty_headline() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    let obs_id = ndxr::memory::store::save_observation(
        &conn,
        &ndxr::memory::store::NewObservation {
            session_id: session_id.clone(),
            kind: "manual".to_owned(),
            content: "No headline observation".to_owned(),
            headline: None,
            detail_level: 2,
            linked_fqns: vec![],
        },
    )
    .unwrap();
    assert!(obs_id > 0);

    let observations = ndxr::memory::store::get_session_observations(&conn, &session_id).unwrap();
    assert_eq!(observations.len(), 1);
    assert!(observations[0].headline.is_none());
    assert_eq!(observations[0].content, "No headline observation");
}

#[test]
fn save_observation_all_valid_kinds() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    let kinds = ["auto", "insight", "decision", "error", "manual"];
    for kind in &kinds {
        ndxr::memory::store::save_observation(
            &conn,
            &ndxr::memory::store::NewObservation {
                session_id: session_id.clone(),
                kind: (*kind).to_owned(),
                content: format!("Observation of kind {kind}"),
                headline: Some(format!("{kind} headline")),
                detail_level: 2,
                linked_fqns: vec![],
            },
        )
        .unwrap();
    }

    let observations = ndxr::memory::store::get_session_observations(&conn, &session_id).unwrap();
    assert_eq!(observations.len(), 5);

    let stored_kinds: Vec<String> = observations.iter().map(|o| o.kind.clone()).collect();
    for kind in &kinds {
        assert!(
            stored_kinds.contains(&(*kind).to_owned()),
            "missing kind: {kind}"
        );
    }
}

#[test]
fn save_observation_duplicate_linked_fqns() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    let fqn = "src/auth.ts::validateToken".to_owned();
    let obs_id = ndxr::memory::store::save_observation(
        &conn,
        &ndxr::memory::store::NewObservation {
            session_id,
            kind: "insight".to_owned(),
            content: "Duplicate FQN test".to_owned(),
            headline: None,
            detail_level: 2,
            linked_fqns: vec![fqn.clone(), fqn.clone()],
        },
    )
    .unwrap();

    let links = ndxr::memory::store::get_observation_links(&conn, obs_id).unwrap();
    assert_eq!(links.len(), 1, "INSERT OR IGNORE should deduplicate FQNs");
    assert_eq!(links[0], fqn);
}

#[test]
fn get_recent_sessions_multiple_sessions() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();

    for _ in 0..5 {
        ndxr::memory::store::create_session(&conn).unwrap();
    }

    let sessions = ndxr::memory::store::get_recent_sessions(&conn, 3, true).unwrap();
    assert_eq!(sessions.len(), 3, "should return exactly 3 of 5 sessions");

    let all_sessions = ndxr::memory::store::get_recent_sessions(&conn, 10, true).unwrap();
    assert_eq!(all_sessions.len(), 5, "should return all 5 sessions");
}

// ===========================================================================
// Search edge cases
// ===========================================================================

#[test]
fn search_memory_special_chars_only() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    ndxr::memory::store::save_observation(
        &conn,
        &ndxr::memory::store::NewObservation {
            session_id,
            kind: "insight".to_owned(),
            content: "Some normal content here".to_owned(),
            headline: Some("Normal headline".to_owned()),
            detail_level: 2,
            linked_fqns: vec![],
        },
    )
    .unwrap();

    let results =
        ndxr::memory::search::search_memories(&conn, "(){}[]***", &[], 10, true, 7.0, None)
            .unwrap();
    assert!(
        results.is_empty(),
        "query with only FTS5 special chars should return empty"
    );
}

#[test]
fn search_memory_very_long_query() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    ndxr::memory::store::save_observation(
        &conn,
        &ndxr::memory::store::NewObservation {
            session_id,
            kind: "insight".to_owned(),
            content: "Token validation logic".to_owned(),
            headline: Some("Token validation".to_owned()),
            detail_level: 2,
            linked_fqns: vec![],
        },
    )
    .unwrap();

    let long_query = "token ".repeat(1_700); // ~10k chars
    let results =
        ndxr::memory::search::search_memories(&conn, &long_query, &[], 10, true, 7.0, None)
            .unwrap();
    // Should not crash. May or may not return results depending on FTS5 behaviour.
    assert!(results.len() <= 10);
}

#[test]
fn search_memory_empty_pivot_fqns() {
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

    let results = ndxr::memory::search::search_memories(
        &conn,
        "JWT authentication",
        &[],
        10,
        true,
        7.0,
        None,
    )
    .unwrap();
    assert!(!results.is_empty());
    // With empty pivot_fqns, proximity component should be 0.0. The observation
    // has linked FQNs but none overlap with the empty pivot set, so proximity = 0.
    // We cannot inspect the internal component directly, but we verify the result
    // is still found (BM25 + TF-IDF + recency alone are enough).
    assert!(results[0].observation.content.contains("JWT"));
}

#[test]
fn search_memory_include_stale_true_vs_false() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    // Save two observations: one that will become stale, one that stays fresh.
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
            content: "JWT expiry check runs daily".to_owned(),
            headline: Some("JWT expiry".to_owned()),
            detail_level: 2,
            linked_fqns: vec![],
        },
    )
    .unwrap();

    // Mark the first one stale.
    let changed = vec![ndxr::memory::staleness::ChangedSymbol {
        fqn: "src/auth.ts::validateToken".to_owned(),
        change_type: ndxr::memory::staleness::SymbolChange::BodyChanged,
    }];
    ndxr::memory::staleness::detect_staleness(&conn, &changed).unwrap();

    // Including stale: should return both.
    let with_stale =
        ndxr::memory::search::search_memories(&conn, "JWT", &[], 10, true, 7.0, None).unwrap();
    // Excluding stale: should return fewer.
    let without_stale =
        ndxr::memory::search::search_memories(&conn, "JWT", &[], 10, false, 7.0, None).unwrap();

    assert!(
        without_stale.len() <= with_stale.len(),
        "stale-excluding set ({}) must be a subset of stale-including set ({})",
        without_stale.len(),
        with_stale.len()
    );
    // The fresh observation should appear in both.
    assert!(!without_stale.is_empty(), "fresh observation should appear");
    assert!(
        with_stale.len() >= 2,
        "both observations should appear when including stale"
    );
}

#[test]
fn search_memory_limit_zero() {
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
            linked_fqns: vec![],
        },
    )
    .unwrap();

    let results =
        ndxr::memory::search::search_memories(&conn, "JWT authentication", &[], 0, true, 7.0, None)
            .unwrap();
    assert!(results.is_empty(), "limit=0 should return empty results");
}

#[test]
fn search_memory_returns_linked_fqns() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    let fqns = vec![
        "src/auth.ts::validateToken".to_owned(),
        "src/auth.ts::refreshToken".to_owned(),
    ];
    ndxr::memory::store::save_observation(
        &conn,
        &ndxr::memory::store::NewObservation {
            session_id,
            kind: "insight".to_owned(),
            content: "Authentication uses JWT tokens for validation".to_owned(),
            headline: Some("JWT auth tokens".to_owned()),
            detail_level: 2,
            linked_fqns: fqns.clone(),
        },
    )
    .unwrap();

    let results = ndxr::memory::search::search_memories(
        &conn,
        "JWT authentication",
        &[],
        10,
        true,
        7.0,
        None,
    )
    .unwrap();
    assert!(!results.is_empty());

    let result_fqns = &results[0].linked_fqns;
    assert_eq!(result_fqns.len(), 2);
    for fqn in &fqns {
        assert!(
            result_fqns.contains(fqn),
            "search result should contain linked FQN: {fqn}"
        );
    }
}

// ===========================================================================
// Staleness edge cases
// ===========================================================================

#[test]
fn staleness_empty_changes_marks_nothing() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    ndxr::memory::store::save_observation(
        &conn,
        &ndxr::memory::store::NewObservation {
            session_id,
            kind: "insight".to_owned(),
            content: "Some observation".to_owned(),
            headline: None,
            detail_level: 2,
            linked_fqns: vec!["src/auth.ts::validateToken".to_owned()],
        },
    )
    .unwrap();

    let marked = ndxr::memory::staleness::detect_staleness(&conn, &[]).unwrap();
    assert_eq!(marked, 0, "empty changes list should mark 0 observations");
}

#[test]
fn staleness_multiple_observations_same_symbol() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    let fqn = "src/auth.ts::validateToken".to_owned();
    for i in 0..3 {
        ndxr::memory::store::save_observation(
            &conn,
            &ndxr::memory::store::NewObservation {
                session_id: session_id.clone(),
                kind: "insight".to_owned(),
                content: format!("Observation {i} about validateToken"),
                headline: None,
                detail_level: 2,
                linked_fqns: vec![fqn.clone()],
            },
        )
        .unwrap();
    }

    let changed = vec![ndxr::memory::staleness::ChangedSymbol {
        fqn,
        change_type: ndxr::memory::staleness::SymbolChange::BodyChanged,
    }];
    let marked = ndxr::memory::staleness::detect_staleness(&conn, &changed).unwrap();
    assert_eq!(
        marked, 3,
        "all 3 observations linked to the symbol should be marked stale"
    );

    // Verify all are actually stale in the DB.
    let stale_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM observations WHERE session_id = ?1 AND is_stale = 1",
            rusqlite::params![session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stale_count, 3);
}

#[test]
fn staleness_all_change_types_mark_stale() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    // Three observations linked to three different FQNs.
    let fqns = [
        "src/auth.ts::fnDeleted",
        "src/auth.ts::fnSigChanged",
        "src/auth.ts::fnBodyChanged",
    ];
    for fqn in &fqns {
        ndxr::memory::store::save_observation(
            &conn,
            &ndxr::memory::store::NewObservation {
                session_id: session_id.clone(),
                kind: "insight".to_owned(),
                content: format!("Observation about {fqn}"),
                headline: None,
                detail_level: 2,
                linked_fqns: vec![(*fqn).to_owned()],
            },
        )
        .unwrap();
    }

    let changed = vec![
        ndxr::memory::staleness::ChangedSymbol {
            fqn: fqns[0].to_owned(),
            change_type: ndxr::memory::staleness::SymbolChange::Deleted,
        },
        ndxr::memory::staleness::ChangedSymbol {
            fqn: fqns[1].to_owned(),
            change_type: ndxr::memory::staleness::SymbolChange::SignatureChanged,
        },
        ndxr::memory::staleness::ChangedSymbol {
            fqn: fqns[2].to_owned(),
            change_type: ndxr::memory::staleness::SymbolChange::BodyChanged,
        },
    ];
    let marked = ndxr::memory::staleness::detect_staleness(&conn, &changed).unwrap();
    assert_eq!(
        marked, 3,
        "Deleted, SignatureChanged, and BodyChanged should all mark stale"
    );
}

#[test]
fn staleness_observation_linked_to_multiple_symbols_partial_change() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    // One observation linked to three FQNs.
    ndxr::memory::store::save_observation(
        &conn,
        &ndxr::memory::store::NewObservation {
            session_id: session_id.clone(),
            kind: "insight".to_owned(),
            content: "Observation about a, b, and c".to_owned(),
            headline: None,
            detail_level: 2,
            linked_fqns: vec![
                "src/auth.ts::fnA".to_owned(),
                "src/auth.ts::fnB".to_owned(),
                "src/auth.ts::fnC".to_owned(),
            ],
        },
    )
    .unwrap();

    // Only fnB changes.
    let changed = vec![ndxr::memory::staleness::ChangedSymbol {
        fqn: "src/auth.ts::fnB".to_owned(),
        change_type: ndxr::memory::staleness::SymbolChange::SignatureChanged,
    }];
    let marked = ndxr::memory::staleness::detect_staleness(&conn, &changed).unwrap();
    assert_eq!(
        marked, 1,
        "observation linked to [a, b, c] should be marked stale when only b changes"
    );

    let is_stale: bool = conn
        .query_row(
            "SELECT is_stale FROM observations WHERE session_id = ?1",
            rusqlite::params![session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(is_stale);
}

// ===========================================================================
// Compression edge cases
// ===========================================================================

#[test]
fn compression_all_auto_observations_deleted() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();

    let session_id = create_old_session(&conn, 10_000);
    let old_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .cast_signed()
        - 10_000;

    // Insert 5 auto observations.
    for i in 0..5 {
        insert_observation_raw(
            &conn,
            &session_id,
            "auto",
            &format!("auto obs {i}"),
            Some(&format!("auto {i}")),
            old_ts,
        );
    }

    let compressed = ndxr::memory::compression::compress_inactive_sessions(&conn, 7200).unwrap();
    assert_eq!(compressed, 1);

    let remaining: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM observations WHERE session_id = ?1",
            rusqlite::params![session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        remaining, 0,
        "all auto observations should be deleted after compression"
    );
}

#[test]
fn compression_preserves_all_non_auto_kinds() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();

    let session_id = create_old_session(&conn, 10_000);
    let old_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .cast_signed()
        - 10_000;

    let preserved_kinds = ["insight", "decision", "error", "manual"];
    for kind in &preserved_kinds {
        insert_observation_raw(
            &conn,
            &session_id,
            kind,
            &format!("{kind} observation content"),
            Some(&format!("{kind} headline")),
            old_ts,
        );
    }

    let compressed = ndxr::memory::compression::compress_inactive_sessions(&conn, 7200).unwrap();
    assert_eq!(compressed, 1);

    let remaining: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM observations WHERE session_id = ?1",
            rusqlite::params![session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        remaining, 4,
        "insight, decision, error, and manual observations should all be preserved"
    );

    // Verify each kind is still present.
    for kind in &preserved_kinds {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM observations WHERE session_id = ?1 AND kind = ?2",
                rusqlite::params![session_id, kind],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "kind '{kind}' should be preserved after compression"
        );
    }
}

#[test]
fn compression_idempotent() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();

    let session_id = create_old_session(&conn, 10_000);
    let old_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .cast_signed()
        - 10_000;

    insert_observation_raw(&conn, &session_id, "auto", "auto obs", Some("auto"), old_ts);
    insert_observation_raw(
        &conn,
        &session_id,
        "insight",
        "important",
        Some("insight"),
        old_ts,
    );

    let first = ndxr::memory::compression::compress_inactive_sessions(&conn, 7200).unwrap();
    assert_eq!(first, 1, "first compression should process 1 session");

    let second = ndxr::memory::compression::compress_inactive_sessions(&conn, 7200).unwrap();
    assert_eq!(
        second, 0,
        "second compression should process 0 sessions (already compressed)"
    );

    // Insight should still be there.
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM observations WHERE session_id = ?1 AND kind = 'insight'",
            rusqlite::params![session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn compression_multiple_sessions() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();

    let old_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .cast_signed()
        - 10_000;

    // Create 3 old sessions (qualify for compression).
    for i in 0..3 {
        let sid = create_old_session(&conn, 10_000);
        insert_observation_raw(
            &conn,
            &sid,
            "auto",
            &format!("old auto {i}"),
            Some("auto"),
            old_ts,
        );
    }

    // Create 2 new sessions (should NOT be compressed).
    for _ in 0..2 {
        let sid = ndxr::memory::store::create_session(&conn).unwrap();
        ndxr::memory::store::save_observation(
            &conn,
            &ndxr::memory::store::NewObservation {
                session_id: sid,
                kind: "auto".to_owned(),
                content: "recent auto".to_owned(),
                headline: Some("recent".to_owned()),
                detail_level: 2,
                linked_fqns: vec![],
            },
        )
        .unwrap();
    }

    let compressed = ndxr::memory::compression::compress_inactive_sessions(&conn, 7200).unwrap();
    assert_eq!(compressed, 3, "exactly 3 old sessions should be compressed");

    let compressed_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE is_compressed = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(compressed_count, 3);

    let uncompressed_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE is_compressed = 0",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(uncompressed_count, 2);
}

// ===========================================================================
// Capture edge cases
// ===========================================================================

#[test]
fn auto_capture_all_capturable_tools() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    let capturable_tools = [
        "run_pipeline",
        "get_context_capsule",
        "get_skeleton",
        "get_impact_graph",
        "get_session_context",
    ];

    for tool_name in &capturable_tools {
        let record = ndxr::memory::capture::ToolCallRecord {
            tool_name: (*tool_name).to_owned(),
            intent: Some("explore".to_owned()),
            query: Some(format!("query for {tool_name}")),
            pivot_fqns: vec!["src/auth.ts::validateToken".to_owned()],
            result_summary: "1p 2s".to_owned(),
        };
        assert!(
            record.should_capture(),
            "tool '{tool_name}' should be capturable"
        );
        ndxr::memory::capture::auto_capture(&conn, &session_id, &record).unwrap();
    }

    let observations = ndxr::memory::store::get_session_observations(&conn, &session_id).unwrap();
    assert_eq!(
        observations.len(),
        capturable_tools.len(),
        "each capturable tool should generate exactly one observation"
    );

    for obs in &observations {
        assert_eq!(obs.kind, "auto");
        assert!(obs.content.starts_with("Tool: "));
    }
}

#[test]
fn auto_capture_empty_query() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    let record = ndxr::memory::capture::ToolCallRecord {
        tool_name: "run_pipeline".to_owned(),
        intent: Some("explore".to_owned()),
        query: Some(String::new()),
        pivot_fqns: vec![],
        result_summary: "0 results".to_owned(),
    };

    ndxr::memory::capture::auto_capture(&conn, &session_id, &record).unwrap();

    let observations = ndxr::memory::store::get_session_observations(&conn, &session_id).unwrap();
    assert_eq!(observations.len(), 1);
    assert!(observations[0].content.contains("run_pipeline"));
}

#[test]
fn auto_capture_truncation_multibyte() {
    let (_tmp, config) = helpers::setup_indexed_workspace();
    let conn = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let session_id = ndxr::memory::store::create_session(&conn).unwrap();

    // Build a query that has emoji near the 60-char truncation boundary used in to_content.
    // Each emoji is 4 bytes. Place them near char 60 to stress the truncation logic.
    let mut query = "a".repeat(55);
    query.push_str("\u{1f600}\u{1f601}\u{1f602}\u{1f603}\u{1f604}"); // 5 emoji at boundary

    let record = ndxr::memory::capture::ToolCallRecord {
        tool_name: "get_context_capsule".to_owned(),
        intent: None,
        query: Some(query),
        pivot_fqns: vec![],
        result_summary: "1 pivot".to_owned(),
    };

    // Should not panic even with multibyte chars at the truncation boundary.
    ndxr::memory::capture::auto_capture(&conn, &session_id, &record).unwrap();

    let observations = ndxr::memory::store::get_session_observations(&conn, &session_id).unwrap();
    assert_eq!(observations.len(), 1);
    // Verify the content is valid UTF-8 (would have panicked above if not).
    assert!(
        observations[0]
            .content
            .is_char_boundary(observations[0].content.len())
    );
}
