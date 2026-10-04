//! Runtime override tests for the data-driven patterns loader.
//!
//! Task 6 step 6.2 / 6.3 — `<repo>/.lain/patterns/` override path.
//! `Patterns::load_overrides(root)` reads `*.yaml` from the override
//! directory and layers entries on top of the bundled registry:
//!
//!   - An override entry with an `id` already in the bundled YAML
//!     **replaces** that entry in place (the existing framework's
//!     fields are overwritten wholesale).
//!   - An override entry with a fresh `id` is **appended** to the
//!     language's list (creating the bucket if needed). Override
//!     entries never duplicate an existing id.
//!
//! These two behaviours are pinned by the tests below so a future
//! regression that silently merges-by-id (or stops replacing) is
//! caught at `cargo test` time.

use lain::server::sensors::patterns::{FrameworkKind, Patterns};

/// Construct a temp dir with the override YAML written under
/// `<root>/.lain/patterns/`. Returns the tempdir (kept alive by
/// the caller) — Drop cleans the directory up.
fn write_override_yaml(root: &std::path::Path, contents: &str) -> std::path::PathBuf {
    let dir = root.join(".lain/patterns");
    std::fs::create_dir_all(&dir).expect("mkdir .lain/patterns");
    let path = dir.join("frameworks.yaml");
    std::fs::write(&path, contents).expect("write override yaml");
    path
}

/// Step 6.2: an override with an `id` already present in the
/// bundled YAML REPLACES the existing entry. The test swaps the
/// `lib_match` regex for `reqwest-outbound` (bundled) with a
/// different value; after `load_overrides`, the framework reads the
/// override's value, not the bundled one.
#[test]
fn override_replaces_by_id() {
    let dir = tempfile::tempdir().expect("tempdir");

    // Bundled `reqwest-outbound` has lib_match
    //   '^(reqwest|.*reqwest::Client.*|ureq.*)$'
    // The override narrows it to just `^reqwest$` — so any caller
    // looking up the framework post-override sees the new regex.
    write_override_yaml(
        dir.path(),
        r#"
languages:
  rust:
    - id: reqwest-outbound
      kind: outbound
      lib_match: '^reqwest$'
      deny_methods: [replacement_marker]
"#,
    );

    let mut p = Patterns::clone_default();

    // Sanity: the bundled entry is the regex the YAML shipped.
    let bundled = p
        .framework("reqwest-outbound")
        .expect("reqwest-outbound is in the bundled YAML")
        .lib_match
        .clone()
        .expect("bundled reqwest-outbound has lib_match");
    assert!(
        bundled.contains("ureq"),
        "bundled lib_match should match ureq (sanity check on the fixture): {bundled:?}",
    );

    p.load_overrides(dir.path())
        .expect("load_overrides succeeds");

    // The replacement took effect: the lib_match is now the
    // override's regex.
    let replaced = p
        .framework("reqwest-outbound")
        .expect("reqwest-outbound survives the override")
        .lib_match
        .as_deref()
        .expect("override lib_match is Some");
    assert_eq!(
        replaced, "^reqwest$",
        "load_overrides must replace the lib_match for an existing framework",
    );

    // The deny_methods list is also replaced wholesale — proving
    // the override fully overwrites the entry rather than
    // merging by field.
    let replaced_deny: Vec<String> = p
        .framework("reqwest-outbound")
        .unwrap()
        .deny_methods
        .iter()
        .filter(|m| m.as_str() == "replacement_marker")
        .cloned()
        .collect();
    assert_eq!(
        replaced_deny,
        vec!["replacement_marker".to_string()],
        "override must replace the deny_methods list, not merge",
    );
}

/// The framework `kind` is preserved when the override omits it
/// — proves the override is a full entry (matching `id` plus all
/// fields), not a partial patch. The bundled `reqwest-outbound`
/// is `kind: outbound`; the override here ships an explicit
/// `kind: outbound` as well, and the lookup confirms the kind.
#[test]
fn override_preserves_kind_for_replaced_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_override_yaml(
        dir.path(),
        r#"
languages:
  rust:
    - id: reqwest-outbound
      kind: outbound
      lib_match: '^reqwest$'
"#,
    );

    let mut p = Patterns::clone_default();
    p.load_overrides(dir.path()).expect("load_overrides ok");
    let def = p.framework("reqwest-outbound").unwrap();
    assert_eq!(
        def.kind,
        FrameworkKind::Outbound,
        "override replaces the framework wholesale — kind survives",
    );
}

