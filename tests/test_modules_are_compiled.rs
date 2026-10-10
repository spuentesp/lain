//! Every `.rs` file under `src/` must be declared as a module.
//!
//! A file that is never declared is never compiled: not type-checked,
//! not linted, not run. It looks exactly like working code in the tree.
//!
//! This repo has hit it twice. Commit `b45ebf0` wired up six orphaned
//! `*_tests.rs` modules, and this guard was written then — but scoped to
//! `*_tests.rs` only, so it could not see production files. It missed
//! `server/sensors/http_sensor.rs`, which sat undeclared long enough to
//! accumulate two malformed raw-string literals and a call to
//! `GraphDatabase::find_nodes_by_name`, a method that does not exist.
//! None of it was a compile error, because none of it was ever compiled.
//! The guard now covers every `.rs` file, not just the test ones.

use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        // `src/bin/*.rs` are binary entry points declared in `Cargo.toml`
        // via `[[bin]] path = ...`. They are crate roots for their own
        // binaries, not modules of the lib, so they don't need a
        // `mod` declaration. Skip the whole `src/bin/` subtree.
        if p.is_dir() {
            if p.file_name().and_then(|n| n.to_str()) == Some("bin") {
                continue;
            }
            rust_files(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
            // `lib.rs` and `main.rs` are crate roots and `mod.rs` declares
            // its own directory; none of them is declared from elsewhere.
            // `build.rs` is a Cargo build script — cargo invokes it
            // directly via the `build = "..."` manifest setting; it is
            // not a regular lib module and is excluded here.
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !matches!(name, "lib.rs" | "main.rs" | "mod.rs" | "build.rs") {
                out.push(p);
            }
        }
    }
}

/// True when `line` is a `mod <stem>;` declaration under any visibility:
/// `mod x;`, `pub mod x;`, `pub(crate) mod x;`, `pub(super) mod x;`,
/// `pub(in path) mod x;`.
///
/// The earlier check compared against three exact spellings, so a
/// `pub(super) mod x;` — a perfectly good declaration — read as an
/// orphan. Visibility form is not evidence of orphanhood; an absent
/// declaration is.
fn is_mod_decl(line: &str, stem: &str) -> bool {
    let l = line.trim();
    let Some(rest) = l.strip_suffix(';') else {
        return false;
    };
    let Some((vis, name)) = rest.trim_end().split_once("mod ") else {
        return false;
    };
    if name.trim() != stem {
        return false;
    }
    let vis = vis.trim();
    vis.is_empty() || vis == "pub" || (vis.starts_with("pub(") && vis.ends_with(')'))
}

#[test]
fn every_rust_file_under_src_is_declared_as_a_module() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    assert!(
        !files.is_empty(),
        "found no .rs files to check under {root:?}"
    );

    let mut orphans = Vec::new();
    for f in &files {
        let stem = f.file_stem().unwrap().to_string_lossy().to_string();
        let dir = f.parent().unwrap();
        // A module is declared either in its directory's `mod.rs` or in
        // the sibling `<dir>.rs` that stands in for it.
        let mut parents = vec![dir.join("mod.rs")];
        if let Some(name) = dir.file_name() {
            parents.push(dir.with_file_name(format!("{}.rs", name.to_string_lossy())));
        }
        // Top-level files under `src/` are declared from a crate root.
        parents.push(root.join("lib.rs"));
        parents.push(root.join("main.rs"));
        let declared = parents.iter().any(|p| {
            std::fs::read_to_string(p)
                .map(|s| s.lines().any(|l| is_mod_decl(l, &stem)))
                .unwrap_or(false)
        });
        // ...or by an explicit `#[path = "<file>.rs"]` attribute in a sibling
        // module file (the verification suites use this to keep a module's
        // models and proofs next to it without growing the module itself).
        let file_name = f.file_name().unwrap().to_string_lossy().to_string();
        let path_attr = format!("#[path = \"{file_name}\"]");
        let declared = declared
            || std::fs::read_dir(dir)
                .map(|entries| {
                    entries.flatten().any(|e| {
                        let sib = e.path();
                        sib != *f
                            && sib.extension().is_some_and(|x| x == "rs")
                            && std::fs::read_to_string(&sib)
                                .map(|s| s.lines().any(|l| l.trim() == path_attr))
                                .unwrap_or(false)
                    })
                })
                .unwrap_or(false);
        if !declared {
            orphans.push(f.strip_prefix(&root).unwrap_or(f).display().to_string());
        }
    }

    assert!(
        orphans.is_empty(),
        "these files are never compiled — nothing type-checks or lints them. \
         Declare each with `mod <name>;` (or `#[cfg(test)] mod <name>;` for test-only ones):\n{}",
        orphans.join("\n")
    );
}

/// A `#[test]` attribute must actually sit on a function.
///
/// Inserting code between an attribute and its `fn` silently detaches the
/// attribute: the original function stops being collected by the harness
/// and becomes ordinary dead code, while the attribute lands on whatever
/// now follows it. Nothing fails — the suite just quietly runs one fewer
/// test. This happened while adding a test to `watcher.rs`, and it is the
/// same failure mode as an undeclared module: coverage that looks present
/// in the tree and is not present in the run.
#[test]
fn every_test_attribute_sits_on_a_function() {
    fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                rs_files(&p, out);
            } else if p.extension().and_then(|x| x.to_str()) == Some("rs") {
                out.push(p);
            }
        }
    }

    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rs_files(&manifest.join("src"), &mut files);
    rs_files(&manifest.join("tests"), &mut files);
    assert!(!files.is_empty());

    let mut orphans = Vec::new();
    for f in &files {
        let Ok(src) = std::fs::read_to_string(f) else {
            continue;
        };
        let lines: Vec<&str> = src.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim();
            if t != "#[test]" && t != "#[tokio::test]" {
                continue;
            }
            // Skip forward over further attributes and doc comments; the
            // next substantive line must declare a function.
            let mut j = i + 1;
            while j < lines.len() {
                let n = lines[j].trim();
                if n.is_empty() || n.starts_with("#[") || n.starts_with("//") {
                    j += 1;
                } else {
                    break;
                }
            }
            let next = lines.get(j).map(|l| l.trim()).unwrap_or("");
            if !(next.starts_with("fn ")
                || next.starts_with("async fn ")
                || next.starts_with("pub fn ")
                || next.starts_with("pub async fn "))
            {
                orphans.push(format!(
                    "{}:{} — `{}` is followed by `{}`, not a function",
                    f.strip_prefix(manifest).unwrap_or(f).display(),
                    i + 1,
                    t,
                    next
                ));
            }
        }
    }

    assert!(
        orphans.is_empty(),
        "these test attributes are detached from their function — the test \
         below them no longer runs:\n{}",
        orphans.join("\n")
    );
}
