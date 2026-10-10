//! Executable counterparts of `docs/formal/ReloadBus.tla`.
use super::*;

#[cfg(not(lain_loom))]
mod sequential {
    use super::*;
    use proptest::prelude::*;
    use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};

    /// Channel semantics the TLA model assumes: a request issued before
    /// anyone subscribes is dropped (so the loop MUST subscribe before the
    /// producers go live — cli/server.rs), and overflow still yields a signal.
    #[test]
    fn request_with_no_receiver_is_dropped_so_subscribe_must_come_first() {
        let bus = ReloadBus::new();
        bus.request_reload().unwrap();
        let mut late = bus.subscribe();
        assert!(
            late.try_recv().is_err(),
            "pre-subscribe request must not be replayed"
        );

        let mut early = bus.subscribe();
        bus.request_reload().unwrap();
        assert!(early.try_recv().is_ok());
    }

    #[test]
    fn overflowing_the_channel_still_delivers_a_signal() {
        let bus = ReloadBus::new();
        let mut sub = bus.subscribe();
        for _ in 0..100 {
            bus.request_reload().unwrap(); // capacity is 16: receiver lags
        }
        assert!(
            sub.try_recv().is_ok(),
            "Lagged must still surface as a signal"
        );
    }

    #[test]
    fn status_after_failed_keeps_error_until_next_rebuild() {
        let bus = ReloadBus::new();
        bus.apply_state(ReloadState::Failed("boom".into()));
        for _ in 0..3 {
            let s = bus.status();
            assert_eq!(s.state, ReloadState::Failed("boom".into()));
            assert_eq!(s.last_error.as_deref(), Some("boom"));
        }
        bus.apply_state(ReloadState::Rebuilding);
        assert!(bus.status().last_error.is_none());
    }

    #[derive(Clone, Debug)]
    enum Op {
        Rebuilding,
        Idle,
        Failed(String),
    }

    #[derive(Clone, Debug)]
    struct Model {
        state: ReloadState,
        last_error: Option<String>,
        ever_idle: bool,
    }

    struct Ref;
    impl ReferenceStateMachine for Ref {
        type State = Model;
        type Transition = Op;
        fn init_state() -> BoxedStrategy<Model> {
            Just(Model {
                state: ReloadState::Idle,
                last_error: None,
                ever_idle: false,
            })
            .boxed()
        }
        fn transitions(_: &Model) -> BoxedStrategy<Op> {
            prop_oneof![
                Just(Op::Rebuilding),
                Just(Op::Idle),
                "[a-z]{1,6}".prop_map(Op::Failed)
            ]
            .boxed()
        }
        fn apply(mut m: Model, op: &Op) -> Model {
            match op {
                Op::Rebuilding => {
                    m.state = ReloadState::Rebuilding;
                    m.last_error = None;
                }
                Op::Idle => {
                    m.state = ReloadState::Idle;
                    m.ever_idle = true;
                }
                Op::Failed(e) => {
                    m.state = ReloadState::Failed(e.clone());
                    m.last_error = Some(e.clone());
                }
            }
            m
        }
    }

    struct Sut {
        bus: ReloadBus,
        last_reload: Option<SystemTime>,
    }

    impl StateMachineTest for Sut {
        type SystemUnderTest = Sut;
        type Reference = Ref;
        fn init_test(_: &Model) -> Sut {
            Sut {
                bus: ReloadBus::new(),
                last_reload: None,
            }
        }
        fn apply(mut sut: Sut, _: &Model, op: Op) -> Sut {
            sut.bus.apply_state(match op {
                Op::Rebuilding => ReloadState::Rebuilding,
                Op::Idle => ReloadState::Idle,
                Op::Failed(e) => ReloadState::Failed(e),
            });
            sut.last_reload = sut.bus.status().last_reload_at.or(sut.last_reload);
            sut
        }
        fn check_invariants(sut: &Sut, m: &Model) {
            let s = sut.bus.status();
            assert_eq!(s.state, m.state);
            assert_eq!(s.last_error, m.last_error);
            match &s.state {
                ReloadState::Rebuilding => assert!(s.started_at.is_some()),
                ReloadState::Idle | ReloadState::Failed(_) => assert!(s.started_at.is_none()),
            }
            assert_eq!(s.last_reload_at.is_some(), m.ever_idle);
        }
    }

    prop_state_machine! {
        #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]
        #[test]
        fn status_state_machine(sequential 1..40 => Sut);
    }
}

/// `status()` must never fabricate: once the writer has published `Failed`
/// (and never publishes `Idle`), no reader may observe `Idle` afterwards.
/// Run: `RUSTFLAGS="--cfg lain_loom" cargo test --lib loom_ --release`.
#[cfg(lain_loom)]
mod loom_tests {
    use super::*;
    use loom::thread;
    use std::sync::Arc;

    #[test]
    fn loom_status_never_regresses_to_idle_under_contention() {
        loom::model(|| {
            let bus = Arc::new(ReloadBus::new());
            let writer = {
                let bus = bus.clone();
                thread::spawn(move || {
                    bus.apply_state(ReloadState::Rebuilding);
                    bus.apply_state(ReloadState::Failed("boom".into()));
                })
            };
            let reader = {
                let bus = bus.clone();
                thread::spawn(move || {
                    let first = bus.status();
                    let second = bus.status();
                    if first.state != ReloadState::Idle {
                        assert_ne!(second.state, ReloadState::Idle, "fabricated Idle");
                    }
                    if let ReloadState::Failed(_) = first.state {
                        assert_eq!(first.last_error.as_deref(), Some("boom"));
                    }
                })
            };
            writer.join().unwrap();
            reader.join().unwrap();
            assert_eq!(bus.status().state, ReloadState::Failed("boom".into()));
        });
    }
}
