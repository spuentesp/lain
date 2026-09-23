//! Tree-sitter static analysis for extracting definitions, call and
//! type-usage edges, and string literals.
//!
//! Operates purely on source text — no LSP, no network, no side effects.
//! Returns unresolved (line, name, edge_type) tuples; caller resolves to node IDs.
//! Every language in [`LANGS`] is compiled into the binary, so nothing here
//! needs a language server installed.

use crate::schema::{EdgeType, NodeType};
use parking_lot::Mutex;
use std::borrow::Cow;
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

thread_local! {
    static PARSER: Mutex<Parser> = Mutex::new(Parser::new());
}

/// A raw reference found in source code, not yet resolved to graph node IDs.
pub struct StaticRef {
    /// 0-indexed line in the file where this reference occurs.
    pub source_line: u32,
    pub target_name: String,
    pub edge_type: EdgeType,
    /// A call made on some other value (`d.update()`, `arr.push()`), not
    /// a bare call or one on `self` / `this`. The resolver uses it to
    /// avoid linking `dict.update()` to the one user-defined `update`.
    pub foreign_receiver: bool,
}

/// Known-bUILTIN blocklist — only canonical std/lib calls that are unambiguously
/// language builtins. Domain names (map, filter, get, log, error, etc.) are NOT
/// blocked since user code commonly defines these for domain-specific purposes.
const BUILTIN_CALLS: &[&str] = &[
    // Constructors / Conversions
    "new",
    "clone",
    "into",
    "from",
    "to_string",
    "to_owned",
    "as_ref",
    "as_mut",
    // Option/Result
    "unwrap",
    "expect",
    "ok",
    "err",
    "unwrap_or",
    "unwrap_or_else",
    "unwrap_or_default",
    "ok_or",
    "is_some",
    "is_none",
    "is_ok",
    "is_err",
    // Error handling
    "map_err",
    "and_then",
    "or_else",
    "flatten",
    // Iterators (canonical methods, not the closure-based std traits)
    "iter",
    "iter_mut",
    "into_iter",
    "enumerate",
    "zip",
    "flat_map",
    // Boolean
    "any",
    "all",
    // String
    "trim",
    "split",
    "join",
    // Async
    "await",
    "spawn",
    "block_on",
    // I/O / Debug
    "println",
    "print",
    "eprintln",
    "eprint",
    "format",
    "panic",
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    // Threading / I/O primitives
    "lock",
    "write",
    "writeln",
    "read",
    "open",
    "close",
    "flush",
    // Keywords
    "self",
    "super",
    "crate",
    "std",
];

/// Known-bUILTIN types — these are never user-defined types.
const BUILTIN_TYPES: &[&str] = &[
    // Rust stdlib
    "String",
    "Vec",
    "HashMap",
    "HashSet",
    "BTreeMap",
    "BTreeSet",
    "Option",
    "Result",
    "Box",
    "Arc",
    "Rc",
    "Mutex",
    "RwLock",
    "Ok",
    "Err",
    "Some",
    "None",
    "Self",
    "Send",
    "Sync",
    "Clone",
    "Copy",
    "Debug",
    "Display",
    "Default",
    "Drop",
    "Into",
    "From",
    "AsRef",
    "AsMut",
    "Iterator",
    "Future",
    "Pin",
    "Path",
    "PathBuf",
    "Error",
    "Write",
    "Read",
    // Python builtins
    "True",
    "False",
    "NotImplementedError",
    "TypeError",
    "ValueError",
    "KeyError",
    "IndexError",
    "Exception",
    "RuntimeError",
    "StopIteration",
    // JS builtins
    "Promise",
    "Array",
    "Object",
    "Function",
    "Number",
    "Boolean",
    "Symbol",
    "BigInt",
    "Date",
    "Map",
    "Set",
    "WeakMap",
    "WeakSet",
    "Proxy",
    "Reflect",
    "JSON",
    "Math",
    "RegExp",
    "RangeError",
    // Rust primitive wrappers
    "I8",
    "I16",
    "I32",
    "I64",
    "U8",
    "U16",
    "U32",
    "U64",
    "F32",
    "F64",
    "Usize",
    "Isize",
    "Bool",
    "Char",
];

// ── Languages ─────────────────────────────────────────────────────────────────

/// Everything needed to read one language: its grammar and the queries that
/// pull definitions, calls, type references and string literals out of it.
///
/// Every language the registry promises gets one of these, so definitions and
/// the call graph never depend on a language server being installed; an LSP,
/// when present, only adds precision on top.
///
/// Query conventions:
/// - `defs`: one pattern per entry capturing the definition node as `@d` and,
///   when the name is not simply the node's `name` field, the name as `@name`.
/// - `calls` / `types`: capture the referenced name as `@name`.
/// - `strings`: capture the literal as `@s`.
struct LangSpec {
    id: &'static str,
    grammar: fn() -> Language,
    defs: &'static [(&'static str, NodeType)],
    calls: &'static [&'static str],
    types: &'static [&'static str],
    strings: &'static [&'static str],
}

/// The spec for a file extension, or `None` for files no grammar covers.
/// `.vue` / `.svelte` map to the language of their `<script>` block; see
/// [`script_view`].
fn spec_for_ext(ext: &str) -> Option<&'static LangSpec> {
    let id = match ext {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "go" => "go",
        "java" => "java",
        "c" => "c",
        // `.h` is shared by C and C++; the C++ grammar reads C headers too.
        "h" | "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => "cpp",
        "cs" => "csharp",
        "rb" | "rake" => "ruby",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "scala" | "sc" => "scala",
        "php" => "php",
        _ => return None,
    };
    LANGS.iter().find(|l| l.id == id)
}

/// Whether Lain can read `ext` without a language server: the single list the
/// watcher and anything else deciding "is this a source file" should use.
pub fn is_indexed_extension(ext: &str) -> bool {
    language_name(ext).is_some()
}

/// Display name of the language an extension belongs to, for every
/// extension [`is_indexed_extension`] accepts.
pub fn language_name(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "vue" => "Vue",
        "svelte" => "Svelte",
        // `.h` parses with the C++ grammar but is labelled as C, as before.
        "c" | "h" => "C",
        _ => match spec_for_ext(ext)?.id {
            "rust" => "Rust",
            "python" => "Python",
            "javascript" => "JavaScript",
            "typescript" | "tsx" => "TypeScript",
            "go" => "Go",
            "java" => "Java",
            "cpp" => "C++",
            "csharp" => "C#",
            "ruby" => "Ruby",
            "swift" => "Swift",
            "kotlin" => "Kotlin",
            "scala" => "Scala",
            "php" => "PHP",
            _ => return None,
        },
    })
}

