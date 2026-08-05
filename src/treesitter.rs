//! Tree-sitter static analysis for extracting call and type-usage edges
//!
//! Operates purely on source text — no LSP, no network, no side effects.
//! Returns unresolved (line, name, edge_type) tuples; caller resolves to node IDs.

use crate::schema::{EdgeType, NodeType};
use std::collections::HashSet;
use std::path::Path;
use parking_lot::Mutex;
use tree_sitter::{Language, Parser, Query, QueryCursor};

thread_local! {
    static PARSER: Mutex<Parser> = Mutex::new(Parser::new());
}

/// A raw reference found in source code, not yet resolved to graph node IDs.
pub struct StaticRef {
    /// 0-indexed line in the file where this reference occurs.
    pub source_line: u32,
    pub target_name: String,
    pub edge_type: EdgeType,
}

/// Known-bUILTIN blocklist — only canonical std/lib calls that are unambiguously
/// language builtins. Domain names (map, filter, get, log, error, etc.) are NOT
/// blocked since user code commonly defines these for domain-specific purposes.
const BUILTIN_CALLS: &[&str] = &[
    // Constructors / Conversions
    "new", "clone", "into", "from", "to_string", "to_owned", "as_ref", "as_mut",
    // Option/Result
    "unwrap", "expect", "ok", "err", "unwrap_or", "unwrap_or_else", "unwrap_or_default",
    "ok_or", "is_some", "is_none", "is_ok", "is_err",
    // Error handling
    "map_err", "and_then", "or_else", "flatten",
    // Iterators (canonical methods, not the closure-based std traits)
    "iter", "iter_mut", "into_iter", "enumerate", "zip", "flat_map",
    // Boolean
    "any", "all",
    // String
    "trim", "split", "join",
    // Async
    "await", "spawn", "block_on",
    // I/O / Debug
    "println", "print", "eprintln", "eprint", "format", "panic",
    "assert", "assert_eq", "assert_ne", "debug_assert",
    // Threading / I/O primitives
    "lock", "write", "writeln", "read", "open", "close", "flush",
    // Keywords
    "self", "super", "crate", "std",
];

/// Known-bUILTIN types — these are never user-defined types.
const BUILTIN_TYPES: &[&str] = &[
    // Rust stdlib
    "String", "Vec", "HashMap", "HashSet", "BTreeMap", "BTreeSet",
    "Option", "Result", "Box", "Arc", "Rc", "Mutex", "RwLock",
    "Ok", "Err", "Some", "None", "Self", "Send", "Sync",
    "Clone", "Copy", "Debug", "Display", "Default", "Drop",
    "Into", "From", "AsRef", "AsMut", "Iterator", "Future",
    "Pin", "Path", "PathBuf", "Error", "Write", "Read",
    // Python builtins
    "True", "False", "NotImplementedError",
    "TypeError", "ValueError", "KeyError", "IndexError",
    "Exception", "RuntimeError", "StopIteration",
    // JS builtins
    "Promise", "Array", "Object", "Function", "Number",
    "Boolean", "Symbol", "BigInt", "Date", "Map", "Set",
    "WeakMap", "WeakSet", "Proxy", "Reflect", "JSON",
    "Math", "RegExp", "RangeError",
    // Rust primitive wrappers
    "I8", "I16", "I32", "I64", "U8", "U16", "U32", "U64",
    "F32", "F64", "Usize", "Isize", "Bool", "Char",
];

/// Extract all call and type-usage references from a source file.
/// Returns an empty vec for unsupported file types.
pub fn extract_refs(path: &Path, source: &str) -> Vec<StaticRef> {
    extract_refs_with_locals(path, source, &HashSet::new())
}

/// Extract references with knowledge of locally-defined symbols.
/// Locals are used for secondary classification: if a symbol is defined locally,
/// it's classified as user-defined even if it matches a builtin pattern.
pub fn extract_refs_with_locals(
    path: &Path,
    source: &str,
    local_definitions: &HashSet<String>,
) -> Vec<StaticRef> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        "rs" => extract(
            source,
            tree_sitter_rust::language(),
            &[RUST_CALLS_1, RUST_CALLS_2, RUST_CALLS_3],
            &[RUST_TYPES],
            local_definitions,
        ),
        "py" => extract(
            source,
            tree_sitter_python::language(),
            &[PY_CALLS_1, PY_CALLS_2],
            &[PY_TYPES],
            local_definitions,
        ),
        "js" | "jsx" | "ts" | "tsx" => extract(
            source,
            tree_sitter_javascript::language(),
            &[JS_CALLS_1, JS_CALLS_2, JS_NEW],
            &[JS_TYPES],
            local_definitions,
        ),
        _ => vec![],
    }
}

