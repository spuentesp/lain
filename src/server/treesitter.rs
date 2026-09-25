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
    /// A call on the caller's own object (`self.x()`, `this.x()`, a Go
    /// method's receiver variable).
    pub self_receiver: bool,
    /// `Registry` in `Registry::new()`: the type or module a path-qualified
    /// call names.
    pub qualifier: Option<String>,
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
    MEMBER_FUNCTION_ASSIGNMENT,
];
/// `res.sendStatus = function sendStatus(…) {…}`, `exports.parse = (…) => …`,
/// `Foo.prototype.bar = function () {…}`: how pre-class JavaScript defines
/// methods — all of Express's `app` and `res` API. Named by the property,
/// which is what callers write (`res.sendStatus(404)`). Assigning to
/// `module.exports` itself names no method and is left out.
const MEMBER_FUNCTION_ASSIGNMENT: (&str, NodeType) = (
    "((assignment_expression left: (member_expression object: (_) @obj property: (property_identifier) @name) \
     right: [(function_expression) (arrow_function) (generator_function)]) @d \
     (#not-eq? @name \"exports\") \
     (#not-match? @obj \"^(console|window|global|globalThis|document|process|navigator|self|Math|JSON|Object|Array|Promise)$\"))",
    NodeType::Function,
);
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
    MEMBER_FUNCTION_ASSIGNMENT,
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
            // Inside a macro (`format!`, `writeln!`, `assert_eq!`, `vec!`)
            // the arguments are an unparsed token tree: a call is a name
            // directly followed by a `(…)` group. Without this every call
            // made inside a macro was invisible.
            "((token_tree (identifier) @name . (token_tree) @args) (#match? @args \"^\\\\(\"))",
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
            "(call_expression function: (template_function name: (identifier) @name))",
            // `a::f()`, `a::b::f()`, `detail::check_signed_range<T>(…)`:
            // each `::` nests another `qualified_identifier`, and queries
            // cannot recurse, so the depths are spelled out. Only the first
            // level was matched, which missed every call through a nested
            // namespace.
            "(call_expression function: [\
               (qualified_identifier name: [(identifier) @name (template_function name: (identifier) @name)])\
               (qualified_identifier name: (qualified_identifier name: [(identifier) @name (template_function name: (identifier) @name)]))\
               (qualified_identifier name: (qualified_identifier name: (qualified_identifier name: [(identifier) @name (template_function name: (identifier) @name)])))\
               (qualified_identifier name: (qualified_identifier name: (qualified_identifier name: (qualified_identifier name: [(identifier) @name (template_function name: (identifier) @name)]))))])",
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
            // Implicit member: `session.request(.basicAuth())`, the type
            // coming from context.
            "(call_expression (prefix_expression \".\" (simple_identifier) @name))",
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
    if ext == "cs" {
        return Some((spec_for_ext(ext)?, blank_cs_conditionals(source)));
    }
    let spec = spec_for_ext(ext)?;
    if spec.id == "c" || spec.id == "cpp" {
        return Some((spec, blank_attribute_macro_lines(spec, source)));
    }
    Some((spec_for_ext(ext)?, Cow::Borrowed(source)))
}

