//! Integration tests for AST structural diff detection.

mod helpers;

use ndxr::memory::changes::{ChangeKind, SymbolDiff, query_recent_changes, store_symbol_changes};

#[test]
fn store_and_query_round_trip() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join("test.db");
    let conn = ndxr::storage::db::open_or_create(&db_path).unwrap();

    let diffs = vec![SymbolDiff {
        fqn: "test::foo".to_owned(),
        file_path: "src/test.rs".to_owned(),
        kind: ChangeKind::SignatureChanged,
        old_value: Some("fn foo(x: i32)".to_owned()),
        new_value: Some("fn foo(x: i64)".to_owned()),
    }];

    store_symbol_changes(&conn, &diffs, None).unwrap();

    let recent = query_recent_changes(&conn, &["test::foo".to_owned()], 0, 10).unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].change_kind, "signature_changed");
}

#[test]
fn indexer_populates_symbol_changes_on_reindex() {
    let tmp = tempfile::TempDir::new().unwrap();
    helpers::create_search_project(&tmp);
    let (config, _conn, _graph) = helpers::index_and_build(&tmp);

    // Modify a file to trigger changes on re-index.
    std::fs::write(
        tmp.path().join("src/auth.ts"),
        r#"
export function validateToken(token: string): boolean {
    return token.length > 0;
}

export function newFunction(): void {
    console.log('new');
}
"#,
    )
    .unwrap();

    // Re-index — the indexer should detect and store diffs.
    ndxr::indexer::index(&config).unwrap();

    // The symbol_changes table should be queryable after re-index.
    let conn2 = ndxr::storage::db::open_or_create(&config.db_path).unwrap();
    let total: i64 = conn2
        .query_row("SELECT COUNT(*) FROM symbol_changes", [], |row| row.get(0))
        .unwrap();
    assert!(
        total > 0,
        "symbol_changes should have entries after re-index with modifications"
    );
}

#[test]
fn multiple_change_kinds_stored() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join("test.db");
    let conn = ndxr::storage::db::open_or_create(&db_path).unwrap();

    let diffs = vec![
        SymbolDiff {
            fqn: "test::added".to_owned(),
            file_path: "src/test.rs".to_owned(),
            kind: ChangeKind::Added,
            old_value: None,
            new_value: Some("fn added()".to_owned()),
        },
        SymbolDiff {
            fqn: "test::removed".to_owned(),
            file_path: "src/test.rs".to_owned(),
            kind: ChangeKind::Removed,
            old_value: Some("fn removed()".to_owned()),
            new_value: None,
        },
        SymbolDiff {
            fqn: "test::changed".to_owned(),
            file_path: "src/test.rs".to_owned(),
            kind: ChangeKind::BodyChanged,
            old_value: None,
            new_value: None,
        },
    ];

    let stored = store_symbol_changes(&conn, &diffs, None).unwrap();
    assert_eq!(stored, 3);

    let recent = query_recent_changes(
        &conn,
        &[
            "test::added".to_owned(),
            "test::removed".to_owned(),
            "test::changed".to_owned(),
        ],
        0,
        10,
    )
    .unwrap();
    assert_eq!(recent.len(), 3);
}
