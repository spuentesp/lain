//! Build script — runs in two phases:
//!
//! 1. Capture the current git short SHA into the `LAIN_GIT_SHA`
//!    env var so `lain doctor` and friends can show "which commit is
//!    this binary?" without shelling out at runtime. This is the
//!    logic that lived at `build.rs` at the package root before
//!    Task 1 of the data-driven sensor-patterns plan moved the build
//!    entry point here.
//!
//! 2. Generate `OUT_DIR/queries.rs` from the bundled `.scm` files
//!    under `src/server/sensors/patterns/{lang}/*.scm`. The main
//!    crate pulls the file in via
//!    `include!(env!("LAIN_PATTERNS_GENERATED_QUERIES"))`.
//!
//! ## Generated output shape
//!
//! A static sorted slice of `(lang, framework, body)` triples — one
//! per `.scm` file. The key in the lookup API is
//! `<lang>/<framework>.scm` (e.g. `rust/axum-route.scm`); the
//! runtime consumer binary-searches the slice for O(log n) lookup.
//!
//! Why a sorted slice, not a `phf::Map`? `phf` is not in the existing
//! dependency graph and adding a runtime crate for a small set of
//! patterns is not justified at this scale.
//!
//! ## Re-emit triggers
//!
//! `cargo:rerun-if-changed=` covers: `build.rs`, `frameworks.yaml`,
//! `src`, `.git/HEAD`, and every `<lang>/` directory so any `.scm`
//! edit triggers a rebuild.

use std::env as env_mod;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const LANG_DIRS: &[&str] = &["rust", "go", "java", "csharp", "ruby", "kotlin"];

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
    for lang in LANG_DIRS {
        println!("cargo:rerun-if-changed={lang}");
    }

    let mut entries: Vec<(String, String)> = Vec::new();
    for lang in LANG_DIRS {
        let dir = patterns_root.join(lang);
        if !dir.is_dir() {
            continue;
        }
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

    let out_dir = PathBuf::from(env_mod::var_os("OUT_DIR").unwrap());
    let out_path = out_dir.join("queries.rs");
    let body = render(&entries);
    fs::write(&out_path, body).expect("write OUT_DIR/queries.rs");

    let out_str = out_path.to_string_lossy().into_owned();
    println!("cargo:rustc-env=LAIN_PATTERNS_GENERATED_QUERIES={out_str}");
}

/// Build the text of `OUT_DIR/queries.rs`.
///
/// The generated module exposes:
///
/// ```text
/// pub struct Query { pub lang: &'static str, pub framework: &'static str, pub body: &'static str }
/// pub static QUERIES: &[(&'static str, &'static str, &'static str)]   // sorted by key
/// pub fn get(key: &str) -> Option<Query>                              // binary search
/// pub const LEN: usize
/// ```
fn render(entries: &[(String, String)]) -> String {
    use std::fmt::Write;

    // Leak the strings so they have a `'static` lifetime.
    let leaked: Vec<(&'static str, &'static str, &'static str)> = entries
        .iter()
        .map(|(key, body)| {
            let (lang, framework) = key.split_once('/').unwrap_or((key.as_str(), ""));
            let lang_static: &'static str = Box::leak(lang.to_string().into_boxed_str());
            let framework_static: &'static str = Box::leak(framework.to_string().into_boxed_str());
            let body_static: &'static str = Box::leak(body.clone().into_boxed_str());
            (lang_static, framework_static, body_static)
        })
        .collect();

    let mut out = String::new();
    out.push_str("// Generated by patterns/build.rs — do not edit.\n");
    out.push_str(
        "// Source: src/server/sensors/patterns/{rust,go,java,csharp,ruby,kotlin}/*.scm\n",
    );
    out.push_str("// Re-generated when build.rs, frameworks.yaml, or any .scm file changes.\n\n");

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
         pub static QUERIES: &[(&str, &str, &str)] = &[\n",
    );
    for (lang, framework, body) in &leaked {
        let body_lit = body.replace('\\', "\\\\").replace('"', "\\\"");
        writeln!(out, "    (\"{lang}\", \"{framework}\", \"{body_lit}\"),").unwrap();
    }
    out.push_str("];\n\n");

    out.push_str(
        "/// Binary-search lookup by `<lang>/<framework>.scm` key.\n\
         pub fn get(key: &str) -> Option<Query> {\n\
         match QUERIES.binary_search_by(|(l, f, _)| {\n\
         let candidate: String = format!(\"{}/{}\", l, f);\n\
         candidate.as_str().cmp(key)\n\
         }) {\n\
         Ok(idx) => {\n\
         let (l, f, b) = QUERIES[idx];\n\
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
