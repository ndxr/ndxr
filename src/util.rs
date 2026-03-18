//! Shared utility functions.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Normalizes a path to use forward slashes for consistent cross-platform storage.
///
/// On Unix this is effectively a no-op. On Windows it converts `\` to `/`,
/// ensuring paths stored in the database and used in FQN construction are
/// identical regardless of the host OS.
#[must_use]
pub fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Returns the current Unix timestamp in seconds.
///
/// # Panics
///
/// Panics if the system clock is before the Unix epoch.
#[must_use]
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_secs()
        .try_into()
        .expect("timestamp exceeds i64 range")
}
