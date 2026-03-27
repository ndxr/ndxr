//! File watcher with debounced incremental re-indexing.
//!
//! Uses the `notify` crate to watch the workspace for changes and triggers
//! incremental re-indexing after a configurable debounce interval.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::languages;
use crate::mcp::server::CoreEngine;
use anyhow::Result;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

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
    /// The watcher filters events through `.ndxrignore` and `.gitignore`
    /// patterns (loaded once at startup), as well as `.ndxr/` paths and files
    /// with unsupported extensions.
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
        let mut ignore_matcher = build_ignore_matcher(&ws_root);
        tokio::spawn(async move {
            let mut pending: HashSet<PathBuf> = HashSet::new();
            let mut debounce_deadline: Option<tokio::time::Instant> = None;

            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    Some(event) = event_rx.recv() => {
                        // Hot-reload ignore matcher when .ndxrignore or .gitignore change.
                        let ignore_changed = event.paths.iter().any(|p| {
                            p.file_name()
                                .is_some_and(|n| n == ".ndxrignore" || n == ".gitignore")
                        });
                        if ignore_changed {
                            ignore_matcher = build_ignore_matcher(&ws_root);
                            tracing::info!("watcher: reloaded ignore patterns");
                        }

                        for path in event.paths {
                            if should_process_path(&ws_root, &path, &ignore_matcher) {
                                pending.insert(path);
                            }
                        }
                        // Reset the debounce deadline on every new event.
                        debounce_deadline = Some(
                            tokio::time::Instant::now()
                                + std::time::Duration::from_millis(debounce_ms)
                        );
                    }
                    () = async {
                        match debounce_deadline {
                            Some(deadline) => tokio::time::sleep_until(deadline).await,
                            None => std::future::pending::<()>().await,
                        }
                    } => {
                        debounce_deadline = None;

                        if pending.is_empty() {
                            continue;
                        }

                        let paths: Vec<PathBuf> = pending.drain().collect();
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
                                        // Run change-based anti-pattern detectors.
                                        run_antipattern_detectors(&engine_clone.config.db_path);
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("watcher re-index failed: {e}");
                                    // Skip graph rebuild — DB may be in inconsistent state.
                                    return None;
                                }
                            }
                            crate::graph::builder::rebuild_graph_from_db(&engine_clone.config.db_path)
                        }).await;

                        match graph_result {
                            Ok(Some(graph)) => {
                                let mut graph_lock = engine.graph.write().await;
                                *graph_lock = Some(graph);
                            }
                            Ok(None) => {
                                tracing::warn!("watcher: graph rebuild skipped or failed");
                            }
                            Err(e) => {
                                tracing::error!("watcher: spawn_blocking panicked: {e}");
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

/// Runs anti-pattern detectors against the most recent session.
///
/// Opens a separate database connection (the engine's `Mutex<Connection>`
/// cannot be used inside `spawn_blocking`). All failures are best-effort:
/// detection errors are logged but never fail the re-index pipeline.
fn run_antipattern_detectors(db_path: &Path) {
    let conn = match crate::storage::db::open_or_create(db_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("anti-pattern detection: failed to open db: {e}");
            return;
        }
    };
    let session_id: String = match conn.query_row(
        "SELECT id FROM sessions ORDER BY last_active DESC LIMIT 1",
        [],
        |row| row.get(0),
    ) {
        Ok(id) => id,
        Err(rusqlite::Error::QueryReturnedNoRows) => return,
        Err(e) => {
            tracing::warn!("anti-pattern detection: session query failed: {e}");
            return;
        }
    };
    let detectors = crate::memory::antipatterns::default_detectors();
    let ctx = crate::memory::antipatterns::DetectionContext {
        conn: &conn,
        session_id: &session_id,
        window_secs: crate::memory::antipatterns::DEFAULT_WINDOW_SECS,
    };
    let patterns =
        crate::memory::antipatterns::run_all_detectors(&ctx, &detectors).unwrap_or_default();
    for pattern in &patterns {
        // Deduplicate: skip if this warning was already stored in this session.
        let already_warned = match conn.query_row(
            "SELECT COUNT(*) FROM observations \
             WHERE session_id = ?1 AND kind = 'warning' AND content LIKE ?2",
            rusqlite::params![session_id, format!("[{}]%", pattern.rule_name)],
            |row| row.get::<_, i64>(0),
        ) {
            Ok(n) => n > 0,
            Err(e) => {
                tracing::warn!("anti-pattern dedup check failed: {e}");
                false
            }
        };
        if already_warned {
            continue;
        }

        tracing::warn!(
            rule = pattern.rule_name,
            "anti-pattern detected: {}",
            pattern.summary
        );
        let obs = crate::memory::store::NewObservation {
            session_id: session_id.clone(),
            kind: "warning".to_owned(),
            content: format!("[{}] {}", pattern.rule_name, pattern.summary),
            headline: Some(pattern.summary.clone()),
            detail_level: 2,
            linked_fqns: pattern.involved_fqns.clone(),
        };
        let _ = crate::memory::store::save_observation(&conn, &obs);
    }
}

/// Default directories always excluded from the file watcher.
const DEFAULT_IGNORED_DIRS: &[&str] = &[
    "target/",
    "build/",
    "bin/",
    "node_modules/",
    ".git/",
    "dist/",
];

/// Builds an ignore matcher from `.ndxrignore` and `.gitignore` files.
///
/// Always includes a baseline set of commonly ignored directories
/// (`target/`, `build/`, `bin/`, `node_modules/`, `.git/`, `dist/`).
/// Then loads `.ndxrignore` (project-specific overrides), followed by
/// `.gitignore` as a fallback.  If neither file exists, only the default
/// patterns apply.
fn build_ignore_matcher(workspace_root: &Path) -> Gitignore {
    let mut builder = GitignoreBuilder::new(workspace_root);

    for pattern in DEFAULT_IGNORED_DIRS {
        let _ = builder.add_line(None, pattern);
    }

    let ndxrignore = workspace_root.join(".ndxrignore");
    if ndxrignore.is_file() {
        let _ = builder.add(ndxrignore);
    }

    let gitignore = workspace_root.join(".gitignore");
    if gitignore.is_file() {
        let _ = builder.add(gitignore);
    }

    builder.build().unwrap_or_else(|e| {
        tracing::warn!("failed to build ignore matcher: {e}");
        Gitignore::empty()
    })
}

/// Checks if a file event path should trigger re-indexing.
///
/// Returns `false` for paths inside `.ndxr/`, paths matched by
/// `.ndxrignore`/`.gitignore`, paths that are not regular files, and paths
/// with unsupported file extensions.  Returns `true` otherwise.
fn should_process_path(workspace_root: &Path, path: &Path, ignore: &Gitignore) -> bool {
    // Skip if path contains .ndxr/ component.
    if path.components().any(|c| c.as_os_str() == ".ndxr") {
        return false;
    }

    // Must be relative to the workspace root.
    if !path.starts_with(workspace_root) {
        return false;
    }

    // Skip if matched by .ndxrignore / .gitignore (checks parent dirs too,
    // so a pattern like `target/` also excludes `target/debug/build.rs`).
    let rel = path.strip_prefix(workspace_root).unwrap_or(path);
    if ignore
        .matched_path_or_any_parents(rel, path.is_dir())
        .is_ignore()
    {
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

    /// Helper: empty matcher (no ignore files).
    fn empty_ignore(root: &Path) -> Gitignore {
        GitignoreBuilder::new(root).build().unwrap()
    }

    #[test]
    fn skips_ndxr_directory() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let ndxr_file = root.join(".ndxr").join("index.db");
        fs::create_dir_all(ndxr_file.parent().unwrap()).unwrap();
        fs::write(&ndxr_file, "data").unwrap();

        assert!(!should_process_path(root, &ndxr_file, &empty_ignore(root)));
    }

    #[test]
    fn skips_unsupported_extensions() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let file = root.join("notes.txt");
        fs::write(&file, "hello").unwrap();

        assert!(!should_process_path(root, &file, &empty_ignore(root)));
    }

    #[test]
    fn accepts_supported_extensions() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let file = root.join("main.ts");
        fs::write(&file, "export function main() {}").unwrap();

        assert!(should_process_path(root, &file, &empty_ignore(root)));
    }

    #[test]
    fn skips_files_without_extension() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let file = root.join("Makefile");
        fs::write(&file, "all:").unwrap();

        assert!(!should_process_path(root, &file, &empty_ignore(root)));
    }

    #[test]
    fn skips_directories() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let dir = root.join("src");
        fs::create_dir_all(&dir).unwrap();

        assert!(!should_process_path(root, &dir, &empty_ignore(root)));
    }

    #[test]
    fn skips_paths_outside_workspace() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let outside = PathBuf::from("/tmp/outside/main.ts");

        assert!(!should_process_path(root, &outside, &empty_ignore(root)));
    }

    #[test]
    fn accepts_python_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let file = root.join("app.py");
        fs::write(&file, "def main(): pass").unwrap();

        assert!(should_process_path(root, &file, &empty_ignore(root)));
    }

    #[test]
    fn accepts_rust_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let file = root.join("lib.rs");
        fs::write(&file, "fn main() {}").unwrap();

        assert!(should_process_path(root, &file, &empty_ignore(root)));
    }

    #[test]
    fn skips_gitignored_paths() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::write(root.join(".gitignore"), "target/\n").unwrap();
        let target_file = root.join("target").join("debug").join("build.rs");
        fs::create_dir_all(target_file.parent().unwrap()).unwrap();
        fs::write(&target_file, "fn main() {}").unwrap();

        let matcher = build_ignore_matcher(root);
        assert!(!should_process_path(root, &target_file, &matcher));
    }

    #[test]
    fn skips_ndxrignored_paths() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::write(root.join(".ndxrignore"), "generated/\n").unwrap();
        let gen_file = root.join("generated").join("schema.rs");
        fs::create_dir_all(gen_file.parent().unwrap()).unwrap();
        fs::write(&gen_file, "pub struct S {}").unwrap();

        let matcher = build_ignore_matcher(root);
        assert!(!should_process_path(root, &gen_file, &matcher));
    }

    #[test]
    fn ndxrignore_and_gitignore_both_apply() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::write(root.join(".gitignore"), "target/\n").unwrap();
        fs::write(root.join(".ndxrignore"), "vendor/\n").unwrap();

        let matcher = build_ignore_matcher(root);

        // gitignore pattern
        let target_file = root.join("target").join("lib.rs");
        fs::create_dir_all(target_file.parent().unwrap()).unwrap();
        fs::write(&target_file, "fn a() {}").unwrap();
        assert!(!should_process_path(root, &target_file, &matcher));

        // ndxrignore pattern
        let vendor_file = root.join("vendor").join("dep.rs");
        fs::create_dir_all(vendor_file.parent().unwrap()).unwrap();
        fs::write(&vendor_file, "fn b() {}").unwrap();
        assert!(!should_process_path(root, &vendor_file, &matcher));

        // non-ignored file passes through
        let src_file = root.join("src").join("main.rs");
        fs::create_dir_all(src_file.parent().unwrap()).unwrap();
        fs::write(&src_file, "fn main() {}").unwrap();
        assert!(should_process_path(root, &src_file, &matcher));
    }

    #[test]
    fn gitignore_fallback_when_no_ndxrignore() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::write(root.join(".gitignore"), "build/\n").unwrap();
        // No .ndxrignore

        let matcher = build_ignore_matcher(root);

        let build_file = root.join("build").join("output.rs");
        fs::create_dir_all(build_file.parent().unwrap()).unwrap();
        fs::write(&build_file, "fn x() {}").unwrap();
        assert!(!should_process_path(root, &build_file, &matcher));
    }

    #[test]
    fn no_ignore_files_allows_source_paths() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // No .gitignore, no .ndxrignore

        let matcher = build_ignore_matcher(root);

        let file = root.join("src").join("lib.rs");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "fn y() {}").unwrap();
        assert!(should_process_path(root, &file, &matcher));
    }

    #[test]
    fn hot_reload_ndxrignore_rejects_newly_ignored_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Create a source file.
        let file = root.join("generated").join("output.rs");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "fn gen() {}").unwrap();

        // Initially no .ndxrignore — file should pass.
        let matcher = build_ignore_matcher(root);
        assert!(should_process_path(root, &file, &matcher));

        // Create .ndxrignore that excludes generated/.
        fs::write(root.join(".ndxrignore"), "generated/\n").unwrap();

        // Rebuild matcher — file should now be rejected.
        let matcher = build_ignore_matcher(root);
        assert!(!should_process_path(root, &file, &matcher));
    }

    #[test]
    fn hot_reload_gitignore_rejects_newly_ignored_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let file = root.join("logs").join("app.rs");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "fn log() {}").unwrap();

        // Initially no .gitignore — file should pass.
        let matcher = build_ignore_matcher(root);
        assert!(should_process_path(root, &file, &matcher));

        // Create .gitignore that excludes logs/.
        fs::write(root.join(".gitignore"), "logs/\n").unwrap();

        // Rebuild matcher — file should now be rejected.
        let matcher = build_ignore_matcher(root);
        assert!(!should_process_path(root, &file, &matcher));
    }

    #[test]
    fn default_patterns_skip_target_and_build() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // No .gitignore, no .ndxrignore — defaults still apply

        let matcher = build_ignore_matcher(root);

        for dir in &["target", "build", "bin", "node_modules", "dist"] {
            let file = root.join(dir).join("output.rs");
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, "fn z() {}").unwrap();
            assert!(
                !should_process_path(root, &file, &matcher),
                "{dir}/ should be excluded by default"
            );
        }
    }
}
