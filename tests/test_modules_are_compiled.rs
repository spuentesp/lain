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
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
            // `lib.rs` and `main.rs` are crate roots and `mod.rs` declares
            // its own directory; none of them is declared from elsewhere.
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !matches!(name, "lib.rs" | "main.rs" | "mod.rs") {
                out.push(p);
            }
        }
    }
}

#[test]
fn every_rust_file_under_src_is_declared_as_a_module() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    assert!(!files.is_empty(), "found no .rs files to check under {root:?}");

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
                .map(|s| {
                    s.lines().any(|l| {
                        let l = l.trim();
                        l == format!("mod {stem};")
                            || l == format!("pub mod {stem};")
                            || l == format!("pub(crate) mod {stem};")
                    })
                })
                .unwrap_or(false)
        });
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
        let Ok(entries) = std::fs::read_dir(dir) else { return };
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
        let Ok(src) = std::fs::read_to_string(f) else { continue };
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
