//! Soundness tests for the thread-local `&Patterns` pointer. Written to be
//! run under Miri (`cargo +nightly miri test --lib util::verification`),
//! which checks the raw-pointer accesses for use-after-free / aliasing.
use super::*;

fn installed() -> *const Patterns {
    CURRENT_PATTERNS.with(|c| *c.borrow())
}

#[test]
fn pointer_is_installed_only_inside_the_scope_and_restored_after() {
    let local = Patterns::patterns().clone();
    assert!(installed().is_null());
    with_current_patterns(&local, || {
        assert_eq!(installed(), &local as *const Patterns);
        // Reads through the pointer while the pointee is alive.
        with_active_patterns(|p| assert!(std::ptr::eq(p, &local)));
    });
    assert!(installed().is_null(), "pointer must not outlive the scope");
}

#[test]
fn nested_scopes_restore_the_outer_pointer() {
    let outer = Patterns::patterns().clone();
    let inner = Patterns::patterns().clone();
    with_current_patterns(&outer, || {
        with_current_patterns(&inner, || {
            with_active_patterns(|p| assert!(std::ptr::eq(p, &inner)));
        });
        with_active_patterns(|p| assert!(std::ptr::eq(p, &outer)));
    });
    assert!(installed().is_null());
}

#[test]
fn pointer_is_cleared_when_the_scope_panics() {
    let local = Patterns::patterns().clone();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        with_current_patterns(&local, || panic!("scan blew up"));
    }));
    assert!(r.is_err());
    assert!(
        installed().is_null(),
        "a panicking scan must not leave a dangling pointer"
    );
}

#[test]
fn outside_a_scan_the_bundled_singleton_is_used() {
    with_active_patterns(|p| assert!(std::ptr::eq(p, Patterns::patterns())));
}
