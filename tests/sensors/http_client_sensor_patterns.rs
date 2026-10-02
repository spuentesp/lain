//! Regression tests for the data-driven `http_client_sensor` walker.
//!
//! Task 3 of the data-driven sensor-patterns plan migrated
//! `http_client_sensor.rs` from its 8 per-language `detect_*_call`
//! functions to a single tree-sitter walker that consumes the per-
//! (lang, framework) `.scm` queries published by
//! [`Patterns::compiled_queries`]. The walker must:
//!
//!   1. Drive outbound-call detection from
//!      `Patterns::outbound_patterns(Lang)`, not a hardcoded match arm
//!      per language.
//!   2. Resolve each call's `(library, verb, url)` from the captures
//!      `Patterns::compiled_queries()` returns — every framework in
//!      the registry must surface its `.scm` query.
//!   3. Use the existing language-agnostic URL-parts extractor
//!      (`parts_from_node_inner`) for the captured URL subtree.
//!
//! The hermetic `pr13_hermetic_precision_recall_over_t1_fixture` is
//! the contract this refactor must not break. The per-sensor
//! regression here is the local guard.

use lain::server::federation::contracts::model::{CallVia, HttpMethod, MethodSpec};
use lain::server::sensors::http_client_sensor::detect_calls;
use lain::server::sensors::patterns::Patterns;
use lain::server::sensors::util::Lang;

fn outbound_ids_for(lang: Lang) -> Vec<String> {
    Patterns::patterns()
        .outbound_patterns(lang)
        .map(|f| f.id.clone())
        .collect()
}

fn compiled_query_body(key: &str) -> Option<&'static str> {
    Patterns::patterns()
        .compiled_queries()
        .iter()
        .find(|(k, _, _, _)| *k == key)
        .map(|(_, _, _, body)| *body)
}

fn has_query_content(body: &str) -> bool {
    body.lines().any(|line| {
        let trimmed = line.trim_start();
        !trimmed.is_empty() && !trimmed.starts_with(';')
    })
}

/// Step 3.1 — Construct a Rust source file with
/// `reqwest::get("https://api.example.com/users")` and assert the
/// consumer surface picks it up via
/// `Patterns::outbound_patterns(Lang::Rust)` plus its compiled
/// `.scm` query body.
#[test]
fn outbound_query_for_call_site() {
    // 1. The Patterns registry must carry `reqwest-outbound` under
    //    `Patterns::outbound_patterns(Lang::Rust)`.
    let rust_outbound = outbound_ids_for(Lang::Rust);
    assert!(
        rust_outbound.iter().any(|id| id == "reqwest-outbound"),
        "Patterns::outbound_patterns(Rust) must include reqwest-outbound; got {rust_outbound:?}"
    );

    // 2. The compiled query body for `rust/reqwest-outbound.scm`
    //    must be non-empty (a comment-only stub would short-circuit
    //    the tree-sitter walker and revert to the inline detector).
    let body = compiled_query_body("rust/reqwest-outbound.scm")
        .expect("rust/reqwest-outbound.scm must be in the compiled query map");
    assert!(
        has_query_content(body),
        "rust/reqwest-outbound.scm must have a non-empty query body; got {body:?}"
    );

    // 3. `detect_calls` must consume the .scm query and emit a
    //    `HttpClientCall` with library=`reqwest`, method=GET, and the
    //    captured URL — exactly what the prior `detect_rust_call`
    //    did, but now data-driven through Patterns.
    let src = "fn main() { let _ = reqwest::get(\"https://api.example.com/users\"); }\n";
    let calls = detect_calls(std::path::Path::new("main.rs"), src, Lang::Rust);
    assert_eq!(
        calls.len(),
        1,
        "reqwest::get must be detected through Patterns-driven outbound scan; got {calls:?}"
    );
    let c = &calls[0];
    assert_eq!(c.method, MethodSpec::Known(HttpMethod::Get));
    assert!(
        matches!(
            &c.via,
            CallVia::Library { name } if name == "reqwest"
        ),
        "via must be Library(reqwest); got {:?}",
        c.via
    );
}

/// Every outbound framework in the bundled YAML must surface a
/// non-empty `.scm` body — the inline per-language detector goes
/// away in Task 3, so any framework that lacks a compiled query
/// becomes invisible.
#[test]
fn every_outbound_framework_has_a_compiled_scm_body() {
    let langs = [
        Lang::Python,
        Lang::TsJs,
        Lang::Rust,
        Lang::Go,
        Lang::Java,
        Lang::CSharp,
        Lang::Ruby,
        Lang::Kotlin,
    ];
    for lang in langs {
        for def in Patterns::patterns().outbound_patterns(lang) {
            let key = format!("{}/{}.scm", lang_yaml_key(lang), def.id);
            let body = compiled_query_body(&key).unwrap_or_else(|| {
                panic!("{key} must be in compiled_queries() — outbound framework {}/{} is missing its .scm body",
                    lang_yaml_key(lang), def.id)
            });
            assert!(
                has_query_content(body),
                "{key} is comment-only — the walker will skip it and silently drop {}/{}",
                lang_yaml_key(lang),
                def.id
            );
        }
    }
}

fn lang_yaml_key(lang: Lang) -> &'static str {
    match lang {
        Lang::Python => "python",
        Lang::TsJs | Lang::Ts | Lang::Tsx => "tsjs",
        Lang::Rust => "rust",
        Lang::Go => "go",
        Lang::Java => "java",
        Lang::CSharp => "csharp",
        Lang::Ruby => "ruby",
        Lang::Kotlin => "kotlin",
    }
}
