//! Ruby language configuration and tree-sitter queries.

use super::LanguageConfig;

/// Ruby (`.rb`) language configuration.
pub static RUBY: LanguageConfig = LanguageConfig {
    extensions: &[".rb"],
    language: tree_sitter_ruby::LANGUAGE,
    name: "ruby",
    symbol_query: SYMBOL_QUERY,
    import_query: IMPORT_QUERY,
    call_query: CALL_QUERY,
};

const SYMBOL_QUERY: &str = "
(method
  name: (identifier) @name) @definition

(singleton_method
  name: (identifier) @name) @definition

(class
  name: (constant) @name) @definition

(module
  name: (constant) @name) @definition
";

const IMPORT_QUERY: &str = "
(call
  method: (identifier) @_method
  arguments: (argument_list
    (string (string_content) @source))
  (#match? @_method \"^(require|require_relative)$\"))
";

const CALL_QUERY: &str = "
(call
  method: (identifier) @function) @call
";
