//! Build script — runs in three phases:
//!
//! 1. Capture the current git short SHA into the `LAIN_GIT_SHA`
//!    env var so `lain doctor` and friends can show "which commit is
//!    this binary?" without shelling out at runtime. This is the
//!    logic that lived at `build.rs` at the package root before
//!    Task 1 of the data-driven sensor-patterns plan moved the build
//!    entry point here.
//!
//! 2. Generate `OUT_DIR/queries.rs` from the bundled `.scm` files
//!    under `src/server/sensors/patterns/<lang>/*.scm`. The main
//!    crate pulls the file in via
//!    `include!(env!("LAIN_PATTERNS_GENERATED_QUERIES"))`.
//!
//! 3. Validate every bundled `.scm` body with `tree_sitter::Query::new`
//!    against the grammar of its `<lang>/` bucket. A syntax error
//!    (unbalanced parens, invalid node type, invalid field, …) panics
//!    with the offending path + the parse error, causing `cargo build`
//!    to fail. This is the build-time compilation pass Task 6 of the
//!    data-driven-sensor-patterns plan required — a malformed
//!    Camp-B query surfaces here, not as a runtime crash inside the
//!    http_sensor / http_client_sensor / entry_point_sensor walkers.
//!
//! ## Generated output shape
//!
//! A static sorted slice of `(key, lang, framework, body)` tuples —
//! one per `.scm` file. `key` is the precomputed
//! `<lang>/<framework>.scm` string, so the binary-search lookup
//! compares a `&str` directly without allocating. Tasks 2-4
//! iterate the slice and parse each body via `tree_sitter::Query`.
//!
//! Why a sorted slice, not a `phf::Map`? `phf` is not in the existing
//! dependency graph and adding a runtime crate for a small set of
//! patterns is not justified at this scale.
//!
//! ## Re-emit triggers
//!
//! `cargo:rerun-if-changed=` covers: `build.rs`, `frameworks.yaml`,
//! `.git/HEAD`, and every `<lang>/` directory so any `.scm` edit
//! triggers a rebuild. The dir-list walk runs unconditionally; the
//! `rerun-if-changed` lines are emitted for each discovered
//! sub-directory at build time.

use std::env as env_mod;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tree_sitter::Language;

