//! Regression tests for the `display_name` YAML field on outbound
//! frameworks.
//!
//! The data-driven sensor-patterns refactor (Task B of the parked-Minor
//! cleanup pass #1) added `FrameworkDef::display_name` + `effective_id()`
//! so that the public gem-name / library surface for a framework can be
//! expressed as data. The walker at `http_client_sensor.rs` reads
//! `effective_id()` instead of `framework.id` when a framework declares
//! `display_name`.
//!
//! Earlier coverage proved the *bundled* data shape
//! (`tests/sensors/http_client_sensor_patterns.rs`) but did not exercise
//! the end-to-end `display_name` resolution path with a real Ruby / Java
//! source file. A regression that drops the outer `display_name` arm in
//! `process_outbound_match` would silently revert to the hardcoded Ruby
//! `("httparty-outbound", "HTTParty")` arm and the wrong `via_lib` for
//! any HTTParty-via-source-file not directly invoked on `HTTParty`.
//! These tests pin the `display_name` resolution end-to-end across:
//!
//!   1. HTTParty direct (`HTTParty.get(...)`).
//!   2. HTTParty inside a method body — confirms the display_name
//!      path is class- and method-stable (the constant receiver form
//!      is the same as scenario 1, but the call is wrapped in a
//!      typical Ruby class/method shape).
//!   3. Faraday direct — exercises the surviving hardcoded Ruby arm
//!      (Faraday has no display_name; the inner arm fires).
//!   4. Net::HTTP direct — exercises the surviving hardcoded Ruby arm
//!      that maps `Net::HTTP` to `net/http`.
//!   5. Java OkHttp `client.newCall(...).execute()` — exercises the
//!      display_name path through the framework's `.scm` query, with
//!      the URL living upstream (the OkHttp `.scm` is execute-only).
//!
//! ## On scenario 2 (originally `client = HTTParty; client.get(url)`)
//!
//! The brief asks for a "HTTParty with receiver" test using the alias
//! form `client = HTTParty; client.get(url)`. The walker does not
//! currently have Ruby ctor-base_url / alias resolution (the
//! [`FileContext`] collector's Ruby arm is intentionally empty — see
//! `http_client_sensor.rs::collect_ruby_context`, which falls through to
//! `{}`). Adding alias resolution for outbound calls is out of scope
//! for this cleanup pass. Scenario 2 here is therefore adapted to a
//! shape that exercises the same `display_name` resolution code path
//! while still being testable: the constant receiver `HTTParty`
//! inside a class/method body. A future change that adds Ruby alias
//! resolution can extend this test to assert via="httparty" for
//! `client.get("...")` after `client = HTTParty`.

use lain::server::federation::contracts::model::{CallVia, HttpMethod, MethodSpec};
use lain::server::sensors::http_client_sensor::detect_calls;
use lain::server::sensors::patterns::Patterns;
use lain::server::sensors::util::Lang;

fn ruby_calls(src: &str) -> Vec<lain::server::sensors::http_client_sensor::HttpClientCall> {
    detect_calls(
        std::path::Path::new("test.rb"),
        src,
        Lang::Ruby,
        Patterns::patterns(),
    )
}

fn java_calls(src: &str) -> Vec<lain::server::sensors::http_client_sensor::HttpClientCall> {
    detect_calls(
        std::path::Path::new("Test.java"),
        src,
        Lang::Java,
        Patterns::patterns(),
    )
}

/// HTTParty.get direct — `display_name: httparty` resolves to
/// `via_lib = "httparty"` regardless of the captured receiver text
/// (the walker uses `effective_id()` instead of `framework.id` for
/// any framework whose `display_name` is set).
#[test]
fn httparty_direct_get_resolves_to_display_name() {
    let src = "HTTParty.get(\"https://example.com/users\")\n";
    let calls = ruby_calls(src);
    assert_eq!(
        calls.len(),
        1,
        "HTTParty.get must be detected via httparty-outbound.scm; got {calls:?}"
    );
    let c = &calls[0];
    assert_eq!(c.method, MethodSpec::Known(HttpMethod::Get));
    assert_eq!(c.url.template.as_deref(), Some("/users"));
    assert!(
        matches!(&c.via, CallVia::Library { name } if name == "httparty"),
        "via must be Library {{ name: \"httparty\" }} via display_name resolution; got {:?}",
        c.via
    );
}

