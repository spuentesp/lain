//! Kani: `DirtyFlag` rules hold for every bounded op sequence.
//! Run: `cargo kani --harness dirty_flag_matches_its_model`.
use super::*;

#[kani::proof]
#[kani::unwind(7)]
fn dirty_flag_matches_its_model() {
    let init: bool = kani::any();
    let f = DirtyFlag::new(init);
    let mut model = init;
    let steps: usize = kani::any();
    kani::assume(steps <= 5);
    for _ in 0..steps {
        if kani::any::<bool>() {
            f.mark();
            model = true;
        } else {
            let fail: bool = kani::any();
            let mut ran = false;
            let r = f.run_if_dirty(|| {
                ran = true;
                if fail {
                    Err(())
                } else {
                    Ok(())
                }
            });
            // Runs the rebuild iff dirty.
            assert_eq!(ran, model);
            // A failed rebuild keeps the change pending; a successful one clears it.
            model = ran && fail;
            assert_eq!(r.is_err(), ran && fail);
        }
        assert_eq!(f.is_dirty(), model);
    }
}
