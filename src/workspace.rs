//! Workspace root detection by walking the filesystem upward to find `.git/`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Walks from `start` upward until a `.git/` directory is found.
///
/// Returns the canonicalized directory containing `.git/` (the workspace root).
///
/// # Errors
///
/// Returns an error if `start` cannot be resolved or if no `.git/` directory
/// is found before reaching the filesystem root.
pub fn find_workspace_root(start: &Path) -> Result<PathBuf> {
    let start = start
        .canonicalize()
        .with_context(|| format!("cannot resolve path: {}", start.display()))?;

    let mut current = start.as_path();
    loop {
        if current.join(".git").is_dir() {
            return Ok(current.to_path_buf());
        }
        current = match current.parent() {
            Some(parent) => parent,
            None => bail!("reached filesystem root without finding .git/"),
        };
    }
}