// ── Rust queries ──────────────────────────────────────────────────────────────

const RUST_CALLS_1: &str = "(call_expression function: (identifier) @name)";
const RUST_CALLS_2: &str =
    "(call_expression function: (field_expression field: (field_identifier) @name))";
const RUST_CALLS_3: &str =
    "(call_expression function: (scoped_identifier name: (identifier) @name))";
const RUST_TYPES: &str = "(type_identifier) @name";

// ── Python queries ────────────────────────────────────────────────────────────

const PY_CALLS_1: &str = "(call function: (identifier) @name)";
const PY_CALLS_2: &str = "(call function: (attribute attribute: (identifier) @name))";
const PY_TYPES: &str = "(identifier) @name";

// ── JavaScript / TypeScript queries ──────────────────────────────────────────

const JS_CALLS_1: &str = "(call_expression function: (identifier) @name)";
const JS_CALLS_2: &str =
    "(call_expression function: (member_expression property: (property_identifier) @name))";
const JS_NEW: &str = "(new_expression constructor: (identifier) @name)";
const JS_TYPES: &str = "(identifier) @name";

// ── Core extractor ────────────────────────────────────────────────────────────

fn extract(
    source: &str,
    language: Language,
    call_patterns: &[&str],
    type_patterns: &[&str],
    local_definitions: &HashSet<String>,
) -> Vec<StaticRef> {
    PARSER.with(|parser| {
        let mut parser = parser.lock();
        if parser.set_language(&language).is_err() {
            return vec![];
        }
        let Some(tree) = parser.parse(source, None) else {
            return vec![];
        };

        let src_bytes = source.as_bytes();
        let mut refs = Vec::new();

        // Calls
        for pattern in call_patterns {
            if let Ok(query) = Query::new(&language, pattern) {
                let mut cursor = QueryCursor::new();
                for m in cursor.matches(&query, tree.root_node(), src_bytes) {
                    for cap in m.captures {
                        if let Ok(name) = std::str::from_utf8(&src_bytes[cap.node.byte_range()]) {
                            if is_user_defined_call(name, local_definitions) {
                                refs.push(StaticRef {
                                    source_line: cap.node.start_position().row as u32,
                                    target_name: name.to_string(),
                                    edge_type: EdgeType::Calls,
                                });
                            }
                        }
                    }
                }
            }
        }

        // Type usages
        for pattern in type_patterns {
            if let Ok(query) = Query::new(&language, pattern) {
                let mut cursor = QueryCursor::new();
                for m in cursor.matches(&query, tree.root_node(), src_bytes) {
                    for cap in m.captures {
                        if let Ok(name) = std::str::from_utf8(&src_bytes[cap.node.byte_range()]) {
                            if is_user_defined_type(name, local_definitions) {
                                refs.push(StaticRef {
                                    source_line: cap.node.start_position().row as u32,
                                    target_name: name.to_string(),
                                    edge_type: EdgeType::Uses,
                                });
                            }
                        }
                    }
                }
            }
        }

        refs
    })
}

// ── Filters ────────────────────────────────────────────────────────────────────

/// Classifies a call as "user-defined" if:
/// 1. It's NOT in the builtin blocklist, OR
/// 2. It IS defined locally (secondary classification via local_definitions)
fn is_user_defined_call(name: &str, local_definitions: &HashSet<String>) -> bool {
    if name.len() <= 1 {
        return false;
    }
    // Secondary classification: locally-defined symbols override builtin blocklist
    if local_definitions.contains(name) {
        return true;
    }
    // Primary filter: not a known builtin
    !BUILTIN_CALLS.contains(&name)
}

