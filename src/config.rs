//! Runtime configuration derived from the workspace root and environment.

use std::path::PathBuf;

/// Runtime configuration for an ndxr session.
///
/// All paths are derived deterministically from the workspace root. Token
/// budget limits are read from environment variables at construction time,
/// falling back to sensible defaults.
#[derive(Debug, Clone)]
pub struct NdxrConfig {
    /// Absolute path to the workspace root (directory containing `.git/`).
    pub workspace_root: PathBuf,
    /// Path to the `.ndxr/` directory inside the workspace root.
    pub ndxr_dir: PathBuf,
    /// Path to the `SQLite` database file (`.ndxr/index.db`).
    pub db_path: PathBuf,
    /// Maximum number of tokens returned per MCP response (`None` = unlimited).
    pub max_tokens: Option<usize>,
    /// Average characters per token for budget estimation.
    pub chars_per_token: f64,
    /// Maximum age (in seconds) before inactive sessions are compressed.
    pub compression_age_secs: u64,
    /// Recency half-life (in days) for memory search scoring.
    pub recency_half_life_days: f64,
    /// Debounce interval (in milliseconds) for the file watcher.
    pub debounce_ms: u64,
}

/// Default maximum token budget when `NDXR_MAX_TOKENS` is unset or invalid.
const DEFAULT_MAX_TOKENS: usize = 20_000;

/// Default session compression age: 24 hours.
const DEFAULT_COMPRESSION_AGE_SECS: u64 = 86_400;

/// Default recency half-life: 7 days.
const DEFAULT_RECENCY_HALF_LIFE_DAYS: f64 = 7.0;

/// Default file watcher debounce interval: 500 ms.
const DEFAULT_DEBOUNCE_MS: u64 = 500;

/// Environment variable name for overriding the maximum token budget.
const MAX_TOKENS_ENV: &str = "NDXR_MAX_TOKENS";

/// Environment variable name for overriding the characters-per-token ratio.
const CHARS_PER_TOKEN_ENV: &str = "NDXR_CHARS_PER_TOKEN";

impl NdxrConfig {
    /// Constructs a new configuration rooted at `workspace_root`.
    ///
    /// Reads `NDXR_MAX_TOKENS` from the environment to override the default
    /// token budget. Invalid or missing values fall back to 20 000 tokens.
    #[must_use]
    pub fn from_workspace(workspace_root: PathBuf) -> Self {
        let ndxr_dir = workspace_root.join(".ndxr");
        let db_path = ndxr_dir.join("index.db");

        let max_tokens = match std::env::var(MAX_TOKENS_ENV).ok().as_deref() {
            Some("-1") => None,
            Some(v) => Some(v.parse::<usize>().unwrap_or(DEFAULT_MAX_TOKENS)),
            None => Some(DEFAULT_MAX_TOKENS),
        };

        let chars_per_token = std::env::var(CHARS_PER_TOKEN_ENV)
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(DEFAULT_CHARS_PER_TOKEN);

        Self {
            workspace_root,
            ndxr_dir,
            db_path,
            max_tokens,
            chars_per_token,
            compression_age_secs: DEFAULT_COMPRESSION_AGE_SECS,
            recency_half_life_days: DEFAULT_RECENCY_HALF_LIFE_DAYS,
            debounce_ms: DEFAULT_DEBOUNCE_MS,
        }
    }
}

/// Estimates token counts from character lengths using a fixed ratio.
///
/// Different models tokenize text differently, so this provides a
/// configurable approximation rather than coupling to any specific tokenizer.
#[derive(Debug, Clone)]
pub struct TokenEstimator {
    /// Average number of characters per token.
    chars_per_token: f64,
}

/// Default average characters per token (empirically reasonable for code).
const DEFAULT_CHARS_PER_TOKEN: f64 = 3.5;

impl TokenEstimator {
    /// Creates a new estimator with the given characters-per-token ratio.
    ///
    /// # Panics
    ///
    /// Panics if `chars_per_token` is not positive and finite.
    #[must_use]
    pub fn new(chars_per_token: f64) -> Self {
        assert!(
            chars_per_token.is_finite() && chars_per_token > 0.0,
            "chars_per_token must be positive and finite"
        );
        Self { chars_per_token }
    }