/// Blank C/C++ lines holding nothing but an ALL-CAPS macro name, where the
/// parser fails around them — keeping byte offsets and line numbers.
///
/// An attribute-like macro on its own line (`CXXOPTS_NODISCARD`,
/// `API_EXPORT`) expands to nothing the parser can see; it reads the macro
/// as the return type and recovers badly — cxxopts' `make_storage()` was
/// lost that way. But a lone upper-case word is just as often the return
/// type itself (`DWORD`, `HRESULT`, `BOOL` on the line above the name, the
/// Windows style), and blanking that lost the function instead. So only
/// lines within two lines of a parse error are blanked: where the grammar
/// already copes, the source is left alone.
fn blank_attribute_macro_lines<'a>(spec: &'static LangSpec, source: &'a str) -> Cow<'a, str> {
    let is_macro_line = |line: &str| {
        let t = line.trim();
        t.len() >= 3
            && t.starts_with(|c: char| c.is_ascii_uppercase())
            && t.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    };
    if !source.lines().any(is_macro_line) {
        return Cow::Borrowed(source);
    }
    let Some(tree) = compiled(spec).and_then(|c| parse(&c.language, source)) else {
        return Cow::Borrowed(source);
    };
    if !tree.root_node().has_error() {
        return Cow::Borrowed(source);
    }
    // Rows where an ERROR node starts, or a node is missing.
    let mut error_rows = std::collections::BTreeSet::new();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if !n.has_error() {
            continue;
        }
        if n.is_error() || n.is_missing() {
            error_rows.insert(n.start_position().row);
        }
        let mut c = n.walk();
        stack.extend(n.children(&mut c));
    }
    let near_error = |row: usize| error_rows.range(row..=row + 2).next().is_some();
    // In a run of such lines directly above a declarator (`name(`,
    // `Cls::name(`), the first is the return type (`DWORD` above `WINAPI`
    // above `worker1(`) and stays; the rest are calling-convention or
    // attribute macros. Above a type line (cxxopts' macro above
    // `std::shared_ptr<Value>`), all of the run is macros.
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let is_declarator = |line: &str| {
        let t = line.trim_start();
        let name_end = t
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':' || c == '~'))
            .unwrap_or(t.len());
        name_end > 0 && t[name_end..].trim_start().starts_with('(')
    };
    let mut blank = vec![false; lines.len()];
    let mut row = 0;
    while row < lines.len() {
        if !is_macro_line(lines[row]) {
            row += 1;
            continue;
        }
        let start = row;
        while row < lines.len() && is_macro_line(lines[row]) {
            row += 1;
        }
        let keeps_return_type = lines.get(row).is_some_and(|l| is_declarator(l));
        for (r, b) in blank.iter_mut().enumerate().take(row).skip(start) {
            *b = near_error(r) && !(keeps_return_type && r == start);
        }
    }
    let mut changed = false;
    let mut out = String::with_capacity(source.len());
    for (row, line) in lines.iter().enumerate() {
        if blank[row] {
            changed = true;
            out.extend(
                line.chars()
                    .map(|c| if c == '\n' || c == '\r' { c } else { ' ' }),
            );
        } else {
            out.push_str(line);
        }
    }
    if changed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(source)
    }
}

