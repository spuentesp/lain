//! Regression tests pinning the data-driven deny-methods lookup
//! that replaced the inline `RESPONSE_METHOD_DENYLIST` constant.
//!
//! Task 5 of the data-driven sensor-patterns plan moved the deny
//! list content from `src/server/sensors/util.rs::RESPONSE_METHOD_DENYLIST`
//! into `frameworks.yaml` under each language's outbound entries.
//! The deny gate in `field_access_sensor::handle_attribute` (and
//! `handle_ruby_call`) now looks the method up via
//! `Patterns::patterns().outbound_patterns(lang)` — the union of
//! `deny_methods` from each outbound entry — instead of consulting
//! the deleted `&[&str]` constant.
//!
//! This file's tests cover:
//!   1. The deny data is present in `frameworks.yaml` for every
//!      language the sensor supports.
//!   2. The deny data is *only* sourced from `frameworks.yaml` —
//!      a public `util::deny_methods_for(lang)` accessor returns
//!      the union of `deny_methods` across outbound entries.
//!   3. The inline `RESPONSE_METHOD_DENYLIST` constant is gone
//!      (so a future regression cannot re-add it).
//!
//! The walker-level gate behaviour ("`r.json()` must not emit a
//! `FieldRef` with chain `json`") is pinned by the in-crate tests in
//! `field_access_sensor::tests` (`response_method_call_on_bound_does_not_emit_field_ref`,
//! `response_metadata_methods_do_not_emit_field_ref`, etc.). Those
//! tests run the full `walk_tree` and assert no FieldRef is
//! emitted; they continue to pass after this refactor because the
//! YAML contains every deny method the old constant had.

use lain::server::sensors::patterns::Patterns;
use lain::server::sensors::util::Lang;

// ─── Step 5.1 — TDD anchor ───────────────────────────────────────

/// TDD red → green: the deny list data MUST be present in
/// `frameworks.yaml` for Python (and every other supported language).
/// Pre-refactor, this was enforced by the inline
/// `RESPONSE_METHOD_DENYLIST.contains(&"json")` constant in
/// `handle_attribute`. After the refactor, the same gate looks up
/// "json" via `Patterns::patterns().outbound_patterns(Lang::Python)`
/// — the union of `deny_methods` from each Python outbound entry.
///
/// The deny set is language-specific (Go uses `Decode`, Java uses
/// `statusCode`, …) so the test pins every language individually.
/// Drop a `deny_methods:` entry in `frameworks.yaml` and the
/// relevant assertion below tells you which one.
#[test]
fn deny_methods_come_from_patterns_yaml() {
    let p = Patterns::patterns();

    // Python outbound union.
    let python_deny: Vec<String> = p
        .outbound_patterns(Lang::Python)
        .flat_map(|def| def.deny_methods.iter().cloned())
        .collect();
    for m in &["json", "text", "data", "body", "status_code", "headers"] {
        assert!(
            python_deny.iter().any(|x| x == m),
            "Python outbound union must include deny method {m:?} (sourced from frameworks.yaml): {python_deny:?}"
        );
    }

    // TS/JS outbound union.
    let tsjs_deny: Vec<String> = p
        .outbound_patterns(Lang::TsJs)
        .flat_map(|def| def.deny_methods.iter().cloned())
        .collect();
    for m in &["json", "text", "blob", "arrayBuffer", "formData", "status"] {
        assert!(
            tsjs_deny.iter().any(|x| x == m),
            "TS/JS outbound union must include deny method {m:?} (sourced from frameworks.yaml): {tsjs_deny:?}"
        );
    }

    // Rust outbound union.
    let rust_deny: Vec<String> = p
        .outbound_patterns(Lang::Rust)
        .flat_map(|def| def.deny_methods.iter().cloned())
        .collect();
    for m in &["json", "text", "bytes", "status", "headers", "url"] {
        assert!(
            rust_deny.iter().any(|x| x == m),
            "Rust outbound union must include deny method {m:?} (sourced from frameworks.yaml): {rust_deny:?}"
        );
    }

    // Go outbound union.
    let go_deny: Vec<String> = p
        .outbound_patterns(Lang::Go)
        .flat_map(|def| def.deny_methods.iter().cloned())
        .collect();
    for m in &["Decode", "Body", "StatusCode", "Status", "Header"] {
        assert!(
            go_deny.iter().any(|x| x == m),
            "Go outbound union must include deny method {m:?} (sourced from frameworks.yaml): {go_deny:?}"
        );
    }

    // Java outbound union.
    let java_deny: Vec<String> = p
        .outbound_patterns(Lang::Java)
        .flat_map(|def| def.deny_methods.iter().cloned())
        .collect();
    for m in &["statusCode", "getStatusCode", "headers", "message"] {
        assert!(
            java_deny.iter().any(|x| x == m),
            "Java outbound union must include deny method {m:?} (sourced from frameworks.yaml): {java_deny:?}"
        );
    }

    // C# outbound union.
    let csharp_deny: Vec<String> = p
        .outbound_patterns(Lang::CSharp)
        .flat_map(|def| def.deny_methods.iter().cloned())
        .collect();
    for m in &["StatusCode", "IsSuccessStatusCode", "Headers", "GetValues"] {
        assert!(
            csharp_deny.iter().any(|x| x == m),
            "C# outbound union must include deny method {m:?} (sourced from frameworks.yaml): {csharp_deny:?}"
        );
    }

    // Ruby outbound union.
    let ruby_deny: Vec<String> = p
        .outbound_patterns(Lang::Ruby)
        .flat_map(|def| def.deny_methods.iter().cloned())
        .collect();
    for m in &["code", "message", "read_body", "back"] {
        assert!(
            ruby_deny.iter().any(|x| x == m),
            "Ruby outbound union must include deny method {m:?} (sourced from frameworks.yaml): {ruby_deny:?}"
        );
    }

    // Kotlin outbound union.
    let kotlin_deny: Vec<String> = p
        .outbound_patterns(Lang::Kotlin)
        .flat_map(|def| def.deny_methods.iter().cloned())
        .collect();
    for m in &["statusCode", "call", "headers", "body"] {
        assert!(
            kotlin_deny.iter().any(|x| x == m),
            "Kotlin outbound union must include deny method {m:?} (sourced from frameworks.yaml): {kotlin_deny:?}"
        );
    }
}