/// Classifies a type as "user-defined" if:
/// 1. It's PascalCase AND not in the builtin blocklist, OR
/// 2. It IS defined locally (secondary classification via local_definitions)
fn is_user_defined_type(name: &str, local_definitions: &HashSet<String>) -> bool {
    if name.len() < 2 {
        return false;
    }
    // Secondary classification: locally-defined symbols override builtin blocklist
    if local_definitions.contains(name) {
        return true;
    }
    // Primary filter: PascalCase and not a known builtin
    let first = name.chars().next().unwrap();
    first.is_uppercase() && !BUILTIN_TYPES.contains(&name)
}

// ── String Literal Extraction for Semantic Boundaries ──────────────────────

/// A string literal found in source, used for cross-boundary pattern detection
#[derive(Debug, Clone)]
pub struct StringLiteral {
    pub source_line: u32,
    pub value: String,
}

/// Extract all string literals from a source file for semantic boundary analysis.
/// Unlike call/type refs, these are NOT resolved to node IDs - they're analyzed
/// as a group to find shared patterns across files.
pub fn extract_strings(path: &Path, source: &str) -> Vec<StringLiteral> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        "rs" => extract_string_literals(source, tree_sitter_rust::language()),
        "py" => extract_string_literals(source, tree_sitter_python::language()),
        "js" | "jsx" | "ts" | "tsx" => {
            extract_string_literals(source, tree_sitter_javascript::language())
        }
        _ => vec![],
    }
}

// ── Symbol Definition Extraction ─────────────────────────────────────────────

/// A symbol definition found in source: a function, struct, trait, etc.,
/// not yet wired into the graph. Caller maps this to a `GraphNode`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolDef {
    pub name: String,
    pub kind: NodeType,
    /// 0-indexed line where the definition starts.
    pub line_start: u32,
    /// 0-indexed line where the definition ends (inclusive).
    pub line_end: u32,
    /// True if the symbol is marked `#[deprecated]`.
    pub is_deprecated: bool,
    /// Optional labels attached to the symbol (e.g. "test", "async").
    pub labels: Vec<String>,
}

/// Extract all top-level (and impl-block) symbol definitions from a source file.
/// Acts as a fallback when LSP is unavailable: every symbol here becomes a
/// graph node so downstream `find Function` / `get_blast_radius` queries work.
///
/// Returns an empty vec for unsupported file types.
pub fn extract_definitions(path: &Path, source: &str) -> Vec<SymbolDef> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        "rs" => extract_definitions_rust(source),
        "py" => extract_definitions_python(source),
        "js" | "jsx" | "ts" | "tsx" => extract_definitions_js(source),
        _ => vec![],
    }
}

fn extract_definitions_rust(source: &str) -> Vec<SymbolDef> {
    PARSER.with(|parser| {
        let mut parser = parser.lock();
        if parser.set_language(&tree_sitter_rust::language()).is_err() {
            return vec![];
        }
        let Some(tree) = parser.parse(source, None) else {
            return vec![];
        };

        let src_bytes = source.as_bytes();
        let mut defs: Vec<SymbolDef> = Vec::new();
        let mut seen_keys: HashSet<(String, u32, u32)> = HashSet::new();

        // Run queries that match definitions at any depth (impl methods included).
        // The matched node's start/end rows give the line range; the `name`
        // field gives the identifier child.
        let patterns: &[(&str, NodeType)] = &[
            ("(function_item) @d", NodeType::Function),
            ("(struct_item) @d", NodeType::Struct),
            ("(trait_item) @d", NodeType::Trait),
            ("(enum_item) @d", NodeType::Enum),
        ];

        for (pattern, kind) in patterns {
            let Ok(query) = Query::new(&tree_sitter_rust::language(), pattern) else {
                continue;
            };
            let mut cursor = QueryCursor::new();
            for m in cursor.matches(&query, tree.root_node(), src_bytes) {
                for cap in m.captures {
                    let node = cap.node;
                    let Some(name_node) = node.child_by_field_name("name") else {
                        continue;
                    };
                    let Ok(name) = name_node.utf8_text(src_bytes) else {
                        continue;
                    };
                    let line_start = node.start_position().row as u32;
                    let line_end = node.end_position().row as u32;
                    let key = (name.to_string(), line_start, line_end);
                    if seen_keys.insert(key.clone()) {
                        let (is_deprecated, labels) = collect_rust_metadata(&node, src_bytes);
                        defs.push(SymbolDef {
                            name: name.to_string(),
                            kind: kind.clone(),
                            line_start,
                            line_end,
                            is_deprecated,
                            labels,
                        });
                    }
                }
            }
        }

        defs
    })
}