fn main() {
    // ── Phase 1: git SHA capture (moved from the package-root
    // build.rs by Task 1). Re-emit on HEAD and any tracked source
    // change so worktree dev-loop rebuilds see the dirty marker flip.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=src");

    let mut sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());

    let dirty = Command::new("git")
        .args(["diff", "--quiet"])
        .status()
        .map(|s| !s.success())
        .unwrap_or(false);
    if dirty {
        sha.push_str("-dirty");
    }
    println!("cargo:rustc-env=LAIN_GIT_SHA={sha}");

    // ── Phase 2: patterns OUT_DIR query generation.
    let manifest_dir = PathBuf::from(env_mod::var("CARGO_MANIFEST_DIR").unwrap());
    let patterns_root = manifest_dir.join("src/server/sensors/patterns");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=frameworks.yaml");

    // Walk `patterns/<lang>/` instead of hard-coding a denylist of
    // language directories. New languages are picked up automatically;
    // a hidden sub-directory (`.foo`) is skipped so a future scratch
    // dir doesn't get enumerated as a language.
    let lang_dirs = discover_lang_dirs(&patterns_root);
    for lang in &lang_dirs {
        println!("cargo:rerun-if-changed={lang}");
    }

    let mut entries: Vec<(String, String)> = Vec::new();
    for lang in &lang_dirs {
        let dir = patterns_root.join(lang);
        let read_dir = match fs::read_dir(&dir) {
            Ok(d) => d,
            Err(e) => panic!(
                "patterns/build.rs: cannot read pattern dir {}: {e}",
                dir.display()
            ),
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("scm") {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_else(|| panic!("invalid .scm path: {}", path.display()));
            let key = format!("{lang}/{name}");
            let body = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            entries.push((key, body));
        }
    }

    // Stable order so two builds of the same tree produce a
    // byte-identical generated file (helps cargo's content-hash skip
    // unnecessary recompiles).
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    // ── Phase 3: build-time query compilation.
    //
    // Compile each bundled `.scm` body against the grammar of its
    // `<lang>/` bucket. A `Syntax`, `Field`, `NodeType`, or
    // `Capture` error fails the build with the offending path so
    // the operator can fix the prompt without booting LAIN.
    for (key, body) in &entries {
        let (lang_yaml, framework_scm) = key.split_once('/').unwrap_or((key.as_str(), ""));
        let grammar = match grammar_for_lang_yaml(lang_yaml) {
            Some(g) => g,
            None => panic!(
                "patterns/build.rs: no tree-sitter grammar registered for lang bucket {lang_yaml:?} \
                 (declared by {key}); add it next to the other build-deps in Cargo.toml"
            ),
        };
        if let Err(e) = tree_sitter::Query::new(&grammar, body) {
            panic!(
                "patterns/build.rs: invalid tree-sitter query syntax in \
                 src/server/sensors/patterns/{lang_yaml}/{framework_scm} \
                 (row {}, offset {}): {e}",
                e.row, e.offset
            );
        }
    }

    let out_dir = PathBuf::from(env_mod::var_os("OUT_DIR").unwrap());
    let out_path = out_dir.join("queries.rs");
    let body = render(&entries);
    fs::write(&out_path, body).expect("write OUT_DIR/queries.rs");

    let out_str = out_path.to_string_lossy().into_owned();
    println!("cargo:rustc-env=LAIN_PATTERNS_GENERATED_QUERIES={out_str}");
}

/// Enumerate the language sub-directories under `patterns_root`,
/// sorted lexicographically. Hidden directories (`.foo`) are skipped
/// so a future scratch dir doesn't get enumerated as a language.
fn discover_lang_dirs(patterns_root: &PathBuf) -> Vec<String> {
    let read_dir = match fs::read_dir(patterns_root) {
        Ok(d) => d,
        Err(e) => panic!(
            "patterns/build.rs: cannot read patterns root {}: {e}",
            patterns_root.display()
        ),
    };
    let mut langs: Vec<String> = read_dir
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect();
    langs.sort();
    langs
}

/// Resolve a tree-sitter [`Language`] for the language bucket key
/// declared by `<org>.scm`. The mapping mirrors
/// `src/server/sensors/util.rs::grammar_for` so a build.rs
/// validation panic and a runtime crash on a misconfigured
/// `<lang>/` show the same grammar name.
fn grammar_for_lang_yaml(lang_yaml: &str) -> Option<Language> {
    match lang_yaml {
        "rust" => Some(tree_sitter_rust::LANGUAGE.into()),
        "python" => Some(tree_sitter_python::LANGUAGE.into()),
        // `tsjs` covers `.ts`/`.js`/`.tsx`/`.jsx`/`.mjs`/`.cjs` per
        // `util.rs::lang_for_path`. The runtime uses the JavaScript
        // grammar for all of them; build.rs does the same.
        "tsjs" => Some(tree_sitter_javascript::LANGUAGE.into()),
        "go" => Some(tree_sitter_go::LANGUAGE.into()),
        "java" => Some(tree_sitter_java::LANGUAGE.into()),
        "csharp" => Some(tree_sitter_c_sharp::LANGUAGE.into()),
        "ruby" => Some(tree_sitter_ruby::LANGUAGE.into()),
        "kotlin" => Some(tree_sitter_kotlin_ng::LANGUAGE.into()),
        // `ts` / `tsx` directories are not currently emitted by
        // `discover_lang_dirs` — the bundled patterns split
        // TypeScript and JavaScript under a single `tsjs/` bucket
        // — but if a future change adds them, route to the
        // TypeScript grammar instead of silently skipping.
        "ts" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        _ => None,
    }
}

