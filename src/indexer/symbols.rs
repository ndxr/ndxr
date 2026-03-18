//! Symbol and edge extraction from tree-sitter ASTs.
//!
//! Parses source code using tree-sitter grammars and extracts structured symbol
//! definitions (functions, classes, types, etc.) and edges (imports, calls)
//! using the language-specific queries defined in [`crate::languages`].

use anyhow::{Context, Result};
use tree_sitter::{Node, Parser, Query, QueryCursor, QueryMatch, StreamingIterator};

use crate::languages::LanguageConfig;

/// A symbol extracted from source code.
#[derive(Debug, Clone)]
pub struct ExtractedSymbol {
    /// The symbol's local name (e.g., `validateToken`).
    pub name: String,
    /// The symbol kind: function, method, class, interface, type, enum,
    /// variable, struct, trait, or module.
    pub kind: String,
    /// Fully-qualified name: `"file_path::ParentClass::SymbolName"`.
    pub fqn: String,
    /// The declaration signature without the body.
    pub signature: Option<String>,
    /// Documentation comment preceding the symbol, if any.
    pub docstring: Option<String>,
    /// One-indexed start line of the symbol definition.
    pub start_line: usize,
    /// One-indexed end line of the symbol definition.
    pub end_line: usize,
    /// Whether this symbol is exported / public.
    pub is_exported: bool,
    /// BLAKE3 hash of the symbol's body text.
    pub body_hash: Option<String>,
}

/// An edge (relationship) extracted from source code.
#[derive(Debug, Clone)]
pub struct ExtractedEdge {
    /// FQN of the containing symbol (or file path for file-level imports).
    pub from_fqn: String,
    /// Name of the referenced symbol (resolved to an ID later).
    pub to_name: String,
    /// Edge kind: `imports`, `calls`.
    pub kind: String,
}

/// Extracts symbols from source code using the language's symbol query.
///
/// Each matched query pattern produces one [`ExtractedSymbol`] with its kind
/// inferred from the tree-sitter node type, its FQN built from file path and
/// parent scope, and its body hashed with BLAKE3.
///
/// # Errors
///
/// Returns an error if the source cannot be parsed or the query is invalid.
pub fn extract_symbols(
    file_path: &str,
    source: &str,
    lang: &LanguageConfig,
) -> Result<Vec<ExtractedSymbol>> {
    let tree = parse_source(source, lang)?;
    let root = tree.root_node();
    let ts_lang: tree_sitter::Language = lang.language.into();
    let query =
        Query::new(&ts_lang, lang.symbol_query).context("failed to compile symbol query")?;

    let name_idx = query
        .capture_index_for_name("name")
        .context("symbol query must have a @name capture")?;
    let def_idx = query
        .capture_index_for_name("definition")
        .context("symbol query must have a @definition capture")?;

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, root, source.as_bytes());

    let source_bytes = source.as_bytes();
    let mut symbols = Vec::new();

    while let Some(m) = {
        matches.advance();
        matches.get()
    } {
        let Some(name_node) = find_capture(m, name_idx) else {
            continue;
        };
        let Some(def_node) = find_capture(m, def_idx) else {
            continue;
        };

        let name = node_text(name_node, source_bytes);
        if name.is_empty() {
            continue;
        }

        let kind = determine_kind(def_node);
        let parent_fqn = find_parent_scope(def_node, source_bytes, file_path);
        let fqn = format!("{parent_fqn}::{name}");

        let start_line = def_node.start_position().row + 1;
        let end_line = def_node.end_position().row + 1;

        let body_text = node_text(def_node, source_bytes);
        let body_hash = if body_text.is_empty() {
            None
        } else {
            Some(blake3::hash(body_text.as_bytes()).to_hex().to_string())
        };

        let signature = extract_signature(def_node, source_bytes, &kind);
        let docstring = extract_docstring(def_node, source_bytes, lang.name);
        let is_exported = detect_export(def_node, &name, lang.name);

        symbols.push(ExtractedSymbol {
            name,
            kind,
            fqn,
            signature,
            docstring,
            start_line,
            end_line,
            is_exported,
            body_hash,
        });
    }

    Ok(symbols)
}

