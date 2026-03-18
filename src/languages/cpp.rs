//! C++ language configuration and tree-sitter queries.

use super::LanguageConfig;

/// C++ (`.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh`, `.hxx`) language configuration.
pub static CPP: LanguageConfig = LanguageConfig {
    extensions: &[".cpp", ".cc", ".cxx", ".hpp", ".hh", ".hxx"],
    language: tree_sitter_cpp::LANGUAGE,
    name: "cpp",
    symbol_query: SYMBOL_QUERY,
    import_query: IMPORT_QUERY,
    call_query: CALL_QUERY,
};

const SYMBOL_QUERY: &str = "
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name)) @definition

(function_definition
  declarator: (function_declarator
    declarator: (qualified_identifier
      name: (identifier) @name))) @definition

(declaration
  declarator: (function_declarator
    declarator: (identifier) @name)) @definition

(class_specifier
  name: (type_identifier) @name) @definition

(struct_specifier
  name: (type_identifier) @name) @definition

(enum_specifier
  name: (type_identifier) @name) @definition

(namespace_definition
  name: (namespace_identifier) @name) @definition
";

const IMPORT_QUERY: &str = "
(preproc_include
  path: (string_literal) @source)

(preproc_include
  path: (system_lib_string) @source)

(using_declaration
  (_) @source)
";

const CALL_QUERY: &str = "
(call_expression
  function: (identifier) @function) @call

(call_expression
  function: (qualified_identifier
    name: (identifier) @function)) @call

(call_expression
  function: (field_expression
    field: (field_identifier) @function)) @call
";