const JS_DEFS: &[(&str, NodeType)] = &[
    ("(function_declaration) @d", NodeType::Function),
    ("(generator_function_declaration) @d", NodeType::Function),
    ("(method_definition) @d", NodeType::Function),
    (
        "(variable_declarator value: [(arrow_function) (function_expression) (generator_function)]) @d",
        NodeType::Function,
    ),
    (
        "(field_definition value: [(arrow_function) (function_expression) (generator_function)]) @d",
        NodeType::Function,
    ),
    ("(class_declaration) @d", NodeType::Class),
    EXPORTED_CALL_CONST,
];
/// An exported binding to a call's result — a Pinia store
/// (`export const useCart = defineStore(…)`), a composable, a `styled.div`,
/// a Redux slice. Callers invoke it by that name, so without a definition
/// every `useCart()` was a call to nothing. Unexported bindings are left
/// out: `const route = useRoute()` in a component is local state, not a
/// symbol, and indexing it would credit the component's calls to it.
const EXPORTED_CALL_CONST: (&str, NodeType) = (
    "(program (export_statement (lexical_declaration (variable_declarator value: (call_expression)) @d)))",
    NodeType::Constant,
);
/// TypeScript's grammar names a class field `public_field_definition` and
/// adds abstract classes, interfaces and enums.
const TS_DEFS: &[(&str, NodeType)] = &[
    ("(function_declaration) @d", NodeType::Function),
    ("(generator_function_declaration) @d", NodeType::Function),
    ("(method_definition) @d", NodeType::Function),
    (
        "(variable_declarator value: [(arrow_function) (function_expression) (generator_function)]) @d",
        NodeType::Function,
    ),
    (
        "(public_field_definition value: [(arrow_function) (function_expression) (generator_function)]) @d",
        NodeType::Function,
    ),
    ("(class_declaration) @d", NodeType::Class),
    ("(abstract_class_declaration) @d", NodeType::Class),
    ("(interface_declaration) @d", NodeType::Interface),
    ("(enum_declaration) @d", NodeType::Enum),
    EXPORTED_CALL_CONST,
];
const JS_CALLS: &[&str] = &[
    "(call_expression function: (identifier) @name)",
    "(call_expression function: (member_expression property: (property_identifier) @name))",
    // `this.#retry()` — a private method's name is its own node kind.
    "(call_expression function: (member_expression property: (private_property_identifier) @name))",
    "(new_expression constructor: (identifier) @name)",
];

const C_DEFS: &[(&str, NodeType)] = &[
    // The name sits inside the declarator chain (pointers, references,
    // `Foo::bar`); [`def_name`] walks it.
    ("(function_definition) @d", NodeType::Function),
    (
        "(struct_specifier name: (type_identifier) body: (_)) @d",
        NodeType::Struct,
    ),
    (
        "(enum_specifier name: (type_identifier) body: (_)) @d",
        NodeType::Enum,
    ),
];
const C_CALLS: &[&str] = &[
    "(call_expression function: (identifier) @name)",
    "(call_expression function: (field_expression field: (field_identifier) @name))",
];