/// Extracts edges (imports and calls) from source code.
///
/// Import queries produce `"imports"` edges at file scope; call queries produce
/// `"calls"` edges attributed to the innermost enclosing symbol.
///
/// # Errors
///
/// Returns an error if the source cannot be parsed or a query is invalid.
pub fn extract_edges(
    file_path: &str,
    source: &str,
    lang: &LanguageConfig,
) -> Result<Vec<ExtractedEdge>> {
    let tree = parse_source(source, lang)?;
    let root = tree.root_node();
    let ts_lang: tree_sitter::Language = lang.language.into();
    let source_bytes = source.as_bytes();

    let mut edges = Vec::new();

    if !lang.import_query.is_empty() {
        extract_import_edges(
            &ts_lang,
            lang.import_query,
            root,
            source_bytes,
            file_path,
            &mut edges,
        )?;
    }

    if !lang.call_query.is_empty() {
        extract_call_edges(
            &ts_lang,
            lang.call_query,
            root,
            source_bytes,
            file_path,
            lang.name,
            &mut edges,
        )?;
    }

    Ok(edges)
}

/// Extracts import edges from the AST.
fn extract_import_edges(
    ts_lang: &tree_sitter::Language,
    import_query_str: &str,
    root: Node<'_>,
    source_bytes: &[u8],
    file_path: &str,
    edges: &mut Vec<ExtractedEdge>,
) -> Result<()> {
    let query = Query::new(ts_lang, import_query_str).context("failed to compile import query")?;
    let source_idx = query
        .capture_index_for_name("source")
        .context("import query must have a @source capture")?;

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, root, source_bytes);

    while let Some(m) = {
        matches.advance();
        matches.get()
    } {
        if let Some(source_node) = find_capture(m, source_idx) {
            let raw = node_text(source_node, source_bytes);
            let import_name = clean_import_name(&raw);
            if !import_name.is_empty() {
                edges.push(ExtractedEdge {
                    from_fqn: file_path.to_owned(),
                    to_name: import_name,
                    kind: "imports".to_owned(),
                });
            }
        }
    }

    Ok(())
}

/// Extracts call edges from the AST.
fn extract_call_edges(
    ts_lang: &tree_sitter::Language,
    call_query_str: &str,
    root: Node<'_>,
    source_bytes: &[u8],
    file_path: &str,
    lang_name: &str,
    edges: &mut Vec<ExtractedEdge>,
) -> Result<()> {
    let query = Query::new(ts_lang, call_query_str).context("failed to compile call query")?;
    let func_idx = query
        .capture_index_for_name("function")
        .context("call query must have a @function capture")?;

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, root, source_bytes);

    while let Some(m) = {
        matches.advance();
        matches.get()
    } {
        if let Some(func_node) = find_capture(m, func_idx) {
            let called_name = node_text(func_node, source_bytes);
            if called_name.is_empty() {
                continue;
            }

            let from_fqn = find_enclosing_symbol_fqn(func_node, source_bytes, file_path, lang_name);

            edges.push(ExtractedEdge {
                from_fqn,
                to_name: called_name,
                kind: "calls".to_owned(),
            });
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Parses source code with the given language config.
fn parse_source(source: &str, lang: &LanguageConfig) -> Result<tree_sitter::Tree> {
    let mut parser = Parser::new();
    let ts_lang: tree_sitter::Language = lang.language.into();
    parser
        .set_language(&ts_lang)
        .context("failed to set parser language")?;
    parser
        .parse(source, None)
        .context("tree-sitter parse returned None")
}

/// Finds the first capture node for a given capture index in a match.
fn find_capture<'a>(m: &'a QueryMatch<'a, 'a>, idx: u32) -> Option<Node<'a>> {
    m.captures.iter().find(|c| c.index == idx).map(|c| c.node)
}

/// Extracts the UTF-8 text of a node from source bytes.
fn node_text(node: Node<'_>, source: &[u8]) -> String {
    node.utf8_text(source).unwrap_or_default().to_owned()
}

