//! Shared helpers for protocol sensors.
//!
//! Sensors register via [`crate::server::sensors::Sensor`]; everything
//! here is imported, never re-implemented per sensor.

use crate::graph::GraphDatabase;
use crate::schema::GraphNode;
use std::path::Path;
use tree_sitter::{Language, Parser, Tree};

// ─── Language classification ────────────────────────────────────

/// Source-file language for sensors that walk code. PR 14 added Rust +
/// Go; PR 16 (Workstream 5) adds Java, C#, Ruby, Kotlin. The shared
/// [`lang_for_path`] and [`parse_for_lang`] helpers (per sensor
/// wrapper) keep every sensor's per-language walker pointed at the
/// right tree-sitter grammar without duplicating the lookup table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Python,
    TsJs,
    Ts,
    Tsx,
    Rust,
    Go,
    Java,
    CSharp,
    Ruby,
    Kotlin,
}

/// Map a source-file path's extension to a [`Lang`]. `None` for files
/// no sensor in this crate parses. The order is fixed (it's the
/// identity test the §8.3 determinism contract checks for).
pub fn lang_for_path(path: &str) -> Option<Lang> {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "py" => Some(Lang::Python),
        "ts" => Some(Lang::Ts),
        "tsx" => Some(Lang::Tsx),
        "js" | "jsx" | "mjs" | "cjs" => Some(Lang::TsJs),
        "rs" => Some(Lang::Rust),
        "go" => Some(Lang::Go),
        "java" => Some(Lang::Java),
        "cs" => Some(Lang::CSharp),
        "rb" => Some(Lang::Ruby),
        "kts" | "kt" => Some(Lang::Kotlin),
        _ => None,
    }
}

/// Build a tree-sitter [`Tree`] for `lang`. Each sensor's per-language
/// parser consumes this — none of them re-implement the grammar
/// lookup. Returns `None` when the grammar refuses to set (shouldn't
/// happen for the ten languages above) or the parser returns no tree.
pub fn parse_for_lang(lang: Lang, src: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    let grammar: Language = grammar_for(lang);
    parser.set_language(&grammar).ok()?;
    parser.parse(src, None)
}

fn grammar_for(lang: Lang) -> Language {
    match lang {
        Lang::Python => tree_sitter_python::LANGUAGE.into(),
        Lang::TsJs => tree_sitter_javascript::LANGUAGE.into(),
        Lang::Ts => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
        Lang::Go => tree_sitter_go::LANGUAGE.into(),
        Lang::Java => tree_sitter_java::LANGUAGE.into(),
        Lang::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
        Lang::Ruby => tree_sitter_ruby::LANGUAGE.into(),
        Lang::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
    }
}

// ─── Response-metadata denylist (Workstream 5, §6.5) ─────────────

/// Method / property names whose access on a bound HTTP response is
/// never a payload-field read. Bug B of the contract-federation bug
/// fixes; the sensor's attribute walker suppresses `ReadsField`
/// emission when the attribute on a bound receiver is in this list.
///
/// `field_access_sensor` owns the policy; this list lives in
/// `sensors/util.rs` because Workstream 5 expanded it with
/// language-specific entries (Java `getStatusCode()`, C# `StatusCode`,
/// Ruby `code`, Kotlin `statusCode`, …) and the cross-sensor helper
/// needs the same surface.
pub const RESPONSE_METHOD_DENYLIST: &[&str] = &[
    // body parsing (rule 2 already rebinds to the same path)
    "json",
    "text",
    "data",
    "body",
    // HTTP-response / Fetch-API metadata
    "status_code",
    "headers",
    "url",
    "encoding",
    "content",
    "raise_for_status",
    "is_redirect",
    "ok",
    "elapsed",
    // TS / JS Fetch equivalents
    "blob",
    "arrayBuffer",
    "formData",
    "status",
    "redirected",
    // Go body decoding / metadata — see pre-PR-14 comment.
    "decode",
    "Decode",
    "Close",
    "Body",
    "StatusCode",
    "Status",
    "Header",
    "Proto",
    "Request",
    "TLS",
    "Trailer",
    "ContentLength",
    // Java HttpResponse / `okhttp3.Response` metadata
    // (`HttpResponse.statusCode()`, `Response.headers()`, etc.). These
    // are method-form getters — `getStatusCode()` is the Java-Bean
    // spelling, `statusCode()` is the record-style. The body of a
    // Java response is the `body()` method (not in this list — it
    // already rebinds via rule 2). Headers, status, and code / message
    // are response metadata, not payload fields.
    "getStatusCode",
    "statusCode",
    "getBody",
    "getHeaders",
    "headers",
    "firstValue",
    "getMessage",
    "message",
    // C# `HttpResponseMessage` / `HttpClient` properties. `StatusCode`
    // and `IsSuccessStatusCode` are response metadata; `Headers` and
    // `GetValues` are response-header access. Body decoding is
    // `ReadAsStringAsync` / `ReadFromJsonAsync` (treated as body
    // parsers — `Body` is in the list above).
    "IsSuccessStatusCode",
    "StatusCode",
    "Headers",
    "GetValues",
    "DownloadString",
    "DownloadStringTaskAsync",
    // Ruby `Net::HTTPResponse` / `HTTParty::Response` accessors. The
    // `code` and `message` methods are status / status-text;
    // `body` is the response payload — but `body` is already in the
    // body-parser list above and gets the rule-2 rebind.
    "code",
    "message",
    "read_body",
    "back",
    // Kotlin Ktor `HttpResponse` properties.
    "statusCode",
    "call",
    "headers",
    "body",
];

