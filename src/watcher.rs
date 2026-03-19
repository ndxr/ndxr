//! File watcher with debounced incremental re-indexing.
//!
//! Uses the `notify` crate to watch the workspace for changes and triggers
//! incremental re-indexing after a configurable debounce interval.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::Mutex;

use crate::graph::builder::SymbolGraph;
use crate::languages;
use crate::mcp::server::CoreEngine;

/// Active file watcher that monitors the workspace for changes.
///
/// Watches the workspace root recursively and triggers incremental re-indexing
/// when files are created, modified, or deleted. Changes are debounced over a
/// configurable interval to avoid redundant indexing during rapid edits.
pub struct FileWatcher {
    /// The underlying `notify` watcher handle. Kept alive for the lifetime
    /// of the `FileWatcher` to maintain the OS-level file watch.
    _watcher: RecommendedWatcher,
    /// Sends a shutdown signal to the background debounce/re-index task.
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
}

impl FileWatcher {
    /// Starts watching the workspace root for file changes.
    ///
    /// Changes are debounced over `config.debounce_ms` milliseconds. When the
    /// debounce window closes, affected files are incrementally re-indexed via
    /// the standard indexing pipeline.
    ///
    /// The watcher filters events through the same rules as the indexer:
    /// `.gitignore`/`.ndxrignore` are **not** evaluated at the event level
    /// (the OS notifier does not support them), but the indexer itself will
    /// skip ignored files. The watcher does filter out `.ndxr/` paths and
    /// files with unsupported extensions to avoid unnecessary wake-ups.
    ///
    /// # Errors
    ///
    /// Returns an error if the OS file watcher cannot be created or if
    /// watching the workspace root fails.
    #[allow(clippy::needless_pass_by_value)] // ownership moved into async task
    pub fn start(workspace_root: PathBuf, engine: Arc<CoreEngine>) -> Result<Self> {
        let debounce_ms = engine.config.debounce_ms;
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(256);

        // Create notify watcher that sends events to the channel.
        let mut watcher = RecommendedWatcher::new(
            move |result: Result<Event, notify::Error>| {
                if let Ok(event) = result {
                    let _ = event_tx.blocking_send(event);
                }
            },
            notify::Config::default(),
        )?;

        watcher.watch(&workspace_root, RecursiveMode::Recursive)?;

        // Spawn debounce + re-index task.
        let ws_root = workspace_root.clone();
        tokio::spawn(async move {
            let pending: Mutex<HashSet<PathBuf>> = Mutex::new(HashSet::new());
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(debounce_ms));

            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    Some(event) = event_rx.recv() => {
                        let mut pending_lock = pending.lock().await;
                        for path in event.paths {
                            if should_process_path(&ws_root, &path) {
                                pending_lock.insert(path);
                            }
                        }
                    }
                    _ = interval.tick() => {
                        let mut pending_lock = pending.lock().await;
                        if pending_lock.is_empty() {
                            drop(pending_lock);
                        } else {
                            let paths: Vec<PathBuf> = pending_lock.drain().collect();
                            drop(pending_lock);
                            // Targeted re-index of only the changed files via
                            // spawn_blocking since the indexer is synchronous.
                            // The closure returns the rebuilt graph so it can be
                            // stored in the async context via write().await,
                            // ensuring graph updates are never dropped.
                            let engine_clone = engine.clone();
                            let graph_result = tokio::task::spawn_blocking(move || {
                                match crate::indexer::index_paths(&engine_clone.config, &paths) {
                                    Ok(stats) => {
                                        if stats.files_indexed > 0 || stats.files_deleted > 0 {
                                            tracing::info!(
                                                indexed = stats.files_indexed,
                                                deleted = stats.files_deleted,
                                                skipped = stats.skipped,
                                                "watcher re-index complete"
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        tracing::error!("Watcher re-index failed: {e}");
                                    }
                                }
                                // Rebuild the graph + PageRank on a fresh connection.
                                // `index_paths` skips graph computation (to avoid
                                // duplicate work), so this is the single place the
                                // graph is built after a watcher-triggered re-index.
                                rebuild_graph(&engine_clone.config.db_path)
                            }).await;

                            // Store the rebuilt graph in CoreEngine, waiting for any
                            // in-progress reads to finish. This ensures graph updates
                            // are ALWAYS applied (unlike the previous try_lock approach).
                            if let Ok(Some(graph)) = graph_result {
                                let mut graph_lock = engine.graph.write().await;
                                *graph_lock = Some(graph);
                            }
                        }
                    }
                }
            }
        });

        Ok(Self {
            _watcher: watcher,
            shutdown_tx,
        })
    }

    /// Signals the watcher to stop and drops the OS file watch.
    pub fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
    }
}

/// Rebuilds the symbol graph and computes `PageRank` centrality on a fresh
/// database connection. Returns `None` if the connection or graph build fails.
fn rebuild_graph(db_path: &Path) -> Option<SymbolGraph> {
    let conn = crate::storage::db::open_or_create(db_path).ok()?;
    let graph = crate::graph::builder::build_graph(&conn).ok()?;
    let _ = crate::graph::centrality::compute_and_store(&conn, &graph);
    Some(graph)
}

/// Checks if a file event path should trigger re-indexing.
///
/// Returns `false` for paths inside `.ndxr/`, paths that are not regular files,
/// and paths with unsupported file extensions. Returns `true` otherwise.
fn should_process_path(workspace_root: &Path, path: &Path) -> bool {
    // Skip if path contains .ndxr/ component.
    if path.components().any(|c| c.as_os_str() == ".ndxr") {
        return false;
    }

    // Must be relative to the workspace root.
    if !path.starts_with(workspace_root) {
        return false;
    }

    // Skip if not a file (directories, symlinks, etc.).
    if path.is_dir() {
        return false;
    }

    // Check if extension is supported via the language registry.
    let ext = match path.extension().and_then(|e| e.to_str()) {
        Some(e) => format!(".{e}"),
        None => return false,
    };

    languages::get_language_config(&ext).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn skips_ndxr_directory() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let ndxr_file = root.join(".ndxr").join("index.db");
        fs::create_dir_all(ndxr_file.parent().unwrap()).unwrap();
        fs::write(&ndxr_file, "data").unwrap();

        assert!(!should_process_path(root, &ndxr_file));
    }

    #[test]
    fn skips_unsupported_extensions() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let file = root.join("notes.txt");
        fs::write(&file, "hello").unwrap();

        assert!(!should_process_path(root, &file));
    }

    #[test]
    fn accepts_supported_extensions() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let file = root.join("main.ts");
        fs::write(&file, "export function main() {}").unwrap();

        assert!(should_process_path(root, &file));
    }

    #[test]
    fn skips_files_without_extension() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let file = root.join("Makefile");
        fs::write(&file, "all:").unwrap();

        assert!(!should_process_path(root, &file));
    }

    #[test]
    fn skips_directories() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let dir = root.join("src");
        fs::create_dir_all(&dir).unwrap();

        assert!(!should_process_path(root, &dir));
    }

    #[test]
    fn skips_paths_outside_workspace() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let outside = PathBuf::from("/tmp/outside/main.ts");

        assert!(!should_process_path(root, &outside));
    }

    #[test]
    fn accepts_python_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let file = root.join("app.py");
        fs::write(&file, "def main(): pass").unwrap();

        assert!(should_process_path(root, &file));
    }

    #[test]
    fn accepts_rust_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let file = root.join("lib.rs");
        fs::write(&file, "fn main() {}").unwrap();

        assert!(should_process_path(root, &file));
    }
}
