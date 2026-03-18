//! C language configuration and tree-sitter queries.

use super::LanguageConfig;

/// C (`.c`, `.h`) language configuration.
pub static C: LanguageConfig = LanguageConfig {
    extensions: &[".c", ".h"],
    language: tree_sitter_c::LANGUAGE,
    name: "c",
    symbol_query: SYMBOL_QUERY,
    import_query: IMPORT_QUERY,
    call_query: CALL_QUERY,
};

const SYMBOL_QUERY: &str = "
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name)) @definition

(declaration
  declarator: (function_declarator
    declarator: (identifier) @name)) @definition

(struct_specifier
  name: (type_identifier) @name) @definition

(enum_specifier
  name: (type_identifier) @name) @definition

(type_definition
  declarator: (type_identifier) @name) @definition
";

const IMPORT_QUERY: &str = "
(preproc_include
  path: (string_literal) @source)

(preproc_include
  path: (system_lib_string) @source)
";

const CALL_QUERY: &str = "
(call_expression
  function: (identifier) @function) @call

(call_expression
  function: (field_expression
    field: (field_identifier) @function)) @call
";
