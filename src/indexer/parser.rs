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
    let source = std::fs::read_to_string(abs_path)
        .with_context(|| format!("cannot read file: {}", abs_path.display()))?;

    let blake3_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    parse_file_from_content(workspace_root, abs_path, &source, blake3_hash)
}

/// Parses a file from already-read content and a pre-computed hash.
///
/// This avoids redundant disk reads and hash computation when the caller has
/// already read the file (e.g., for incremental indexing where BLAKE3 hashes
/// are computed upfront). The source is parsed once with tree-sitter, and both
/// symbols and edges are extracted from the same AST.
///
/// # Errors
///
/// Returns an error if the language is unsupported or parsing fails.
pub fn parse_file_from_content(
    workspace_root: &Path,
    abs_path: &Path,
    source: &str,
    content_hash: String,
) -> Result<ParseResult> {
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

    let line_count = source.lines().count();

    #[allow(clippy::cast_possible_truncation)] // file sizes are well within u64
    let byte_size = source.len() as u64;

    let rel_path_str = crate::util::normalize_path(&relative);

    // Parse once and reuse the tree for both symbol and edge extraction.
    let tree = super::symbols::parse_source(source, lang_config)
        .with_context(|| format!("tree-sitter parse failed: {}", abs_path.display()))?;

    let symbols =
        super::symbols::extract_symbols_from_tree(&rel_path_str, source, lang_config, &tree)
            .with_context(|| format!("symbol extraction failed: {}", abs_path.display()))?;
    let edges = super::symbols::extract_edges_from_tree(&rel_path_str, source, lang_config, &tree)
        .with_context(|| format!("edge extraction failed: {}", abs_path.display()))?;

    Ok(ParseResult {
        path: relative,
        language: lang_config.name.to_owned(),
        blake3_hash: content_hash,
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

/// Pre-read file content with its absolute path, source text, and BLAKE3 hash.
///
/// Used by [`parse_files_parallel_from_content`] to avoid redundant disk reads
/// during incremental indexing, where hashes have already been computed.
pub struct PreReadFile {
    /// Absolute path to the file on disk.
    pub abs_path: PathBuf,
    /// File contents as a UTF-8 string.
    pub source: String,
    /// Pre-computed BLAKE3 hex digest of the contents.
    pub blake3_hash: String,
}

/// Parses multiple pre-read files in parallel using rayon.
///
/// Unlike [`parse_files_parallel`], this accepts files whose content and BLAKE3
/// hash have already been computed, eliminating redundant disk reads and hash
/// computations. Files that fail to parse are logged at warning level and
/// excluded from the results.
#[must_use]
pub fn parse_files_parallel_from_content(
    workspace_root: &Path,
    files: Vec<PreReadFile>,
) -> Vec<ParseResult> {
    files
        .into_par_iter()
        .filter_map(|file| {
            let display_path = file.abs_path.display().to_string();
            match parse_file_from_content(
                workspace_root,
                &file.abs_path,
                &file.source,
                file.blake3_hash,
            ) {
                Ok(result) => Some(result),
                Err(err) => {
                    warn!("skipping {display_path}: {err:#}");
                    None
                }
            }
        })
        .collect()
}
