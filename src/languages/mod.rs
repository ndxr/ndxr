//! Language registry mapping file extensions to tree-sitter grammars and queries.

use tree_sitter_language::LanguageFn;

pub mod bash;
pub mod c_lang;
pub mod cpp;
pub mod csharp;
pub mod go_lang;
pub mod java;
pub mod javascript;
pub mod php;
pub mod python;
pub mod ruby;
pub mod rust_lang;
pub mod typescript;
pub mod zig;

/// Configuration for a single language: grammar and extraction queries.
#[derive(Clone, Copy)]
pub struct LanguageConfig {
    /// File extensions this language handles (including the dot).
    pub extensions: &'static [&'static str],
    /// tree-sitter grammar function.
    pub language: LanguageFn,
    /// Human-readable language name.
    pub name: &'static str,
    /// tree-sitter S-expression query for symbol extraction.
    pub symbol_query: &'static str,
    /// tree-sitter S-expression query for import extraction.
    pub import_query: &'static str,
    /// tree-sitter S-expression query for call extraction.
    pub call_query: &'static str,
}

/// Returns the language config for a file extension (including the dot).
///
/// Returns `None` if the extension is not supported.
#[must_use]
pub fn get_language_config(extension: &str) -> Option<&'static LanguageConfig> {
    ALL_LANGUAGES
        .iter()
        .find(|lang| lang.extensions.contains(&extension))
}

/// Returns all registered language configurations.
#[must_use]
pub fn all_languages() -> &'static [LanguageConfig] {
    ALL_LANGUAGES
}

/// Returns all supported file extensions across all languages.
#[must_use]
pub fn all_extensions() -> Vec<&'static str> {
    ALL_LANGUAGES
        .iter()
        .flat_map(|l| l.extensions.iter().copied())
        .collect()
}

static ALL_LANGUAGES: &[LanguageConfig] = &[
    typescript::TYPESCRIPT,
    typescript::TSX,
    javascript::JAVASCRIPT,
    python::PYTHON,
    go_lang::GO,
    rust_lang::RUST,
    java::JAVA,
    csharp::CSHARP,
    ruby::RUBY,
    bash::BASH,
    php::PHP,
    zig::ZIG,
    c_lang::C,
    cpp::CPP,
];
