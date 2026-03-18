//! Rust language configuration and tree-sitter queries.

use super::LanguageConfig;

/// Rust (`.rs`) language configuration.
pub static RUST: LanguageConfig = LanguageConfig {
    extensions: &[".rs"],
    language: tree_sitter_rust::LANGUAGE,
    name: "rust",
    symbol_query: SYMBOL_QUERY,
    import_query: IMPORT_QUERY,
    call_query: CALL_QUERY,
};

const SYMBOL_QUERY: &str = "
(function_item
  name: (identifier) @name) @definition

(struct_item
  name: (type_identifier) @name) @definition

(enum_item
  name: (type_identifier) @name) @definition

(trait_item
  name: (type_identifier) @name) @definition

(impl_item
  trait: (type_identifier) @name) @definition

(impl_item
  type: (type_identifier) @name) @definition

(type_item
  name: (type_identifier) @name) @definition

(mod_item
  name: (identifier) @name) @definition

(const_item
  name: (identifier) @name) @definition

(static_item
  name: (identifier) @name) @definition
";

const IMPORT_QUERY: &str = "
(use_declaration
  argument: (scoped_identifier) @source)

(use_declaration
  argument: (scoped_use_list) @source)

(use_declaration
  argument: (identifier) @source)

(use_declaration
  argument: (use_as_clause) @source)
";

const CALL_QUERY: &str = "
(call_expression
  function: (identifier) @function) @call

(call_expression
  function: (scoped_identifier
    name: (identifier) @function)) @call

(call_expression
  function: (field_expression
    field: (field_identifier) @function)) @call

(macro_invocation
  macro: (identifier) @function) @call

(macro_invocation
  macro: (scoped_identifier
    name: (identifier) @function)) @call
";
