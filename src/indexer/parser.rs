//! Parser dispatch: routes files to their language-specific tree-sitter grammar.
//!
//! Provides both single-file and parallel multi-file parsing. Each file is
//! mapped to its language by extension, parsed via tree-sitter, and its symbols
//! and edges extracted.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rayon::prelude::*;
use tracing::warn;

use super::symbols::{ExtractedEdge, ExtractedSymbol};
use crate::languages;

/// Result of parsing a single source file.
#[derive(Debug)]
pub struct ParseResult {
    /// Relative path from the workspace root.
    pub path: PathBuf,
    /// Language name (e.g., `"typescript"`, `"rust"`).
    pub language: String,
    /// BLAKE3 hex digest of the file contents.
    pub blake3_hash: String,
    /// Number of lines in the file.
    pub line_count: usize,
    /// File size in bytes.
    pub byte_size: u64,
    /// Extracted symbol definitions.
    pub symbols: Vec<ExtractedSymbol>,
    /// Extracted edges (imports, calls).
    pub edges: Vec<ExtractedEdge>,
}

/// Parses a single file using the appropriate language grammar.
///
/// The relative path is computed from `workspace_root`. The file is read,
/// hashed, and its symbols and edges extracted using the matching
/// [`LanguageConfig`](crate::languages::LanguageConfig).
///
/// # Errors
///
/// Returns an error if the file cannot be read, has an unsupported extension,
/// or if symbol/edge extraction fails.
pub fn parse_file(workspace_root: &Path, abs_path: &Path) -> Result<ParseResult> {
    let relative = abs_path
        .strip_prefix(workspace_root)
        .with_context(|| {
            format!(
                "{} is not under workspace root {}",
                abs_path.display(),
                workspace_root.display()
            )
        })?
        .to_path_buf();

    let ext = abs_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .with_context(|| format!("no extension: {}", abs_path.display()))?;

    let lang_config = languages::get_language_config(&ext)
        .with_context(|| format!("unsupported extension: {ext}"))?;

    let source = std::fs::read_to_string(abs_path)
        .with_context(|| format!("cannot read file: {}", abs_path.display()))?;

    let blake3_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let line_count = source.lines().count();

    #[allow(clippy::cast_possible_truncation)]
    let byte_size = source.len() as u64;

    let rel_path_str = crate::util::normalize_path(&relative);
    let symbols = super::symbols::extract_symbols(&rel_path_str, &source, lang_config)
        .with_context(|| format!("symbol extraction failed: {}", abs_path.display()))?;
    let edges = super::symbols::extract_edges(&rel_path_str, &source, lang_config)
        .with_context(|| format!("edge extraction failed: {}", abs_path.display()))?;

    Ok(ParseResult {
        path: relative,
        language: lang_config.name.to_owned(),
        blake3_hash,
        line_count,
        byte_size,
        symbols,
        edges,
    })
}

/// Parses multiple files in parallel using rayon.
///
/// Files that fail to parse are logged at warning level and excluded from the
/// results. The returned vector contains only successful parse results.
#[must_use]
pub fn parse_files_parallel(workspace_root: &Path, files: &[PathBuf]) -> Vec<ParseResult> {
    files
        .par_iter()
        .filter_map(|file| match parse_file(workspace_root, file) {
            Ok(result) => Some(result),
            Err(err) => {
                warn!("skipping {}: {err:#}", file.display());
                None
            }
        })
        .collect()
}