/// Determines the symbol kind from the definition node's type.
fn determine_kind(node: Node<'_>) -> String {
    let node_type = node.kind();
    let effective_type = unwrap_definition_node(node);

    match effective_type {
        "function_declaration"
        | "function_definition"
        | "function_item"
        | "function_signature_item"
        | "arrow_function"
        | "test_declaration" => "function",

        "method_definition"
        | "method_declaration"
        | "method"
        | "singleton_method"
        | "constructor_declaration" => "method",

        "class_declaration" | "class_definition" | "class_specifier" | "class" | "impl_item" => {
            "class"
        }

        "interface_declaration" => "interface",

        "type_alias_declaration"
        | "type_definition"
        | "type_item"
        | "type_declaration"
        | "type_spec" => "type",

        "enum_declaration" | "enum_item" | "enum_specifier" => "enum",

        "struct_item" | "struct_specifier" | "struct_declaration" => "struct",

        "trait_item" | "trait_declaration" => "trait",

        "mod_item" | "module" | "namespace_definition" => "module",

        "variable_declarator"
        | "lexical_declaration"
        | "variable_declaration"
        | "const_item"
        | "static_item" => "variable",

        // For Go type declarations via the outer node.
        _ if node_type == "type_declaration" => "type",

        _ => "function",
    }
    .to_owned()
}

/// Unwraps wrapper nodes (`export_statement`, `decorated_definition`) to find the
/// actual definition node type string.
fn unwrap_definition_node(node: Node<'_>) -> &str {
    let node_type = node.kind();

    match node_type {
        "export_statement" => {
            if let Some(decl) = node.child_by_field_name("declaration") {
                return unwrap_definition_node(decl);
            }
            for i in 0..u32_child_count(node) {
                if let Some(child) = node.child(i) {
                    let ck = child.kind();
                    if ck != "export" && ck != "default" && !ck.starts_with("comment") {
                        return unwrap_definition_node(child);
                    }
                }
            }
            node_type
        }
        "decorated_definition" => {
            if let Some(def) = node.child_by_field_name("definition") {
                return unwrap_definition_node(def);
            }
            node_type
        }
        _ => node_type,
    }
}

/// Walks up the AST to find the parent scope for FQN construction.
fn find_parent_scope(node: Node<'_>, source: &[u8], file_path: &str) -> String {
    let mut scope_parts: Vec<String> = Vec::new();
    let mut current = node.parent();

    while let Some(parent) = current {
        if let Some(scope_name) = extract_scope_name(parent, source) {
            scope_parts.push(scope_name);
        }
        current = parent.parent();
    }

    scope_parts.reverse();

    if scope_parts.is_empty() {
        file_path.to_owned()
    } else {
        format!("{file_path}::{}", scope_parts.join("::"))
    }
}

