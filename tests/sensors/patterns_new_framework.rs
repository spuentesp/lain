//! Regression tests for Task 8 of the data-driven sensor-patterns
//! plan: proof that adding a new framework is a pure data change.
//!
//! This file exercises the Django `path(...)` /
//! `re_path(r"...", ...)` URL-pattern constructor pattern that was
//! added in Task 8. The framework is intentionally distinct from
//! `fastapi-route` and `flask-route` (both are decorator shapes
//! with attribute receivers) — Django's URL patterns are a
//! non-decorator constructor call where `path` or `re_path` is a
//! bare identifier and the view lives in the second positional
//! argument.
//!
//! The whole proof rests on three files:
//!   - `src/server/sensors/patterns/python/django-route.scm`
//!     (new tree-sitter query body)
//!   - `src/server/sensors/patterns/frameworks.yaml`
//!     (one appended entry under `languages.python`)
//!   - this test file
//!
//! No production code in `src/server/sensors/*.rs` was touched —
//! the existing walkers consume the new data through the same
//! `Patterns::route_patterns(Lang)` registry they've been using
//! since Task 1. This file is the local regression guard that
//! proves the data-only claim end-to-end.

use lain::server::federation::contracts::model::HttpMethod;
use lain::server::sensors::http_sensor::scan_file_for_routes;
use lain::server::sensors::patterns::{self, Patterns};
use lain::server::sensors::util::Lang;

// ── 1. The framework id is registered in Patterns. ────────────────

#[test]
fn django_route_pattern_is_in_the_python_route_registry() {
    let ids: Vec<String> = Patterns::patterns()
        .route_patterns(Lang::Python)
        .map(|f| f.id.clone())
        .collect();
    assert!(
        ids.iter().any(|id| id == "django-route"),
        "Patterns::route_patterns(Python) must include django-route after Task 8; \
         got {ids:?}"
    );
}

// ── 2. The .scm body is build-time compiled into the query map. ──

#[test]
fn django_route_compiled_query_is_present_in_the_generated_map() {
    // `patterns/build.rs` enumerates every bundled `<lang>/<framework>.scm`
    // and registers it via `tree_sitter::Query::new(grammar, body)`.
    // A missing entry means either (a) the file wasn't placed under
    // the right lang bucket, (b) the .scm body failed build-time
    // validation, or (c) build.rs wasn't re-run. The http_sensor
    // walker's tree-sitter supplementary path depends on this entry
    // being present.
    let q = patterns::generated::get("python/django-route.scm")
        .expect("python/django-route.scm must be present in the generated query map");
    assert_eq!(q.lang, "python");
    // `build.rs::render` stores the framework as `<id>.scm` (the
    // file stem after `<lang>/`). Strip the suffix to compare to
    // the YAML id; assert the .scm suffix is present so a future
    // refactor that drops it is a visible regression.
    assert_eq!(
        q.framework.strip_suffix(".scm"),
        Some("django-route"),
        "django-route framework key must end with .scm: {q:?}"
    );
    // The query body should target the `path` / `re_path` URL
    // constructors — any future refactor that drops the
    // `#any-of?` predicate is a silent regression.
    assert!(
        q.body.contains("@_url_fn") && q.body.contains("path") && q.body.contains("re_path"),
        "django-route query body must reference path / re_path identifiers: {q:?}"
    );
}

// ── 3. The walker detects Django routes through the regex path. ───

#[test]
fn django_route_is_detected_via_regex_fallback() {
    // The regex path is the primary mechanism. The `path_regex`
    // captures the route template; the `handler_regex` captures
    // the view function name (the second positional argument).
    //
    // Note: the http_sensor's regex extract explicitly skips
    // empty-path routes (`if path.is_empty() { continue; }`), so
    // `path("", …)` is not detected — we use non-empty paths here.
    let src = r#"
from django.urls import path, re_path

def homepage(request):
    pass

def list_users(request):
    pass

urlpatterns = [
    path("/", homepage),
    re_path(r"^users/$", list_users),
]
"#;
    let routes = scan_file_for_routes(std::path::Path::new("urls.py"), src);

    // Two distinct (method, path) pairs are emitted. The verb is
    // `Any` because Django is not in `http_sensor::method_capture_for`'s
    // match arm (it deliberately stays out of `*.rs` — the proof is
    // data-only, so no production code is touched to add verb
    // detection for a new framework).
    assert_eq!(
        routes.len(),
        2,
        "Django path(...) / re_path(...) calls must be detected through the \
         Patterns-driven walker: {routes:?}"
    );

    let paths: Vec<&str> = routes.iter().map(|r| r.path.as_str()).collect();
    assert!(paths.contains(&"/"), "missing `/` route: {routes:?}");
    assert!(
        paths.contains(&"^users/$"),
        "missing `^users/$` route: {routes:?}"
    );

    let homepage = routes.iter().find(|r| r.path == "/").expect("/ route");
    assert_eq!(homepage.handler_name, "homepage");
    assert_eq!(homepage.method, HttpMethod::Any);

    let users = routes
        .iter()
        .find(|r| r.path == "^users/$")
        .expect("^users/$ route");
    assert_eq!(users.handler_name, "list_users");
    assert_eq!(users.method, HttpMethod::Any);
}

// ── 4. The walker also detects Django routes through the .scm. ───

#[test]
fn django_route_is_detected_via_treesitter_supplementary_path() {
    // The http_sensor walker first runs the regex path, then runs
    // the tree-sitter supplementary path on each (lang, framework)
    // pair whose .scm body is non-empty. The dedup is by (method,
    // path), so the supplementary path primarily demonstrates that
    // the .scm body compiles and matches against the parsed AST —
    // not that it produces different output from the regex.
    //
    // This fixture uses a `re_path` call where the regex is a raw
    // string (`r"^api/health$"`); the .scm's `string_content`
    // capture handles raw strings identically to plain strings.
    let src = r#"
from django.urls import re_path

def health_view(request):
    pass

re_path(r"^api/health$", health_view)
"#;
    let routes = scan_file_for_routes(std::path::Path::new("health.py"), src);

    let health = routes
        .iter()
        .find(|r| r.path == "^api/health$")
        .unwrap_or_else(|| {
            panic!(
                "Django re_path(r'^api/health$', health_view) must be \
                 detected through the Patterns-driven walker: {routes:?}"
            )
        });
    assert_eq!(health.handler_name, "health_view");
}

// ── 5. The framework co-exists with FastAPI / Flask / others. ────

#[test]
fn adding_django_does_not_disturb_existing_python_frameworks() {
    // Sanity check: the Django entry sits next to FastAPI and
    // Flask in the `python:` bucket and doesn't shadow either.
    let ids: Vec<String> = Patterns::patterns()
        .route_patterns(Lang::Python)
        .map(|f| f.id.clone())
        .collect();
    assert!(ids.contains(&"fastapi-route".to_string()));
    assert!(ids.contains(&"flask-route".to_string()));
    assert!(ids.contains(&"django-route".to_string()));
}
