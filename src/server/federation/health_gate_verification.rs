//! Executable counterpart of `docs/formal/HoldGate*.cfg`.
use super::*;

#[cfg(not(lain_loom))]
mod sequential {
    use super::*;

    #[test]
    fn hold_defers_ready_and_release_promotes_a_finished_pass() {
        let g = HealthGate::new(RepoHealth::Indexing);
        g.set_hold(true, || false);
        g.mark_ready(); // pass finished while held
        assert_eq!(g.health(), RepoHealth::Indexing);
        g.set_hold(false, || true);
        assert_eq!(g.health(), RepoHealth::Ready);
    }

    #[test]
    fn release_before_the_pass_finishes_does_not_promote() {
        let g = HealthGate::new(RepoHealth::Indexing);
        g.set_hold(true, || false);
        g.set_hold(false, || false); // nothing indexed yet
        assert_eq!(g.health(), RepoHealth::Indexing);
        g.mark_ready();
        assert_eq!(g.health(), RepoHealth::Ready);
    }

    #[test]
    fn release_never_masks_degraded() {
        let g = HealthGate::new(RepoHealth::Indexing);
        g.set_hold(true, || false);
        g.set_health(RepoHealth::Degraded);
        g.set_hold(false, || true);
        assert_eq!(g.health(), RepoHealth::Degraded);
    }
}

/// Every interleaving of "indexer finishes" against "startup hold released":
/// the repo ends `Ready`, never stuck `Indexing`.
/// Run: `RUSTFLAGS="--cfg lain_loom" cargo test --lib loom_ --release`.
#[cfg(lain_loom)]
mod loom_tests {
    use super::*;
    use crate::sync::{AtomicBool, Ordering};
    use loom::thread;
    use std::sync::Arc;

    #[test]
    fn loom_release_racing_the_end_of_indexing_never_sticks_indexing() {
        loom::model(|| {
            let gate = Arc::new(HealthGate::new(RepoHealth::Indexing));
            gate.set_hold(true, || false);
            let indexed = Arc::new(AtomicBool::new(false));

            let indexer = {
                let (gate, indexed) = (gate.clone(), indexed.clone());
                thread::spawn(move || {
                    indexed.store(true, Ordering::SeqCst); // last_indexed = now
                    gate.mark_ready();
                })
            };
            let releaser = {
                let (gate, indexed) = (gate.clone(), indexed.clone());
                thread::spawn(move || {
                    gate.set_hold(false, || indexed.load(Ordering::SeqCst));
                })
            };
            indexer.join().unwrap();
            releaser.join().unwrap();
            assert_eq!(
                gate.health(),
                RepoHealth::Ready,
                "stuck Indexing after release"
            );
        });
    }

    /// Negative control: the pre-fix shape (hold and health read/written in
    /// separate steps) must be caught by the same model.
    #[test]
    #[should_panic(expected = "stuck Indexing")]
    fn loom_separate_steps_are_caught() {
        use crate::sync::Mutex;
        struct Racy {
            hold: AtomicBool,
            health: Mutex<RepoHealth>,
        }
        loom::model(|| {
            let g = Arc::new(Racy {
                hold: AtomicBool::new(true),
                health: Mutex::new(RepoHealth::Indexing),
            });
            let indexed = Arc::new(AtomicBool::new(false));
            let indexer = {
                let (g, indexed) = (g.clone(), indexed.clone());
                thread::spawn(move || {
                    indexed.store(true, Ordering::SeqCst);
                    let held = g.hold.load(Ordering::SeqCst); // read ...
                    *g.health.lock() = if held {
                        RepoHealth::Indexing
                    } else {
                        RepoHealth::Ready
                    }; // ... write later
                })
            };
            let releaser = {
                let (g, indexed) = (g.clone(), indexed.clone());
                thread::spawn(move || {
                    g.hold.store(false, Ordering::SeqCst);
                    let promote =
                        *g.health.lock() == RepoHealth::Indexing && indexed.load(Ordering::SeqCst);
                    if promote {
                        *g.health.lock() = RepoHealth::Ready;
                    }
                })
            };
            indexer.join().unwrap();
            releaser.join().unwrap();
            assert_eq!(
                *g.health.lock(),
                RepoHealth::Ready,
                "stuck Indexing after release"
            );
        });
    }
}
