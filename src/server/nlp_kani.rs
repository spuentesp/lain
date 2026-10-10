//! Kani: the thread-count rule over every `(max_threads, cores)`.
use super::*;

#[kani::proof]
fn intra_threads_is_always_positive_and_bounded() {
    let max: usize = kani::any();
    let cores: usize = kani::any();
    let n = intra_threads_for(max, cores);
    assert!(n >= 1, "never zero threads");
    if max == 0 {
        assert!((1..=4).contains(&n), "automatic choice is clamped to 1..=4");
        if (1..=4).contains(&cores) {
            assert_eq!(n, cores);
        }
    } else {
        assert_eq!(n, max, "an explicit cap is honoured as given");
    }
}