    /// Estimates the number of tokens in the given text.
    ///
    /// Uses byte length as a proxy for character count, which is accurate
    /// for ASCII-dominated source code. Returns 0 for empty text.
    #[must_use]
    pub fn estimate(&self, text: &str) -> usize {
        if text.is_empty() {
            return 0;
        }
        #[allow(
            clippy::cast_possible_truncation, // token estimate fits in usize
            clippy::cast_sign_loss,           // ceil of positive ratio is non-negative
            clippy::cast_precision_loss       // usize->f64 loss negligible for text lengths
        )]
        let tokens = (text.len() as f64 / self.chars_per_token).ceil() as usize;
        tokens
    }
}

impl Default for TokenEstimator {
    fn default() -> Self {
        Self::new(DEFAULT_CHARS_PER_TOKEN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_estimator_default_ratio() {
        let est = TokenEstimator::default();
        // 35 chars / 3.5 = 10 tokens exactly
        assert_eq!(est.estimate("a]b]c]d]e]f]g]h]i]j]k]l]m]n]o]p]q]"), 10);
        // 7 chars / 3.5 = 2 tokens exactly
        assert_eq!(est.estimate("abcdefg"), 2);
    }

    #[test]
    fn token_estimator_empty_string() {
        let est = TokenEstimator::default();
        assert_eq!(est.estimate(""), 0);
    }

    #[test]
    fn token_estimator_single_char() {
        let est = TokenEstimator::default();
        assert_eq!(est.estimate("x"), 1);
    }

    #[test]
    fn config_from_workspace_derives_correct_paths() {
        let root = PathBuf::from("/tmp/test-project");
        let config = NdxrConfig::from_workspace(root.clone());

        assert_eq!(config.workspace_root, root);
        assert_eq!(config.ndxr_dir, root.join(".ndxr"));
        assert_eq!(config.db_path, root.join(".ndxr").join("index.db"));
        assert_eq!(config.max_tokens, Some(DEFAULT_MAX_TOKENS));
    }

    #[test]
    fn config_max_tokens_env_parsing() {
        // All env-var-mutating assertions in one test to avoid parallel races.
        // SAFETY: test-only; env var mutation is not thread-safe but acceptable
        // when all mutations are in a single test function.

        // Default (no env var set) → Some(DEFAULT_MAX_TOKENS)
        unsafe { std::env::remove_var(MAX_TOKENS_ENV) };
        let config = NdxrConfig::from_workspace(PathBuf::from("/tmp/test-default"));
        assert_eq!(config.max_tokens, Some(DEFAULT_MAX_TOKENS));

        // "-1" → None (unlimited)
        unsafe { std::env::set_var(MAX_TOKENS_ENV, "-1") };
        let config = NdxrConfig::from_workspace(PathBuf::from("/tmp/test-unlimited"));
        assert_eq!(config.max_tokens, None);

        // Positive value → Some(value)
        unsafe { std::env::set_var(MAX_TOKENS_ENV, "30000") };
        let config = NdxrConfig::from_workspace(PathBuf::from("/tmp/test-custom"));
        assert_eq!(config.max_tokens, Some(30_000));

        // Clean up
        unsafe { std::env::remove_var(MAX_TOKENS_ENV) };
    }

    #[test]
    fn config_chars_per_token_env_parsing() {
        // All env-var-mutating assertions in one test to avoid parallel races.
        // SAFETY: test-only; env var mutation is not thread-safe but acceptable
        // when all mutations are in a single test function.

        // Default (no env var set) → DEFAULT_CHARS_PER_TOKEN
        unsafe { std::env::remove_var(CHARS_PER_TOKEN_ENV) };
        let config = NdxrConfig::from_workspace(PathBuf::from("/tmp/test-cpt"));
        assert!((config.chars_per_token - DEFAULT_CHARS_PER_TOKEN).abs() < f64::EPSILON);

        // Custom value → parsed
        unsafe { std::env::set_var(CHARS_PER_TOKEN_ENV, "4.0") };
        let config = NdxrConfig::from_workspace(PathBuf::from("/tmp/test-cpt-custom"));
        assert!((config.chars_per_token - 4.0).abs() < f64::EPSILON);

        // Invalid value → falls back to default
        unsafe { std::env::set_var(CHARS_PER_TOKEN_ENV, "-1.0") };
        let config = NdxrConfig::from_workspace(PathBuf::from("/tmp/test-cpt-invalid"));
        assert!((config.chars_per_token - DEFAULT_CHARS_PER_TOKEN).abs() < f64::EPSILON);

        // Clean up
        unsafe { std::env::remove_var(CHARS_PER_TOKEN_ENV) };
    }
}
