//! Zig language configuration and tree-sitter queries.

use super::LanguageConfig;

/// Zig (`.zig`) language configuration.
pub static ZIG: LanguageConfig = LanguageConfig {
    extensions: &[".zig"],
    language: tree_sitter_zig::LANGUAGE,
    name: "zig",
    symbol_query: SYMBOL_QUERY,
    import_query: IMPORT_QUERY,
    call_query: CALL_QUERY,
};

const SYMBOL_QUERY: &str = "
(function_declaration
  name: (identifier) @name) @definition

(variable_declaration
  (identifier) @name) @definition

(test_declaration
  (string (string_content) @name)) @definition
";

const IMPORT_QUERY: &str = "
(builtin_function
  (builtin_identifier) @_fn
  (arguments
    (expression
      (string) @source))
  (#eq? @_fn \"@import\"))
";

const CALL_QUERY: &str = "
(call_expression
  function: (expression
    (identifier) @function)) @call

(call_expression
  function: (expression
    (field_expression
      (identifier) @function))) @call

(builtin_function
  (builtin_identifier) @function) @call
";
