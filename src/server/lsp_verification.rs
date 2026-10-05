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

// ---- the pool ----------------------------------------------------------------------------

/// A pool must always hand out a multiplexer, whatever size tuning asked for.
/// (`lsp_pool_size = 0` in `.lain/tuning.toml` made `next()` compute
/// `counter % 0`.)
#[test]
fn a_zero_sized_pool_still_serves() {
    let pool = LspPool::new(Path::new("."), 0, &crate::tuning::RuntimeConfig::default()).unwrap();
    assert!(pool.size() >= 1, "a pool of size 0 has nothing to hand out");
    let _ = pool.next(); // must not panic
}

#[test]
fn a_tuning_file_cannot_configure_an_empty_pool() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".lain")).unwrap();
    std::fs::write(
        dir.path().join(".lain/tuning.toml"),
        "[ingestion]\nlsp_pool_size = 0\n",
    )
    .unwrap();
    let cfg = crate::tuning::load_tuning_config(dir.path());
    assert!(
        cfg.ingestion.lsp_pool_size >= 1,
        "tuning accepted lsp_pool_size = 0"
    );
}

/// Round-robin spreads calls evenly even when clones race.
#[test]
fn round_robin_is_balanced_across_racing_clones() {
    let pool = LspPool::new(Path::new("."), 3, &crate::tuning::RuntimeConfig::default()).unwrap();
    let hits: Vec<std::sync::Arc<std::sync::atomic::AtomicUsize>> =
        (0..3).map(|_| Default::default()).collect();
    let muxes: Vec<_> = (0..3).map(|_| pool.next()).collect(); // one full cycle -> the 3 distinct muxes
    let handles: Vec<_> = (0..6)
        .map(|_| {
            let (pool, hits, muxes) = (pool.clone(), hits.clone(), muxes.clone());
            std::thread::spawn(move || {
                for _ in 0..300 {
                    let m = pool.next();
                    let i = muxes
                        .iter()
                        .position(|x| std::sync::Arc::ptr_eq(x, &m))
                        .unwrap();
                    hits[i].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let counts: Vec<usize> = hits
        .iter()
        .map(|h| h.load(std::sync::atomic::Ordering::Relaxed))
        .collect();
    assert_eq!(counts.iter().sum::<usize>(), 1800);
    assert!(
        counts.iter().max().unwrap() - counts.iter().min().unwrap() <= 1,
        "unbalanced: {counts:?}"
    );
}