static LANGS: &[LangSpec] = &[
    LangSpec {
        id: "rust",
        grammar: || tree_sitter_rust::LANGUAGE.into(),
        defs: &[
            ("(function_item) @d", NodeType::Function),
            ("(struct_item) @d", NodeType::Struct),
            ("(trait_item) @d", NodeType::Trait),
            ("(enum_item) @d", NodeType::Enum),
        ],
        calls: &[
            "(call_expression function: (identifier) @name)",
            "(call_expression function: (field_expression field: (field_identifier) @name))",
            "(call_expression function: (scoped_identifier name: (identifier) @name))",
        ],
        types: &["(type_identifier) @name"],
        strings: &["(string_literal) @s", "(raw_string_literal) @s"],
    },
    LangSpec {
        id: "python",
        grammar: || tree_sitter_python::LANGUAGE.into(),
        // Any depth: methods live in class bodies, and a decorated definition
        // is a `decorated_definition` wrapping the `function_definition`.
        defs: &[
            ("(function_definition) @d", NodeType::Function),
            ("(class_definition) @d", NodeType::Class),
        ],
        calls: &[
            "(call function: (identifier) @name)",
            "(call function: (attribute attribute: (identifier) @name))",
        ],
        types: &["(identifier) @name"],
        strings: &["(string) @s"],
    },
    LangSpec {
        id: "javascript",
        grammar: || tree_sitter_javascript::LANGUAGE.into(),
        defs: JS_DEFS,
        calls: JS_CALLS,
        types: &["(identifier) @name"],
        strings: &["(string) @s", "(template_string) @s"],
    },
    LangSpec {
        id: "typescript",
        grammar: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        defs: TS_DEFS,
        calls: JS_CALLS,
        types: &["(identifier) @name", "(type_identifier) @name"],
        strings: &["(string) @s", "(template_string) @s"],
    },
    LangSpec {
        id: "tsx",
        grammar: || tree_sitter_typescript::LANGUAGE_TSX.into(),
        defs: TS_DEFS,
        calls: JS_CALLS,
        types: &["(identifier) @name", "(type_identifier) @name"],
        strings: &["(string) @s", "(template_string) @s"],
    },
    LangSpec {
        id: "go",
        grammar: || tree_sitter_go::LANGUAGE.into(),
        defs: &[
            ("(function_declaration) @d", NodeType::Function),
            ("(method_declaration) @d", NodeType::Function),
            ("(type_spec type: (struct_type)) @d", NodeType::Struct),
            ("(type_spec type: (interface_type)) @d", NodeType::Interface),
        ],
        calls: &[
            "(call_expression function: (identifier) @name)",
            "(call_expression function: (selector_expression field: (field_identifier) @name))",
        ],
        types: &["(type_identifier) @name"],
        strings: &["(interpreted_string_literal) @s", "(raw_string_literal) @s"],
    },
    LangSpec {
        id: "java",
        grammar: || tree_sitter_java::LANGUAGE.into(),
        defs: &[
            ("(method_declaration) @d", NodeType::Function),
            ("(constructor_declaration) @d", NodeType::Function),
            ("(class_declaration) @d", NodeType::Class),
            ("(record_declaration) @d", NodeType::Class),
            ("(interface_declaration) @d", NodeType::Interface),
            ("(enum_declaration) @d", NodeType::Enum),
        ],
        calls: &[
            "(method_invocation name: (identifier) @name)",
            "(object_creation_expression type: (type_identifier) @name)",
        ],
        types: &["(type_identifier) @name"],
        strings: &["(string_literal) @s"],
    },
    LangSpec {
        id: "c",
        grammar: || tree_sitter_c::LANGUAGE.into(),
        defs: C_DEFS,
        calls: C_CALLS,
        types: &["(type_identifier) @name"],
        strings: &["(string_literal) @s"],
    },
    LangSpec {
        id: "cpp",
        grammar: || tree_sitter_cpp::LANGUAGE.into(),
        defs: &[
            ("(function_definition) @d", NodeType::Function),
            ("(struct_specifier name: (type_identifier) body: (_)) @d", NodeType::Struct),
            ("(enum_specifier name: (type_identifier) body: (_)) @d", NodeType::Enum),
            ("(class_specifier name: (type_identifier) body: (_)) @d", NodeType::Class),
        ],
        calls: &[
            "(call_expression function: (identifier) @name)",
            "(call_expression function: (field_expression field: (field_identifier) @name))",
            "(call_expression function: (qualified_identifier name: (identifier) @name))",
            "(call_expression function: (template_function name: (identifier) @name))",
            "(call_expression function: (field_expression field: (template_method name: (field_identifier) @name)))",
            "(new_expression type: (type_identifier) @name)",
        ],
        types: &["(type_identifier) @name"],
        strings: &["(string_literal) @s", "(raw_string_literal) @s"],
    },
    LangSpec {
        id: "csharp",
        grammar: || tree_sitter_c_sharp::LANGUAGE.into(),
        defs: &[
            ("(method_declaration) @d", NodeType::Function),
            ("(constructor_declaration) @d", NodeType::Function),
            ("(local_function_statement) @d", NodeType::Function),
            ("(class_declaration) @d", NodeType::Class),
            ("(record_declaration) @d", NodeType::Class),
            ("(struct_declaration) @d", NodeType::Struct),
            ("(interface_declaration) @d", NodeType::Interface),
            ("(enum_declaration) @d", NodeType::Enum),
        ],
        calls: &[
            "(invocation_expression function: (identifier) @name)",
            "(invocation_expression function: (generic_name (identifier) @name))",
            "(invocation_expression function: (member_access_expression name: (identifier) @name))",
            "(invocation_expression function: (member_access_expression name: (generic_name (identifier) @name)))",
            "(object_creation_expression type: (identifier) @name)",
        ],
        // C# has no dedicated type-name node; a type is an identifier in a
        // `type:` field.
        types: &["(_ type: (identifier) @name)"],
        strings: &[
            "(string_literal) @s",
            "(verbatim_string_literal) @s",
            "(raw_string_literal) @s",
        ],
    },
    LangSpec {
        id: "ruby",
        grammar: || tree_sitter_ruby::LANGUAGE.into(),
        defs: &[
            ("(method) @d", NodeType::Function),
            ("(singleton_method) @d", NodeType::Function),
            ("(class name: (constant) @name) @d", NodeType::Class),
            ("(class name: (scope_resolution name: (constant) @name)) @d", NodeType::Class),
            ("(module name: (constant) @name) @d", NodeType::Module),
            ("(module name: (scope_resolution name: (constant) @name)) @d", NodeType::Module),
        ],
        calls: &[
            "(call method: (identifier) @name)",
            // `Foo.new` constructs a `Foo`.
            "(call receiver: (constant) @name method: (identifier) @m (#eq? @m \"new\"))",
        ],
        types: &["(constant) @name"],
        strings: &["(string) @s"],
    },
    LangSpec {
        id: "swift",
        grammar: || tree_sitter_swift::LANGUAGE.into(),
        // `class_declaration` also covers struct / enum / actor / extension;
        // [`refine_kind`] reads `declaration_kind`, and an extension's name
        // is a type expression, which [`def_name`] declines.
        defs: &[
            ("(function_declaration) @d", NodeType::Function),
            ("(protocol_function_declaration) @d", NodeType::Function),
            ("(class_declaration) @d", NodeType::Class),
            ("(protocol_declaration) @d", NodeType::Interface),
        ],
        calls: &[
            "(call_expression (simple_identifier) @name)",
            "(call_expression (navigation_expression suffix: (navigation_suffix suffix: (simple_identifier) @name)))",
        ],
        types: &["(type_identifier) @name"],
        strings: &["(line_string_literal) @s"],
    },
    LangSpec {
        id: "kotlin",
        grammar: || tree_sitter_kotlin_ng::LANGUAGE.into(),
        // `class_declaration` also covers `interface` and `enum class`;
        // see [`refine_kind`].
        defs: &[
            ("(function_declaration) @d", NodeType::Function),
            ("(class_declaration) @d", NodeType::Class),
            ("(object_declaration) @d", NodeType::Class),
        ],
        calls: &[
            "(call_expression (identifier) @name)",
            // The member is the navigation's last child: `a.b.c()` calls `c`.
            "(call_expression (navigation_expression (identifier) @name .))",
            // The grammar binds a prefix operator tighter than the call:
            // `!saveAsArg(x)` parses as a call *of* `!saveAsArg`.
            "(call_expression (unary_expression (identifier) @name .))",
        ],
        types: &["(user_type (identifier) @name)"],
        strings: &["(string_literal) @s"],
    },
    LangSpec {
        id: "scala",
        grammar: || tree_sitter_scala::LANGUAGE.into(),
        defs: &[
            ("(function_definition) @d", NodeType::Function),
            ("(function_declaration) @d", NodeType::Function),
            ("(class_definition) @d", NodeType::Class),
            ("(object_definition) @d", NodeType::Class),
            ("(trait_definition) @d", NodeType::Trait),
            ("(enum_definition) @d", NodeType::Enum),
        ],
        calls: &[
            "(call_expression function: (identifier) @name)",
            "(call_expression function: (field_expression field: (identifier) @name))",
        ],
        types: &[],
        strings: &["(string) @s"],
    },
    LangSpec {
        id: "php",
        grammar: || tree_sitter_php::LANGUAGE_PHP.into(),
        defs: &[
            ("(function_definition) @d", NodeType::Function),
            ("(method_declaration) @d", NodeType::Function),
            ("(class_declaration) @d", NodeType::Class),
            ("(interface_declaration) @d", NodeType::Interface),
            ("(trait_declaration) @d", NodeType::Trait),
            ("(enum_declaration) @d", NodeType::Enum),
        ],
        calls: &[
            "(function_call_expression function: (name) @name)",
            "(function_call_expression function: (qualified_name (name) @name .))",
            "(member_call_expression name: (name) @name)",
            "(nullsafe_member_call_expression name: (name) @name)",
            "(scoped_call_expression name: (name) @name)",
            "(object_creation_expression (name) @name)",
        ],
        types: &["(named_type (name) @name)"],
        strings: &["(string) @s", "(encapsed_string) @s"],
    },
];

