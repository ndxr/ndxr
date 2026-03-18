//! Go language configuration and tree-sitter queries.

use super::LanguageConfig;

/// Go (`.go`) language configuration.
pub static GO: LanguageConfig = LanguageConfig {
    extensions: &[".go"],
    language: tree_sitter_go::LANGUAGE,
    name: "go",
    symbol_query: SYMBOL_QUERY,
    import_query: IMPORT_QUERY,
    call_query: CALL_QUERY,
};

const SYMBOL_QUERY: &str = "
(function_declaration
  name: (identifier) @name) @definition

(method_declaration
  name: (field_identifier) @name) @definition

(type_declaration
  (type_spec
    name: (type_identifier) @name)) @definition
";

const IMPORT_QUERY: &str = "
(import_spec
  path: (interpreted_string_literal) @source)
";

const CALL_QUERY: &str = "
(call_expression
  function: (identifier) @function) @call

(call_expression
  function: (selector_expression
    field: (field_identifier) @function)) @call
";