/// Keep one configuration of C# conditional compilation: the first branch
/// of every `#if`. Directive lines and the bodies of `#elif`/`#else`
/// branches are blanked, keeping byte offsets and line numbers.
///
/// The grammar handles a directive between statements but not inside an
/// expression, and it does not recover: commandlineparser's
/// `TypeConverter.cs` splits a ternary with `#if !SKIP_FSHARP`, the whole
/// file parsed as one `ERROR`, and every method after it was lost. Keeping
/// *both* branches instead turned `#if NET6_0 sealed class Widget #else
/// class Widget #endif { … }` into two nested classes.
fn blank_cs_conditionals(source: &str) -> Cow<'_, str> {
    // `#if X`, `# if X`, `#if(X)`, `#endif//note` — `#` then optional
    // blanks, then the directive word not followed by an identifier char.
    let directive = |line: &str| -> Option<&'static str> {
        let rest = line.trim_start().strip_prefix('#')?.trim_start();
        ["if", "elif", "else", "endif"].into_iter().find(|d| {
            rest.strip_prefix(d)
                .is_some_and(|r| !r.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_'))
        })
    };
    if !source.lines().any(|l| directive(l).is_some()) {
        return Cow::Borrowed(source);
    }
    // One entry per open `#if`: whether this level is in a skipped branch.
    let mut skipping: Vec<bool> = Vec::new();
    let mut out = String::with_capacity(source.len());
    for line in source.split_inclusive('\n') {
        let outer_skipped = skipping.iter().any(|&s| s);
        let blank_line = match directive(line) {
            Some("if") => {
                skipping.push(false);
                true
            }
            Some("elif") | Some("else") => {
                if let Some(top) = skipping.last_mut() {
                    *top = true;
                }
                true
            }
            Some(_) => {
                skipping.pop();
                true
            }
            None => outer_skipped,
        };
        if blank_line {
            out.extend(
                line.chars()
                    .map(|c| if c == '\n' || c == '\r' { c } else { ' ' }),
            );
        } else {
            out.push_str(line);
        }
    }
    Cow::Owned(out)
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
///
/// Names the file defines itself are passed as locals, so a method that
/// shares a builtin's name still gets its calls: phpdotenv's
/// `Validator::assert` is called as `$this->assert(…)`, and with no locals
/// the builtin blocklist dropped every such call.
pub fn extract_refs(path: &Path, source: &str) -> Vec<StaticRef> {
    let locals: HashSet<String> = extract_definitions(path, source)
        .into_iter()
        .map(|d| d.name)
        .collect();
    extract_refs_with_locals(path, source, &locals)
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
                if !is_adjacent_macro_call(&cap.node) {
                    continue;
                }
                if let Ok(name) = cap.node.utf8_text(src_bytes) {
                    let recv = if edge_type == EdgeType::Calls {
                        call_receiver(&cap.node, src_bytes)
                    } else {
                        Receiver::None
                    };
                    // `Registry::new()`: a type-qualified call names its
                    // target precisely, so a builtin-looking name (`new`)
                    // is kept; the resolver matches it to Registry's `new`
                    // or to nothing.
                    let type_qualified = matches!(&recv, Receiver::Qualified(q)
                        if q.starts_with(|c: char| c.is_ascii_uppercase()));
                    if type_qualified || keep(name) {
                        refs.push(StaticRef {
                            source_line: cap.node.start_position().row as u32,
                            target_name: name.to_string(),
                            edge_type: edge_type.clone(),
                            foreign_receiver: matches!(
                                recv,
                                Receiver::Foreign | Receiver::Qualified(_)
                            ),
                            self_receiver: recv == Receiver::OwnSelf,
                            qualifier: match recv {
                                Receiver::Qualified(q) => Some(q),
                                _ => None,
                            },
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
    if spec.id == "c" || spec.id == "cpp" {
        refs.extend(macro_body_calls(&tree, src_bytes, local_definitions));
    }
    refs
}

/// Words followed by `(` in C/C++ that are not calls.
const C_KEYWORDS: &[&str] = &[
    "if",
    "while",
    "for",
    "switch",
    "return",
    "sizeof",
    "alignof",
    "_Alignof",
    "offsetof",
    "defined",
    "typeof",
    "__typeof__",
    "decltype",
    "static_assert",
    "_Static_assert",
    "static_cast",
    "dynamic_cast",
    "reinterpret_cast",
    "const_cast",
    "noexcept",
    "alignas",
    "__attribute__",
    "__declspec",
    "do",
    "else",
    "case",
];

/// Calls written inside `#define` bodies.
///
/// The grammar keeps a macro's body as raw text, so a function a macro
/// expands to had no caller at all — in Unity (cJSON's test framework)
/// every `TEST_ASSERT_EACH_EQUAL_*` expands to `UnityNumToPtr(…)`, and
/// "who calls `UnityNumToPtr`" answered only the one direct call. Each
/// `name(` in the body counts, at the `#define`'s line; outside any
/// function, that attributes it to the header, like other module-scope
/// calls. The resolver still links only names the repo defines, so a
/// macro calling another macro adds nothing.
fn macro_body_calls(
    tree: &tree_sitter::Tree,
    src: &[u8],
    local_definitions: &HashSet<String>,
) -> Vec<StaticRef> {
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "preproc_function_def" | "preproc_def") {
            let Some(body) = node.child_by_field_name("value") else {
                continue;
            };
            let Ok(text) = body.utf8_text(src) else {
                continue;
            };
            let bytes = text.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                let c = bytes[i];
                if c.is_ascii_alphabetic() || c == b'_' {
                    let start = i;
                    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
                    {
                        i += 1;
                    }
                    let preceded = start > 0 && matches!(bytes[start - 1], b'.' | b'>' | b'#');
                    if !preceded && bytes.get(i) == Some(&b'(') {
                        let name = &text[start..i];
                        if !C_KEYWORDS.contains(&name)
                            && is_user_defined_call(name, local_definitions)
                        {
                            let line = text[..start].matches('\n').count();
                            out.push(StaticRef {
                                source_line: (body.start_position().row + line) as u32,
                                target_name: name.to_string(),
                                edge_type: EdgeType::Calls,
                                foreign_receiver: false,
                                self_receiver: false,
                                qualifier: None,
                            });
                        }
                    }
                } else {
                    i += 1;
                }
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    out
}

/// Whether a called name is reached through a receiver other than
/// `self` / `this`: `d.update()`, `this.items.push()`, `list.get(0)`.
///
/// Walks up from the name to the member-access node (whatever the grammar
/// calls it) and reads its receiver field; stops at the call node itself,
/// which in Java, Ruby and PHP carries the receiver directly.
/// Inside a macro's token tree, only `name(` with nothing between counts as
/// a call. A space means some other syntax: `just`'s test macros hold an
/// S-expression DSL, `(call env_var_or_default (+ "a" "b"))`, which the
/// token-tree call pattern otherwise read as a call. Real calls are written
/// without the space. Always true outside token trees.
fn is_adjacent_macro_call(name: &tree_sitter::Node) -> bool {
    if !name.parent().is_some_and(|p| p.kind() == "token_tree") {
        return true;
    }
    name.next_sibling()
        .is_some_and(|args| args.start_byte() == name.end_byte())
}

/// Who a call is made on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Receiver {
    /// A bare call: `helper()`.
    None,
    /// The caller's own object: `self.x()`, `this.x()`, Go's receiver
    /// variable (`s.update()` inside `func (s *Server) …`), `Self::new()`.
    OwnSelf,
    /// Some other value: `d.update()`, `child.walk()`.
    Foreign,
    /// A type or module path: `Registry::new()`, `String::new()`,
    /// `Foo::bar()` — the last path segment.
    Qualified(String),
}

const SELF_LIKE: &[&str] = &[
    "self", "this", "cls", "super", "Self", "$this", "base", "parent", "static",
];

/// The receiver of the call whose callee name is `name`.
pub(crate) fn call_receiver(name: &tree_sitter::Node, src_bytes: &[u8]) -> Receiver {
    const RECEIVER_FIELDS: &[&str] = &[
        "object",
        "receiver",
        "operand",
        "value",
        "argument",
        "expression",
        "target",
    ];
    let text_of = |n: tree_sitter::Node| {
        n.utf8_text(src_bytes)
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    let classify = |text: &str| -> Receiver {
        // `super().update()` (Python) and `parent::get()` / `static::`
        // (PHP) reach the class's own hierarchy, like `self`.
        if SELF_LIKE.contains(&text)
            || text.starts_with("super(")
            || go_receiver_name(name, src_bytes).as_deref() == Some(text)
        {
            Receiver::OwnSelf
        } else {
            Receiver::Foreign
        }
    };
    // Swift implicit member (`.basicAuth()`): the receiver is a type the
    // context implies, never `self`.
    if name.parent().is_some_and(|p| {
        p.kind() == "prefix_expression" && p.child(0).is_some_and(|c| c.kind() == ".")
    }) {
        return Receiver::Foreign;
    }
    // Token-tree calls (inside a Rust macro) have no call node: the
    // receiver, if any, is the token before a preceding `.`.
    if name.parent().is_some_and(|p| p.kind() == "token_tree") {
        let Some(dot) = name.prev_sibling().filter(|p| p.kind() == ".") else {
            return Receiver::None;
        };
        let text = dot.prev_sibling().map(text_of).unwrap_or_default();
        return classify(&text);
    }
    let mut node = *name;
    for _ in 0..4 {
        let Some(parent) = node.parent() else {
            return Receiver::None;
        };
        // A path: Rust `a::b::f` (`path`), C++ `A::f` / PHP `A::f` (`scope`).
        let scope = match parent.kind() {
            "scoped_identifier" => parent.child_by_field_name("path"),
            "qualified_identifier" | "scoped_call_expression" => {
                parent.child_by_field_name("scope")
            }
            _ => None,
        }
        .filter(|r| r.end_byte() <= name.start_byte());
        if let Some(r) = scope {
            let text = text_of(r);
            let last = text
                .rsplit("::")
                .next()
                .unwrap_or(&text)
                .trim_start_matches('\\');
            let last = last.split('<').next().unwrap_or(last).to_string();
            return if SELF_LIKE.contains(&last.as_str()) {
                Receiver::OwnSelf
            } else {
                Receiver::Qualified(last)
            };
        }
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
            return classify(&text_of(r));
        }
        let kind = parent.kind();
        if kind.contains("call") || kind.contains("invocation") {
            return Receiver::None;
        }
        node = parent;
    }
    Receiver::None
}

/// In a Go method, the receiver variable's name (`s` in
/// `func (s *Server) Refresh()`): calls on it are calls on the method's own
/// value, like `self`.
fn go_receiver_name(name: &tree_sitter::Node, src_bytes: &[u8]) -> Option<String> {
    let mut cur = name.parent();
    while let Some(n) = cur {
        if n.kind() == "method_declaration" {
            let recv = n.child_by_field_name("receiver")?;
            let mut c = recv.walk();
            let param = recv.named_children(&mut c).next()?;
            return param
                .child_by_field_name("name")
                .and_then(|id| id.utf8_text(src_bytes).ok())
                .map(str::to_string);
        }
        if n.kind() == "function_declaration" || n.kind() == "source_file" {
            return None;
        }
        cur = n.parent();
    }
    None
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
    /// The type the definition belongs to; see [`container_of`].
    pub container: Option<String>,
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
            container: container_of(&node, src_bytes),
        });
    }
    defs
}