/// A file found by [`walk_workspace`].
pub struct WalkedFile(std::path::PathBuf);

impl WalkedFile {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// Walk every file under `root`: what the main scan indexes, so a sensor
/// sees the same code the graph has. That is every path the walker yields
/// honouring `.gitignore` and hidden-file rules, plus every file git
/// tracks even though `.gitignore` matches it (generated code that is
/// checked in) — skipping those left routes defined there without
/// `CallsHttp` edges although their handlers were indexed. Per-entry errors
/// are flattened away — a malformed symlink or a target that vanishes
/// mid-walk must not abort ingestion.
pub fn walk_workspace(root: &Path) -> impl Iterator<Item = WalkedFile> {
    let mut seen: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    let mut files: Vec<std::path::PathBuf> = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .build()
        .flatten()
        .map(|e| e.into_path())
        .inspect(|p| {
            seen.insert(p.clone());
        })
        .collect();
    if let Ok(repo) = git2::Repository::open(root) {
        if let (Ok(index), Some(workdir)) = (repo.index(), repo.workdir()) {
            // Compare canonical forms (macOS `/var` → `/private/var`,
            // Windows `\\?\`), but report paths under `root` as the caller
            // spelled it, so they match the walk above.
            let canon = |p: &Path| dunce::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            let (canon_root, canon_wd) = (canon(root), canon(workdir));
            for entry in index.iter() {
                let rel = String::from_utf8_lossy(&entry.path).to_string();
                // Hidden paths stay out, as in the walk above.
                if rel.split('/').any(|c| c.starts_with('.')) {
                    continue;
                }
                let Ok(below_root) = canon_wd
                    .join(&rel)
                    .strip_prefix(&canon_root)
                    .map(Path::to_path_buf)
                else {
                    continue;
                };
                let path = root.join(below_root);
                if path.is_file() && !seen.contains(&path) {
                    seen.insert(path.clone());
                    files.push(path);
                }
            }
        }
    }
    files.into_iter().map(WalkedFile)
}

/// `CamelCase` / `mixedCase` → `snake_case`. Each non-initial uppercase
/// char introduces a `_` separator.
pub fn to_snake_case(name: &str) -> String {
    let mut result = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            result.push('_');
        }
        result.push(c.to_ascii_lowercase());
    }
    result
}

/// `snake_case` → `camelCase`. The first non-`_` char stays as-is;
/// each subsequent `_`-delimited segment is title-cased.
pub fn to_camel_case(name: &str) -> String {
    let mut result = String::new();
    let mut capitalize = false;
    for c in name.chars() {
        if c == '_' {
            capitalize = true;
        } else if capitalize {
            result.push(c.to_ascii_uppercase());
            capitalize = false;
        } else {
            result.push(c);
        }
    }
    result
}

/// Look up a handler node by `name`, trying exact, then `snake_case`,
/// then `camelCase`. Empty `name` returns `None`.
///
/// Unifying the priority to `exact → snake → camel` is a strict
/// superset of proto's old `exact → snake` (proto gains camelCase
/// matches) and graphql's old `exact → camel → snake`. In practice the
/// three orderings agree on every input that doesn't start with `_`,
/// because `to_camel_case(x)` of a string with no `_` returns `x`
/// unchanged — so `camel` and `snake` swap positions without changing
/// the resolved set. The only realistic divergence is when both a
/// snake-case and a camelCase form exist as separate nodes; the new
/// order picks snake. The audit's case-mismatch incidents all turned
/// on missing one of the forms, not on choosing between them.
pub fn find_handler_in_graph(graph: &GraphDatabase, name: &str) -> Option<GraphNode> {
    if name.is_empty() {
        return None;
    }
    graph
        .find_node_by_name(name)
        .or_else(|| graph.find_node_by_name(&to_snake_case(name)))
        .or_else(|| graph.find_node_by_name(&to_camel_case(name)))
}