/// Build the text of `OUT_DIR/queries.rs`.
///
/// The generated module exposes:
///
/// ```text
/// pub struct Query { pub lang: &'static str, pub framework: &'static str, pub body: &'static str }
/// pub static QUERIES: &[(&'static str, &'static str, &'static str, &'static str)]
///                     // sorted by key (lang/framework.scm)
/// pub fn get(key: &str) -> Option<Query>     // binary search over the precomputed key
/// pub const LEN: usize
/// ```
fn render(entries: &[(String, String)]) -> String {
    use std::fmt::Write;

    // Leak the strings so they have a `'static` lifetime.
    let leaked: Vec<(&'static str, &'static str, &'static str, &'static str)> = entries
        .iter()
        .map(|(key, body)| {
            let (lang, framework) = key.split_once('/').unwrap_or((key.as_str(), ""));
            let key_static: &'static str = Box::leak(key.clone().into_boxed_str());
            let lang_static: &'static str = Box::leak(lang.to_string().into_boxed_str());
            let framework_static: &'static str = Box::leak(framework.to_string().into_boxed_str());
            let body_static: &'static str = Box::leak(body.clone().into_boxed_str());
            (key_static, lang_static, framework_static, body_static)
        })
        .collect();

    let mut out = String::new();
    out.push_str("// Generated by patterns/build.rs — do not edit.\n");
    out.push_str("// Source: src/server/sensors/patterns/<lang>/*.scm (walked at build time).\n");
    out.push_str("// Every entry below has been compile-checked via\n");
    out.push_str("// `tree_sitter::Query::new(grammar, body)` in build.rs (Task 6 of the\n");
    out.push_str("// data-driven-sensor-patterns plan) so a syntax error fails the build,\n");
    out.push_str("// not the runtime sensor walker.\n\n");

    out.push_str(
        "/// One Camp-B pattern, keyed by `<lang>/<framework>.scm`.\n\
         #[derive(Copy, Clone, Debug)]\n\
         pub struct Query {\n\
         pub lang: &'static str,\n\
         pub framework: &'static str,\n\
         pub body: &'static str,\n\
         }\n\n",
    );

    out.push_str(
        "/// Every `.scm` file compiled into the binary, sorted by `<lang>/<framework>.scm`.\n\
         pub static QUERIES: &[(&str, &str, &str, &str)] = &[\n",
    );
    for (key, lang, framework, body) in &leaked {
        let body_lit = body.replace('\\', "\\\\").replace('"', "\\\"");
        writeln!(
            out,
            "    (\"{key}\", \"{lang}\", \"{framework}\", \"{body_lit}\"),"
        )
        .unwrap();
    }
    out.push_str("];\n\n");

    out.push_str(
        "/// Binary-search lookup by `<lang>/<framework>.scm` key. The key is\n\
         /// precomputed into each tuple's first element, so the comparison\n\
         /// is `&str`-vs-`&str` with no per-call allocation.\n\
         pub fn get(key: &str) -> Option<Query> {\n\
         match QUERIES.binary_search_by(|(k, _, _, _)| k.cmp(&key)) {\n\
         Ok(idx) => {\n\
         let (_, l, f, b) = QUERIES[idx];\n\
         Some(Query { lang: l, framework: f, body: b })\n\
         }\n\
         Err(_) => None,\n\
         }\n\
         }\n\n",
    );

    writeln!(
        out,
        "/// Total number of compiled patterns — for tests / diagnostics."
    )
    .unwrap();
    writeln!(out, "pub const LEN: usize = {};", leaked.len()).unwrap();
    out
}

// `Path` is referenced implicitly via `PathBuf` for the `discover_lang_dirs`
// signature above; keep an explicit import so a future refactor that
// drops `PathBuf` doesn't silently unimport the trait methods.
#[allow(dead_code)]
fn _path_marker(_: &Path) {}
