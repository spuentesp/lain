//! Executable counterparts of `docs/formal/ReadinessLifecycle.tla`.
//!
//! * `overlapping_pass_*`      — deterministic regression for the TLC counterexample.
//! * `state_machine`           — proptest-state-machine: the real handle vs. the TLA model.
//! * `loom_*`                  — every thread interleaving (`--cfg lain_loom`).
use super::*;

#[cfg(not(lain_loom))]
mod sequential {
    use super::*;
    use proptest::prelude::*;
    use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};

    /// The TLC counterexample, replayed on the real code: pass 1 finishes,
    /// pass 2 begins, then pass 1's caller publishes `ready`.
    #[test]
    fn overlapping_pass_cannot_open_the_gate() {
        let h = ReadinessHandle::default();
        let p1 = h.begin_pass();
        drop(p1); // pass 1 returns; its caller is now in sync_volatile_overlay()
        let p2 = h.begin_pass(); // pass 2 starts mutating the graph
        assert!(
            !h.ready(Some("c1".into())),
            "pass 1's late ready must be ignored"
        );
        assert_eq!(h.snapshot().state, IndexState::WarmingUp);
        drop(p2);
        assert!(h.ready(Some("c2".into())));
        let s = h.snapshot();
        assert_eq!(s.state, IndexState::Ready);
        assert_eq!(s.indexed_commit.as_deref(), Some("c2"));
    }

    #[test]
    fn cancelled_is_terminal() {
        let h = ReadinessHandle::default();
        h.cancelled();
        assert!(!h.ready(None));
        assert!(!h.failed("late".into()));
        let s = h.snapshot();
        assert_eq!(s.state, IndexState::UnavailableError);
        assert_eq!(s.problem.unwrap().code, INDEX_CANCELLED_CODE);
    }

    #[test]
    fn dropping_a_guard_on_unwind_releases_the_pass() {
        let h = ReadinessHandle::default();
        let h2 = h.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _pass = h2.begin_pass();
            panic!("pass blew up");
        }));
        assert!(
            h.ready(None),
            "a panicked pass must not wedge the gate shut"
        );
    }

    #[test]
    fn failed_then_later_success_recovers() {
        let h = ReadinessHandle::default();
        drop(h.begin_pass());
        assert!(h.failed("boom".into()));
        assert_eq!(h.snapshot().state, IndexState::UnavailableError);
        let _p = h.begin_pass();
        assert_eq!(h.snapshot().state, IndexState::WarmingUp);
        assert!(h.snapshot().problem.is_none());
    }

    /// Timestamps are real wall-clock milliseconds, and a completion never
    /// predates the start of the attempt it completes (mutation testing found
    /// `unix_ms` could return a constant without any test noticing).
    #[test]
    fn timestamps_are_real_and_ordered() {
        const YEAR_2023_MS: u64 = 1_672_531_200_000;
        let h = ReadinessHandle::default();
        let started = h.snapshot().started_at_unix_ms;
        assert!(
            started > YEAR_2023_MS,
            "started_at is not a wall-clock time: {started}"
        );
        assert!(h.snapshot().completed_at_unix_ms.is_none());

        assert!(h.ready(None));
        let done = h
            .snapshot()
            .completed_at_unix_ms
            .expect("ready stamps completion");
        assert!(
            done >= started,
            "completed before it started: {done} < {started}"
        );

        let h = ReadinessHandle::default();
        h.failed("e".into());
        let s = h.snapshot();
        assert!(s.completed_at_unix_ms.unwrap() >= s.started_at_unix_ms);

        let h = ReadinessHandle::default();
        h.cancelled();
        let s = h.snapshot();
        assert!(s.completed_at_unix_ms.unwrap() >= s.started_at_unix_ms);
        // Re-arming clears the completion stamp.
        let h = ReadinessHandle::default();
        h.ready(None);
        let _p = h.begin_pass();
        assert!(h.snapshot().completed_at_unix_ms.is_none());
        assert!(unix_ms() >= started);
    }

    // ---- proptest-state-machine -------------------------------------------------

    #[derive(Clone, Debug)]
    enum Op {
        Begin,
        EndOldest,
        Ready,
        Failed,
        Cancelled,
    }

    /// Mirrors the TLA+ variables: `Guarded = TRUE` semantics.
    #[derive(Clone, Debug)]
    struct Model {
        in_flight: u32,
        state: IndexState,
        cancelled: bool,
        attempt: u64,
    }

    struct Ref;
    impl ReferenceStateMachine for Ref {
        type State = Model;
        type Transition = Op;

        fn init_state() -> BoxedStrategy<Model> {
            Just(Model {
                in_flight: 0,
                state: IndexState::WarmingUp,
                cancelled: false,
                attempt: 1,
            })
            .boxed()
        }

        fn transitions(m: &Model) -> BoxedStrategy<Op> {
            let mut ops = vec![
                Just(Op::Ready).boxed(),
                Just(Op::Failed).boxed(),
                Just(Op::Cancelled).boxed(),
            ];
            ops.push(Just(Op::Begin).boxed());
            if m.in_flight > 0 {
                ops.push(Just(Op::EndOldest).boxed());
            }
            proptest::strategy::Union::new(ops).boxed()
        }

        fn apply(mut m: Model, op: &Op) -> Model {
            match op {
                Op::Begin => {
                    m.in_flight += 1;
                    if !m.cancelled {
                        m.state = IndexState::WarmingUp;
                        m.attempt += 1;
                    }
                }
                Op::EndOldest => m.in_flight -= 1,
                Op::Ready => {
                    if m.in_flight == 0 && !m.cancelled {
                        m.state = IndexState::Ready;
                    }
                }
                Op::Failed => {
                    if !m.cancelled {
                        m.state = IndexState::UnavailableError;
                    }
                }
                Op::Cancelled => {
                    m.state = IndexState::UnavailableError;
                    m.cancelled = true;
                }
            }
            m
        }
    }

    struct Sut {
        handle: ReadinessHandle,
        guards: Vec<PassGuard>,
    }

    impl StateMachineTest for Sut {
        type SystemUnderTest = Sut;
        type Reference = Ref;

        fn init_test(_: &Model) -> Sut {
            Sut {
                handle: ReadinessHandle::default(),
                guards: Vec::new(),
            }
        }

        fn apply(mut sut: Sut, _: &Model, op: Op) -> Sut {
            match op {
                Op::Begin => sut.guards.push(sut.handle.begin_pass()),
                Op::EndOldest => drop(sut.guards.remove(0)),
                Op::Ready => {
                    sut.handle.ready(Some("c".into()));
                }
                Op::Failed => {
                    sut.handle.failed("e".into());
                }
                Op::Cancelled => sut.handle.cancelled(),
            }
            sut
        }

        fn check_invariants(sut: &Sut, m: &Model) {
            let s = sut.handle.snapshot();
            // Refinement: the real handle tracks the model exactly.
            assert_eq!(s.state, m.state);
            assert_eq!(s.attempt_id, m.attempt);
            // The TLA invariants.
            if s.state == IndexState::Ready {
                assert_eq!(sut.guards.len(), 0, "GateOpenImpliesQuiescent");
                assert!(!m.cancelled, "CancelledIsTerminal");
            }
            if m.cancelled {
                assert_ne!(s.state, IndexState::Ready, "CancelledIsTerminal");
            }
            // Sequence numbers are strictly monotone observation points.
        }
    }

    prop_state_machine! {
        #![proptest_config(ProptestConfig { cases: 256, max_shrink_iters: 2000, ..ProptestConfig::default() })]
        #[test]
        fn state_machine(sequential 1..40 => Sut);
    }

    proptest! {
        /// `sequence` never decreases across any publication.
        #[test]
        fn sequence_is_monotone(ops in proptest::collection::vec(0u8..5, 0..60)) {
            let h = ReadinessHandle::default();
            let mut guards = Vec::new();
            let mut last = h.snapshot().sequence;
            for op in ops {
                match op {
                    0 => guards.push(h.begin_pass()),
                    1 => if !guards.is_empty() { drop(guards.remove(0)) },
                    2 => { h.ready(None); }
                    3 => { h.failed("x".into()); }
                    _ => h.cancelled(),
                }
                let now = h.snapshot().sequence;
                prop_assert!(now >= last);
                last = now;
            }
        }
    }
}