/// Walk a definition node's preceding siblings for `#[...]` attribute items and
/// extract `(is_deprecated, labels)`. Recognised: `#[deprecated]`, `#[test]`,
/// `#[async_trait]`, `#[no_mangle]`, and any other attribute's identifier is
/// captured as a label.
fn collect_rust_metadata(node: &tree_sitter::Node, src_bytes: &[u8]) -> (bool, Vec<String>) {
    let mut is_deprecated = false;
    let mut labels = Vec::new();
    let Some(parent) = node.parent() else { return (false, labels) };

    // Walk the parent's children backwards starting from the node. Stop as
    // soon as we encounter a non-attribute sibling — anything beyond that is
    // attached to a different definition.
    let node_start = node.start_position().row;
    let mut cursor = parent.walk();
    let mut siblings: Vec<tree_sitter::Node> = Vec::new();
    for sibling in parent.children(&mut cursor) {
        if sibling.start_position().row >= node_start {
            break;
        }
        siblings.push(sibling);
    }

    // Now iterate from the closest attribute backwards, stopping at the first
    // non-attribute sibling.
    let mut collected = false;
    for sibling in siblings.iter().rev() {
        if sibling.kind() != "attribute_item" {
            // We've gone past the contiguous attribute chain.
            break;
        }
        collected = true;
        let mut inner = sibling.walk();
        for child in sibling.children(&mut inner) {
            if child.kind() == "identifier" {
                if let Ok(s) = child.utf8_text(src_bytes) {
                    labels.push(s.to_string());
                    if s == "deprecated" {
                        is_deprecated = true;
                    }
                }
            } else if child.kind() == "attribute" {
                let mut path = child.walk();
                for p in child.children(&mut path) {
                    if p.kind() == "identifier" {
                        if let Ok(s) = p.utf8_text(src_bytes) {
                            labels.push(s.to_string());
                            if s == "deprecated" {
                                is_deprecated = true;
                            }
                        }
                    }
                }
            }
        }
    }
    if !collected {
        // Nothing matched — leave defaults.
    }
    // Labels are collected newest-first; reverse for predictable order.
    labels.reverse();
    (is_deprecated, labels)
}

fn extract_definitions_python(source: &str) -> Vec<SymbolDef> {
    PARSER.with(|parser| {
        let mut parser = parser.lock();
        if parser.set_language(&tree_sitter_python::language()).is_err() {
            return vec![];
        }
        let Some(tree) = parser.parse(source, None) else {
            return vec![];
        };

        let mut defs = Vec::new();
        let root = tree.root_node();

        // Top-level module: walk for function_definition, class_definition
        for child in root.children(&mut root.walk()) {
            match child.kind() {
                "function_definition" => {
                    if let Some(name) = python_def_name(&child, source) {
                        defs.push(SymbolDef {
                            name,
                            kind: NodeType::Function,
                            line_start: child.start_position().row as u32,
                            line_end: child.end_position().row as u32,
                            is_deprecated: false,
                            labels: Vec::new(),
                        });
                    }
                }
                "class_definition" => {
                    if let Some(name) = python_def_name(&child, source) {
                        defs.push(SymbolDef {
                            name,
                            kind: NodeType::Class,
                            line_start: child.start_position().row as u32,
                            line_end: child.end_position().row as u32,
                            is_deprecated: false,
                            labels: Vec::new(),
                        });
                    }
                }
                _ => {}
            }
        }

        defs
    })
}

fn python_def_name(node: &tree_sitter::Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" {
            return child.utf8_text(source.as_bytes()).ok().map(|s| s.to_string());
        }
    }
    None
}

