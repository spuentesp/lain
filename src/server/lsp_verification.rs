//! State machine for the per-binary LSP circuit breaker / restart budget.
//!
//! Specification (from the module docs):
//! * `RequestError`/`Timeout` bump a consecutive-failure counter; reaching
//!   `MAX_CONSECUTIVE_LSP_FAILURES` marks the binary unavailable.
//! * `record_success` resets that counter (and only that).
//! * `ProcessExited` on an available binary counts a restart in a sliding
//!   window; more than `LSP_RESTART_BUDGET` inside `LSP_RESTART_WINDOW`
//!   marks it unavailable (and clears the failure counter).
//! * `unavailable` is absorbing for the life of the process.
use super::*;
use proptest::prelude::*;
use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};

const BIN: &str = "rust-analyzer";
const WINDOW_MS: u64 = LSP_RESTART_WINDOW.as_millis() as u64;

#[derive(Clone, Debug)]
enum Op {
    Fail(FailureKind),
    Exit { advance_ms: u64 },
    Success,
    MarkUnavailable,
}

#[derive(Clone, Debug)]
struct Model {
    now_ms: u64,
    consecutive: u32,
    unavailable: bool,
    /// (count, window_start) once any restart was recorded.
    restarts: Option<(u32, u64)>,
}

struct Ref;
impl ReferenceStateMachine for Ref {
    type State = Model;
    type Transition = Op;

    fn init_state() -> BoxedStrategy<Model> {
        Just(Model {
            now_ms: 1_000_000,
            consecutive: 0,
            unavailable: false,
            restarts: None,
        })
        .boxed()
    }

    fn transitions(_: &Model) -> BoxedStrategy<Op> {
        prop_oneof![
            3 => prop_oneof![Just(FailureKind::RequestError), Just(FailureKind::Timeout)].prop_map(Op::Fail),
            3 => prop_oneof![0u64..5_000, 55_000u64..70_000, 0u64..200]
                    .prop_map(|advance_ms| Op::Exit { advance_ms }),
            2 => Just(Op::Success),
            1 => Just(Op::MarkUnavailable),
        ]
        .boxed()
    }

    fn apply(mut m: Model, op: &Op) -> Model {
        match op {
            Op::Fail(_) => {
                m.consecutive += 1;
                if m.consecutive >= MAX_CONSECUTIVE_LSP_FAILURES {
                    m.unavailable = true;
                }
            }
            Op::Exit { advance_ms } => {
                m.now_ms += advance_ms;
                if !m.unavailable {
                    let (count, start) = m.restarts.unwrap_or((0, m.now_ms));
                    let (c, s) = if m.now_ms.saturating_sub(start) > WINDOW_MS {
                        (1, m.now_ms)
                    } else {
                        (count + 1, start)
                    };
                    m.restarts = Some((c, s));
                    if c > LSP_RESTART_BUDGET {
                        m.unavailable = true;
                        m.consecutive = 0;
                    }
                }
            }
            Op::Success => m.consecutive = 0,
            Op::MarkUnavailable => m.unavailable = true,
        }
        m
    }
}

struct Sut(LspMultiplexer);

impl StateMachineTest for Sut {
    type SystemUnderTest = Sut;
    type Reference = Ref;

    fn init_test(_: &Model) -> Sut {
        Sut(LspMultiplexer::new(Path::new("."), &crate::tuning::RuntimeConfig::default()).unwrap())
    }

    fn apply(mut sut: Sut, model: &Model, op: Op) -> Sut {
        match op {
            Op::Fail(kind) => sut.0.record_lsp_failure_at(BIN, kind, model.now_ms),
            // `model` is the reference state AFTER this transition, so its
            // clock already includes `advance_ms`.
            Op::Exit { .. } => {
                sut.0
                    .record_lsp_failure_at(BIN, FailureKind::ProcessExited, model.now_ms)
            }
            Op::Success => sut.0.record_success(BIN),
            Op::MarkUnavailable => sut.0.mark_unavailable(BIN),
        }
        sut
    }

    fn check_invariants(sut: &Sut, m: &Model) {
        assert_eq!(
            sut.0.unavailable.contains(BIN),
            m.unavailable,
            "unavailable"
        );
        assert_eq!(
            sut.0.consecutive_failures.get(BIN).copied().unwrap_or(0),
            m.consecutive,
            "consecutive failures"
        );
        assert_eq!(
            sut.0.restart_budget.get(BIN).copied(),
            m.restarts,
            "restart budget"
        );
        // Threshold: the counter can never sit at/above the limit on an
        // available binary.
        if m.consecutive >= MAX_CONSECUTIVE_LSP_FAILURES {
            assert!(sut.0.unavailable.contains(BIN));
        }
    }
}

prop_state_machine! {
    #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]
    #[test]
    fn circuit_breaker_refines_specification(sequential 1..40 => Sut);
}