/// A language's queries, compiled once. Compiling a query costs far more than
/// running it, and the extractors run once per file.
struct Compiled {
    language: Language,
    defs: Query,
    /// Node kind per `defs` pattern index.
    def_kinds: Vec<NodeType>,
    calls: Query,
    types: Option<Query>,
    strings: Option<Query>,
}

fn compile(spec: &LangSpec) -> Result<Compiled, String> {
    let language = (spec.grammar)();
    let defs_list = spec.defs;
    let join = |patterns: &[&str]| patterns.join("\n");
    let query = |what: &str, src: String| {
        Query::new(&language, &src).map_err(|e| format!("{} {what} query: {e}", spec.id))
    };
    let defs = query(
        "definition",
        join(&defs_list.iter().map(|(p, _)| *p).collect::<Vec<_>>()),
    )?;
    let def_kinds = defs_list.iter().map(|(_, k)| k.clone()).collect();
    let calls = query("call", join(spec.calls))?;
    let types = if spec.types.is_empty() {
        None
    } else {
        Some(query("type", join(spec.types))?)
    };
    let strings = if spec.strings.is_empty() {
        None
    } else {
        Some(query("string", join(spec.strings))?)
    };
    Ok(Compiled {
        language,
        defs,
        def_kinds,
        calls,
        types,
        strings,
    })
}

/// The compiled queries for `spec`. A query that fails to compile is a bug in
/// the table above — `every_language_compiles` catches it — so production
/// logs it once and treats the language as unsupported rather than panicking.
fn compiled(spec: &'static LangSpec) -> Option<&'static Compiled> {
    static CACHE: OnceLock<Vec<Option<Compiled>>> = OnceLock::new();
    let all = CACHE.get_or_init(|| {
        LANGS
            .iter()
            .map(|s| {
                compile(s)
                    .map_err(|e| tracing::error!("tree-sitter: {e}"))
                    .ok()
            })
            .collect()
    });
    let idx = LANGS.iter().position(|l| std::ptr::eq(l, spec))?;
    all[idx].as_ref()
}

/// The language and source text the extractors should read for `path`.
///
/// For single-file components (`.vue`, `.svelte`) everything outside the
/// `<script>` blocks is blanked to spaces, newlines kept, so the script parses
/// as ordinary JS/TS while every byte offset and line number still points into
/// the original file.
fn script_view<'a>(path: &Path, source: &'a str) -> Option<(&'static LangSpec, Cow<'a, str>)> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext == "vue" || ext == "svelte" {
        let (masked, is_ts) = mask_outside_scripts(source);
        let spec = spec_for_ext(if is_ts { "ts" } else { "js" })?;
        return Some((spec, Cow::Owned(masked)));
    }
    Some((spec_for_ext(ext)?, Cow::Borrowed(source)))
}

/// Blank everything outside `<script …>…</script>`, preserving newlines and
/// byte length. Returns whether any script block declares TypeScript.
fn mask_outside_scripts(source: &str) -> (String, bool) {
    let lower = source.to_ascii_lowercase();
    let mut keep = vec![false; source.len()];
    let mut is_ts = false;
    let mut from = 0;
    while let Some(open) = lower[from..].find("<script").map(|i| i + from) {
        let Some(tag_end) = lower[open..].find('>').map(|i| i + open + 1) else {
            break;
        };
        let tag = &lower[open..tag_end];
        if tag.contains("lang=\"ts\"")
            || tag.contains("lang='ts'")
            || tag.contains("lang=\"typescript\"")
            || tag.contains("lang='typescript'")
        {
            is_ts = true;
        }
        let close = lower[tag_end..]
            .find("</script")
            .map(|i| i + tag_end)
            .unwrap_or(source.len());
        keep[tag_end..close].iter_mut().for_each(|k| *k = true);
        from = close;
    }
    // Replacing bytes (not chars) with ASCII keeps the length and still
    // yields valid UTF-8: kept regions are whole, since they are bounded by
    // ASCII tag characters.
    let masked: Vec<u8> = source
        .bytes()
        .zip(keep)
        .map(|(b, k)| if k || b == b'\n' { b } else { b' ' })
        .collect();
    (String::from_utf8(masked).unwrap_or_default(), is_ts)
}

fn parse(language: &Language, source: &str) -> Option<tree_sitter::Tree> {
    PARSER.with(|parser| {
        let mut parser = parser.lock();
        parser.set_language(language).ok()?;
        parser.parse(source, None)
    })
}

// ── References ───────────────────────────────────────────────────────────────

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
    let Some((spec, text)) = script_view(path, source) else {
        return vec![];
    };
    let Some(c) = compiled(spec) else {
        return vec![];
    };
    let Some(tree) = parse(&c.language, &text) else {
        return vec![];
    };
    let src_bytes = text.as_bytes();
    let mut refs = Vec::new();

    let mut collect = |query: &Query, edge_type: EdgeType, keep: &dyn Fn(&str) -> bool| {
        let Some(name_idx) = query.capture_index_for_name("name") else {
            return;
        };
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), src_bytes);
        while let Some(m) = matches.next() {
            for cap in m.captures.iter().filter(|c| c.index == name_idx) {
                if let Ok(name) = cap.node.utf8_text(src_bytes) {
                    if keep(name) {
                        refs.push(StaticRef {
                            source_line: cap.node.start_position().row as u32,
                            target_name: name.to_string(),
                            edge_type: edge_type.clone(),
                            foreign_receiver: edge_type == EdgeType::Calls
                                && has_foreign_receiver(&cap.node, src_bytes),
                        });
                    }
                }
            }
        }
    };
    collect(&c.calls, EdgeType::Calls, &|n| {
        is_user_defined_call(n, local_definitions)
    });
    if let Some(types) = &c.types {
        collect(types, EdgeType::Uses, &|n| {
            is_user_defined_type(n, local_definitions)
        });
    }
    refs
}

