//! Pin the per-scan leak elimination in `http_sensor` (Item A of
//! the parked-cleanup pass).
//!
//! Pre-cleanup, `get_route_patterns()` was rebuilt on every
//! `scan_file_for_routes` call. The build walked every (lang,
// framework) pair, calling `route_pattern_key`, `handler_regex_for`,
//! and `method_capture_for` — each of which `Box::leak`'d a
//! freshly-built `String`. The leak budget was roughly
//! `16 frameworks × 3 leaks = 48` per scan; over a workspace of
//! 10,000 source files the cumulative leak was hundreds of KB of
//! permanent allocations.
//!
//! The cleanup moves `get_route_patterns()` to a `OnceLock`-backed
//! cache, so the leaks happen exactly once at first call and every
//! subsequent scan returns a `&'static` reference to the cached
//! `BTreeMap`. This test calls `scan_file_for_routes` 1000× and
//! asserts the cached pointer is identical between calls — a
//! pointer difference would mean the cache rebuilt (and re-leaked).

use lain::server::sensors::http_sensor::{get_route_patterns, scan_file_for_routes};

/// 1000 scans against the same fixture must share a single
/// underlying cache — no per-call leak. Each scan returns the
/// same `&'static` reference to the cached BTreeMap.
#[test]
fn scan_does_not_per_scan_leak() {
    let src = "fn main() { let _ = axum::Router::new().route(\"/x\", axum::get); }\n";

    // First call: builds the cache (one-time cost).
    let _ = scan_file_for_routes(std::path::Path::new("main.rs"), src);
    let pointer_after_first = get_route_patterns() as *const _;

    // Subsequent calls: must reuse the cache.
    for i in 0..1000 {
        let _ = scan_file_for_routes(std::path::Path::new("main.rs"), src);
        let p = get_route_patterns() as *const _;
        assert_eq!(
            p, pointer_after_first,
            "scan #{i} returned a different cached pointer than the first call \
             — the cache was rebuilt and Box::leak ran on every scan",
        );
    }
}

/// `get_route_patterns` itself returns the same `&'static`
/// reference across calls. The test calls it twice in a row and
/// asserts pointer equality; a regression that rebuilt the map
/// per call (e.g. dropped the `OnceLock`) would fail this.
#[test]
fn get_route_patterns_returns_a_static_reference() {
    let first = get_route_patterns() as *const _;
    let second = get_route_patterns() as *const _;
    assert_eq!(
        first, second,
        "get_route_patterns() must return the same &'_ reference across calls \
         (OnceLock cache — a per-call rebuild would re-leak every scan)",
    );
    // Sanity: the cached map is non-empty.
    assert!(
        !get_route_patterns().is_empty(),
        "the cached map must still expose route patterns"
    );
}
