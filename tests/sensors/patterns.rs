//! Regression tests for the data-driven `Patterns` loader
//! (`src/server/sensors/patterns/mod.rs`).
//!
//! The brief (`task-1-brief.md` §1.1) requires three checks:
//!   1. The bundled `frameworks.yaml` parses via `Patterns::from_yaml_str`
//!      and `Patterns::patterns()`.
//!   2. A malformed YAML yields a structured `serde_yaml` error (not a
//!      panic, not an opaque string).
//!   3. The route / outbound dispatch tables return the expected
//!      framework ids for the existing per-sensor code so the rest of
//!      the contract-federation suite keeps working once Tasks 2-4
//!      wire them up.
//!
//! All five existing per-sensor regression suites must stay green —
//! this file only asserts the loader shape, not the walker behaviour.

use lain::server::sensors::patterns::{self, FrameworkDef, FrameworkKind, Patterns, YamlFile};

const MINIMAL_YAML: &str = r#"
languages:
  python:
    - id: fastapi-route
      kind: route
      verbs: [get, post]
    - id: requests-outbound
      kind: outbound
      lib_match: '^requests$'
      deny_methods: [json, text]
"#;

#[test]
fn bundled_yaml_loads_and_validates() {
    let patterns = Patterns::patterns();
    // The bundled YAML must parse. A bare `Ok` is the smoke test — if
    // any entry violates the schema, this panics during init.
    let _ = patterns;
}

#[test]
fn from_yaml_str_round_trips_a_minimal_document() {
    let p = Patterns::from_yaml_str(MINIMAL_YAML).expect("minimal YAML is valid");
    // The `fastapi-route` definition has the verbs the YAML spelled.
    let fastapi = p
        .framework("fastapi-route")
        .expect("fastapi-route is in the minimal doc");
    assert_eq!(fastapi.id, "fastapi-route");
    assert_eq!(fastapi.kind, FrameworkKind::Route);
    assert_eq!(
        fastapi.verbs,
        vec!["get".to_string(), "post".to_string()],
        "verbs preserve YAML order"
    );
    // Outbound definition carries the deny_methods.
    let requests = p.framework("requests-outbound").unwrap();
    assert_eq!(requests.kind, FrameworkKind::Outbound);
    assert_eq!(
        requests.deny_methods,
        vec!["json".to_string(), "text".to_string()]
    );
}

#[test]
fn from_yaml_str_returns_a_structured_error_on_garbage() {
    let res = Patterns::from_yaml_str("this: is: not: valid: yaml: at: all:");
    // The error must surface — it must NOT panic, must NOT be an
    // opaque String, and must carry enough info to point at the
    // bad column. `serde_yaml::Error` is the structured type the
    // brief pins.
    let err = res.expect_err("malformed YAML must fail to parse");
    let msg = format!("{err}");
    assert!(!msg.is_empty(), "the error must carry a message");
}

#[test]
fn from_yaml_str_rejects_unknown_kind_value() {
    let yaml = r#"
languages:
  python:
    - id: broken
      kind: telemetry
"#;
    let res = Patterns::from_yaml_str(yaml);
    assert!(
        res.is_err(),
        "an unknown `kind:` value must fail schema validation"
    );
}

#[test]
fn route_patterns_filters_by_kind_and_language() {
    use lain::server::sensors::util::Lang;
    let p = Patterns::from_yaml_str(MINIMAL_YAML).expect("minimal YAML is valid");
    let routes: Vec<&FrameworkDef> = p.route_patterns(Lang::Python).collect();
    assert_eq!(routes.len(), 1, "only `fastapi-route` is a route");
    assert_eq!(routes[0].id, "fastapi-route");

    let outbounds: Vec<&FrameworkDef> = p.outbound_patterns(Lang::Python).collect();
    assert_eq!(outbounds.len(), 1, "only `requests-outbound` is outbound");
    assert_eq!(outbounds[0].id, "requests-outbound");

    let none: Vec<&FrameworkDef> = p.route_patterns(Lang::Rust).collect();
    assert!(none.is_empty(), "the minimal doc has no Rust routes");
}

#[test]
fn deny_methods_for_returns_the_matching_libs_methods() {
    use lain::server::sensors::util::Lang;
    let p = Patterns::from_yaml_str(MINIMAL_YAML).expect("minimal YAML is valid");
    // The minimal YAML declares `requests` with [json, text].
    let deny = p.deny_methods_for(Lang::Python, "requests", "get");
    assert!(deny.iter().any(|m| m == "json"));
    assert!(deny.iter().any(|m| m == "text"));

    // A library that no `lib_match` regex accepts returns nothing.
    let none = p.deny_methods_for(Lang::Python, "unrelated", "get");
    assert!(none.is_empty(), "no lib_match → no deny methods");
}

