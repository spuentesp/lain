//! Shared gate for tests that exercise real zoanthrope arbitration.
//!
//! Included via `#[path = "support/zoanthrope_gate.rs"]` (the
//! `tests/support/` harness pattern) by the coordination battery
//! tests. Those tests document *today's* contract and are only
//! meaningful against a live `zoanthrope`: with the binary absent the
//! proxy degrades to advisory semantics (claims always grant, never
//! conflict) and every assertion about arbitration would be vacuous.
//!
//! Skip policy: a bare local `cargo test` skips with a loud message.
//! The CI lanes that install the product export
//! `CI_REQUIRE_ZOANTHROPE=1`, which turns that skip into a hard
//! failure — the battery must actually run in CI, never silently
//! self-disable.

/// True when a `zoanthrope` binary is resolvable (`$ZOANTHROPE_BIN`,
/// then `PATH`). When `CI_REQUIRE_ZOANTHROPE=1` and no binary is
/// found, panics instead of skipping.
pub fn require_zoanthrope() -> bool {
    if lain::server::coordination::resolve_zoanthrope_bin().is_some() {
        return true;
    }
    let required = std::env::var_os("CI_REQUIRE_ZOANTHROPE")
        .map(|v| v == "1")
        .unwrap_or(false);
    assert!(
        !required,
        "zoanthrope is required (CI_REQUIRE_ZOANTHROPE=1) but no binary was found on \
         PATH or via $ZOANTHROPE_BIN — the coordination battery must run against real \
         arbitration, not the degraded advisory fallback"
    );
    eprintln!(
        "SKIP: zoanthrope not on PATH (coordination degraded/advisory); \
         CI sets CI_REQUIRE_ZOANTHROPE=1 to turn this skip into a failure"
    );
    false
}
