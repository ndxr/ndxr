//! C# language configuration and tree-sitter queries.

use super::LanguageConfig;

/// C# (`.cs`) language configuration.
pub static CSHARP: LanguageConfig = LanguageConfig {
    extensions: &[".cs"],
    language: tree_sitter_c_sharp::LANGUAGE,
    name: "csharp",
    symbol_query: SYMBOL_QUERY,
    import_query: IMPORT_QUERY,
    call_query: CALL_QUERY,
};

const SYMBOL_QUERY: &str = "
(class_declaration
  name: (identifier) @name) @definition

(interface_declaration
  name: (identifier) @name) @definition

(method_declaration
  name: (identifier) @name) @definition

(enum_declaration
  name: (identifier) @name) @definition

(struct_declaration
  name: (identifier) @name) @definition

(constructor_declaration
  name: (identifier) @name) @definition
";

const IMPORT_QUERY: &str = "
(using_directive
  (qualified_name) @source)

(using_directive
  (identifier) @source)
";

const CALL_QUERY: &str = "
(invocation_expression
  function: (identifier) @function) @call

(invocation_expression
  function: (member_access_expression
    name: (identifier) @function)) @call
";