/// Whether a called name is reached through a receiver other than
/// `self` / `this`: `d.update()`, `this.items.push()`, `list.get(0)`.
///
/// Walks up from the name to the member-access node (whatever the grammar
/// calls it) and reads its receiver field; stops at the call node itself,
/// which in Java, Ruby and PHP carries the receiver directly.
fn has_foreign_receiver(name: &tree_sitter::Node, src_bytes: &[u8]) -> bool {
    const RECEIVER_FIELDS: &[&str] = &[
        "object",
        "receiver",
        "operand",
        "value",
        "argument",
        "expression",
        "scope",
        "target",
    ];
    const SELF_LIKE: &[&str] = &[
        "self", "this", "cls", "super", "Self", "$this", "base", "parent", "static",
    ];
    let mut node = *name;
    for _ in 0..4 {
        let Some(parent) = node.parent() else {
            return false;
        };
        let receiver = RECEIVER_FIELDS.iter().find_map(|f| {
            parent
                .child_by_field_name(f)
                .filter(|r| r.end_byte() <= name.start_byte())
        });
        // Kotlin's `navigation_expression` has no field names: the
        // receiver is its first named child.
        let receiver = receiver.or_else(|| {
            (parent.kind() == "navigation_expression")
                .then(|| parent.named_child(0))
                .flatten()
                .filter(|r| r.end_byte() <= name.start_byte())
        });
        if let Some(r) = receiver {
            let text = r.utf8_text(src_bytes).unwrap_or_default().trim();
            // `super().update()` (Python) and `parent::get()` / `static::`
            // (PHP) reach the class's own hierarchy, like `self`.
            let own = SELF_LIKE.contains(&text) || text.starts_with("super(");
            return !own;
        }
        let kind = parent.kind();
        if kind.contains("call") || kind.contains("invocation") {
            return false;
        }
        node = parent;
    }
    false
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
    let Some((spec, text)) = script_view(path, source) else {
        return vec![];
    };
    let Some(c) = compiled(spec) else {
        return vec![];
    };
    let Some(query) = &c.strings else {
        return vec![];
    };
    let Some(tree) = parse(&c.language, &text) else {
        return vec![];
    };
    let src_bytes = text.as_bytes();
    let mut literals = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), src_bytes);
    while let Some(m) = matches.next() {
        for cap in m.captures {
            if let Ok(s) = cap.node.utf8_text(src_bytes) {
                let value = strip_string_delimiters(s);
                if is_semantic_candidate(value) {
                    literals.push(StringLiteral {
                        source_line: cap.node.start_position().row as u32,
                        value: value.to_string(),
                    });
                }
            }
        }
    }
    literals
}

/// The body of a string literal: drops a short prefix before the opening
/// quote (`r#`, `f`, `b`, `@`, `$`), the quotes themselves (single, double,
/// triple, backtick) and a raw string's trailing `#`s. A leading `$` that is
/// part of the content (`"$HOME"`) is kept, since it is inside the quotes.
fn strip_string_delimiters(s: &str) -> &str {
    const QUOTES: [char; 3] = ['"', '\'', '`'];
    let body = match s.find(QUOTES) {
        Some(i)
            if i <= 3
                && s[..i]
                    .chars()
                    .all(|c| c.is_ascii_alphabetic() || matches!(c, '@' | '$' | '#')) =>
        {
            &s[i..]
        }
        _ => s,
    };
    body.trim_start_matches(QUOTES)
        .trim_end_matches('#')
        .trim_end_matches(QUOTES)
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
    /// Byte offset of the first byte of the definition, straight from
    /// the tree-sitter node. Lets callers hash the exact byte range
    /// without re-deriving it from line numbers (which would normalize
    /// CRLF, drop trailing newlines, or conflate multiple definitions
    /// on one line).
    pub byte_start: u32,
    /// Byte offset one past the last byte of the definition.
    pub byte_end: u32,
    /// True if the symbol is marked `#[deprecated]`.
    pub is_deprecated: bool,
    /// Optional labels attached to the symbol (e.g. "test", "async").
    pub labels: Vec<String>,
}

/// Extract symbol definitions at any depth (methods, nested and exported
/// declarations included) from a source file. Acts as a fallback when LSP is
/// unavailable: every symbol here becomes a graph node so downstream
/// `find Function` / `get_blast_radius` queries work.
///
/// Returns an empty vec for unsupported file types.
pub fn extract_definitions(path: &Path, source: &str) -> Vec<SymbolDef> {
    let Some((spec, text)) = script_view(path, source) else {
        return vec![];
    };
    let Some(c) = compiled(spec) else {
        return vec![];
    };
    let Some(tree) = parse(&c.language, &text) else {
        return vec![];
    };
    let src_bytes = text.as_bytes();
    let d_idx = c.defs.capture_index_for_name("d");
    let name_idx = c.defs.capture_index_for_name("name");
    let mut defs = Vec::new();
    let mut seen: HashSet<(String, u32, u32)> = HashSet::new();

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&c.defs, tree.root_node(), src_bytes);
    while let Some(m) = matches.next() {
        let Some(node) = m
            .captures
            .iter()
            .find(|cap| Some(cap.index) == d_idx)
            .map(|cap| cap.node)
        else {
            continue;
        };
        let explicit = m
            .captures
            .iter()
            .find(|cap| Some(cap.index) == name_idx)
            .map(|cap| cap.node);
        let Some(name) = explicit
            .or_else(|| def_name_node(&node))
            .filter(|n| is_name_kind(n.kind()))
            .and_then(|n| n.utf8_text(src_bytes).ok())
        else {
            continue;
        };
        if spec.id == "python" && is_typing_overload(&node, src_bytes) {
            continue;
        }
        let line_start = node.start_position().row as u32;
        let line_end = node.end_position().row as u32;
        if !seen.insert((name.to_string(), line_start, line_end)) {
            continue;
        }
        let (is_deprecated, labels) = if spec.id == "rust" {
            collect_rust_metadata(&node, src_bytes)
        } else {
            (false, Vec::new())
        };
        defs.push(SymbolDef {
            name: name.to_string(),
            kind: refine_kind(spec, &node, src_bytes, &c.def_kinds[m.pattern_index]),
            line_start,
            line_end,
            byte_start: node.start_byte() as u32,
            byte_end: node.end_byte() as u32,
            is_deprecated,
            labels,
        });
    }
    defs
}

/// A `@typing.overload` stub: a type signature, not code. Indexing it gave
/// every overloaded function several same-file definitions, which the
/// resolver treats as ambiguous, so callers in other files never linked.
fn is_typing_overload(node: &tree_sitter::Node, src_bytes: &[u8]) -> bool {
    let Some(parent) = node.parent().filter(|p| p.kind() == "decorated_definition") else {
        return false;
    };
    let mut cursor = parent.walk();
    let found = parent.children(&mut cursor).any(|c| {
        c.kind() == "decorator"
            && c.utf8_text(src_bytes).is_ok_and(|t| {
                let t = t.trim_start_matches('@').trim();
                t == "overload" || t.ends_with(".overload")
            })
    });
    found
}

