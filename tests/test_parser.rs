//! Tests for the language registry, grammar parsing, and query compilation.

use ndxr::languages;

// --- Registry tests ---

#[test]
fn registry_returns_none_for_unknown() {
    assert!(languages::get_language_config(".xyz").is_none());
}

#[test]
fn all_languages_returns_14_configs() {
    assert_eq!(languages::all_languages().len(), 14);
}

#[test]
fn all_extensions_contains_expected() {
    let exts = languages::all_extensions();
    assert!(exts.contains(&".ts"));
    assert!(exts.contains(&".py"));
    assert!(exts.contains(&".rs"));
    assert!(exts.contains(&".go"));
    assert!(exts.contains(&".java"));
    assert!(exts.contains(&".cs"));
    assert!(exts.contains(&".rb"));
    assert!(exts.contains(&".sh"));
    assert!(exts.contains(&".php"));
    assert!(exts.contains(&".zig"));
    assert!(exts.contains(&".c"));
    assert!(exts.contains(&".cpp"));
}

// --- TypeScript ---

#[test]
fn typescript_resolves() {
    let config = languages::get_language_config(".ts").unwrap();
    assert_eq!(config.name, "typescript");
}

#[test]
fn typescript_grammar_parses() {
    let config = languages::get_language_config(".ts").unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&config.language.into()).unwrap();
    let tree = parser.parse("function foo() {}", None).unwrap();
    assert!(!tree.root_node().has_error());
}

#[test]
fn typescript_queries_compile() {
    let config = languages::get_language_config(".ts").unwrap();
    let lang: tree_sitter::Language = config.language.into();
    tree_sitter::Query::new(&lang, config.symbol_query).expect("symbol query should compile");
    tree_sitter::Query::new(&lang, config.import_query).expect("import query should compile");
    tree_sitter::Query::new(&lang, config.call_query).expect("call query should compile");
}

// --- TSX ---

#[test]
fn tsx_resolves() {
    let config = languages::get_language_config(".tsx").unwrap();
    assert_eq!(config.name, "tsx");
}

#[test]
fn tsx_grammar_parses() {
    let config = languages::get_language_config(".tsx").unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&config.language.into()).unwrap();
    let tree = parser
        .parse("function Foo() { return <div/>; }", None)
        .unwrap();
    assert!(!tree.root_node().has_error());
}

#[test]
fn tsx_queries_compile() {
    let config = languages::get_language_config(".tsx").unwrap();
    let lang: tree_sitter::Language = config.language.into();
    tree_sitter::Query::new(&lang, config.symbol_query).expect("symbol query should compile");
    tree_sitter::Query::new(&lang, config.import_query).expect("import query should compile");
    tree_sitter::Query::new(&lang, config.call_query).expect("call query should compile");
}

// --- JavaScript ---

#[test]
fn javascript_resolves() {
    let config = languages::get_language_config(".js").unwrap();
    assert_eq!(config.name, "javascript");
}

#[test]
fn javascript_resolves_jsx() {
    assert!(languages::get_language_config(".jsx").is_some());
}

#[test]
fn javascript_resolves_mjs() {
    assert!(languages::get_language_config(".mjs").is_some());
}

#[test]
fn javascript_resolves_cjs() {
    assert!(languages::get_language_config(".cjs").is_some());
}

#[test]
fn javascript_grammar_parses() {
    let config = languages::get_language_config(".js").unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&config.language.into()).unwrap();
    let tree = parser.parse("function foo() {}", None).unwrap();
    assert!(!tree.root_node().has_error());
}

#[test]
fn javascript_queries_compile() {
    let config = languages::get_language_config(".js").unwrap();
    let lang: tree_sitter::Language = config.language.into();
    tree_sitter::Query::new(&lang, config.symbol_query).expect("symbol query should compile");
    tree_sitter::Query::new(&lang, config.import_query).expect("import query should compile");
    tree_sitter::Query::new(&lang, config.call_query).expect("call query should compile");
}

// --- Python ---

#[test]
fn python_resolves() {
    let config = languages::get_language_config(".py").unwrap();
    assert_eq!(config.name, "python");
}

#[test]
fn python_resolves_pyi() {
    assert!(languages::get_language_config(".pyi").is_some());
}

#[test]
fn python_grammar_parses() {
    let config = languages::get_language_config(".py").unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&config.language.into()).unwrap();
    let tree = parser.parse("def foo(): pass", None).unwrap();
    assert!(!tree.root_node().has_error());
}

