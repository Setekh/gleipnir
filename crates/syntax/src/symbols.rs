//! Finds the declarations in a file: types, functions, constants and the like. This is what lets
//! symbol search work before, or without, a language server.

use crate::Language;
use std::collections::HashMap;
use std::sync::OnceLock;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, Query, QueryCursor};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SymbolKind {
    Class,
    Interface,
    Enum,
    Function,
    Method,
    Constant,
    Variable,
    Module,
    Macro,
    EnumMember,
}

impl SymbolKind {
    /// Whether this is a type, which is what IntelliJ's "Classes" search lists.
    pub fn is_type(self) -> bool {
        matches!(self, Self::Class | Self::Interface | Self::Enum)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Class => "class",
            Self::Interface => "interface",
            Self::Enum => "enum",
            Self::Function => "function",
            Self::Method => "method",
            Self::Constant => "constant",
            Self::Variable => "variable",
            Self::Module => "module",
            Self::Macro => "macro",
            Self::EnumMember => "enum member",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// What it's declared in, such as the type a method belongs to.
    pub container: Option<String>,
    /// Where the name is, zero-based, with the column in bytes.
    pub line: u32,
    pub column: u32,
}

/// Each pattern captures the declaration as `@<kind>` and its name as `@name`. Data and markup
/// languages declare nothing worth searching for: Markdown's headings would bury a project's code
/// in every README's "Usage", and Marksman gives a file's structure its headings.
fn query_source(language: Language) -> Option<&'static str> {
    Some(match language {
        Language::Rust => {
            r#"
            (struct_item name: (type_identifier) @name) @class
            (union_item name: (type_identifier) @name) @class
            (type_item name: (type_identifier) @name) @class
            (enum_item name: (type_identifier) @name) @enum
            (enum_variant name: (identifier) @name) @enum_member
            (trait_item name: (type_identifier) @name) @interface
            (function_item name: (identifier) @name) @function
            (function_signature_item name: (identifier) @name) @function
            (mod_item name: (identifier) @name) @module
            (const_item name: (identifier) @name) @constant
            (static_item name: (identifier) @name) @constant
            (macro_definition name: (identifier) @name) @macro
            "#
        }
        Language::Go => {
            r#"
            (function_declaration name: (identifier) @name) @function
            (method_declaration name: (field_identifier) @name) @method
            (type_spec name: (type_identifier) @name) @class
            (method_elem name: (field_identifier) @name) @method
            (const_spec name: (identifier) @name) @constant
            (var_spec name: (identifier) @name) @variable
            "#
        }
        Language::Odin => {
            r#"
            (procedure_declaration . (identifier) @name) @function
            (overloaded_procedure_declaration . (identifier) @name) @function
            (struct_declaration . (identifier) @name) @class
            (union_declaration . (identifier) @name) @class
            (bit_field_declaration . (identifier) @name) @class
            (enum_declaration . (identifier) @name) @enum
            (const_declaration . (identifier) @name) @constant
            (variable_declaration . (identifier) @name) @variable
            "#
        }
        Language::JavaScript => JS_QUERY,
        Language::TypeScript | Language::Tsx => TS_QUERY,
        Language::CSharp => CSHARP_QUERY,
        Language::Json | Language::Html | Language::Css | Language::Markdown => return None,
    })
}

const CSHARP_QUERY: &str = r#"
    (class_declaration name: (identifier) @name) @class
    (record_declaration name: (identifier) @name) @class
    (struct_declaration name: (identifier) @name) @class
    (delegate_declaration name: (identifier) @name) @class
    (interface_declaration name: (identifier) @name) @interface
    (enum_declaration name: (identifier) @name) @enum
    (enum_member_declaration name: (identifier) @name) @enum_member
    (method_declaration name: (identifier) @name) @method
    (constructor_declaration name: (identifier) @name) @method
    (property_declaration name: (identifier) @name) @variable
    (event_declaration name: (identifier) @name) @variable
    (event_field_declaration
        (variable_declaration (variable_declarator name: (identifier) @name))) @variable
    (namespace_declaration name: (_) @name) @module
    (file_scoped_namespace_declaration name: (_) @name) @module
"#;

/// JavaScript's grammar has no type declarations, so it gets a subset of TypeScript's patterns.
const JS_QUERY: &str = r#"
    (class_declaration name: (_) @name) @class
    (function_declaration name: (_) @name) @function
    (generator_function_declaration name: (_) @name) @function
    (method_definition name: (_) @name) @method
    (variable_declarator name: (identifier) @name) @variable