/// HTTParty.get inside a class method body — confirms the
/// display_name path is class- and method-stable. The constant
/// receiver `HTTParty` still parses as `(constant)` per tree-sitter-
/// ruby, so `httparty-outbound.scm` matches. The walker resolves
/// `via_lib = "httparty"` via the framework's `display_name`, not
/// the captured receiver text.
#[test]
fn httparty_get_in_method_body_resolves_to_display_name() {
    let src = "\
class Foo
  def bar
    HTTParty.get(\"https://example.com/users\")
  end
end
";
    let calls = ruby_calls(src);
    assert_eq!(
        calls.len(),
        1,
        "HTTParty.get inside a class/method must be detected via httparty-outbound.scm; got {calls:?}"
    );
    let c = &calls[0];
    assert!(
        matches!(&c.via, CallVia::Library { name } if name == "httparty"),
        "via must be Library {{ name: \"httparty\" }} via display_name resolution; got {:?}",
        c.via
    );
}

/// Faraday direct — `faraday-outbound` does NOT have `display_name`
/// set (it's an internal constant-to-gem-name hardcoded arm). The
/// hardcoded arm survives Item 2 of the parked-Minor #2 pass; this
/// test pins that it still fires so a future cleanup removing the arm
/// would not silently break Faraday's `via_lib`.
#[test]
fn faraday_direct_get_resolves_to_gem_name() {
    let src = "Faraday.get(\"https://example.com/users\")\n";
    let calls = ruby_calls(src);
    assert_eq!(
        calls.len(),
        1,
        "Faraday.get must be detected via faraday-outbound.scm; got {calls:?}"
    );
    let c = &calls[0];
    assert!(
        matches!(&c.via, CallVia::Library { name } if name == "faraday"),
        "via must be Library {{ name: \"faraday\" }} via the hardcoded Ruby arm; got {:?}",
        c.via
    );
}

/// Net::HTTP direct — `net-http-outbound` does NOT have `display_name`
/// set. The captured receiver text is `Net::HTTP` (a `scope_resolution`
/// node); the hardcoded Ruby arm
/// `("net-http-outbound", "Net::HTTP") => "net/http"` must still
/// produce `via_lib = "net/http"`. `display_name` would NOT give us
/// "net/http" (the display_name would be a single token — `"net"` or
/// `"http"`, neither canonical), which is exactly why the hardcoded
/// arm survives the cleanup pass.
#[test]
fn net_http_direct_get_resolves_to_canonical_lib_path() {
    let src = "Net::HTTP.get(URI(\"https://example.com/users\"))\n";
    let calls = ruby_calls(src);
    assert_eq!(
        calls.len(),
        1,
        "Net::HTTP.get must be detected via net-http-outbound.scm; got {calls:?}"
    );
    let c = &calls[0];
    assert!(
        matches!(&c.via, CallVia::Library { name } if name == "net/http"),
        "via must be Library {{ name: \"net/http\" }} via the hardcoded Ruby arm; got {:?}",
        c.via
    );
}

/// Java OkHttp — `okhttp-outbound` carries `display_name: okhttp`
/// (Item 2 of the parked-Minor #2 pass). The walker falls through to
/// the `ktor / okhttp / httpclient` synthetic-URL branch (the .scm
/// matches `execute()` only — no `@url` capture and no `@lib`
/// capture), reads `framework.effective_id()` to mint both the
/// synthetic URL prefix and the `via_lib`. The expected `via_lib`
/// is `"okhttp"` — the public gem-name declared via `display_name`
/// in `frameworks.yaml`. A regression that drops the synthetic-URL
/// branch's `lib_text` fallback to `effective_id()` would emit
/// `Library { name: "" }` for this shape — the regression this test
/// pins.
#[test]
fn java_okhttp_execute_resolves_to_display_name() {
    let src = "\
class Foo {
    void bar() {
        OkHttpClient client = new OkHttpClient();
        client.newCall(new Request.Builder().url(\"https://example.com/users\").build()).execute();
    }
}
";
    let calls = java_calls(src);
    assert!(
        !calls.is_empty(),
        "OkHttpClient.newCall(...).execute() must be detected via okhttp-outbound.scm; got empty"
    );
    // The OkHttp `.scm` captures `execute` only — no `@url` and no
    // `@lib` — so the walker emits a synthetic URL. Pin that exactly
    // one call was emitted and that via is "okhttp".
    assert_eq!(calls.len(), 1, "got {calls:?}");
    let c = &calls[0];
    assert!(
        matches!(&c.via, CallVia::Library { name } if name == "okhttp"),
        "via must be Library {{ name: \"okhttp\" }} via display_name resolution; got {:?}",
        c.via
    );
}