/// The `Function` or `Method` in `path` with the smallest range
/// `line_start..=line_end` that contains `line` (§6.1).
///
/// Ties (two symbols span the same number of lines) go to the
/// symbol whose `line_start` is later — a nested function is more
/// specific than the module-level one that wraps it. Returns
/// `None` if `path` is not in the graph, no symbol in it covers
/// `line`, or every candidate is a non-function/non-method node
/// (`SendsHttp` and `Calls` attach to enclosing functions, never to
/// the file node itself).
pub fn enclosing_symbol(graph: &GraphDatabase, path: &str, line: u32) -> Option<GraphNode> {
    graph
        .get_nodes_by_types(&[
            crate::schema::NodeType::Function,
            crate::schema::NodeType::Method,
        ])
        .ok()
        .into_iter()
        .flatten()
        .filter(|n| n.path == path)
        .filter_map(|n| match (n.line_start, n.line_end) {
            (Some(s), Some(e)) if s <= line && e >= line => Some((n, e.saturating_sub(s))),
            _ => None,
        })
        .min_by(|a, b| {
            // Smallest range first; tie → later line_start.
            a.1.cmp(&b.1)
                .then_with(|| b.0.line_start.cmp(&a.0.line_start))
        })
        .map(|(n, _)| n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{GraphNode, NodeType, RepoNamespace};

    fn db(_name: &str) -> GraphDatabase {
        // `tempfile::tempdir()` gives each test its own directory
        // (cleaned up on Drop), so concurrent test processes don't
        // race over a fixed `/tmp/util_…` path. The other walker
        // tests in this file already use this style. The
        // `GraphDatabase` opens an in-file index inside the temp
        // dir; the dir itself is the file's `memory_path`.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("graph.bin");
        GraphDatabase::new(&path).unwrap()
    }

    fn fn_with_range(name: &str, path: &str, start: u32, end: u32) -> GraphNode {
        let ns = RepoNamespace::for_test();
        GraphNode::new_in(NodeType::Function, name.to_string(), path.to_string(), &ns)
            .with_location_in(start, end, &ns)
    }

    #[test]
    fn snake_case_handles_camel_and_pascal() {
        assert_eq!(to_snake_case("getUser"), "get_user");
        assert_eq!(to_snake_case("GetUser"), "get_user");
        assert_eq!(to_snake_case("URL"), "u_r_l");
        assert_eq!(to_snake_case(""), "");
    }

    #[test]
    fn camel_case_handles_snake_and_passthrough() {
        assert_eq!(to_camel_case("get_user"), "getUser");
        assert_eq!(to_camel_case("GetUser"), "GetUser");
        assert_eq!(to_camel_case(""), "");
    }

    #[test]
    fn walker_visits_tracked_files_that_gitignore_matches() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("gen")).unwrap();
        std::fs::write(root.join(".gitignore"), "gen/\n").unwrap();
        std::fs::write(root.join("gen/routes.py"), "x = 1\n").unwrap();
        std::fs::write(root.join("gen/untracked.py"), "y = 1\n").unwrap();
        let repo = git2::Repository::init(root).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("gen/routes.py")).unwrap();
        index.write().unwrap();
        let names: Vec<_> = walk_workspace(root)
            .filter_map(|e| e.path().file_name().map(|n| n.to_owned()))
            .collect();
        assert!(names.iter().any(|n| n == "routes.py"), "{names:?}");
        assert!(!names.iter().any(|n| n == "untracked.py"), "{names:?}");
    }

    #[test]
    fn walker_visits_a_written_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.proto"), "syntax = \"proto3\";\n").unwrap();
        let names: Vec<_> = walk_workspace(dir.path())
            .map(|e| e.path().to_path_buf())
            .filter_map(|p| p.file_name().map(|n| n.to_owned()))
            .collect();
        assert!(
            names.iter().any(|n| n == "a.proto"),
            "walker missed the file: {names:?}"
        );
    }

    /// `enclosing_symbol` returns the smallest range covering the
    /// line. A 50-line nested function must beat a 200-line
    /// surrounding one for a line inside both — otherwise the
    /// joiner (PR 7) would attach every `SendsHttp` to the
    /// outermost symbol.
    #[test]
    fn enclosing_symbol_returns_the_smallest_range() {
        let g = db("enclosing_smallest");
        let outer = fn_with_range("outer", "src/x.py", 1, 200);
        let inner = fn_with_range("inner", "src/x.py", 50, 100);
        g.insert_nodes_batch(&[outer.clone(), inner.clone()])
            .unwrap();

        let found = enclosing_symbol(&g, "src/x.py", 75).expect("a symbol covers line 75");
        assert_eq!(found.name, "inner", "smallest range wins");
        assert_eq!(found.id, inner.id);
    }

    /// When two symbols span exactly the same number of lines, the
    /// one whose `line_start` is later wins — a nested helper
    /// declared further down the file is more specific than the
    /// outer block that starts at the top.
    #[test]
    fn enclosing_symbol_breaks_range_ties_by_later_line_start() {
        let g = db("enclosing_tie");
        // Both ranges span 100 lines; B's line_start (50) is later
        // than A's (1), so B wins on line 75.
        let a = fn_with_range("a", "src/x.py", 1, 101);
        let b = fn_with_range("b", "src/x.py", 50, 150);
        g.insert_nodes_batch(&[a.clone(), b.clone()]).unwrap();

        let found = enclosing_symbol(&g, "src/x.py", 75).expect("a symbol covers line 75");
        assert_eq!(found.name, "b", "tie → later line_start");
        assert_eq!(found.id, b.id);
    }

    /// A `File` node whose own range covers the line must not be
    /// returned, even when no `Function`/`Method` covers it. The
    /// doc contract is "no candidate → None"; the only way that
    /// path triggers in practice is when the candidate set has
    /// nothing but `File` (or other non-`Function`/`Method`)
    /// nodes, because the resolver only attaches `SendsHttp` and
    /// `Calls` to enclosing functions/methods. A regression that
    /// widened the candidate set to include `File` would return
    /// the file node here and fail.
    #[test]
    fn enclosing_symbol_returns_none_when_only_a_file_covers_the_line() {
        let g = db("enclosing_file_only");
        let ns = RepoNamespace::for_test();
        // A file-shaped node covering the whole file.
        let file = GraphNode::new_in(
            NodeType::File,
            "x.py".to_string(),
            "src/x.py".to_string(),
            &ns,
        )
        .with_location_in(1, 200, &ns);
        g.insert_nodes_batch(&[file]).unwrap();

        // No Function/Method covers line 50 — only the File node
        // does. The type filter must drop it; without the filter,
        // the function under test would return the File.
        assert!(
            enclosing_symbol(&g, "src/x.py", 50).is_none(),
            "a File node covering the line is not a Function/Method; the type filter must drop it"
        );
    }

    /// Variant of the same rule for a file-shape node that is the
    /// *smallest* covering range. Without the type filter, smallest-
    /// range wins and the File would be returned — this test pins
    /// the filter as load-bearing even when range selection would
    /// otherwise pick the File.
    #[test]
    fn enclosing_symbol_excludes_a_file_with_the_smallest_covering_range() {
        let g = db("enclosing_file_smallest");
        let ns = RepoNamespace::for_test();
        // File covers lines 1-3 (smallest possible range); the
        // surrounding Function covers 1-200.
        let file = GraphNode::new_in(
            NodeType::File,
            "x.py".to_string(),
            "src/x.py".to_string(),
            &ns,
        )
        .with_location_in(1, 3, &ns);
        let func = fn_with_range("the_function", "src/x.py", 1, 200);
        g.insert_nodes_batch(&[file, func.clone()]).unwrap();

        let found = enclosing_symbol(&g, "src/x.py", 2)
            .expect("a Function covers line 2 even though File does too");
        assert_eq!(found.node_type, NodeType::Function);
        assert_eq!(found.name, "the_function");
        // A regression that loosened the candidate set to include
        // File would return the file node — smallest range wins
        // before the type filter — and the assertion above would
        // fail on `found.node_type == NodeType::File`.
    }

    /// A `Method` node is also accepted — both `Function` and
    /// `Method` are in the candidate set per §6.1.
    #[test]
    fn enclosing_symbol_accepts_methods() {
        let g = db("enclosing_method");
        let ns = RepoNamespace::for_test();
        let method = GraphNode::new_in(
            NodeType::Method,
            "the_method".to_string(),
            "src/x.rs".to_string(),
            &ns,
        )
        .with_location_in(10, 20, &ns);
        g.insert_nodes_batch(std::slice::from_ref(&method)).unwrap();

        let found = enclosing_symbol(&g, "src/x.rs", 15).expect("a method covers line 15");
        assert_eq!(found.name, "the_method");
        assert_eq!(found.node_type, NodeType::Method);
    }

    /// A line outside every range returns `None` — the sensor
    /// shouldn't claim an enclosing symbol for, say, a top-of-file
    /// import.
    #[test]
    fn enclosing_symbol_returns_none_when_no_range_covers_the_line() {
        let g = db("enclosing_no_match");
        let f = fn_with_range("the_function", "src/x.py", 50, 100);
        g.insert_nodes_batch(&[f]).unwrap();

        assert!(
            enclosing_symbol(&g, "src/x.py", 10).is_none(),
            "line 10 is before the function"
        );
        assert!(
            enclosing_symbol(&g, "src/x.py", 200).is_none(),
            "line 200 is after the function"
        );
        assert!(
            enclosing_symbol(&g, "src/other.py", 75).is_none(),
            "path that doesn't exist in the graph returns None"
        );
    }
}
