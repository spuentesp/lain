//! Verification of [`DirtyFlag`]'s clear-before-read protocol.
//! TLA+ counterpart: `docs/formal/RejoinProtocol*.tla`.
use super::*;

#[cfg(not(lain_loom))]
mod sequential {
    use super::*;

    #[test]
    fn clean_flag_does_not_run_the_rebuild() {
        let f = DirtyFlag::new(false);
        let mut ran = false;
        f.run_if_dirty::<()>(|| {
            ran = true;
            Ok(())
        })
        .unwrap();
        assert!(!ran);
    }

    #[test]
    fn successful_rebuild_clears_and_failed_rebuild_restores() {
        let f = DirtyFlag::new(true);
        assert!(f.run_if_dirty::<()>(|| Ok(())).is_ok());
        assert!(!f.is_dirty());
        f.mark();
        assert_eq!(f.run_if_dirty(|| Err("boom")), Err("boom"));
        assert!(f.is_dirty(), "a failed rebuild must not swallow the mark");
    }

    #[test]
    fn a_mark_during_the_rebuild_survives_it() {
        let f = DirtyFlag::new(true);
        f.run_if_dirty::<()>(|| {
            f.mark(); // writer lands mid-rebuild
            Ok(())
        })
        .unwrap();
        assert!(f.is_dirty(), "mid-rebuild mark must re-arm the flag");
    }
}

/// loom: a writer (`config = c1; mark`) races the rebuilder under every
/// interleaving. After a final quiescent rebuild the derived value must
/// equal the input - the `Convergence` invariant of RejoinProtocol.tla.
/// Run: `RUSTFLAGS="--cfg lain_loom" cargo test --lib loom_ --release`.
#[cfg(lain_loom)]
mod loom_tests {
    use super::*;
    use loom::thread;
    use std::sync::Arc;

    struct World {
        projection_lock: Mutex<()>,
        flag: DirtyFlag,
        config: Mutex<u32>,
        derived: Mutex<u32>,
    }

    impl World {
        fn new() -> Self {
            Self {
                projection_lock: Mutex::new(()),
                flag: DirtyFlag::new(true),
                config: Mutex::new(0),
                derived: Mutex::new(u32::MAX), // never computed
            }
        }
        fn set_config(&self, v: u32) {
            *self.config.lock() = v;
            self.flag.mark();
        }
        fn rejoin(&self) {
            let _g = self.projection_lock.lock();
            self.flag
                .run_if_dirty::<()>(|| {
                    let c = *self.config.lock(); // read inputs AFTER the clear
                    *self.derived.lock() = c;
                    Ok(())
                })
                .unwrap();
        }
    }

    #[test]
    fn loom_writer_racing_rebuilder_converges() {
        loom::model(|| {
            let w = Arc::new(World::new());
            let writer = {
                let w = w.clone();
                thread::spawn(move || w.set_config(1))
            };
            let rejoiner = {
                let w = w.clone();
                thread::spawn(move || w.rejoin())
            };
            writer.join().unwrap();
            rejoiner.join().unwrap();
            // Quiescent: one more rebuild attempt, as the next reader would.
            w.rejoin();
            assert!(!w.flag.is_dirty());
            assert_eq!(*w.derived.lock(), *w.config.lock(), "stale derived state");
        });
    }

    /// Negative control: the pre-fix protocol (clear AFTER the rebuild) must
    /// be caught by the same model, proving the test has teeth.
    #[test]
    #[should_panic(expected = "stale derived state")]
    fn loom_clear_after_read_is_caught() {
        loom::model(|| {
            let w = Arc::new(World::new());
            let writer = {
                let w = w.clone();
                thread::spawn(move || w.set_config(1))
            };
            let rejoiner = {
                let w = w.clone();
                thread::spawn(move || {
                    let _g = w.projection_lock.lock();
                    if w.flag.is_dirty() {
                        let c = *w.config.lock(); // read first ...
                        *w.derived.lock() = c;
                        w.flag.0.store(false, Ordering::Release); // ... clear last (bug)
                    }
                })
            };
            writer.join().unwrap();
            rejoiner.join().unwrap();
            w.rejoin();
            assert_eq!(*w.derived.lock(), *w.config.lock(), "stale derived state");
        });
    }
}
