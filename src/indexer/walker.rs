//! Filesystem walker that discovers indexable source files.
//!
//! Uses the [`ignore`] crate to walk the workspace tree while respecting
//! `.gitignore`, `.ndxrignore`, and other standard exclusion patterns.

use std::path::{Path, PathBuf};

use anyhow::Result;
use tracing::trace;

use crate::languages;

/// Default maximum file size in bytes (1 MiB).
const DEFAULT_MAX_FILE_SIZE: u64 = 1_048_576;

/// Walks the workspace and returns paths to all indexable source files.
///
/// Respects `.gitignore`, `.ndxrignore`, skips hidden files, the `.ndxr/`
/// directory, files larger than 1 MiB, and files with unsupported extensions.
///
/// # Errors
///
/// Returns an error if the walk builder cannot be constructed or if the root
/// path is invalid.
pub fn walk_workspace(root: &Path) -> Result<Vec<PathBuf>> {
    walk_workspace_with_max_size(root, DEFAULT_MAX_FILE_SIZE)
}

/// Like [`walk_workspace`] but with a configurable file size limit.
///
/// Files exceeding `max_file_size` bytes are silently skipped. All other
/// filtering rules from [`walk_workspace`] apply.
///
/// # Errors
///
/// Returns an error if the walk builder cannot be constructed or if the root
/// path is invalid.
pub fn walk_workspace_with_max_size(root: &Path, max_file_size: u64) -> Result<Vec<PathBuf>> {
    let supported_extensions: Vec<&str> = languages::all_extensions();

    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .add_custom_ignore_filename(".ndxrignore")
        .follow_links(false)
        .sort_by_file_path(Ord::cmp)
        .build();

    let mut files = Vec::new();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                trace!("walk error: {err}");
                continue;
            }
        };

        // Must be a regular file (not directory, not symlink).
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }

        let path = entry.path();

        // Skip anything inside a `.ndxr/` directory.
        if path.components().any(|c| c.as_os_str() == ".ndxr") {
            continue;
        }

        // Skip files exceeding the size limit.
        if let Ok(metadata) = path.metadata()
            && metadata.len() > max_file_size
        {
            trace!("skipping oversized file: {}", path.display());
            continue;
        }

        // Must have a supported extension (dot-prefixed).
        let ext = match path.extension().and_then(|e| e.to_str()) {
            Some(e) => format!(".{e}"),
            None => continue,
        };

        if !supported_extensions.contains(&ext.as_str()) {
            continue;
        }

        files.push(path.to_path_buf());
    }

    Ok(files)
}
