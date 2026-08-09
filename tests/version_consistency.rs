//! Asserts that all version-bumping files share the same version string.
//! This catches the "bumped one but not the others" bug surfaced during the
//! 2026-08-09 federation merge.
//!
//! Also asserts that every `"version"` occurrence inside a JSON file matches
//! the top-level one. `server.json` has a nested `packages[0].version` for
//! the `@spuentesp/lain-mcp` NPM package — when that was missed by the
//! 0.3.0→0.4.0 bump (Task 14 review), the top-level-only check above passed
//! silently. The nested-field check makes that bug class impossible to recur.

use std::fs;

fn read_version(path: &str) -> String {
    let content = fs::read_to_string(path).expect(path);
    // Parse "version": "0.4.0" or `version "0.4.0"` depending on the format.
    let needle = if path.ends_with(".json") {
        "\"version\""
    } else if path.ends_with(".rb") {
        "version "
    } else {
        panic!("don't know how to parse {}", path);
    };
    let idx = content.find(needle).expect(needle);
    let after = &content[idx + needle.len()..];
    let after = after.trim_start().trim_start_matches(':').trim_start();
    let after = after.trim_start_matches('"');
    let end = after.find(|c: char| c == '"' || c == ',' || c == '\n').unwrap();
    after[..end].to_string()
}

/// For a JSON file, every `"version": "..."` field must equal `expected`.
/// Catches the "bumped the top-level version but left a nested copy stale"
/// bug class (e.g. `server.json: packages[0].version`).
fn assert_all_json_versions_match(path: &str, expected: &str) {
    let content = fs::read_to_string(path).expect(path);
    let needle = "\"version\"";
    let mut search_from = 0usize;
    let mut occurrence = 0usize;
    while let Some(rel) = content[search_from..].find(needle) {
        let abs = search_from + rel;
        // Same parse as read_version, starting at the matched needle.
        let after = &content[abs + needle.len()..];
        let after = after.trim_start().trim_start_matches(':').trim_start();
        let after = after.trim_start_matches('"');
        let end = after.find(|c: char| c == '"' || c == ',' || c == '\n').unwrap();
        let found = &after[..end];
        assert_eq!(
            found, expected,
            "{} has \"version\": \"{}\" at occurrence #{}, expected \"{}\" — \
             nested version fields must be bumped together with the top-level one",
            path, found, occurrence, expected
        );
        search_from = abs + needle.len();
        occurrence += 1;
    }
    assert!(
        occurrence >= 1,
        "{} has no \"version\" field — read_version should have caught this",
        path
    );
}

#[test]
fn all_versions_match() {
    let files = [
        "server.json",
        "npm-shim/package.json",
        "Formula/lain.rb",
    ];
    let versions: Vec<(&str, String)> = files.iter().map(|f| (*f, read_version(f))).collect();
    let first = &versions[0].1;
    for (name, v) in &versions {
        assert_eq!(v, first, "{} has version {}, expected {}", name, v, first);
    }
    assert!(!first.is_empty());
    assert!(first.contains('.'), "version {} should be semver (e.g. 0.4.0)", first);

    // For each JSON file, also assert that any nested "version" field matches
    // the top-level one. This guards against partial bumps (e.g. a
    // `packages[0].version` left stale in `server.json`).
    for (name, _v) in &versions {
        if name.ends_with(".json") {
            assert_all_json_versions_match(name, first);
        }
    }
}