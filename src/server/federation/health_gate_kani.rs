//! Kani: over every bounded sequence of gate operations, the decision rules
//! hold. (The interleavings are covered by loom and `HoldGate.tla`; this
//! proves the sequential rules for all inputs, not sampled ones.)
//! Run: `cargo kani --harness gate_rules_hold_for_every_operation_sequence`.
use super::*;

fn any_health() -> RepoHealth {
    match kani::any::<u8>() % 5 {
        0 => RepoHealth::Ready,
        1 => RepoHealth::Indexing,
        2 => RepoHealth::Degraded,
        3 => RepoHealth::Unavailable,
        _ => RepoHealth::Missing,
    }
}

#[kani::proof]
#[kani::unwind(6)]
fn gate_rules_hold_for_every_operation_sequence() {
    let gate = HealthGate::new(any_health());
    // Shadow model of the hold flag.
    let mut hold = false;
    let steps: usize = kani::any();
    kani::assume(steps <= 4);
    for _ in 0..steps {
        match kani::any::<u8>() % 3 {
            0 => {
                gate.mark_ready();
                // A finished pass is Ready, unless the startup hold is on.
                let expect = if hold {
                    RepoHealth::Indexing
                } else {
                    RepoHealth::Ready
                };
                assert_eq!(gate.health(), expect);
            }
            1 => {
                let h = any_health();
                gate.set_health(h);
                assert_eq!(gate.health(), h);
            }
            _ => {
                let new_hold: bool = kani::any();
                let indexed: bool = kani::any();
                let before = gate.health();
                gate.set_hold(new_hold, || indexed);
                hold = new_hold;
                let after = gate.health();
                if !new_hold && before == RepoHealth::Indexing && indexed {
                    assert_eq!(after, RepoHealth::Ready); // release promotes
                } else {
                    assert_eq!(after, before); // otherwise health is untouched
                }
            }
        }
    }
}
