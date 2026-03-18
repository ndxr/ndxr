//! Bash language configuration and tree-sitter queries.

use super::LanguageConfig;

/// Bash (`.sh`, `.bash`) language configuration.
pub static BASH: LanguageConfig = LanguageConfig {
    extensions: &[".sh", ".bash"],
    language: tree_sitter_bash::LANGUAGE,
    name: "bash",
    symbol_query: SYMBOL_QUERY,
    import_query: IMPORT_QUERY,
    call_query: CALL_QUERY,
};

const SYMBOL_QUERY: &str = "
(function_definition
  name: (word) @name) @definition
";

const IMPORT_QUERY: &str = "
(command
  name: (command_name) @_cmd
  argument: (word) @source
  (#match? @_cmd \"^(source|\\\\.)$\"))
";

const CALL_QUERY: &str = "
(command
  name: (command_name) @function) @call
";
