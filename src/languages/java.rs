//! Java language configuration and tree-sitter queries.

use super::LanguageConfig;

/// Java (`.java`) language configuration.
pub static JAVA: LanguageConfig = LanguageConfig {
    extensions: &[".java"],
    language: tree_sitter_java::LANGUAGE,
    name: "java",
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

(constructor_declaration
  name: (identifier) @name) @definition
";

const IMPORT_QUERY: &str = "
(import_declaration
  (scoped_identifier) @source)
";

const CALL_QUERY: &str = "
(method_invocation
  name: (identifier) @function) @call
";