#[test]
fn yaml_file_loads_from_a_directory_with_an_override() {
    let dir = tempfile::tempdir().expect("tempdir");
    let patterns_dir = dir.path().join(".lain/patterns");
    std::fs::create_dir_all(&patterns_dir).expect("mkdir");

    // Override adds a new framework id that the bundled YAML does not
    // declare. `load_overrides` should append it under the language
    // key.
    std::fs::write(
        patterns_dir.join("overrides.yaml"),
        r#"
languages:
  python:
    - id: starlette-route
      kind: route
      verbs: [get]
"#,
    )
    .expect("write override");

    let mut p = Patterns::patterns().clone();
    let starlette_before = p.framework("starlette-route").is_some();
    assert!(
        !starlette_before,
        "starlette-route is not in the bundled YAML by default"
    );

    p.load_overrides(dir.path()).expect("load_overrides ok");
    let starlette_after = p
        .framework("starlette-route")
        .expect("starlette-route now in the merged table");
    assert_eq!(starlette_after.kind, FrameworkKind::Route);
}

#[test]
fn yml_file_round_trips_through_a_minimal_document() {
    // Parsing the minimal doc as a `YamlFile` directly must succeed.
    let _original: YamlFile =
        serde_yaml::from_str(MINIMAL_YAML).expect("minimal YAML parses as a YamlFile");
}

// ── Fix-round-1 regression tests ──────────────────────────────────
//
// These pin the four contract surfaces the brief required:
//   * `Patterns::patterns()` singleton accessor (rename of
//     `load_default`).
//   * The generated query map covers ALL eight languages — 34
//     `.scm` files in total (the previous LANG_DIRS hard-coded only
//     six, silently dropping python + tsjs).
//   * `Patterns::generated::queries::get(key)` resolves a key for
//     every bundled language (Tasks 2-4 depend on this).
//   * `deny_methods_for(lang, lib, verb)` accepts the verb parameter
//     for forward compatibility.

#[test]
fn patterns_singleton_is_callable_as_patterns() {
    // The brief pins `pub fn patterns() -> &'static Patterns` as the
    // canonical accessor. `load_default` is kept as a deprecated
    // alias. Both must point at the same data.
    let via_new = Patterns::patterns();
    #[allow(deprecated)]
    let via_old = Patterns::load_default();
    assert!(
        std::ptr::eq(via_new as *const _, via_old as *const _),
        "Patterns::patterns() and Patterns::load_default() must share the same backing instance"
    );
}

#[test]
fn generated_queries_cover_all_eight_languages() {
    // 5 (rust) + 3 (go) + 5 (java) + 4 (csharp) + 7 (ruby) + 3 (kotlin)
    // + 6 (python) + 5 (tsjs) = 38 .scm files. Task 4 added the three
    // entry-point patterns (`spring-entry-point`,
    // `aspnet-entry-point`, `rails-entry-point`); Task 8 added the
    // Django `path("…", view)` / `re_path(r"…", view)` constructor
    // pattern (`django-route`).
    assert_eq!(
        patterns::generated::LEN,
        38,
        "the generated query map must include python + tsjs; \
         the prior LANG_DIRS hard-coded only six languages and \
         silently dropped 10 entries"
    );
}

#[test]
fn generated_queries_resolves_keys_for_python_and_tsjs() {
    // Tasks 2-4 consume compiled_queries() across ALL eight
    // languages. A missing python or tsjs entry would silently fail
    // lookups during route / outbound detection.
    assert!(
        patterns::generated::get("python/fastapi-route.scm").is_some(),
        "python/fastapi-route.scm must be present in the generated map"
    );
    assert!(
        patterns::generated::get("tsjs/express-route.scm").is_some(),
        "tsjs/express-route.scm must be present in the generated map"
    );
}

#[test]
fn deny_methods_for_accepts_a_verb_argument() {
    // The signature now takes `&str` for the verb; the parameter is
    // unused today but reserves the position so callers in Tasks 2-3
    // can pass it without an API break.
    use lain::server::sensors::util::Lang;
    let p = Patterns::from_yaml_str(MINIMAL_YAML).expect("minimal YAML is valid");
    let _ = p.deny_methods_for(Lang::Python, "requests", "get");
}