#[test]
fn python_queries_compile() {
    let config = languages::get_language_config(".py").unwrap();
    let lang: tree_sitter::Language = config.language.into();
    tree_sitter::Query::new(&lang, config.symbol_query).expect("symbol query should compile");
    tree_sitter::Query::new(&lang, config.import_query).expect("import query should compile");
    tree_sitter::Query::new(&lang, config.call_query).expect("call query should compile");
}

// --- Go ---

#[test]
fn go_resolves() {
    let config = languages::get_language_config(".go").unwrap();
    assert_eq!(config.name, "go");
}

#[test]
fn go_grammar_parses() {
    let config = languages::get_language_config(".go").unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&config.language.into()).unwrap();
    let tree = parser.parse("package main\nfunc Foo() {}", None).unwrap();
    assert!(!tree.root_node().has_error());
}

#[test]
fn go_queries_compile() {
    let config = languages::get_language_config(".go").unwrap();
    let lang: tree_sitter::Language = config.language.into();
    tree_sitter::Query::new(&lang, config.symbol_query).expect("symbol query should compile");
    tree_sitter::Query::new(&lang, config.import_query).expect("import query should compile");
    tree_sitter::Query::new(&lang, config.call_query).expect("call query should compile");
}

// --- Rust ---

#[test]
fn rust_resolves() {
    let config = languages::get_language_config(".rs").unwrap();
    assert_eq!(config.name, "rust");
}

#[test]
fn rust_grammar_parses() {
    let config = languages::get_language_config(".rs").unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&config.language.into()).unwrap();
    let tree = parser.parse("fn foo() {}", None).unwrap();
    assert!(!tree.root_node().has_error());
}

#[test]
fn rust_queries_compile() {
    let config = languages::get_language_config(".rs").unwrap();
    let lang: tree_sitter::Language = config.language.into();
    tree_sitter::Query::new(&lang, config.symbol_query).expect("symbol query should compile");
    tree_sitter::Query::new(&lang, config.import_query).expect("import query should compile");
    tree_sitter::Query::new(&lang, config.call_query).expect("call query should compile");
}

// --- Java ---

#[test]
fn java_resolves() {
    let config = languages::get_language_config(".java").unwrap();
    assert_eq!(config.name, "java");
}

#[test]
fn java_grammar_parses() {
    let config = languages::get_language_config(".java").unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&config.language.into()).unwrap();
    let tree = parser.parse("class Foo { void bar() {} }", None).unwrap();
    assert!(!tree.root_node().has_error());
}

#[test]
fn java_queries_compile() {
    let config = languages::get_language_config(".java").unwrap();
    let lang: tree_sitter::Language = config.language.into();
    tree_sitter::Query::new(&lang, config.symbol_query).expect("symbol query should compile");
    tree_sitter::Query::new(&lang, config.import_query).expect("import query should compile");
    tree_sitter::Query::new(&lang, config.call_query).expect("call query should compile");
}

// --- C# ---

#[test]
fn csharp_resolves() {
    let config = languages::get_language_config(".cs").unwrap();
    assert_eq!(config.name, "csharp");
}

#[test]
fn csharp_grammar_parses() {
    let config = languages::get_language_config(".cs").unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&config.language.into()).unwrap();
    let tree = parser.parse("class Foo { void Bar() {} }", None).unwrap();
    assert!(!tree.root_node().has_error());
}

#[test]
fn csharp_queries_compile() {
    let config = languages::get_language_config(".cs").unwrap();
    let lang: tree_sitter::Language = config.language.into();
    tree_sitter::Query::new(&lang, config.symbol_query).expect("symbol query should compile");
    tree_sitter::Query::new(&lang, config.import_query).expect("import query should compile");
    tree_sitter::Query::new(&lang, config.call_query).expect("call query should compile");
}

// --- Ruby ---

#[test]
fn ruby_resolves() {
    let config = languages::get_language_config(".rb").unwrap();
    assert_eq!(config.name, "ruby");
}

#[test]
fn ruby_grammar_parses() {
    let config = languages::get_language_config(".rb").unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&config.language.into()).unwrap();
    let tree = parser.parse("def foo; end", None).unwrap();
    assert!(!tree.root_node().has_error());
}

#[test]
fn ruby_queries_compile() {
    let config = languages::get_language_config(".rb").unwrap();
    let lang: tree_sitter::Language = config.language.into();
    tree_sitter::Query::new(&lang, config.symbol_query).expect("symbol query should compile");
    tree_sitter::Query::new(&lang, config.import_query).expect("import query should compile");
    tree_sitter::Query::new(&lang, config.call_query).expect("call query should compile");
}

