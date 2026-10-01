//! Regression tests for the data-driven `http_sensor` walker.
//!
//! Task 2 of the data-driven sensor-patterns plan migrated
//! `http_sensor.rs` from its inline regex table to the
//! `Patterns::route_patterns(Lang)` registry (built in Task 1). The
//! walker must:
//!
//!   1. Pull route frameworks from the YAML registry, not a hand-rolled
//!      `BTreeMap<&'static str, RoutePattern>`.
//!   2. Honour each framework's `verbs`, `path_regex`, `handler_regex`
//!      fields exactly as the inline table did (existing tests pin
//!      this).
//!   3. Fall back to the inline-style regex when a framework has no
//!      tree-sitter query body — so adding a `.scm` later is purely
//!      additive.
//!
//! The hermetic `pr13_hermetic_precision_recall_over_t1_fixture` is
//! the contract this refactor must not break. The per-sensor
//! regression here is the local guard.

use lain::server::federation::contracts::model::HttpMethod;
use lain::server::sensors::http_sensor::scan_file_for_routes;
use lain::server::sensors::patterns::Patterns;
use lain::server::sensors::util::Lang;

fn patterns_for(lang: Lang) -> Vec<String> {
    Patterns::patterns()
        .route_patterns(lang)
        .map(|f| f.id.clone())
        .collect()
}

#[test]
fn route_detection_uses_patterns_yaml() {
    // 1. The Patterns registry must carry the route frameworks the
    //    http_sensor walker consumes.
    let rust_routes = patterns_for(Lang::Rust);
    assert!(
        rust_routes.iter().any(|id| id == "axum-route"),
        "Patterns::route_patterns(Rust) must include axum-route; got {rust_routes:?}"
    );
    assert!(
        rust_routes.iter().any(|id| id == "actix-route"),
        "Patterns::route_patterns(Rust) must include actix-route; got {rust_routes:?}"
    );

    let go_routes = patterns_for(Lang::Go);
    assert!(
        go_routes.iter().any(|id| id == "gin-route"),
        "Patterns::route_patterns(Go) must include gin-route; got {go_routes:?}"
    );
    assert!(
        go_routes.iter().any(|id| id == "stdlib-http-route"),
        "Patterns::route_patterns(Go) must include stdlib-http-route; got {go_routes:?}"
    );

    // 2. The scan helper must produce `HttpRoute` records using the
    //    YAML-driven patterns (the inline `get_route_patterns` table
    //    is gone after the Task 2 refactor).
    let axum_src = "let app = Router::new().route(\"/users\", get(get_users));\n";
    let axum = scan_file_for_routes(std::path::Path::new("main.rs"), axum_src);
    assert_eq!(
        axum.len(),
        1,
        "axum route must be detected through the Patterns-driven walker: {axum:?}"
    );
    assert_eq!(axum[0].method, HttpMethod::Get);
    assert_eq!(axum[0].path, "/users");
    assert_eq!(axum[0].handler_name, "get_users");

    let gin_src = "r.GET(\"/foo\", baz)\n";
    let gin = scan_file_for_routes(std::path::Path::new("routes.go"), gin_src);
    assert_eq!(
        gin.len(),
        1,
        "gin route must be detected through the Patterns-driven walker: {gin:?}"
    );
    assert_eq!(gin[0].method, HttpMethod::Get);
    assert_eq!(gin[0].path, "/foo");
    assert_eq!(gin[0].handler_name, "baz");
}

/// Every framework the inline regex table used to cover must now
/// appear in the YAML registry — no silent framework drop during the
/// migration.
#[test]
fn every_legacy_framework_is_in_patterns_yaml() {
    let rust = patterns_for(Lang::Rust);
    for id in ["axum-route", "actix-route"] {
        assert!(rust.contains(&id.to_string()), "{id} missing from Rust");
    }

    let python = patterns_for(Lang::Python);
    for id in ["fastapi-route", "flask-route"] {
        assert!(python.contains(&id.to_string()), "{id} missing from Python");
    }

    let tsjs = patterns_for(Lang::TsJs);
    for id in ["express-route", "fastify-route"] {
        assert!(tsjs.contains(&id.to_string()), "{id} missing from tsjs");
    }

    let go = patterns_for(Lang::Go);
    for id in ["gin-route", "stdlib-http-route"] {
        assert!(go.contains(&id.to_string()), "{id} missing from Go");
    }

    let java = patterns_for(Lang::Java);
    for id in ["spring-route", "jaxrs-route"] {
        assert!(java.contains(&id.to_string()), "{id} missing from Java");
    }

    let csharp = patterns_for(Lang::CSharp);
    for id in ["aspnet-route", "minimal-api-route"] {
        assert!(csharp.contains(&id.to_string()), "{id} missing from CSharp");
    }

    let ruby = patterns_for(Lang::Ruby);
    for id in ["sinatra-route", "rails-route"] {
        assert!(ruby.contains(&id.to_string()), "{id} missing from Ruby");
    }

    let kotlin = patterns_for(Lang::Kotlin);
    assert!(
        kotlin.contains(&"ktor-route".to_string()),
        "ktor-route missing from Kotlin"
    );
}

/// Patterns YAML must carry a regex fallback for every route
/// framework the inline table used to cover — the data-driven walker
/// consumes `path_regex` / `handler_regex` directly.
#[test]
fn every_route_framework_carries_a_regex_fallback() {
    for lang in [
        Lang::Rust,
        Lang::Python,
        Lang::TsJs,
        Lang::Go,
        Lang::Java,
        Lang::CSharp,
        Lang::Ruby,
        Lang::Kotlin,
    ] {
        for def in Patterns::patterns().route_patterns(lang) {
            assert!(
                def.path_regex.is_some(),
                "{} ({lang:?}) is missing path_regex — the http_sensor walker has nothing to match against",
                def.id
            );
        }
    }
}
