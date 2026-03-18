//! Manifest diffing: compare filesystem state against the indexed files table.
//!
//! Enables incremental indexing by identifying which files have been added,
//! changed, deleted, or remain unchanged since the last indexing run.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use rusqlite::Connection;

/// Status of a file compared to the index.
#[derive(Debug, PartialEq, Eq)]
pub enum FileStatus {
    /// File exists on disk but not in the index.
    Added,
    /// File exists in both but its BLAKE3 hash differs.
    Changed {
        /// The previously indexed hash.
        old_hash: String,
    },
    /// File exists in the index but not on disk.
    Deleted,
    /// File exists in both with a matching hash.
    Unchanged,
}

/// Compares current filesystem files against the indexed files table.
///
/// `current_files` is a list of `(relative_path, blake3_hash)` pairs
/// representing the files currently on disk. Returns a categorized list
/// covering all files (both current and previously indexed).
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn diff_files(
    conn: &Connection,
    current_files: &[(PathBuf, String)],
) -> Result<Vec<(PathBuf, FileStatus)>> {
    // Load all indexed files into a map: path_string -> blake3_hash.
    let mut stmt = conn
        .prepare("SELECT path, blake3_hash FROM files")
        .context("prepare files query")?;

    let indexed: HashMap<String, String> = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .context("query indexed files")?
        .filter_map(std::result::Result::ok)
        .collect();

    let mut result = Vec::new();
    let mut seen_paths: HashSet<String> = HashSet::new();

    // Classify current files.
    for (path, hash) in current_files {
        let path_str = crate::util::normalize_path(path);
        seen_paths.insert(path_str.clone());

        match indexed.get(&path_str) {
            None => {
                result.push((path.clone(), FileStatus::Added));
            }
            Some(old_hash) => {
                if old_hash == hash {
                    result.push((path.clone(), FileStatus::Unchanged));
                } else {
                    result.push((
                        path.clone(),
                        FileStatus::Changed {
                            old_hash: old_hash.clone(),
                        },
                    ));
                }
            }
        }
    }

    // Find deleted files (in index but not on disk).
    for path_str in indexed.keys() {
        if !seen_paths.contains(path_str) {
            result.push((PathBuf::from(path_str), FileStatus::Deleted));
        }
    }

    // Sort for deterministic output.
    result.sort_by(|a, b| a.0.cmp(&b.0));

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS files (
                id          INTEGER PRIMARY KEY,
                path        TEXT NOT NULL UNIQUE,
                language    TEXT NOT NULL,
                blake3_hash TEXT NOT NULL,
                line_count  INTEGER NOT NULL DEFAULT 0,
                byte_size   INTEGER NOT NULL DEFAULT 0,
                indexed_at  INTEGER NOT NULL
            );",
        )
        .unwrap();
        conn
    }

    fn insert_file(conn: &Connection, path: &str, hash: &str) {
        conn.execute(
            "INSERT INTO files (path, language, blake3_hash, line_count, byte_size, indexed_at)
             VALUES (?1, 'test', ?2, 10, 100, 0)",
            rusqlite::params![path, hash],
        )
        .unwrap();
    }

    #[test]
    fn detects_added_files() {
        let conn = setup_db();
        let current = vec![(PathBuf::from("src/new.ts"), "hash_new".to_owned())];
        let result = diff_files(&conn, &current).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, FileStatus::Added);
    }

    #[test]
    fn detects_changed_files() {
        let conn = setup_db();
        insert_file(&conn, "src/main.ts", "old_hash");
        let current = vec![(PathBuf::from("src/main.ts"), "new_hash".to_owned())];
        let result = diff_files(&conn, &current).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].1,
            FileStatus::Changed {
                old_hash: "old_hash".to_owned()
            }
        );
    }

    #[test]
    fn detects_deleted_files() {
        let conn = setup_db();
        insert_file(&conn, "src/deleted.ts", "hash_del");
        let current: Vec<(PathBuf, String)> = vec![];
        let result = diff_files(&conn, &current).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, FileStatus::Deleted);
    }

    #[test]
    fn detects_unchanged_files() {
        let conn = setup_db();
        insert_file(&conn, "src/stable.ts", "same_hash");
        let current = vec![(PathBuf::from("src/stable.ts"), "same_hash".to_owned())];
        let result = diff_files(&conn, &current).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, FileStatus::Unchanged);
    }

    #[test]
    fn mixed_statuses() {
        let conn = setup_db();
        insert_file(&conn, "src/kept.ts", "hash_kept");
        insert_file(&conn, "src/changed.ts", "old_hash");
        insert_file(&conn, "src/removed.ts", "hash_removed");

        let current = vec![
            (PathBuf::from("src/kept.ts"), "hash_kept".to_owned()),
            (PathBuf::from("src/changed.ts"), "new_hash".to_owned()),
            (PathBuf::from("src/brand_new.ts"), "hash_brand".to_owned()),
        ];

        let result = diff_files(&conn, &current).unwrap();
        assert_eq!(result.len(), 4);

        let statuses: HashMap<String, &FileStatus> = result
            .iter()
            .map(|(p, s)| (p.display().to_string(), s))
            .collect();

        assert_eq!(statuses["src/brand_new.ts"], &FileStatus::Added);
        assert_eq!(
            statuses["src/changed.ts"],
            &FileStatus::Changed {
                old_hash: "old_hash".to_owned()
            }
        );
        assert_eq!(statuses["src/kept.ts"], &FileStatus::Unchanged);
        assert_eq!(statuses["src/removed.ts"], &FileStatus::Deleted);
    }
}