"#;

const TS_QUERY: &str = r#"
    (class_declaration name: (_) @name) @class
    (abstract_class_declaration name: (_) @name) @class
    (interface_declaration name: (_) @name) @interface
    (type_alias_declaration name: (_) @name) @class
    (enum_declaration name: (_) @name) @enum
    (function_declaration name: (_) @name) @function
    (generator_function_declaration name: (_) @name) @function
    (function_signature name: (_) @name) @function
    (method_definition name: (_) @name) @method
    (method_signature name: (_) @name) @method
    (abstract_method_signature name: (_) @name) @method
    (internal_module name: (_) @name) @module
    (variable_declarator name: (identifier) @name) @variable
"#;

struct SymbolQuery {
    query: Query,
    name_capture: u32,
}

fn symbol_query(language: Language) -> Option<&'static SymbolQuery> {
    static QUERIES: OnceLock<HashMap<Language, SymbolQuery>> = OnceLock::new();
    QUERIES
        .get_or_init(|| {
            Language::ALL
                .into_iter()
                .filter_map(|language| Some((language, query_source(language)?)))
                .map(|(language, source)| {
                    let query = Query::new(&language.grammar(), source)
                        .unwrap_or_else(|e| panic!("{language:?} symbol query: {e}"));
                    let name_capture = query
                        .capture_index_for_name("name")
                        .expect("every query captures @name");
                    (
                        language,
                        SymbolQuery {
                            query,
                            name_capture,
                        },
                    )
                })
                .collect()
        })
        .get(&language)
}

fn kind_for_capture(name: &str) -> Option<SymbolKind> {
    Some(match name {
        "class" => SymbolKind::Class,
        "interface" => SymbolKind::Interface,
        "enum" => SymbolKind::Enum,
        "enum_member" => SymbolKind::EnumMember,
        "function" => SymbolKind::Function,
        "method" => SymbolKind::Method,
        "constant" => SymbolKind::Constant,
        "variable" => SymbolKind::Variable,
        "module" => SymbolKind::Module,
        "macro" => SymbolKind::Macro,
        _ => return None,
    })
}

/// Node kinds that are a function's body, or the function itself. Variables declared inside one
/// are locals, which a project-wide search shouldn't be cluttered with.
const FUNCTION_SCOPES: &[&str] = &[
    "function_item",
    "function_declaration",
    "method_declaration",
    "func_literal",
    "procedure",
    "arrow_function",
    "function_expression",
    "function",
    "method_definition",
    "generator_function_declaration",
    "closure_expression",
    "constructor_declaration",
    "local_function_statement",
    "lambda_expression",
    "accessor_declaration",
];

fn inside_function(node: Node) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if FUNCTION_SCOPES.contains(&parent.kind()) {
            return true;
        }
        current = parent.parent();
    }
    false
}

fn text<'a>(node: Node, source: &'a str) -> &'a str {
    source.get(node.byte_range()).unwrap_or("")
}

/// What a declaration belongs to: the type of a Rust `impl`, a Go method's receiver, the class of a
/// JavaScript method, and so on. Only the nearest one, which is what a person would look for.
fn container_of(node: Node, source: &str) -> Option<String> {
    if node.kind() == "method_declaration"
        && let Some(receiver) = node.child_by_field_name("receiver")
    {
        // A Go method's container is its receiver's type, with any pointer stripped.
        let mut stack = vec![receiver];
        while let Some(n) = stack.pop() {
            if n.kind() == "type_identifier" {
                return Some(text(n, source).to_string());
            }
            let mut cursor = n.walk();
            stack.extend(n.children(&mut cursor));
        }
        return None;
    }
    let mut current = node.parent();
    while let Some(parent) = current {
        let name = match parent.kind() {
            "impl_item" => parent.child_by_field_name("type"),
            "trait_item"
            | "enum_item"
            | "mod_item"
            | "class_declaration"
            | "abstract_class_declaration"
            | "interface_declaration"
            | "internal_module"
            | "struct_declaration"
            | "record_declaration"
            | "enum_declaration"
            | "class" => parent.child_by_field_name("name"),
            "type_spec" => parent.child_by_field_name("name"),
            _ => None,
        };
        if let Some(name) = name {
            return Some(text(name, source).to_string());
        }
        current = parent.parent();
    }
    None
}

