//! Pin the per-file tree-sitter parse hoist in
//! `scan_file_for_routes`. The walker iterates over every
//! applicable (lang, framework) pair to handle multiple route
//! dialects in the same source file (e.g. a TS/JS file with both
//! express and fastify routes). Before the hoist, each iteration
//! called `parse_for_lang(lang, content)` separately — so a file
//! with N applicable frameworks parsed N times. After the hoist
//! (Item C of the parked-cleanup pass), the file is parsed once
//! per scan and the pre-parsed [`tree_sitter::Tree`] is passed
//! through `try_treesitter_extract` for every framework in the
//! loop.
//!
//! The test uses the package-public instrumentation
//! `http_sensor::scan_parse_count` so a future regression that
//! re-introduces the per-framework parse (or skips parsing
//! entirely) is caught at `cargo test` time.
//!
//! Because the parse counter is a process-global `AtomicUsize`
//! (the http_sensor walker is shared across all sensor tests), the
//! tests in this file read the counter as a *delta* — `after -
//! before` — so concurrent scans from unrelated tests cannot
//! false-positive the assertion.

use lain::server::federation::contracts::model::HttpMethod;
use lain::server::sensors::http_sensor::{scan_file_for_routes, scan_parse_count};
// Note: `scan_parse_count` is a per-thread counter (Cell-local),
// so concurrent sensor tests cannot false-positive the delta.

/// TS/JS files have two applicable route frameworks bundled
/// (`express-route` and `fastify-route`); both ship a non-empty
/// `.scm` body. Pre-refactor, that meant `parse_for_lang` ran
/// twice per scan. Post-refactor, the file is parsed once and
/// the resulting `Tree` is reused across iterations.
#[test]
fn tree_is_parsed_once_per_file() {
    // A TS file with one express route and one fastify route —
    // both route frameworks have non-empty `.scm` bodies, so
    // the tree-sitter supplementary path runs for both. The
    // fixture is intentionally compact: the route paths are
    // disjoint so the test can assert both are detected.
    let src = r#"
import express from 'express';
import fastify from 'fastify';

const app = express();
app.get('/api/users', getUser);

const server = fastify();
server.get('/api/orders', listOrders);
"#;

    let before = scan_parse_count();
    let routes = scan_file_for_routes(std::path::Path::new("routes.ts"), src);
    let after = scan_parse_count();

    assert_eq!(
        after - before,
        1,
        "scan_file_for_routes must parse the file exactly once, regardless of how many \
         route frameworks apply (delta = {} — pre-refactor this would be 2)",
        after - before,
    );

    // Sanity: at least one route must be detected. The regex path
    // is the primary mechanism; the tree-sitter supplementary
    // path runs after it. Pinning the count of *detected* routes
    // (≥ 1) is enough — pinning the exact count would couple the
    // test to the regex path's emit behaviour, which is the
    // wrong surface for this assertion.
    assert!(
        !routes.is_empty(),
        "at least one TS/JSX route must be detected: {routes:?}",
    );
    let paths: Vec<&str> = routes.iter().map(|r| r.path.as_str()).collect();
    assert!(
        paths.contains(&"/api/users") || paths.contains(&"/api/orders"),
        "express / fastify route paths must be detected: {routes:?}",
    );

    // Every emitted route must carry a usable method (the
    // tree-sitter walker may produce `HttpMethod::Any` when the
    // verb capture is absent; the regex path produces the right
    // verb for both frameworks, but pinning the invariant keeps
    // a future regression from breaking the tree-sitter leg).
    for r in &routes {
        assert!(
            matches!(r.method, HttpMethod::Get | HttpMethod::Any),
            "route method must be GET or Any, got {:?} for {r:?}",
            r.method,
        );
    }
}

/// Two scans across different files must each parse exactly
/// once. This is the regression-prevention half: a future API
/// change that accidentally shares state across scans (e.g.
/// caching the tree globally keyed on `(lang, content)`) would
/// still pass the single-call test above but fail here.
#[test]
fn parse_count_increments_per_scan_call() {
    let src = "import express from 'express';\napp.get('/x', handler);\n";

    let before_first = scan_parse_count();
    let _ = scan_file_for_routes(std::path::Path::new("a.ts"), src);
    let after_first = scan_parse_count();
    assert_eq!(
        after_first - before_first,
        1,
        "first scan must increment the counter by exactly one"
    );

    let before_second = scan_parse_count();
    let _ = scan_file_for_routes(std::path::Path::new("b.ts"), src);
    let after_second = scan_parse_count();
    assert_eq!(
        after_second - before_second,
        1,
        "second scan must increment the counter by exactly one"
    );
}

/// Files with no applicable route framework (extension not in
/// the `http_sensor` table) must not call `parse_for_lang` at
/// all. The walker short-circuits at the prefix table and
/// returns an empty `Vec<HttpRoute>`. A regression that
/// re-introduces the per-framework parse would otherwise pay a
/// parse cost on every non-route source file.
#[test]
fn no_parse_when_no_route_framework_applies() {
    // `.txt` is not in the `prefixes` table; the walker
    // returns `Vec::new()` before reaching the per-framework loop.
    let src = "this is not source code\n";

    let before = scan_parse_count();
    let routes = scan_file_for_routes(std::path::Path::new("notes.txt"), src);
    let after = scan_parse_count();

    assert!(
        routes.is_empty(),
        "non-source files must produce zero routes"
    );
    assert_eq!(
        after - before,
        0,
        "non-source files must not invoke parse_for_lang; delta = {}",
        after - before,
    );
}
