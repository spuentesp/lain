//! Every `*_tests.rs` under `src/` must be declared as a module.
//!
//! A test file that is never declared is never compiled and never runs,
//! while looking exactly like coverage in the tree. This repo has hit it
//! before — commit `b45ebf0` wired up six such modules — and it happened
//! again while adding `search_tests.rs`, which sat there passing zero
//! tests until it was declared.

use std::path::{Path, PathBuf};

fn rust_test_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_test_files(&p, out);
        } else if p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with("_tests.rs")) {
            out.push(p);
        }
    }
}

#[test]
fn every_test_file_under_src_is_declared_as_a_module() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_test_files(&root, &mut files);
    assert!(!files.is_empty(), "found no *_tests.rs to check under {root:?}");

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
        let declared = parents.iter().any(|p| {
            std::fs::read_to_string(p)
                .map(|s| {
                    s.lines().any(|l| {
                        let l = l.trim();
                        l == format!("mod {stem};") || l == format!("pub mod {stem};")
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
        "these test files are never compiled — declare them with `#[cfg(test)] mod <name>;`:\n{}",
        orphans.join("\n")
    );
}
