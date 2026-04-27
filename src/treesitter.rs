//! Tree-sitter static analysis for extracting call and type-usage edges
//!
//! Operates purely on source text — no LSP, no network, no side effects.
//! Returns unresolved (line, name, edge_type) tuples; caller resolves to node IDs.

use crate::schema::EdgeType;
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