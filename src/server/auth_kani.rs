//! Kani: the constant-time comparison is functionally equal to `==`.
//! Run: `cargo kani --harness constant_time_eq_matches_slice_equality`.
use super::*;

#[kani::proof]
#[kani::unwind(9)]
fn constant_time_eq_matches_slice_equality() {
    let a: [u8; 8] = kani::any();
    let b: [u8; 8] = kani::any();
    let la: usize = kani::any();
    let lb: usize = kani::any();
    kani::assume(la <= 8 && lb <= 8);
    assert_eq!(constant_time_eq(&a[..la], &b[..lb]), a[..la] == b[..lb]);
}