// --- Bash ---

#[test]
fn bash_resolves() {
    let config = languages::get_language_config(".sh").unwrap();
    assert_eq!(config.name, "bash");
}

#[test]
fn bash_resolves_bash_ext() {
    assert!(languages::get_language_config(".bash").is_some());
}

#[test]
fn bash_grammar_parses() {
    let config = languages::get_language_config(".sh").unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&config.language.into()).unwrap();
    let tree = parser.parse("foo() { echo; }", None).unwrap();
    assert!(!tree.root_node().has_error());
}

#[test]
fn bash_queries_compile() {
    let config = languages::get_language_config(".sh").unwrap();
    let lang: tree_sitter::Language = config.language.into();
    tree_sitter::Query::new(&lang, config.symbol_query).expect("symbol query should compile");
    tree_sitter::Query::new(&lang, config.import_query).expect("import query should compile");
    tree_sitter::Query::new(&lang, config.call_query).expect("call query should compile");
}

// --- PHP ---

#[test]
fn php_resolves() {
    let config = languages::get_language_config(".php").unwrap();
    assert_eq!(config.name, "php");
}

#[test]
fn php_grammar_parses() {
    let config = languages::get_language_config(".php").unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&config.language.into()).unwrap();
    let tree = parser.parse("function foo() {}", None).unwrap();
    assert!(!tree.root_node().has_error());
}

#[test]
fn php_queries_compile() {
    let config = languages::get_language_config(".php").unwrap();
    let lang: tree_sitter::Language = config.language.into();
    tree_sitter::Query::new(&lang, config.symbol_query).expect("symbol query should compile");
    tree_sitter::Query::new(&lang, config.import_query).expect("import query should compile");
    tree_sitter::Query::new(&lang, config.call_query).expect("call query should compile");
}

// --- Zig ---

#[test]
fn zig_resolves() {
    let config = languages::get_language_config(".zig").unwrap();
    assert_eq!(config.name, "zig");
}

#[test]
fn zig_grammar_parses() {
    let config = languages::get_language_config(".zig").unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&config.language.into()).unwrap();
    let tree = parser.parse("fn foo() void {}", None).unwrap();
    assert!(!tree.root_node().has_error());
}

#[test]
fn zig_queries_compile() {
    let config = languages::get_language_config(".zig").unwrap();
    let lang: tree_sitter::Language = config.language.into();
    tree_sitter::Query::new(&lang, config.symbol_query).expect("symbol query should compile");
    tree_sitter::Query::new(&lang, config.import_query).expect("import query should compile");
    tree_sitter::Query::new(&lang, config.call_query).expect("call query should compile");
}

// --- C ---

#[test]
fn c_resolves() {
    let config = languages::get_language_config(".c").unwrap();
    assert_eq!(config.name, "c");
}

#[test]
fn c_resolves_h() {
    assert!(languages::get_language_config(".h").is_some());
}

#[test]
fn c_grammar_parses() {
    let config = languages::get_language_config(".c").unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&config.language.into()).unwrap();
    let tree = parser.parse("int foo() { return 0; }", None).unwrap();
    assert!(!tree.root_node().has_error());
}

#[test]
fn c_queries_compile() {
    let config = languages::get_language_config(".c").unwrap();
    let lang: tree_sitter::Language = config.language.into();
    tree_sitter::Query::new(&lang, config.symbol_query).expect("symbol query should compile");
    tree_sitter::Query::new(&lang, config.import_query).expect("import query should compile");
    tree_sitter::Query::new(&lang, config.call_query).expect("call query should compile");
}

// --- C++ ---

#[test]
fn cpp_resolves() {
    let config = languages::get_language_config(".cpp").unwrap();
    assert_eq!(config.name, "cpp");
}

#[test]
fn cpp_resolves_cc() {
    assert!(languages::get_language_config(".cc").is_some());
}

#[test]
fn cpp_resolves_cxx() {
    assert!(languages::get_language_config(".cxx").is_some());
}

#[test]
fn cpp_resolves_hpp() {
    assert!(languages::get_language_config(".hpp").is_some());
}

#[test]
fn cpp_resolves_hh() {
    assert!(languages::get_language_config(".hh").is_some());
}

#[test]
fn cpp_resolves_hxx() {
    assert!(languages::get_language_config(".hxx").is_some());
}