/// loom: every interleaving of two passes + their callers' publications.
/// Run: `RUSTFLAGS="--cfg lain_loom" cargo test --lib loom_ --release`.
#[cfg(lain_loom)]
mod loom_tests {
    use super::*;
    use loom::thread;
    use std::sync::Arc;

    /// Two passes race. At every point the gate may be `Ready` only if no
    /// pass is mutating; once both are done the gate must be `Ready` (no
    /// stranded `warming_up`).
    #[test]
    fn loom_two_passes_never_open_gate_early_and_never_strand() {
        loom::model(|| {
            let h = Arc::new(ReadinessHandle::default());
            let workers: Vec<_> = (0..2)
                .map(|_| {
                    let h = h.clone();
                    thread::spawn(move || {
                        let pass = h.begin_pass();
                        // Gate must be shut while this pass is mutating.
                        assert_ne!(h.snapshot().state, IndexState::Ready);
                        drop(pass);
                        h.ready(Some("c".into()));
                    })
                })
                .collect();
            for w in workers {
                w.join().unwrap();
            }
            let core = h.0.lock();
            assert_eq!(core.in_flight, 0);
            assert_eq!(core.snapshot.state, IndexState::Ready);
        });
    }

    /// A cancel racing a finishing pass: `ready` must never win over a
    /// published cancellation.
    #[test]
    fn loom_cancel_beats_ready() {
        loom::model(|| {
            let h = Arc::new(ReadinessHandle::default());
            let a = {
                let h = h.clone();
                thread::spawn(move || {
                    drop(h.begin_pass());
                    h.ready(None);
                })
            };
            let b = {
                let h = h.clone();
                thread::spawn(move || h.cancelled())
            };
            a.join().unwrap();
            b.join().unwrap();
            // ready-then-cancel ends cancelled; cancel-then-ready is refused.
            // Every interleaving therefore ends cancelled.
            let s = h.snapshot();
            assert_eq!(s.state, IndexState::UnavailableError);
            assert_eq!(s.problem.unwrap().code, INDEX_CANCELLED_CODE);
        });
    }
}
