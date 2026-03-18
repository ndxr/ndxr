//! Python language configuration and tree-sitter queries.

use super::LanguageConfig;

/// Python (`.py`, `.pyi`) language configuration.
pub static PYTHON: LanguageConfig = LanguageConfig {
    extensions: &[".py", ".pyi"],
    language: tree_sitter_python::LANGUAGE,
    name: "python",
    symbol_query: SYMBOL_QUERY,
    import_query: IMPORT_QUERY,
    call_query: CALL_QUERY,
};

const SYMBOL_QUERY: &str = "
(function_definition
  name: (identifier) @name) @definition

(class_definition
  name: (identifier) @name) @definition

(decorated_definition
  definition: (function_definition
    name: (identifier) @name)) @definition

(decorated_definition
  definition: (class_definition
    name: (identifier) @name)) @definition
";

const IMPORT_QUERY: &str = "
(import_statement
  name: (dotted_name) @source)

(import_from_statement
  module_name: (dotted_name) @source)

(import_from_statement
  module_name: (relative_import) @source)
";

const CALL_QUERY: &str = "
(call
  function: (identifier) @function) @call

(call
  function: (attribute
    attribute: (identifier) @function)) @call
";