fn extract_definitions_js(source: &str) -> Vec<SymbolDef> {
    PARSER.with(|parser| {
        let mut parser = parser.lock();
        if parser
            .set_language(&tree_sitter_javascript::language())
            .is_err()
        {
            return vec![];
        }
        let Some(tree) = parser.parse(source, None) else {
            return vec![];
        };

        let mut defs = Vec::new();
        let root = tree.root_node();

        for child in root.children(&mut root.walk()) {
            match child.kind() {
                "function_declaration" => {
                    if let Some(name) = js_function_name(&child, source) {
                        defs.push(SymbolDef {
                            name,
                            kind: NodeType::Function,
                            line_start: child.start_position().row as u32,
                            line_end: child.end_position().row as u32,
                            is_deprecated: false,
                            labels: Vec::new(),
                        });
                    }
                }
                "class_declaration" => {
                    if let Some(name) = js_class_name(&child, source) {
                        defs.push(SymbolDef {
                            name,
                            kind: NodeType::Class,
                            line_start: child.start_position().row as u32,
                            line_end: child.end_position().row as u32,
                            is_deprecated: false,
                            labels: Vec::new(),
                        });
                    }
                }
                _ => {}
            }
        }

        defs
    })
}

fn js_function_name(node: &tree_sitter::Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" {
            return child.utf8_text(source.as_bytes()).ok().map(|s| s.to_string());
        }
    }
    None
}

fn js_class_name(node: &tree_sitter::Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" {
            return child.utf8_text(source.as_bytes()).ok().map(|s| s.to_string());
        }
    }
    None
}

/// Core string literal extractor using tree-sitter
fn extract_string_literals(source: &str, language: Language) -> Vec<StringLiteral> {
    PARSER.with(|parser| {
        let mut parser = parser.lock();
        if parser.set_language(&language).is_err() {
            return vec![];
        }
        let Some(tree) = parser.parse(source, None) else {
            return vec![];
        };

        let src_bytes = source.as_bytes();
        let mut literals = Vec::new();

        // Query for string literals
        // Note: string syntax varies by language, but "(string)" covers most cases
        if let Ok(query) = Query::new(&language, "(string) @str") {
            let mut cursor = QueryCursor::new();
            for m in cursor.matches(&query, tree.root_node(), src_bytes) {
                for cap in m.captures {
                    if let Ok(s) = std::str::from_utf8(&src_bytes[cap.node.byte_range()]) {
                        // Strip quotes
                        let value = s.trim_matches(|c| c == '"' || c == '\'');
                        if is_semantic_candidate(value) {
                            literals.push(StringLiteral {
                                source_line: cap.node.start_position().row as u32,
                                value: value.to_string(),
                            });
                        }
                    }
                }
            }
        }

        literals
    })
}