/// Extracts the name from a potential scope-defining node.
fn extract_scope_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "class_declaration"
        | "class_definition"
        | "class_specifier"
        | "class"
        | "interface_declaration"
        | "enum_declaration"
        | "enum_item"
        | "struct_item"
        | "struct_specifier"
        | "trait_item"
        | "trait_declaration"
        | "impl_item"
        | "mod_item"
        | "module"
        | "namespace_definition" => {
            for field in &["name", "type", "trait"] {
                if let Some(name_node) = node.child_by_field_name(field) {
                    let name = node_text(name_node, source);
                    if !name.is_empty() {
                        return Some(name);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Finds the FQN of the innermost enclosing symbol for edge attribution.
fn find_enclosing_symbol_fqn(
    node: Node<'_>,
    source: &[u8],
    file_path: &str,
    _lang_name: &str,
) -> String {
    let mut current = node.parent();

    while let Some(parent) = current {
        if is_symbol_defining_node(parent.kind())
            && let Some(name_node) = parent.child_by_field_name("name")
        {
            let name = node_text(name_node, source);
            if !name.is_empty() {
                let scope = find_parent_scope(parent, source, file_path);
                return format!("{scope}::{name}");
            }
        }
        current = parent.parent();
    }

    file_path.to_owned()
}

/// Returns `true` if the node type represents a symbol definition.
fn is_symbol_defining_node(node_type: &str) -> bool {
    matches!(
        node_type,
        "function_declaration"
            | "function_definition"
            | "function_item"
            | "method_definition"
            | "method_declaration"
            | "method"
            | "class_declaration"
            | "class_definition"
            | "class"
            | "impl_item"
            | "mod_item"
            | "module"
            | "trait_item"
            | "trait_declaration"
            | "constructor_declaration"
    )
}

/// Extracts the declaration signature (everything before the body).
fn extract_signature(node: Node<'_>, source: &[u8], kind: &str) -> Option<String> {
    let full_text = node_text(node, source);
    if full_text.is_empty() {
        return None;
    }

    let sig = match kind {
        "class" | "struct" | "enum" | "interface" | "trait" | "type" | "module" => {
            extract_type_signature(&full_text)
        }
        "variable" => extract_variable_signature(&full_text),
        // function, method, and everything else
        _ => extract_function_signature(&full_text),
    };

    let collapsed = collapse_whitespace(&sig);
    if collapsed.is_empty() {
        None
    } else {
        Some(collapsed)
    }
}

/// Extracts a variable declaration signature.
fn extract_variable_signature(text: &str) -> String {
    let trimmed = text.trim().trim_end_matches(';').trim();
    trimmed.find('{').map_or_else(
        || trimmed.to_owned(),
        |pos| trimmed[..pos].trim().to_owned(),
    )
}

/// Extracts a function/method signature by cutting at the opening brace or colon.
fn extract_function_signature(text: &str) -> String {
    // Find the first opening brace.
    if let Some(brace_pos) = text.find('{') {
        return text[..brace_pos].trim().to_owned();
    }

    // For Python-style: cut at the colon that ends the def line.
    if let Some(paren_pos) = text.rfind(')') {
        let after_paren = &text[paren_pos + 1..];
        if let Some(colon_offset) = after_paren.find(':') {
            let colon_pos = paren_pos + 1 + colon_offset;
            let between = text[paren_pos + 1..colon_pos].trim();
            if between.is_empty() || between.starts_with("->") {
                return text[..colon_pos].trim().to_owned();
            }
        }
    }

    // Fallback: take the first line.
    text.lines().next().unwrap_or(text).trim().to_owned()
}

/// Extracts a class/struct/enum signature by cutting at the opening brace.
fn extract_type_signature(text: &str) -> String {
    if let Some(brace_pos) = text.find('{') {
        return text[..brace_pos].trim().to_owned();
    }
    if let Some(colon_pos) = text.find(':') {
        return text[..colon_pos].trim().to_owned();
    }
    text.lines().next().unwrap_or(text).trim().to_owned()
}

/// Extracts the docstring/comment preceding a definition node.
fn extract_docstring(node: Node<'_>, source: &[u8], lang_name: &str) -> Option<String> {
    // For Python, look for the first string expression child (docstring inside body).
    if lang_name == "python"
        && let Some(doc) = extract_python_docstring(node, source)
    {
        return Some(doc);
    }

    // Look for comment nodes immediately preceding the definition.
    let prev = if node
        .parent()
        .is_some_and(|p| p.kind() == "export_statement")
    {
        node.parent().and_then(|p| p.prev_sibling())
    } else {
        node.prev_sibling()
    };

    let prev_node = prev?;
    let prev_kind = prev_node.kind();
    if matches!(prev_kind, "comment" | "line_comment" | "block_comment") {
        let def_start_line = node.start_position().row;
        let comment_end_line = prev_node.end_position().row;

        if def_start_line <= comment_end_line + 2 {
            let comment_text = node_text(prev_node, source);
            return Some(clean_comment(&comment_text));
        }
    }

    None
}

/// Extracts a Python docstring from the first `expression_statement` child.
fn extract_python_docstring(node: Node<'_>, source: &[u8]) -> Option<String> {
    let body = node.child_by_field_name("body")?;
    let first_stmt = body.child(0)?;

    if first_stmt.kind() == "expression_statement" {
        let expr = first_stmt.child(0)?;
        if expr.kind() == "string" || expr.kind() == "concatenated_string" {
            let text = node_text(expr, source);
            return Some(clean_python_docstring(&text));
        }
    }

    None
}

/// Cleans a comment string by removing comment markers and leading whitespace.
fn clean_comment(text: &str) -> String {
    let cleaned: Vec<String> = text
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            let stripped = strip_comment_marker(trimmed);
            stripped.trim_end_matches("*/").trim().to_owned()
        })
        .collect();

    cleaned.join("\n").trim().to_owned()
}

/// Strips the leading comment marker from a trimmed line.
fn strip_comment_marker(trimmed: &str) -> &str {
    // Try longest prefixes first to avoid partial matches.
    if let Some(rest) = trimmed.strip_prefix("///") {
        return rest.trim_start();
    }
    if let Some(rest) = trimmed.strip_prefix("//!") {
        return rest.trim_start();
    }
    if let Some(rest) = trimmed.strip_prefix("//") {
        return rest.trim_start();
    }
    if let Some(rest) = trimmed.strip_prefix("/**") {
        return rest.trim_start();
    }
    if let Some(rest) = trimmed.strip_prefix("/*") {
        return rest.trim_start();
    }
    if trimmed.starts_with("*/") {
        return "";
    }
    if let Some(rest) = trimmed.strip_prefix('*') {
        return rest.trim_start();
    }
    if let Some(rest) = trimmed.strip_prefix('#') {
        return rest.trim_start();
    }
    trimmed
}

/// Cleans a Python docstring by removing triple-quote delimiters.
fn clean_python_docstring(text: &str) -> String {
    let trimmed = text.trim();
    let stripped = trimmed
        .strip_prefix("\"\"\"")
        .or_else(|| trimmed.strip_prefix("'''"))
        .unwrap_or(trimmed);
    let stripped = stripped
        .strip_suffix("\"\"\"")
        .or_else(|| stripped.strip_suffix("'''"))
        .unwrap_or(stripped);
    stripped.trim().to_owned()
}

/// Detects whether a symbol is exported/public based on language conventions.
fn detect_export(node: Node<'_>, name: &str, lang_name: &str) -> bool {
    match lang_name {
        "typescript" | "tsx" | "javascript" => {
            node.parent()
                .is_some_and(|p| p.kind() == "export_statement")
                || node.kind() == "export_statement"
        }
        "python" => !name.starts_with('_'),
        "rust" => has_visibility_modifier(node, "visibility_modifier"),
        "go" => name.starts_with(|c: char| c.is_ascii_uppercase()),
        "java" | "csharp" => has_modifier_keyword(node, "public"),
        _ => is_top_level(node),
    }
}

/// Checks if a node has a `visibility_modifier` child (Rust `pub`).
fn has_visibility_modifier(node: Node<'_>, modifier_type: &str) -> bool {
    for i in 0..u32_child_count(node) {
        if let Some(child) = node.child(i)
            && child.kind() == modifier_type
        {
            return true;
        }
    }
    false
}

/// Checks if a node has a modifier containing a specific keyword (Java/C# `public`).
fn has_modifier_keyword(node: Node<'_>, keyword: &str) -> bool {
    for i in 0..u32_child_count(node) {
        if let Some(child) = node.child(i) {
            let kind = child.kind();
            if kind == keyword {
                return true;
            }
            if kind == "modifiers" || kind == "modifier" {
                for j in 0..u32_child_count(child) {
                    if let Some(mod_child) = child.child(j)
                        && mod_child.kind() == keyword
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Returns `true` if the node is at the top level (direct child of the root or
/// program node).
fn is_top_level(node: Node<'_>) -> bool {
    node.parent().is_none()
        || node
            .parent()
            .is_some_and(|p| matches!(p.kind(), "program" | "source_file" | "translation_unit"))
}

/// Cleans an import name by stripping quotes and path prefixes.
fn clean_import_name(raw: &str) -> String {
    let trimmed = raw.trim();
    let stripped = trimmed
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| {
            trimmed
                .strip_prefix('\'')
                .and_then(|s| s.strip_suffix('\''))
        })
        .unwrap_or(trimmed);

    let name = stripped.rsplit('/').next().unwrap_or(stripped);
    name.trim_matches(|c: char| c == '"' || c == '\'' || c == '<' || c == '>')
        .to_owned()
}

/// Collapses multiple whitespace characters into single spaces.
fn collapse_whitespace(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut prev_ws = false;

    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_ws {
                result.push(' ');
            }
            prev_ws = true;
        } else {
            result.push(c);
            prev_ws = false;
        }
    }

    result.trim().to_owned()
}

/// Returns the child count of a tree-sitter node as `u32`.
///
/// tree-sitter's `Node::child()` accepts `u32` but `child_count()` returns
/// `usize`. Truncation is safe because tree-sitter ASTs never approach
/// `u32::MAX` children.
#[allow(clippy::cast_possible_truncation)] // tree-sitter AST nodes cannot have >4B children
fn u32_child_count(node: Node<'_>) -> u32 {
    node.child_count() as u32
}
