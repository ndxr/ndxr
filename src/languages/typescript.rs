//! TypeScript language configuration and tree-sitter queries.

use super::LanguageConfig;

/// TypeScript (`.ts`) language configuration.
pub static TYPESCRIPT: LanguageConfig = LanguageConfig {
    extensions: &[".ts"],
    language: tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
    name: "typescript",
    symbol_query: SYMBOL_QUERY,
    import_query: IMPORT_QUERY,
    call_query: CALL_QUERY,
};

/// TSX (`.tsx`) language configuration.
pub static TSX: LanguageConfig = LanguageConfig {
    extensions: &[".tsx"],
    language: tree_sitter_typescript::LANGUAGE_TSX,
    name: "tsx",
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
  name: (type_identifier) @name) @definition

(method_definition
  name: (property_identifier) @name) @definition

(interface_declaration
  name: (type_identifier) @name) @definition

(type_alias_declaration
  name: (type_identifier) @name) @definition

(enum_declaration
  name: (identifier) @name) @definition

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
