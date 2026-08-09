//! Asserts that all version-bumping files share the same version string.
//! This catches the "bumped one but not the others" bug surfaced during the
//! 2026-08-09 federation merge.

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
}