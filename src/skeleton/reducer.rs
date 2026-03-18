//! Renders symbols as signature-only lines, grouped by file.
//!
//! Symbols are sorted by source position within each file. Class and struct
//! members are detected by line-range containment and indented under their
//! parent. Optionally includes docstrings as `///` comment lines.

use anyhow::{Context, Result};
use rusqlite::Connection;

/// A row of symbol data loaded from the database.
#[derive(Debug, Clone)]
pub struct SymbolRow {
    /// Database ID.
    pub id: i64,
    /// Short name of the symbol.
    pub name: String,
    /// Symbol kind (function, class, method, etc.).
    pub kind: String,
    /// Fully-qualified name.
    pub fqn: String,
    /// Type signature, if available.
    pub signature: Option<String>,
    /// Docstring, if available.
    pub docstring: Option<String>,
    /// First line of the symbol definition.
    pub start_line: i64,
    /// Last line of the symbol definition.
    pub end_line: i64,
    /// Whether the symbol is exported (public API).
    pub is_exported: bool,
    /// Relative file path where the symbol is defined.
    pub file_path: String,
}

/// Kinds that act as containers for child symbols.
const CONTAINER_KINDS: &[&str] = &[
    "class",
    "struct",
    "interface",
    "enum",
    "trait",
    "impl",
    "module",
    "namespace",
];

/// Returns `true` if the given kind is a container that can hold child symbols.
fn is_container_kind(kind: &str) -> bool {
    let lower = kind.to_lowercase();
    CONTAINER_KINDS.iter().any(|&k| lower.contains(k))
}

/// Renders a list of symbols from a single file as a skeleton string.
///
/// Symbols are ordered by `start_line`. Class/struct members are indented
/// under their parent with 2 spaces. Optionally includes docstrings.
///
/// # Example output
///
/// ```text
/// export class AuthService extends BaseService
///   constructor(config: AuthConfig)
///   async validateToken(token: string): Promise<User>
/// export function createAuthService(config: AuthConfig): AuthService
/// ```
#[must_use]
pub fn render_file_skeleton(symbols: &[SymbolRow], include_docs: bool) -> String {
    if symbols.is_empty() {
        return String::new();
    }

    let mut sorted: Vec<&SymbolRow> = symbols.iter().collect();
    sorted.sort_by_key(|s| s.start_line);

    // Identify container (parent) symbols by kind.
    // A symbol is a child if its start_line and end_line fall within a container's range.
    let containers: Vec<&SymbolRow> = sorted
        .iter()
        .filter(|s| is_container_kind(&s.kind))
        .copied()
        .collect();

    let mut lines = Vec::new();

    for sym in &sorted {
        let is_child = containers.iter().any(|parent| {
            parent.id != sym.id
                && sym.start_line >= parent.start_line
                && sym.end_line <= parent.end_line
        });

        let indent = if is_child { "  " } else { "" };

        if include_docs && let Some(ref doc) = sym.docstring {
            for doc_line in doc.lines() {
                lines.push(format!("{indent}/// {doc_line}"));
            }
        }

        lines.push(format!("{indent}{}", render_signature(sym)));
    }

    lines.join("\n")
}

/// Renders a single symbol's signature line.
///
/// Returns the signature if available, otherwise falls back to the symbol name.
#[must_use]
pub fn render_signature(symbol: &SymbolRow) -> String {
    symbol
        .signature
        .as_deref()
        .unwrap_or(&symbol.name)
        .to_string()
}

/// Loads all symbols for the given file paths from the database.
///
/// Results are ordered by file path and then by start line within each file.
///
/// # Errors
///
/// Returns an error if any database query fails.
pub fn load_file_symbols(conn: &Connection, file_paths: &[String]) -> Result<Vec<SymbolRow>> {
    if file_paths.is_empty() {
        return Ok(Vec::new());
    }

    // Build a parameterized IN clause.
    let placeholders: Vec<String> = (1..=file_paths.len()).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "SELECT s.id, s.name, s.kind, s.fqn, s.signature, s.docstring, \
                s.start_line, s.end_line, s.is_exported, f.path \
         FROM symbols s \
         JOIN files f ON s.file_id = f.id \
         WHERE f.path IN ({}) \
         ORDER BY f.path, s.start_line",
        placeholders.join(", ")
    );

    let mut stmt = conn.prepare(&sql).context("prepare load_file_symbols")?;

    let params: Vec<&dyn rusqlite::types::ToSql> = file_paths
        .iter()
        .map(|p| p as &dyn rusqlite::types::ToSql)
        .collect();

    let rows = stmt
        .query_map(params.as_slice(), |row| {
            Ok(SymbolRow {
                id: row.get(0)?,
                name: row.get(1)?,
                kind: row.get(2)?,
                fqn: row.get(3)?,
                signature: row.get(4)?,
                docstring: row.get(5)?,
                start_line: row.get(6)?,
                end_line: row.get(7)?,
                is_exported: row.get(8)?,
                file_path: row.get(9)?,
            })
        })
        .context("query file symbols")?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row.context("read symbol row")?);
    }
    Ok(result)
}

