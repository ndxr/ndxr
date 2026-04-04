//! Workspace root detection by walking the filesystem upward.
//!
//! Searches for `.git/` first, then falls back to `.ndxr/` as a workspace marker.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Walks from `start` upward to find the workspace root.
///
/// Checks for `.git/` first (highest priority). If no `.git/` is found,
/// falls back to `.ndxr/` as a workspace root marker. This allows ndxr
/// to work in non-git directories.
///
/// # Errors
///
/// Returns an error if `start` cannot be resolved or if neither `.git/`
/// nor `.ndxr/` is found before reaching the filesystem root.
pub fn find_workspace_root(start: &Path) -> Result<PathBuf> {
    let start = start
        .canonicalize()
        .with_context(|| format!("cannot resolve path: {}", start.display()))?;

    // First pass: walk upward looking for .git/ (highest priority).
    let mut current = start.as_path();
    loop {
        if current.join(".git").is_dir() {
            return Ok(current.to_path_buf());
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }

    // Second pass: walk upward looking for .ndxr/ as fallback.
    current = start.as_path();
    loop {
        if current.join(".ndxr").is_dir() {
            return Ok(current.to_path_buf());
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }

    bail!(
        "not an ndxr workspace (no .git/ or .ndxr/ directory found).\n\
         Hint: run 'git init' to create a repository, or 'mkdir .ndxr' for a non-git workspace."
    )
}
