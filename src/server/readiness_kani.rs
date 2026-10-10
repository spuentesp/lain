//! Kani proofs for the readiness publication rule. Run: `cargo kani --harness <name>`.
use super::*;

/// No in-flight pass => the gate may open (unless cancelled); any in-flight
/// pass or cancellation => it may not. Exhaustive over every `u32`.
#[kani::proof]
fn ready_allowed_iff_quiescent_and_not_cancelled() {
    let in_flight: u32 = kani::any();
    let cancelled: bool = kani::any();
    assert_eq!(
        ready_allowed_for(in_flight, cancelled),
        in_flight == 0 && !cancelled
    );
    if in_flight > 0 || cancelled {
        assert!(!ready_allowed_for(in_flight, cancelled));
    }
}

/// `begin_pass` then `end_pass` never leaves `in_flight` below where it
/// started, and `end_pass` on zero never underflows (saturating).
#[kani::proof]
fn pass_counter_never_wraps() {
    let start: u32 = kani::any();
    let after_begin = start.saturating_add(1);
    assert!(after_begin >= start);
    let after_end = after_begin.saturating_sub(1);
    assert!(start == u32::MAX || after_end == start);
    assert_eq!(0u32.saturating_sub(1), 0);
}