/// The node holding a definition's name when the query did not capture one.
///
/// Most grammars expose a `name` field; JavaScript's `field_definition` calls
/// it `property`. C and C++ bury it in the declarator chain —
/// `int *Foo::bar(int)` is pointer → function declarator → qualified
/// identifier → name — so that chain is walked down to the identifier.
fn def_name_node<'t>(node: &tree_sitter::Node<'t>) -> Option<tree_sitter::Node<'t>> {
    if let Some(n) = node
        .child_by_field_name("name")
        .or_else(|| node.child_by_field_name("property"))
    {
        return Some(n);
    }
    let mut cur = node.child_by_field_name("declarator")?;
    // Bounded: declarator chains are a handful of levels deep.
    for _ in 0..16 {
        cur = match cur.kind() {
            "qualified_identifier" => cur.child_by_field_name("name")?,
            k if is_name_kind(k) => return Some(cur),
            _ => cur
                .child_by_field_name("declarator")
                // `reference_declarator` holds its declarator as an unnamed-field child.
                .or_else(|| cur.named_child(cur.named_child_count().checked_sub(1)?))?,
        };
    }
    None
}

/// Node kinds that are a bare name. Computed keys, strings, operators,
/// destructuring patterns and type expressions (a Swift extension's
/// `user_type`) name nothing a caller could reference by name.
fn is_name_kind(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "type_identifier"
            | "field_identifier"
            | "property_identifier"
            | "private_property_identifier"
            | "simple_identifier"
            | "constant"
            | "name"
            | "destructor_name"
    )
}