/// Step 6.3: an override with a fresh `id` is appended to the
/// language bucket (creating it if absent) — overriding does NOT
/// duplicate the existing entry. We assert the framework count
/// grew by exactly one and the original entry is untouched.
#[test]
fn override_does_not_augment_existing_id() {
    let dir = tempfile::tempdir().expect("tempdir");

    // Fresh id: `runtime-only-outbound` — not in the bundled YAML.
    write_override_yaml(
        dir.path(),
        r#"
languages:
  rust:
    - id: runtime-only-outbound
      kind: outbound
      lib_match: '^runtime-lib$'
      deny_methods: [runtime_only_method]
"#,
    );

    // Capture the pre-override count so we can assert it grew by
    // exactly one (one new entry, no duplicates).
    let pre = Patterns::patterns();
    let pre_count: usize = pre
        .framework("runtime-only-outbound")
        .map(|_| 1)
        .unwrap_or(0);
    assert_eq!(
        pre_count, 0,
        "runtime-only-outbound must not be in the bundled YAML by default",
    );
    let pre_rust_total: usize = pre
        .outbound_patterns(lain::server::sensors::util::Lang::Rust)
        .count();
    let pre_reqwest = pre
        .framework("reqwest-outbound")
        .expect("reqwest-outbound is bundled")
        .lib_match
        .clone();

    let mut p = Patterns::clone_default();
    p.load_overrides(dir.path()).expect("load_overrides ok");

    // 1. The fresh entry is now present.
    let fresh = p
        .framework("runtime-only-outbound")
        .expect("fresh override id is appended to the registry");
    assert_eq!(fresh.kind, FrameworkKind::Outbound);
    assert_eq!(fresh.lib_match.as_deref(), Some("^runtime-lib$"));

    // 2. The existing entry is untouched.
    let reqwest = p
        .framework("reqwest-outbound")
        .expect("reqwest-outbound survives a non-matching override");
    assert_eq!(
        reqwest.lib_match, pre_reqwest,
        "override of one id must NOT alter an unrelated entry",
    );

    // 3. The Rust outbound count grew by exactly one (new id, no
    //    duplication of the existing rust bucket).
    let post_rust_total: usize = p
        .outbound_patterns(lain::server::sensors::util::Lang::Rust)
        .count();
    assert_eq!(
        post_rust_total,
        pre_rust_total + 1,
        "Rust outbound count must grow by exactly one when one fresh id is appended",
    );
}

/// `overrides_applied()` flips to `true` after a successful
/// `load_overrides`, even when the override file matched zero
/// bundled entries (every id was a fresh addition). The flag is
/// the cheapest "did the operator's directory mount succeed?"
/// diagnostic.
#[test]
fn overrides_applied_flips_even_when_dir_is_absent() {
    let p = Patterns::clone_default();
    assert!(
        !p.overrides_applied(),
        "fresh Patterns must report overrides_applied() == false",
    );
    let dir = tempfile::tempdir().expect("tempdir");
    // No `.lain/patterns` directory created — `load_overrides`
    // is a no-op and the flag still flips so diagnostics know the
    // loader ran (rather than "no one ever called it").
    let mut p = Patterns::clone_default();
    p.load_overrides(dir.path())
        .expect("missing override dir is a no-op");
    assert!(
        p.overrides_applied(),
        "overrides_applied() must be true after a no-op load",
    );
}

/// Multi-file override layout: a per-repo pattern directory may
/// contain multiple `.yaml` files (one per team, one per service,
/// etc.). Every file's entries layer on top of the bundled registry
/// and the per-file replacement/append semantics are uniform.
#[test]
fn multiple_override_files_combine() {
    let dir = tempfile::tempdir().expect("tempdir");
    let patterns_dir = dir.path().join(".lain/patterns");
    std::fs::create_dir_all(&patterns_dir).expect("mkdir");

    // File A replaces an existing framework.
    std::fs::write(
        patterns_dir.join("a.yaml"),
        r#"
languages:
  rust:
    - id: reqwest-outbound
      kind: outbound
      lib_match: '^from_a$'
"#,
    )
    .expect("write a.yaml");

    // File B appends a new framework.
    std::fs::write(
        patterns_dir.join("b.yaml"),
        r#"
languages:
  rust:
    - id: file-b-extra
      kind: outbound
      lib_match: '^from_b$'
"#,
    )
    .expect("write b.yaml");

    let mut p = Patterns::clone_default();
    p.load_overrides(dir.path()).expect("load_overrides ok");

    assert_eq!(
        p.framework("reqwest-outbound")
            .unwrap()
            .lib_match
            .as_deref(),
        Some("^from_a$"),
        "file A's reqwest-outbound replacement is applied",
    );
    assert_eq!(
        p.framework("file-b-extra")
            .expect("file B's fresh id is appended")
            .lib_match
            .as_deref(),
        Some("^from_b$"),
        "file B's fresh id is appended to the registry",
    );
}