#[test]
fn cpp_grammar_parses() {
    let config = languages::get_language_config(".cpp").unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&config.language.into()).unwrap();
    let tree = parser.parse("class Foo { void bar(); };", None).unwrap();
    assert!(!tree.root_node().has_error());
}

#[test]
fn cpp_queries_compile() {
    let config = languages::get_language_config(".cpp").unwrap();
    let lang: tree_sitter::Language = config.language.into();
    tree_sitter::Query::new(&lang, config.symbol_query).expect("symbol query should compile");
    tree_sitter::Query::new(&lang, config.import_query).expect("import query should compile");
    tree_sitter::Query::new(&lang, config.call_query).expect("call query should compile");
}

// --- Symbol extraction tests for multiple languages ---

#[test]
fn extracts_java_class_and_methods() {
    let source =
        "public class UserService {\n    public User findById(String id) { return null; }\n}";
    let config = languages::get_language_config(".java").unwrap();
    let symbols =
        ndxr::indexer::symbols::extract_symbols("UserService.java", source, config).unwrap();
    assert!(
        symbols.iter().any(|s| s.name == "UserService"),
        "should find Java class"
    );
    assert!(
        symbols.iter().any(|s| s.name == "findById"),
        "should find Java method"
    );
}

#[test]
fn extracts_ruby_class_and_methods() {
    let source = "class UserService\n  def find_by_id(id)\n    nil\n  end\nend";
    let config = languages::get_language_config(".rb").unwrap();
    let symbols = ndxr::indexer::symbols::extract_symbols("service.rb", source, config).unwrap();
    assert!(
        symbols.iter().any(|s| s.name == "UserService"),
        "should find Ruby class"
    );
    assert!(
        symbols.iter().any(|s| s.name == "find_by_id"),
        "should find Ruby method"
    );
}

#[test]
fn extracts_c_functions() {
    let source = "int validate(const char* input) { return 1; }\nvoid process(int* data) {}";
    let config = languages::get_language_config(".c").unwrap();
    let symbols = ndxr::indexer::symbols::extract_symbols("validate.c", source, config).unwrap();
    assert!(
        symbols.iter().any(|s| s.name == "validate"),
        "should find C function"
    );
    assert!(
        symbols.iter().any(|s| s.name == "process"),
        "should find C function"
    );
}

#[test]
fn extracts_cpp_class() {
    let source = "class Engine {\npublic:\n    void start();\n    void stop();\n};";
    let config = languages::get_language_config(".cpp").unwrap();
    let symbols = ndxr::indexer::symbols::extract_symbols("engine.cpp", source, config).unwrap();
    assert!(
        symbols.iter().any(|s| s.name == "Engine"),
        "should find C++ class"
    );
}

#[test]
fn extracts_csharp_class_and_methods() {
    let source =
        "public class UserService {\n    public User FindById(string id) { return null; }\n}";
    let config = languages::get_language_config(".cs").unwrap();
    let symbols = ndxr::indexer::symbols::extract_symbols("Service.cs", source, config).unwrap();
    assert!(
        symbols.iter().any(|s| s.name == "UserService"),
        "should find C# class"
    );
}

#[test]
fn extracts_bash_function() {
    let source = "validate_input() {\n    echo \"validating\"\n}";
    let config = languages::get_language_config(".sh").unwrap();
    let symbols = ndxr::indexer::symbols::extract_symbols("script.sh", source, config).unwrap();
    assert!(
        symbols.iter().any(|s| s.name == "validate_input"),
        "should find Bash function"
    );
}

#[test]
fn extracts_php_class_and_function() {
    let source =
        "<?php\nclass UserService {\n    public function findById($id) { return null; }\n}";
    let config = languages::get_language_config(".php").unwrap();
    let symbols = ndxr::indexer::symbols::extract_symbols("Service.php", source, config).unwrap();
    assert!(
        symbols.iter().any(|s| s.name == "UserService"),
        "should find PHP class"
    );
}

#[test]
fn extracts_go_struct_and_function() {
    let source = "package main\n\ntype Config struct {\n    Name string\n}\n\nfunc NewConfig(name string) *Config {\n    return &Config{Name: name}\n}";
    let config = languages::get_language_config(".go").unwrap();
    let symbols = ndxr::indexer::symbols::extract_symbols("config.go", source, config).unwrap();
    assert!(
        symbols.iter().any(|s| s.name == "NewConfig"),
        "should find Go function"
    );
}