/// Check if a string looks like a semantic boundary candidate.
/// These are path-like strings that could indicate cross-boundary coupling.
fn is_semantic_candidate(s: &str) -> bool {
    if s.len() < 3 {
        return false;
    }

    // Path patterns
    if s.starts_with('/') && s.len() > 5 {
        return true; // /api/v1/users, /graphql, /ws/stream
    }

    // Named constants that look like topics/queues/endpoints
    if s.len() > 4
        && s.chars().all(|c| c.is_uppercase() || c == '_' || c.is_numeric())
        && s.contains('_')
    {
        let upper = s.to_uppercase();
        if upper.contains("TOPIC")
            || upper.contains("QUEUE")
            || upper.contains("ENDPOINT")
            || upper.contains("STREAM")
            || upper.contains("SOCKET")
            || upper.contains("ROUTE")
        {
            return true;
        }
    }

    // URL patterns (http, https, ws, wss)
    if s.starts_with("http://")
        || s.starts_with("https://")
        || s.starts_with("ws://")
        || s.starts_with("wss://")
    {
        return true;
    }

    // Environment variable patterns
    if s.starts_with('$') && s.len() > 2 {
        return true;
    }

    // GraphQL or gRPC method names
    if s.starts_with('/') && (s.contains("Mutation") || s.contains("Query") || s.contains("Subscription")) {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::NodeType;
    use std::path::Path;

    #[test]
    fn test_rust_calls() {
        let source = r#"
fn main() {
    let db = GraphDatabase::new("/tmp/test").unwrap();
    db.insert_node(&node).unwrap();
    let result = process(db);
}
"#;
        let refs = extract_refs(Path::new("main.rs"), source);
        let calls: Vec<_> = refs.iter()
            .filter(|r| matches!(r.edge_type, EdgeType::Calls))
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(calls.contains(&"process"), "should find call to process");
    }

    #[test]
    fn test_rust_types() {
        let source = r#"
fn build(db: GraphDatabase, err: LainError) -> Result<ToolExecutor, LainError> {
    todo!()
}
"#;
        let refs = extract_refs(Path::new("lib.rs"), source);
        let types: Vec<_> = refs.iter()
            .filter(|r| matches!(r.edge_type, EdgeType::Uses))
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(types.contains(&"GraphDatabase"));
        assert!(types.contains(&"LainError"));
        assert!(types.contains(&"ToolExecutor"));
    }

    #[test]
    fn test_extract_definitions_finds_rust_function() {
        let source = r#"
pub fn add(a: i32, b: i32) -> i32 { a + b }
pub fn main() { add(1, 2); }
"#;
        let defs = extract_definitions(Path::new("lib.rs"), source);
        let names: Vec<_> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(
            names.contains(&"add"),
            "extract_definitions should find 'add' function, got: {:?}",
            names
        );
        assert!(
            names.contains(&"main"),
            "extract_definitions should find 'main' function, got: {:?}",
            names
        );
        let add = defs.iter().find(|d| d.name == "add").unwrap();
        assert!(matches!(add.kind, NodeType::Function));
        assert_eq!(add.line_start, 1);
    }

    #[test]
    fn test_extract_definitions_finds_rust_struct_and_trait() {
        let source = r#"
pub struct Calc { pub v: i32 }
pub trait Shape { fn area(&self) -> f64; }
impl Shape for Calc { fn area(&self) -> f64 { 0.0 } }
"#;
        let defs = extract_definitions(Path::new("lib.rs"), source);
        let names: Vec<_> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"Calc"), "got: {:?}", names);
        assert!(names.contains(&"Shape"), "got: {:?}", names);
        let calc = defs.iter().find(|d| d.name == "Calc").unwrap();
        assert!(matches!(calc.kind, NodeType::Struct));
        let shape = defs.iter().find(|d| d.name == "Shape").unwrap();
        assert!(matches!(shape.kind, NodeType::Trait));
    }

    #[test]
    fn test_extract_definitions_finds_impl_methods() {
        let source = r#"
pub struct Calc { pub v: i32 }
impl Calc {
    pub fn new(v: i32) -> Self { Self { v } }
    pub fn double(&self) -> i32 { self.v * 2 }
}
"#;
        let defs = extract_definitions(Path::new("lib.rs"), source);
        let names: Vec<_> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(
            names.contains(&"new"),
            "impl method 'new' should be extracted; got: {:?}",
            names
        );
        assert!(
            names.contains(&"double"),
            "impl method 'double' should be extracted; got: {:?}",
            names
        );
        let new = defs.iter().find(|d| d.name == "new").unwrap();
        assert!(matches!(new.kind, NodeType::Function));
        assert_eq!(new.line_start, 3);
    }

    #[test]
    fn test_extract_definitions_captures_deprecated_attribute() {
        let source = r#"#[deprecated]
pub fn old_api() -> i32 { 42 }
pub fn new_api() -> i32 { 1 }
"#;
        let defs = extract_definitions(Path::new("lib.rs"), source);
        let old = defs.iter().find(|d| d.name == "old_api").unwrap();
        assert!(
            old.is_deprecated,
            "old_api should be marked deprecated; got: is_deprecated={} labels={:?}",
            old.is_deprecated, old.labels
        );
        let new = defs.iter().find(|d| d.name == "new_api").unwrap();
        assert!(!new.is_deprecated, "new_api should not be deprecated");
    }

    #[test]
    fn test_extract_definitions_finds_python_function() {
        let source = r#"
def hello(name):
    return name

class Foo:
    def bar(self):
        return 1
"#;
        let defs = extract_definitions(Path::new("foo.py"), source);
        let names: Vec<_> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"hello"), "got: {:?}", names);
        assert!(names.contains(&"Foo"), "got: {:?}", names);
    }

    #[test]
    fn test_locals_override_blocklist() {
        // If "process" is defined locally, it should be tracked even though
        // it's not in our builtin blocklist (and wouldn't be filtered anyway)
        let mut locals = HashSet::new();
        locals.insert("process".to_string());

        let source = r#"
fn process(data: Data) -> Result { todo!() }
fn main() {
    process(something);
}
"#;
        let refs = extract_refs_with_locals(Path::new("main.rs"), source, &locals);
        let calls: Vec<_> = refs.iter()
            .filter(|r| matches!(r.edge_type, EdgeType::Calls))
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(calls.contains(&"process"), "should find process even if in locals");
    }
}