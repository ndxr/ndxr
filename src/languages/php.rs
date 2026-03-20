//! PHP language configuration and tree-sitter queries.

use super::LanguageConfig;

/// PHP (`.php`) language configuration.
///
/// Uses the PHP-only grammar variant which parses PHP code without requiring
/// the `<?php` opening tag.
pub static PHP: LanguageConfig = LanguageConfig {
    extensions: &[".php"],
    language: tree_sitter_php::LANGUAGE_PHP_ONLY,
    name: "php",
    symbol_query: SYMBOL_QUERY,
    import_query: IMPORT_QUERY,
    call_query: CALL_QUERY,
};

const SYMBOL_QUERY: &str = "
(function_definition
  name: (name) @name) @definition

(class_declaration
  name: (name) @name) @definition

(method_declaration
  name: (name) @name) @definition

(interface_declaration
  name: (name) @name) @definition

(trait_declaration
  name: (name) @name) @definition
";

const IMPORT_QUERY: &str = "
(namespace_use_declaration
  (namespace_use_clause
    (qualified_name) @source))

(namespace_use_declaration
  (namespace_name) @source)

(expression_statement
  (require_expression
    (string (string_content) @source)))

(expression_statement
  (require_once_expression
    (string (string_content) @source)))

(expression_statement
  (include_expression
    (string (string_content) @source)))

(expression_statement
  (include_once_expression
    (string (string_content) @source)))
";

const CALL_QUERY: &str = "
(function_call_expression
  function: (name) @function) @call

(function_call_expression
  function: (qualified_name) @function) @call

(member_call_expression
  name: (name) @function) @call
";