/// Parses a file and lists its declarations in order.
pub fn extract_symbols(language: Language, source: &str) -> Vec<Symbol> {
    let mut parser = Parser::new();
    if parser.set_language(&language.grammar()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let Some(symbol_query) = symbol_query(language) else {
        return Vec::new();
    };
    let query = &symbol_query.query;
    let capture_names = query.capture_names();

    let mut symbols = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
    while let Some(found) = matches.next() {
        let mut name_node = None;
        let mut declaration = None;
        for capture in found.captures {
            if capture.index == symbol_query.name_capture {
                name_node = Some(capture.node);
            } else if let Some(kind) = kind_for_capture(capture_names[capture.index as usize]) {
                declaration = Some((capture.node, kind));
            }
        }
        let (Some(name_node), Some((node, mut kind))) = (name_node, declaration) else {
            continue;
        };

        let is_value = matches!(kind, SymbolKind::Variable | SymbolKind::Constant);
        if is_value && inside_function(node) {
            continue;
        }
        let container = container_of(node, source);
        kind = refine_kind(language, node, kind, container.is_some());

        let start = name_node.start_position();
        symbols.push(Symbol {
            name: text(name_node, source).to_string(),
            kind,
            container,
            line: start.row as u32,
            column: start.column as u32,
        });
    }
    symbols.sort_by_key(|s| (s.line, s.column));
    symbols.dedup_by(|a, b| a.line == b.line && a.column == b.column);
    symbols
}

/// Distinctions the patterns can't make on their own.
fn refine_kind(language: Language, node: Node, kind: SymbolKind, contained: bool) -> SymbolKind {
    match (language, kind) {
        // A function inside an `impl` or trait is a method.
        (Language::Rust, SymbolKind::Function) if contained => {
            let mut current = node.parent();
            while let Some(parent) = current {
                if matches!(parent.kind(), "impl_item" | "trait_item") {
                    return SymbolKind::Method;
                }
                if parent.kind() == "mod_item" {
                    break;
                }
                current = parent.parent();
            }
            SymbolKind::Function
        }
        // A Go type is an interface if its definition is one.
        (Language::Go, SymbolKind::Class) => match node.child_by_field_name("type") {
            Some(t) if t.kind() == "interface_type" => SymbolKind::Interface,
            _ => SymbolKind::Class,
        },
        // `const f = () => ...` is how a lot of JavaScript declares functions.
        (Language::JavaScript | Language::TypeScript | Language::Tsx, SymbolKind::Variable) => {
            match node.child_by_field_name("value").map(|v| v.kind()) {
                Some("arrow_function" | "function_expression" | "function") => SymbolKind::Function,
                _ => match node.parent().and_then(|p| p.child(0)).map(|c| c.kind()) {
                    Some("const") => SymbolKind::Constant,
                    _ => SymbolKind::Variable,
                },
            }
        }
        // Odin declares procedures as constants, and a constant's value says which it is.
        (Language::Odin, SymbolKind::Constant) => {
            let mut cursor = node.walk();
            let has_type = node
                .children(&mut cursor)
                .any(|c| matches!(c.kind(), "bit_set_type" | "struct_type" | "enum_type"));
            if has_type {
                SymbolKind::Class
            } else {
                SymbolKind::Constant
            }
        }
        _ => kind,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(language: Language, source: &str) -> Vec<String> {
        extract_symbols(language, source)
            .into_iter()
            .map(|s| match s.container {
                Some(c) => format!("{} {}::{}", s.kind.label(), c, s.name),
                None => format!("{} {}", s.kind.label(), s.name),
            })
            .collect()
    }

    #[test]
    fn every_query_compiles() {
        for language in Language::ALL {
            symbol_query(language);
        }
    }

    #[test]
    fn rust_declarations() {
        let source = r#"
            mod net {
                pub struct Socket;
                pub enum State { Open, Closed }
                pub trait Read { fn read(&self); }
                impl Socket {
                    pub fn new() -> Self { const LOCAL: u8 = 1; Socket }
                }
                pub const MAX: u8 = 1;
                static COUNT: u8 = 2;
                macro_rules! log { () => {} }
                type Bytes = Vec<u8>;
                fn helper() {}
            }
        "#;
        assert_eq!(
            summary(Language::Rust, source),
            [
                "module net",
                "class net::Socket",
                "enum net::State",
                "enum member State::Open",
                "enum member State::Closed",
                "interface net::Read",
                "method Read::read",
                "method Socket::new",
                "constant net::MAX",
                "constant net::COUNT",
                "macro net::log",
                "class net::Bytes",
                "function net::helper",
            ]
        );
    }

    #[test]
    fn go_declarations() {
        let source = "package p\ntype Server struct{}\ntype Handler interface{ Serve() }\nfunc (s *Server) Start() { var local = 1 }\nfunc New() *Server { return nil }\nconst Port = 80\nvar Default = New()\n";
        assert_eq!(
            summary(Language::Go, source),
            [
                "class Server",
                "interface Handler",
                "method Handler::Serve",
                "method Server::Start",
                "function New",
                "constant Port",
                "variable Default",
            ]
        );
    }

    #[test]
    fn odin_declarations() {
        let source = "package p\nPoint :: struct { x: f32 }\nKind :: enum { A, B }\nShape :: union { Point }\nMAX :: 10\nadd :: proc(a: int) -> int { inner := 1\n return a }\nadd_all :: proc{add}\ncounter := 0\nFlags :: bit_set[Kind]\n";
        assert_eq!(
            summary(Language::Odin, source),
            [
                "class Point",
                "enum Kind",
                "class Shape",
                "constant MAX",
                "function add",
                "function add_all",
                "variable counter",
                "class Flags",
            ]
        );
    }

    #[test]
    fn typescript_declarations() {
        let source = "export class Api { fetch(): void { const local = 1; } }\ninterface User { name(): string }\ntype Id = string;\nenum Role { Admin }\nexport const handler = (a: number) => a;\nfunction main() {}\nconst LIMIT = 3;\nlet mutable = 1;\nabstract class Base {}\nnamespace Tools { export function inner() {} }\n";
        assert_eq!(
            summary(Language::TypeScript, source),
            [
                "class Api",
                "method Api::fetch",
                "interface User",
                "method User::name",
                "class Id",
                "enum Role",
                "function handler",
                "function main",
                "constant LIMIT",
                "variable mutable",
                "class Base",
                "module Tools",
                "function Tools::inner",
            ]
        );
    }

    #[test]
    fn csharp_declarations_belong_to_their_types() {
        let source = "namespace Game;\n\npublic interface IDamageable { void Hit(int amount); }\n\npublic class Player : MonoBehaviour, IDamageable\n{\n    public int Health { get; private set; }\n    public event System.Action Died;\n    public Player() {}\n    public void Hit(int amount) { void Local() {} }\n}\n\npublic struct Stats { public float Speed() => 1f; }\npublic enum Team { Red, Blue }\npublic record Score(int Value);\n";
        assert_eq!(
            summary(Language::CSharp, source),
            [
                "module Game",
                "interface IDamageable",
                "method IDamageable::Hit",
                "class Player",
                "variable Player::Health",
                "variable Player::Died",
                "method Player::Player",
                "method Player::Hit",
                "class Stats",
                "method Stats::Speed",
                "enum Team",
                "enum member Team::Red",
                "enum member Team::Blue",
                "class Score",
            ]
        );
    }

    #[test]
    fn markup_languages_have_no_symbols() {
        for (language, source) in [
            (Language::Markdown, "# Usage\n"),
            (Language::Json, "{ \"a\": 1 }"),
            (Language::Html, "<p id=\"a\"></p>"),
            (Language::Css, ".a { color: red; }"),
        ] {
            assert!(extract_symbols(language, source).is_empty(), "{language:?}");
        }
    }

    #[test]
    fn tsx_uses_the_typescript_patterns() {
        let source = "export const View = () => <div/>;\ninterface Props { a: string }\n";
        assert_eq!(
            summary(Language::Tsx, source),
            ["function View", "interface Props"]
        );
    }

    #[test]
    fn javascript_declarations() {
        let source = "class Store { get(key) { let x = 1; } }\nfunction* ids() {}\nexport const add = function(a, b) { return a + b };\nvar legacy = 1;\n";
        assert_eq!(
            summary(Language::JavaScript, source),
            [
                "class Store",
                "method Store::get",
                "function ids",
                "function add",
                "variable legacy"
            ]
        );
    }

    #[test]
    fn positions_point_at_the_name() {
        let symbols = extract_symbols(Language::Rust, "\n  pub fn  target() {}");
        assert_eq!((symbols[0].line, symbols[0].column), (1, 10));
    }

    #[test]
    fn broken_code_still_yields_what_it_can() {
        let symbols = extract_symbols(Language::Rust, "fn good() {}\nfn broken( {\nstruct After;");
        assert!(symbols.iter().any(|s| s.name == "good"));
    }

    #[test]
    fn types_are_what_the_classes_search_lists() {
        assert!(SymbolKind::Class.is_type());
        assert!(SymbolKind::Interface.is_type());
        assert!(!SymbolKind::Function.is_type());
    }
}
