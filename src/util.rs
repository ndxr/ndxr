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
#[must_use]
pub fn unix_now() -> i64 {
    let secs = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs(),
        Err(e) => {
            tracing::warn!("system clock before UNIX epoch: {e}");
            0
        }
    };
    i64::try_from(secs).unwrap_or_else(|_| {
        tracing::warn!("Unix timestamp {secs} exceeds i64 range");
        i64::MAX
    })
}