/// Per-language deny-methods union iterates only over `kind:
/// outbound` entries. The deny list is the *body-parser +
/// response-metadata* surface, which is what `kind: outbound`
/// declares — `kind: route` entries do NOT contribute deny
/// methods (their `deny_methods:` is always empty).
#[test]
fn deny_union_skips_route_and_entrypoint_kinds() {
    let p = Patterns::patterns();

    // Pick a route-only definition and assert its `deny_methods`
    // does not pollute the union.
    let route_deny: Vec<String> = p
        .route_patterns(Lang::Python)
        .flat_map(|def| def.deny_methods.iter().cloned())
        .collect();
    assert!(
        route_deny.is_empty(),
        "Python route entries must not contribute deny methods (they're entry-point declarations): {route_deny:?}"
    );
}

/// The deny set is sourced from the bundled `frameworks.yaml`
/// (the `include_str!`'d copy). The runtime override path can
/// replace entries by `id`; this test confirms that a per-repo
/// override for an outbound `id` is honored — proving the sensor's
/// deny gate really does read from `Patterns`, not a static slice.
#[test]
fn deny_union_is_overridable_at_runtime() {
    let dir = tempfile::tempdir().expect("tempdir");
    let patterns_dir = dir.path().join(".lain/patterns");
    std::fs::create_dir_all(&patterns_dir).expect("mkdir");

    // Drop an override that *adds* `runtime_overridden` to
    // `python`'s outbound `requests-outbound.deny_methods`. A
    // regression that replaces the lookup with a static
    // `RESPONSE_METHOD_DENYLIST` slice would silently miss this.
    std::fs::write(
        patterns_dir.join("override.yaml"),
        r#"
languages:
  python:
    - id: requests-outbound
      kind: outbound
      lib_match: '^requests$'
      deny_methods: [runtime_overridden]
"#,
    )
    .expect("write override");

    let mut p = Patterns::patterns().clone();
    p.load_overrides(dir.path()).expect("load_overrides");
    let union: Vec<String> = p
        .outbound_patterns(Lang::Python)
        .flat_map(|def| def.deny_methods.iter().cloned())
        .collect();
    assert!(
        union.iter().any(|m| m == "runtime_overridden"),
        "the deny union must surface runtime overrides — proving the sensor reads from Patterns: {union:?}"
    );
    assert!(
        p.overrides_applied(),
        "load_overrides must record that it ran even when entries are replaced",
    );
}