/// Renders skeletons for multiple files.
///
/// Returns a list of `(file_path, skeleton_text, symbol_count, original_line_count)` tuples.
/// Files with no symbols are omitted from the output.
///
/// # Errors
///
/// Returns an error if the database queries fail.
pub fn render_skeletons(
    conn: &Connection,
    file_paths: &[String],
    include_docs: bool,
) -> Result<Vec<(String, String, usize, i64)>> {
    let symbols = load_file_symbols(conn, file_paths)?;

    // Group symbols by file_path while preserving order.
    let mut grouped: Vec<(String, Vec<SymbolRow>)> = Vec::new();
    for sym in symbols {
        if let Some(last) = grouped.last_mut()
            && last.0 == sym.file_path
        {
            last.1.push(sym);
            continue;
        }
        let path = sym.file_path.clone();
        grouped.push((path, vec![sym]));
    }

    let mut results = Vec::new();
    for (file_path, file_symbols) in &grouped {
        let skeleton = render_file_skeleton(file_symbols, include_docs);
        let sym_count = file_symbols.len();

        let line_count: i64 = conn
            .query_row(
                "SELECT line_count FROM files WHERE path = ?1",
                [file_path],
                |row| row.get(0),
            )
            .unwrap_or(0);

        results.push((file_path.clone(), skeleton, sym_count, line_count));
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_symbol(
        id: i64,
        name: &str,
        kind: &str,
        signature: Option<&str>,
        docstring: Option<&str>,
        start_line: i64,
        end_line: i64,
    ) -> SymbolRow {
        SymbolRow {
            id,
            name: name.to_string(),
            kind: kind.to_string(),
            fqn: format!("test::{name}"),
            signature: signature.map(ToString::to_string),
            docstring: docstring.map(ToString::to_string),
            start_line,
            end_line,
            is_exported: true,
            file_path: "test.ts".to_string(),
        }
    }

    #[test]
    fn function_symbol_renders_single_line() {
        let syms = vec![make_symbol(
            1,
            "validateToken",
            "function",
            Some("export function validateToken(token: string): boolean"),
            None,
            1,
            3,
        )];
        let result = render_file_skeleton(&syms, false);
        assert_eq!(
            result,
            "export function validateToken(token: string): boolean"
        );
    }

    #[test]
    fn class_with_methods_indented() {
        let syms = vec![
            make_symbol(
                1,
                "AuthService",
                "class",
                Some("export class AuthService"),
                None,
                1,
                10,
            ),
            make_symbol(
                2,
                "validate",
                "method",
                Some("validate(token: string): boolean"),
                None,
                3,
                5,
            ),
            make_symbol(3, "logout", "method", Some("logout(): void"), None, 7, 9),
        ];
        let result = render_file_skeleton(&syms, false);
        let expected =
            "export class AuthService\n  validate(token: string): boolean\n  logout(): void";
        assert_eq!(result, expected);
    }

    #[test]
    fn include_docs_adds_docstrings() {
        let syms = vec![make_symbol(
            1,
            "validateToken",
            "function",
            Some("export function validateToken(token: string): boolean"),
            Some("Validates authentication tokens"),
            1,
            3,
        )];
        let result = render_file_skeleton(&syms, true);
        assert!(result.contains("/// Validates authentication tokens"));
        assert!(result.contains("export function validateToken(token: string): boolean"));
    }

    #[test]
    fn empty_symbols_renders_empty_string() {
        let result = render_file_skeleton(&[], false);
        assert!(result.is_empty());
    }

    #[test]
    fn signature_fallback_to_name() {
        let sym = make_symbol(1, "myFunc", "function", None, None, 1, 3);
        assert_eq!(render_signature(&sym), "myFunc");
    }

    #[test]
    fn multiline_docstring_renders_each_line() {
        let syms = vec![make_symbol(
            1,
            "foo",
            "function",
            Some("foo(): void"),
            Some("Line one\nLine two"),
            1,
            3,
        )];
        let result = render_file_skeleton(&syms, true);
        assert!(result.contains("/// Line one"));
        assert!(result.contains("/// Line two"));
    }
}
