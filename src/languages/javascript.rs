//! JavaScript language configuration and tree-sitter queries.

use super::LanguageConfig;

/// JavaScript (`.js`, `.jsx`, `.mjs`, `.cjs`) language configuration.
pub static JAVASCRIPT: LanguageConfig = LanguageConfig {
    extensions: &[".js", ".jsx", ".mjs", ".cjs"],
    language: tree_sitter_javascript::LANGUAGE,
    name: "javascript",
    symbol_query: SYMBOL_QUERY,
    import_query: IMPORT_QUERY,
    call_query: CALL_QUERY,
};

const SYMBOL_QUERY: &str = "
(function_declaration
  name: (identifier) @name) @definition

(export_statement
  declaration: (function_declaration
    name: (identifier) @name) @definition)

(class_declaration
  name: (identifier) @name) @definition

(method_definition
  name: (property_identifier) @name) @definition

(export_statement
  declaration: (lexical_declaration
    (variable_declarator
      name: (identifier) @name))) @definition
";

const IMPORT_QUERY: &str = "
(import_statement
  source: (string (string_fragment) @source))
";

const CALL_QUERY: &str = "
(call_expression
  function: (identifier) @function) @call

(call_expression
  function: (member_expression
    property: (property_identifier) @function)) @call
";