/// Name of the type a definition belongs to: the nearest enclosing class,
/// struct, interface, trait, `impl` block, object, protocol or Ruby
/// module; a Go method's receiver type; the qualifier of an out-of-class
/// C++ definition (`Foo::bar`). `None` for a free function.
fn container_of(node: &tree_sitter::Node, src: &[u8]) -> Option<String> {
    let text = |n: tree_sitter::Node| n.utf8_text(src).ok().map(str::to_string);
    // Strip generics and pointers: `Registry<T>` / `*Server` -> base name.
    let base = |t: String| -> Option<String> {
        let t = t.trim_start_matches(['*', '&', ' ']);
        let end = t
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(t.len());
        (end > 0).then(|| t[..end].to_string())
    };
    match node.kind() {
        // Go: `func (s *Server) Refresh()`.
        "method_declaration" if node.child_by_field_name("receiver").is_some() => {
            let recv = node.child_by_field_name("receiver")?;
            let mut stack = vec![recv];
            while let Some(n) = stack.pop() {
                if n.kind() == "type_identifier" {
                    return text(n).and_then(base);
                }
                let mut c = n.walk();
                stack.extend(n.children(&mut c));
            }
        }
        // Kotlin extension function: `fun ArgParser.avoidProcessExit()` is
        // called as `parser.avoidProcessExit()`, a member of its receiver
        // type. As a container-less "free function" the resolver dropped
        // every such call made through a receiver.
        "function_declaration" => {
            let mut c = node.walk();
            let kids: Vec<_> = node.children(&mut c).collect();
            if let Some(dot) = kids.iter().position(|k| k.kind() == ".") {
                if let Some(recv) = dot.checked_sub(1).map(|i| kids[i]) {
                    if matches!(recv.kind(), "user_type" | "nullable_type") {
                        return text(recv).and_then(base);
                    }
                }
            }
        }
        // C++: `void Foo::bar() {}` outside the class.
        "function_definition" => {
            let mut d = node.child_by_field_name("declarator");
            while let Some(n) = d {
                if n.kind() == "qualified_identifier" {
                    if let Some(scope) = n.child_by_field_name("scope") {
                        return text(scope).and_then(base);
                    }
                }
                // `int& Foo::bar()`: a reference declarator holds the
                // function declarator as an unnamed child, not a field.
                d = n.child_by_field_name("declarator").or_else(|| {
                    let mut c = n.walk();
                    let next = n
                        .named_children(&mut c)
                        .find(|k| k.kind().ends_with("declarator"));
                    next
                });
            }
        }
        _ => {}
    }
    let mut cur = node.parent();
    while let Some(n) = cur {
        // The file's own root (Python's is also called `module`).
        n.parent()?;
        let k = n.kind();
        // A function nested in a method is local to it, not a member.
        if k.contains("function")
            || k.contains("method")
            || k.contains("closure")
            || k.contains("lambda")
        {
            return None;
        }
        let is_type = k.contains("class")
            || k.contains("struct")
            || k.contains("interface")
            || k.contains("trait")
            || k.contains("protocol")
            || k.contains("record_declaration")
            || k.contains("enum_declaration")
            || k.contains("extension")
            || k == "impl_item"
            || k == "object_declaration"
            || k == "object_definition"
            || k == "module";
        if is_type && !k.ends_with("_body") && !k.contains("specifier_list") {
            let name = n
                .child_by_field_name("name")
                .or_else(|| n.child_by_field_name("type"));
            if let Some(name) = name.and_then(text).and_then(base) {
                return Some(name);
            }
        }
        cur = n.parent();
    }
    None
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
    // Locally-defined symbols override every filter below — including the
    // length one: `def g(): …; g()` is a real call, and one-letter names
    // were dropped before this check was reached.
    if local_definitions.contains(name) {
        return true;
    }
    if name.len() <= 1 {
        return false;
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
    fn receivers_are_classified() {
        let recv = |file: &str, src: &str, name: &str| {
            let r = extract_refs(Path::new(file), src)
                .into_iter()
                .find(|r| r.target_name == name)
                .unwrap_or_else(|| panic!("{name} not extracted from {file}"));
            (r.foreign_receiver, r.self_receiver, r.qualifier)
        };
        // Go: the method's receiver variable is its own value.
        let go = "package p\nfunc (s *Server) Refresh() { s.update(); other.update2() }\n";
        assert_eq!(recv("a.go", go, "update"), (false, true, None));
        assert_eq!(recv("a.go", go, "update2"), (true, false, None));
        // Rust paths.
        let rs =
            "fn f() { let r = Registry::new(); let s = String::from(\"x\"); Self::build(); }\n";
        assert_eq!(
            recv("a.rs", rs, "new"),
            (true, false, Some("Registry".into()))
        );
        assert_eq!(recv("a.rs", rs, "build"), (false, true, None));
        // Python self.
        let py = "class C:\n    def f(self):\n        self.load(1)\n        util.parse(2)\n";
        assert_eq!(recv("a.py", py, "load"), (false, true, None));
        assert_eq!(recv("a.py", py, "parse"), (true, false, None));
    }

    #[test]
    fn assigning_to_a_global_is_not_a_definition() {
        let src = "console.error = (...a) => {};\nglobal.fetch = async () => ({});\nres.sendStatus = function (c) {};\n";
        let defs = def_names("jest.setup.js", src);
        assert!(defs.iter().any(|(n, _)| n == "sendStatus"), "{defs:?}");
        assert!(
            !defs.iter().any(|(n, _)| n == "error" || n == "fetch"),
            "{defs:?}"
        );
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn definitions_know_their_container() {
        let cases: &[(&str, &str, &[(&str, Option<&str>)])] = &[
            ("a.py", "def load(p): pass\nclass Cache:\n    def load(self, k): pass\n",
             &[("load", None), ("Cache", None)]),
            ("a.rs", "struct Registry;\nimpl Registry {\n    fn new() -> Self { Registry }\n}\nfn new() {}\n",
             &[("new", Some("Registry"))]),
            ("a.go", "package p\ntype Server struct{}\nfunc (s *Server) Refresh() {}\nfunc Free() {}\n",
             &[("Refresh", Some("Server")), ("Free", None)]),
            ("a.cpp", "class Foo { void inl() {} };\nvoid Foo::bar() {}\nvoid free_fn() {}\nint const& Foo::ref() const { return x; }\nint* Foo::ptr() { return 0; }\n",
             &[("inl", Some("Foo")), ("bar", Some("Foo")), ("free_fn", None), ("ref", Some("Foo")), ("ptr", Some("Foo"))]),
            ("a.ts", "class Widget { render() {} }\nfunction render2() {}\n",
             &[("render", Some("Widget")), ("render2", None)]),
            ("a.rb", "class Tree\n  def walk(v)\n  end\nend\n", &[("walk", Some("Tree"))]),
            ("b.py", "class C:\n    def run(self):\n        def inner():\n            pass\n", &[("run", Some("C")), ("inner", None)]),
            ("a.kt", "fun ArgParser.quiet() = 1\nfun <T> List<T>.second(): T = this[1]\nfun top() {}\n",
             &[("quiet", Some("ArgParser")), ("second", Some("List")), ("top", None)]),
        ];
        for (file, src, want) in cases {
            let defs = extract_definitions(Path::new(file), src);
            for (name, container) in *want {
                let found: Vec<_> = defs
                    .iter()
                    .filter(|d| d.name == *name)
                    .map(|d| d.container.as_deref())
                    .collect();
                assert!(
                    found.contains(container),
                    "{file}: {name} containers {found:?}, want {container:?}"
                );
            }
        }
        // Python: the method's container is the class.
        let defs = extract_definitions(
            Path::new("a.py"),
            "class Cache:\n    def load(self, k): pass\n",
        );
        assert_eq!(
            defs.iter()
                .find(|d| d.name == "load")
                .unwrap()
                .container
                .as_deref(),
            Some("Cache")
        );
    }

    #[test]
    fn upper_case_return_types_on_their_own_line_are_kept() {
        let src = "DWORD\nWINAPI\nworker1(void *p)\n{ return helper(2); }\n\
                   UINT\nworker4(void *p) { return helper(4); }\n\
                   HRESULT\nFoo::worker7() { return 0; }\n";
        for file in ["w.c", "w.cpp"] {
            let defs = def_names(file, src);
            for want in ["worker1", "worker4"] {
                assert!(
                    defs.iter().any(|(n, _)| n == want),
                    "{file}: {want} lost: {defs:?}"
                );
            }
        }
        let w7 = extract_definitions(Path::new("w.cpp"), src)
            .into_iter()
            .find(|d| d.name == "worker7")
            .expect("worker7");
        assert_eq!(w7.line_start, 6, "starts at its return type line");
    }

    #[test]
    fn attribute_macro_lines_do_not_hide_definitions() {
        let src = "class O {\n  CXXOPTS_NODISCARD\n  std::shared_ptr<Value>\n  make_storage() const\n  {\n    return m_value->clone();\n  }\n};\n\
                   void use(O* details) { details->make_storage(); }\n";
        let defs = def_names("cxxopts.hpp", src);
        let storage = extract_definitions(Path::new("cxxopts.hpp"), src)
            .into_iter()
            .find(|d| d.name == "make_storage")
            .unwrap_or_else(|| panic!("make_storage lost: {defs:?}"));
        assert_eq!(
            storage.line_start, 2,
            "line numbers must survive the blanking"
        );
        assert!(call_names("cxxopts.hpp", src)
            .iter()
            .any(|c| c == "make_storage"));
    }

    #[test]
    fn calls_inside_c_macro_bodies_are_calls() {
        let src =
            "#define ASSERT_EACH(e, n) UnityAssertArray(UnityNumToPtr((int)e, sizeof(int)), n)\n\
                   #define PTR_OF(x) (x)->field\n\
                   #define STRINGIFY(x) #x\n\
                   #define LOOP(x) do { if (x) { while (x) {} } } while (0)\n\
                   int helper(void);\n";
        let calls = call_names("unity_internals.h", src);
        for want in ["UnityAssertArray", "UnityNumToPtr"] {
            assert!(
                calls.iter().any(|c| c == want),
                "{want} missing from {calls:?}"
            );
        }
        assert!(
            !calls.iter().any(|c| c == "sizeof" || c == "field"),
            "{calls:?}"
        );
    }

    #[test]
    fn swift_implicit_member_calls_are_calls() {
        let src = "func t() {\n  session.request(.basicAuth(), interceptor: h)\n  let e: Endpoint = .makeEndpoint(forUser: \"u\")\n}\n";
        let refs = extract_refs(Path::new("Tests/SessionTests.swift"), src);
        for want in ["basicAuth", "makeEndpoint"] {
            let r = refs
                .iter()
                .find(|r| r.target_name == want)
                .unwrap_or_else(|| panic!("{want} missing"));
            assert!(r.foreign_receiver, "{want}: the implied type is not self");
        }
    }

    #[test]
    fn a_method_named_like_a_builtin_keeps_its_same_file_calls() {
        let src = "<?php\nclass Validator {\n  public function required() { return $this->assert(fn() => true, 'x'); }\n  \
                   public function assert(callable $c, string $m) { return $this; }\n}\n";
        let calls = call_names("src/Validator.php", src);
        assert!(calls.iter().any(|c| c == "assert"), "{calls:?}");
        // A file that does not define it still treats it as the builtin.
        let calls = call_names("src/Other.php", "<?php\nfunction f() { assert(true); }\n");
        assert!(!calls.iter().any(|c| c == "assert"), "{calls:?}");
    }

    #[test]
    fn csharp_if_else_class_headers_give_one_class() {
        let src = "#if NET6_0\npublic sealed class Widget : IDisposable\n#else\npublic class Widget\n#endif\n{\n\
                   # if (DEBUG)\n    void Trace() {}\n#endif//debug\n    public void Run() { Trace(); }\n}\n";
        let widgets = def_names("A.cs", src)
            .into_iter()
            .filter(|(n, _)| n == "Widget")
            .count();
        assert_eq!(widgets, 1);
        assert!(def_names("A.cs", src).iter().any(|(n, _)| n == "Run"));
    }

    #[test]
    fn csharp_directives_inside_expressions_do_not_lose_the_file() {
        let src = "class T {\n\
            static object A(bool f) {\n\
                System.Func<object> g = () =>\n\
            #if !SKIP\n\
                    f ? Helper.Convert() :\n\
            #endif\n\
                    Helper.Fallback();\n\
                return Build(f);\n\
            }\n\
            static object Build(bool f) { return null; }\n\
            }\n";
        let defs = def_names("T.cs", src);
        assert!(
            defs.iter().any(|(n, _)| n == "Build"),
            "method after the directive lost: {defs:?}"
        );
        let calls = call_names("T.cs", src);
        for want in ["Convert", "Fallback", "Build"] {
            assert!(
                calls.iter().any(|c| c == want),
                "{want} missing from {calls:?}"
            );
        }
        // Lines are preserved: `Build` is still defined on line 10 (0-based 9).
        let b = extract_definitions(Path::new("T.cs"), src)
            .into_iter()
            .find(|d| d.name == "Build")
            .unwrap();
        assert_eq!(b.line_start, 9);
    }

    #[test]
    fn cpp_qualified_template_calls_are_calls() {
        let src = "void f() { detail::check_signed_range<T>(neg, v, t); a::b::convert<int>(x); \
                   ns1::ns2::plain_call(1); ns1::ns2::ns3::deep_call(); single::level_call(); }";
        let calls = call_names("x.hpp", src);
        for want in [
            "check_signed_range",
            "convert",
            "plain_call",
            "deep_call",
            "level_call",
        ] {
            assert!(
                calls.iter().any(|c| c == want),
                "{want} missing from {calls:?}"
            );
        }
    }

    #[test]
    fn rust_calls_inside_macros_are_calls() {
        let src = "fn f() {\n\
                   writeln!(out, \"{}\", recipe.spaced_path(a)).unwrap();\n\
                   assert_eq!(compute_total(x), Some(3));\n\
                   let v = vec![make_item(1), self.helper_fn(), items[0]];\n\
                   }\n";
        let refs = extract_refs(Path::new("src/lib.rs"), src);
        let call = |n: &str| {
            refs.iter()
                .find(|r| matches!(r.edge_type, EdgeType::Calls) && r.target_name == n)
        };
        assert!(
            call("spaced_path")
                .expect("method call in writeln!")
                .foreign_receiver
        );
        assert!(
            !call("compute_total")
                .expect("call in assert_eq!")
                .foreign_receiver
        );
        assert!(!call("make_item").expect("call in vec!").foreign_receiver);
        assert!(
            !call("helper_fn")
                .expect("self call in vec!")
                .foreign_receiver
        );
        assert!(call("items").is_none(), "indexing is not a call");
        let dsl = "fn t() { test! { tree: (justfile (call env_default (+ \"a\" \"b\"))), } }";
        let refs = extract_refs(Path::new("src/parser.rs"), dsl);
        assert!(
            !refs.iter().any(|r| r.target_name == "env_default"),
            "an S-expression in a macro is not a call"
        );
        assert!(
            call("writeln").is_none() && call("vec").is_none(),
            "macro names are not calls"
        );
    }

    #[test]
    fn member_function_assignments_are_definitions() {
        let src = "res.sendStatus = function sendStatus(code) { return 1 }\n\
                   exports.parseRange = (a) => a\n\
                   Router.prototype.handle = function () {}\n\
                   module.exports = function createApplication() {}\n\
                   res.statusCode = 404\n";
        for file in ["lib/response.js", "lib/response.ts"] {
            let defs = def_names(file, src);
            for want in ["sendStatus", "parseRange", "handle"] {
                assert!(
                    defs.contains(&(want.into(), NodeType::Function)),
                    "{file}: {want} in {defs:?}"
                );
            }
            for absent in ["exports", "statusCode"] {
                assert!(
                    !defs.iter().any(|(n, _)| n == absent),
                    "{file}: {absent} in {defs:?}"
                );
            }
        }
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
    #[allow(clippy::type_complexity)]
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

#[cfg(test)]
mod short_name_tests {
    use super::*;

    /// A one-letter function defined in the file is still a call target.
    #[test]
    fn a_locally_defined_one_letter_call_is_kept() {
        let refs = extract_refs(
            Path::new("a.py"),
            "def g():\n    return 1\n\ndef h():\n    return g() + x()\n",
        );
        let names: Vec<&str> = refs.iter().map(|r| r.target_name.as_str()).collect();
        assert!(names.contains(&"g"), "{names:?}");
        assert!(
            !names.contains(&"x"),
            "undefined one-letter names stay filtered: {names:?}"
        );
    }
}