/// Narrow a kind the grammar leaves ambiguous: Swift's `class_declaration`
/// is also struct / enum / actor / extension (`declaration_kind` says which),
/// and Kotlin's covers `interface` and `enum class` (a keyword child and an
/// `enum` modifier respectively).
fn refine_kind(
    spec: &LangSpec,
    node: &tree_sitter::Node,
    src_bytes: &[u8],
    kind: &NodeType,
) -> NodeType {
    if node.kind() != "class_declaration" {
        return kind.clone();
    }
    match spec.id {
        "swift" => match node
            .child_by_field_name("declaration_kind")
            .and_then(|k| k.utf8_text(src_bytes).ok())
        {
            Some("struct") => NodeType::Struct,
            Some("enum") => NodeType::Enum,
            _ => kind.clone(),
        },
        "kotlin" => {
            let mut cursor = node.walk();
            let children: Vec<_> = node.children(&mut cursor).collect();
            if children.iter().any(|c| c.kind() == "interface") {
                NodeType::Interface
            } else if children.iter().any(|c| {
                c.kind() == "modifiers"
                    && c.utf8_text(src_bytes)
                        .is_ok_and(|t| t.split_whitespace().any(|w| w == "enum"))
            }) {
                NodeType::Enum
            } else {
                kind.clone()
            }
        }
        _ => kind.clone(),
    }
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

/// Walk a definition node's preceding siblings for `#[...]` attribute items and
/// extract `(is_deprecated, labels)`. Recognised: `#[deprecated]`, `#[test]`,
/// `#[async_trait]`, `#[no_mangle]`, and any other attribute's identifier is
/// captured as a label.
fn collect_rust_metadata(node: &tree_sitter::Node, src_bytes: &[u8]) -> (bool, Vec<String>) {
    let mut is_deprecated = false;
    let mut labels = Vec::new();
    let Some(parent) = node.parent() else {
        return (false, labels);
    };

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
                    match p.kind() {
                        "identifier" => {
                            if let Ok(s) = p.utf8_text(src_bytes) {
                                labels.push(s.to_string());
                                if s == "deprecated" {
                                    is_deprecated = true;
                                }
                            }
                        }
                        // `#[tokio::test]`, `#[serial_test::serial]`:
                        // the attribute is a *scoped* identifier, which
                        // the identifier arm never saw — so the single
                        // most common async-test attribute in Rust
                        // produced no labels at all and its functions
                        // were indistinguishable from production code.
                        // The last segment is the meaningful one.
                        "scoped_identifier" => {
                            if let Ok(s) = p.utf8_text(src_bytes) {
                                if let Some(last) = s.rsplit("::").next() {
                                    labels.push(last.to_string());
                                    if last == "deprecated" {
                                        is_deprecated = true;
                                    }
                                }
                            }
                        }
                        _ => {}
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
        && s.chars()
            .all(|c| c.is_uppercase() || c == '_' || c.is_numeric())
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
    if s.starts_with('/')
        && (s.contains("Mutation") || s.contains("Query") || s.contains("Subscription"))
    {
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
        let calls: Vec<_> = refs
            .iter()
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
        let types: Vec<_> = refs
            .iter()
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

    @property
    def baz(self):
        return 2

@decorator
def wrapped():
    pass
"#;
        let defs = extract_definitions(Path::new("foo.py"), source);
        let names: Vec<_> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"hello"), "got: {:?}", names);
        assert!(names.contains(&"Foo"), "got: {:?}", names);
        // Methods and decorated definitions were invisible to the old
        // top-level-only walk.
        assert!(names.contains(&"bar"), "method; got: {:?}", names);
        assert!(names.contains(&"baz"), "decorated method; got: {:?}", names);
        assert!(
            names.contains(&"wrapped"),
            "decorated function; got: {:?}",
            names
        );
        let bar = defs.iter().find(|d| d.name == "bar").unwrap();
        let foo = defs.iter().find(|d| d.name == "Foo").unwrap();
        assert!(
            bar.line_end - bar.line_start < foo.line_end - foo.line_start,
            "method span nests inside its class so calls attribute to the method"
        );
    }

    #[test]
    fn test_extract_definitions_finds_typescript_symbols() {
        let source = r#"
import { thing } from './thing';

export interface Options { retry: number }
export enum Mode { A, B }

export class Ky {
    #options: Options;
    constructor(options: Options) { this.#options = options; }
    async #retry<T>(fn: () => Promise<T>): Promise<T> { return fn(); }
    private handle = (x: number): number => x + 1;
    static create(input: string): Ky { return new Ky({ retry: 2 }); }
}

export const mergeHeaders = (a: Headers, b: Headers): Headers => a;
export function plain(x: number): number { return x; }
export abstract class Base { abstract run(): void; }
const { destructured } = thing;
"#;
        let defs = extract_definitions(Path::new("ky.ts"), source);
        let names: Vec<_> = defs.iter().map(|d| d.name.as_str()).collect();
        for want in [
            "Options",
            "Mode",
            "Ky",
            "constructor",
            "#retry",
            "handle",
            "create",
            "mergeHeaders",
            "plain",
            "Base",
        ] {
            assert!(names.contains(&want), "missing {want}; got: {:?}", names);
        }
        assert!(
            !names.contains(&"destructured"),
            "patterns name nothing; got: {:?}",
            names
        );
        let kind = |n: &str| defs.iter().find(|d| d.name == n).unwrap().kind.clone();
        assert_eq!(kind("Ky"), NodeType::Class);
        assert_eq!(kind("Options"), NodeType::Interface);
        assert_eq!(kind("Mode"), NodeType::Enum);
        assert_eq!(kind("mergeHeaders"), NodeType::Function);
    }

    #[test]
    fn test_extract_definitions_finds_exported_js_symbols() {
        let source = r#"
export class Client {
    #secret = () => 1;
    send(req) { return this.#secret(); }
}
export const build = function (opts) { return opts; };
export default function main() {}
"#;
        let defs = extract_definitions(Path::new("client.js"), source);
        let names: Vec<_> = defs.iter().map(|d| d.name.as_str()).collect();
        for want in ["Client", "#secret", "send", "build", "main"] {
            assert!(names.contains(&want), "missing {want}; got: {:?}", names);
        }
    }

    #[test]
    fn test_extract_refs_finds_typescript_calls() {
        let source = r#"
export class Ky {
    async #retry(): Promise<Response> { return this.#fetch(); }
    #fetch(): Promise<Response> { return fetch(mergeHeaders(a, b)); }
}
"#;
        let refs = extract_refs_with_locals(Path::new("ky.ts"), source, &HashSet::new());
        let calls: Vec<_> = refs
            .iter()
            .filter(|r| matches!(r.edge_type, EdgeType::Calls))
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(calls.contains(&"mergeHeaders"), "got: {:?}", calls);
        assert!(
            calls.contains(&"#fetch"),
            "private method call; got: {:?}",
            calls
        );
    }

    /// A query that fails to compile silently turns a language off in
    /// production (see [`compiled`]); fail loudly here instead.
    #[test]
    fn every_language_compiles() {
        for spec in LANGS {
            if let Err(e) = compile(spec) {
                panic!("{e}");
            }
        }
    }

    fn def_names(file: &str, source: &str) -> Vec<(String, NodeType)> {
        extract_definitions(Path::new(file), source)
            .into_iter()
            .map(|d| (d.name, d.kind))
            .collect()
    }

    #[test]
    fn exported_call_bindings_are_definitions() {
        let src = r#"
import { defineStore } from 'pinia'
export const useCart = defineStore('cart', () => { total() })
const route = useRoute()
function f() { const inner = make() }
"#;
        for file in ["stores/cart.ts", "stores/cart.js"] {
            let defs = def_names(file, src);
            assert!(
                defs.contains(&("useCart".into(), NodeType::Constant)),
                "{file}: {defs:?}"
            );
            for absent in ["route", "inner"] {
                assert!(
                    !defs.iter().any(|(n, _)| n == absent),
                    "{file}: {absent} in {defs:?}"
                );
            }
        }
    }

    #[test]
    fn kotlin_negated_calls_are_calls() {
        let src =
            "fun f() {\n  if (!saveAsArg(a)) {}\n  val y = -offset(1)\n  if (!q.isDone()) {}\n}\n";
        let calls = call_names("A.kt", src);
        for want in ["saveAsArg", "offset", "isDone"] {
            assert!(
                calls.iter().any(|c| c == want),
                "{want} missing from {calls:?}"
            );
        }
    }

    fn call_names(file: &str, source: &str) -> Vec<String> {
        extract_refs(Path::new(file), source)
            .into_iter()
            .filter(|r| matches!(r.edge_type, EdgeType::Calls))
            .map(|r| r.target_name)
            .collect()
    }

    /// Each promised language: definitions (including methods nested in a
    /// type) and the calls between them.
    #[test]
    fn every_language_extracts_definitions_and_calls() {
        use NodeType::*;
        #[rustfmt::skip]
        let cases: &[(&str, &str, &[(&str, NodeType)], &[&str])] = &[
            ("main.go", r#"
package main
type Store struct { n int }
type Reader interface { Read() int }
func (s *Store) Load(k string) int { return helper(k) }
func helper(k string) int { s := &Store{}; return s.Load(k) }
"#, &[("Store", Struct), ("Reader", Interface), ("Load", Function), ("helper", Function)],
                &["helper", "Load"]),
            ("App.java", r#"
public class App {
    public App() {}
    void run() { Service s = new Service(); s.handle(1); }
}
interface Service { void handle(int x); }
enum Color { RED }
record Point(int x, int y) {}
"#, &[("App", Class), ("run", Function), ("Service", Interface), ("Color", Enum), ("Point", Class)],
                &["Service", "handle"]),
            ("lib.c", r#"
struct node { int v; };
enum mode { A, B };
static int *make(int n) { return compute(n); }
int compute(int n) { return n; }
"#, &[("node", Struct), ("mode", Enum), ("make", Function), ("compute", Function)],
                &["compute"]),
            ("lib.cpp", r#"
class Widget {
public:
    int draw() { return render(1); }
    ~Widget() {}
};
int Widget::render(int x) { return helper<int>(x); }
int &pick() { static int v; return v; }
"#, &[("Widget", Class), ("draw", Function), ("~Widget", Function), ("render", Function), ("pick", Function)],
                &["render", "helper"]),
            ("Svc.cs", r#"
namespace N {
    public class Svc {
        public Svc() {}
        public int Run() { var r = new Repo(); return r.Load<int>(1) + Helper(); }
        int Helper() => 1;
    }
    public interface IRepo {}
    public struct Pt {}
    public enum Mode { A }
}
"#, &[("Svc", Class), ("Run", Function), ("Helper", Function), ("IRepo", Interface), ("Pt", Struct), ("Mode", Enum)],
                &["Repo", "Load", "Helper"]),
            ("app.rb", r#"
module Billing
  class Invoice
    def total
      compute_tax(1)
    end
    def self.build
      Invoice.new
    end
  end
end
"#, &[("Billing", Module), ("Invoice", Class), ("total", Function), ("build", Function)],
                &["compute_tax", "Invoice"]),
            ("App.swift", r#"
protocol Store { func load() -> Int }
struct Box { var v: Int }
enum Mode { case a }
class Service {
    func run() -> Int { return helper(1) + store.load() }
}
func helper(_ x: Int) -> Int { x }
"#, &[("Store", Interface), ("Box", Struct), ("Mode", Enum), ("Service", Class), ("run", Function), ("helper", Function)],
                &["helper", "load"]),
            ("App.kt", r#"
interface Repo {
    fun load(): Int
}

enum class Mode { A, B }

object Registry {
    fun get(): Int = 1
}

class Service(val repo: Repo) {
    fun run(): Int = helper(repo.load())
}

fun helper(x: Int): Int = x
"#, &[("Repo", Interface), ("Mode", Enum), ("Registry", Class), ("Service", Class), ("run", Function), ("helper", Function)],
                &["helper", "load"]),
            ("App.scala", r#"
trait Repo { def load(): Int }
object Main { def main(args: Array[String]): Unit = helper(1) }
class Service(repo: Repo) { def run(): Int = repo.load() }
def helper(x: Int): Int = x
"#, &[("Repo", Trait), ("Main", Class), ("Service", Class), ("run", Function), ("helper", Function), ("load", Function)],
                &["helper", "load"]),
            ("app.php", r#"<?php
interface Repo { public function load(): int; }
trait Loggable {}
class Service {
    public function run(): int { $r = new Store(); return $r->load() + Util::help() + helper(); }
}
function helper(): int { return 1; }
"#, &[("Repo", Interface), ("Loggable", Trait), ("Service", Class), ("run", Function), ("helper", Function)],
                &["Store", "load", "help", "helper"]),
        ];
        for (file, src, want_defs, want_calls) in cases {
            let defs = def_names(file, src);
            for (name, kind) in *want_defs {
                assert!(
                    defs.iter().any(|(n, k)| n == name && k == kind),
                    "{file}: missing {name} as {kind:?}; got {defs:?}"
                );
            }
            let calls = call_names(file, src);
            for name in *want_calls {
                assert!(
                    calls.iter().any(|c| c == name),
                    "{file}: missing call {name}; got {calls:?}"
                );
            }
        }
    }

    /// Receiver detection across grammars: `d.update()` is foreign,
    /// `self.update()` / `this.update()` and a bare `update()` are not.
    #[test]
    fn foreign_receivers_are_detected() {
        let cases: &[(&str, &str)] = &[
            (
                "a.py",
                "def f(self, d):\n    d.update(1)\n    self.update(2)\n    update(3)\n",
            ),
            (
                "a.ts",
                "function f(d) { d.update(1); this.update(2); update(3); }",
            ),
            (
                "a.rs",
                "fn f(d: D) { d.update(1); self.update(2); update(3); }",
            ),
            ("a.go", "func f(d D) { d.update(1); update(3) }"),
            (
                "A.java",
                "class A { void f(D d) { d.update(1); this.update(2); update(3); } }",
            ),
            (
                "a.rb",
                "def f(d)\n  d.update(1)\n  self.update(2)\n  update(3)\nend\n",
            ),
            (
                "a.php",
                "<?php function f($d) { $d->update(1); $this->update(2); parent::update(3); update(4); }",
            ),
            (
                "b.py",
                "class A(B):\n    def f(self, d):\n        d.update(1)\n        super().update(2)\n",
            ),
            (
                "A.cs",
                "class A { void F(D d) { d.update(1); this.update(2); update(3); } }",
            ),
            (
                "a.kt",
                "fun f(d: D) {\n    d.update(1)\n    this.update(2)\n    update(3)\n}\n",
            ),
            (
                "a.swift",
                "func f(d: D) {\n    d.update(1)\n    self.update(2)\n    update(3)\n}\n",
            ),
        ];
        for (file, src) in cases {
            let flags: Vec<bool> = extract_refs(Path::new(file), src)
                .into_iter()
                .filter(|r| r.target_name == "update" && matches!(r.edge_type, EdgeType::Calls))
                .map(|r| r.foreign_receiver)
                .collect();
            assert_eq!(
                flags.first(),
                Some(&true),
                "{file}: d.update(); got {flags:?}"
            );
            assert!(
                flags.iter().skip(1).all(|f| !f),
                "{file}: self/bare calls; got {flags:?}"
            );
        }
    }

    /// `@overload` stubs are type signatures; only the implementation is
    /// a definition.
    #[test]
    fn python_overload_stubs_are_not_definitions() {
        let src = "from typing import overload\n\n@overload\ndef f(x: int) -> int: ...\n@typing.overload\ndef f(x: str) -> str: ...\ndef f(x):\n    return x\n";
        let defs = extract_definitions(Path::new("m.py"), src);
        let fs: Vec<_> = defs.iter().filter(|d| d.name == "f").collect();
        assert_eq!(fs.len(), 1, "got {defs:?}");
        assert_eq!(fs[0].line_start, 6);
    }

    /// Single-file components: the `<script>` block parses as TS/JS and every
    /// line number still points into the original file.
    #[test]
    fn vue_and_svelte_scripts_are_indexed_in_place() {
        let vue = "<template>\n  <div>{{ total() }}</div>\n</template>\n<script setup lang=\"ts\">\nimport { load } from './api'\nconst total = (): number => load(1)\n</script>\n";
        let defs = extract_definitions(Path::new("Cart.vue"), vue);
        let total = defs.iter().find(|d| d.name == "total").expect("total");
        assert_eq!(total.line_start, 5, "line points into the .vue file");
        let calls: Vec<_> = extract_refs(Path::new("Cart.vue"), vue)
            .into_iter()
            .filter(|r| matches!(r.edge_type, EdgeType::Calls))
            .map(|r| (r.target_name, r.source_line))
            .collect();
        assert!(calls.contains(&("load".to_string(), 5)), "got {calls:?}");

        let svelte = "<script>\n  function inc() { bump(1) }\n</script>\n<button on:click={inc}>é</button>\n";
        let names = def_names("Counter.svelte", svelte);
        assert!(names.iter().any(|(n, _)| n == "inc"), "got {names:?}");
    }

    #[test]
    fn string_delimiters_are_stripped() {
        assert_eq!(strip_string_delimiters("\"/api/v1\""), "/api/v1");
        assert_eq!(strip_string_delimiters("r#\"/api/v1\"#"), "/api/v1");
        assert_eq!(strip_string_delimiters("f'/api/{x}'"), "/api/{x}");
        assert_eq!(strip_string_delimiters("@\"/api\""), "/api");
        assert_eq!(strip_string_delimiters("`/api/v1`"), "/api/v1");
        assert_eq!(strip_string_delimiters("\"\"\"doc\"\"\""), "doc");
        assert_eq!(strip_string_delimiters("\"$HOME\""), "$HOME");
    }

    /// Rust's string node is `string_literal`; the old shared `(string)`
    /// query never matched it, so Rust had no string literals at all.
    #[test]
    fn rust_string_literals_are_found() {
        let src = "fn r() { get(\"/api/v1/users\"); }\n";
        let lits = extract_strings(Path::new("r.rs"), src);
        assert!(
            lits.iter().any(|l| l.value == "/api/v1/users"),
            "got {lits:?}"
        );
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
        let calls: Vec<_> = refs
            .iter()
            .filter(|r| matches!(r.edge_type, EdgeType::Calls))
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(
            calls.contains(&"process"),
            "should find process even if in locals"
        );
    }
}
