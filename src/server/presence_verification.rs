//! Model-based verification of `OccupancyMap` (claim/release bookkeeping).
//!
//! The reference model is the *specification* stated in the code's own
//! comments (wishlist #5): Read never conflicts; an Edit conflicts with any
//! other agent's Edit on an overlapping scope, where a file-level scope
//! overlaps everything in the file and symbol scopes overlap when they share
//! a symbol; `release` drops all of an agent's scopes on a path.
//! The real map must refine it, and its two mirrored indexes (`by_file`,
//! `by_agent`) must agree after every step.
use super::*;
use proptest::prelude::*;
use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};
use std::collections::BTreeMap;

const FILE_LEVEL: &str = "__file_level__";
const AGENTS: [&str; 3] = ["a", "b", "c"];
const PATHS: [&str; 2] = ["zz_verif_p1.rs", "zz_verif_p2.rs"];
const SYMS: [&str; 2] = ["s1", "s2"];

type Key = (String, String, String); // (agent, path, scope: symbol or FILE_LEVEL)
type Scope = BTreeMap<Key, ClaimIntent>;

#[derive(Clone, Debug)]
struct Req {
    path: &'static str,
    syms: Vec<&'static str>,
    intent: ClaimIntent,
}

#[derive(Clone, Debug)]
enum Op {
    Claim(&'static str, Vec<Req>),
    Release(&'static str, Vec<&'static str>),
    ReleaseAll(&'static str),
}

fn edit_conflicts(m: &Scope, agent: &str, path: &str, syms: &[&str]) -> bool {
    m.iter().any(|((a, p, scope), intent)| {
        a != agent
            && p == path
            && *intent == ClaimIntent::Edit
            && (syms.is_empty() // file-level request overlaps any Edit
                || scope == FILE_LEVEL // holder rewrites the whole file
                || syms.contains(&scope.as_str()))
    })
}

fn model_claim(m: &mut Scope, agent: &str, reqs: &[Req]) {
    for r in reqs {
        if r.intent == ClaimIntent::Edit && edit_conflicts(m, agent, r.path, &r.syms) {
            continue;
        }
        let scopes: Vec<&str> = if r.syms.is_empty() {
            vec![FILE_LEVEL]
        } else {
            r.syms.clone()
        };
        for s in scopes {
            m.insert((agent.into(), r.path.into(), s.into()), r.intent.clone());
        }
    }
}

struct Ref;
impl ReferenceStateMachine for Ref {
    type State = Scope;
    type Transition = Op;

    fn init_state() -> BoxedStrategy<Scope> {
        Just(Scope::new()).boxed()
    }

    fn transitions(_: &Scope) -> BoxedStrategy<Op> {
        let agent = prop::sample::select(AGENTS.to_vec());
        let path = prop::sample::select(PATHS.to_vec());
        let syms = prop::sample::subsequence(SYMS.to_vec(), 0..=2);
        let intent = prop_oneof![Just(ClaimIntent::Read), Just(ClaimIntent::Edit)];
        let req = (path.clone(), syms, intent).prop_map(|(path, syms, intent)| Req {
            path,
            syms,
            intent,
        });
        prop_oneof![
            4 => (agent.clone(), prop::collection::vec(req, 1..3)).prop_map(|(a, r)| Op::Claim(a, r)),
            2 => (agent.clone(), prop::sample::subsequence(PATHS.to_vec(), 1..=2))
                    .prop_map(|(a, p)| Op::Release(a, p)),
            1 => agent.prop_map(Op::ReleaseAll),
        ]
        .boxed()
    }

    fn apply(mut m: Scope, op: &Op) -> Scope {
        match op {
            Op::Claim(a, reqs) => model_claim(&mut m, a, reqs),
            Op::Release(a, paths) => {
                m.retain(|(ag, p, _), _| !(ag == a && paths.contains(&p.as_str())))
            }
            Op::ReleaseAll(a) => m.retain(|(ag, _, _), _| ag != a),
        }
        m
    }
}

fn agent(a: &str) -> AgentId {
    AgentId(a.to_string())
}

fn real_scope(map: &OccupancyMap) -> Scope {
    let s = map.inner.lock();
    let mut out = Scope::new();
    for (path, occ) in &s.by_file {
        for (scope, per_agent) in &occ.intents {
            for (a, intent) in per_agent {
                out.insert(
                    (
                        a.0.clone(),
                        path.to_string_lossy().into_owned(),
                        scope.clone(),
                    ),
                    intent.clone(),
                );
            }
        }
    }
    out
}

struct Sut(OccupancyMap);

impl StateMachineTest for Sut {
    type SystemUnderTest = Sut;
    type Reference = Ref;

    fn init_test(_: &Scope) -> Sut {
        Sut(OccupancyMap::new())
    }

    fn apply(sut: Sut, _: &Scope, op: Op) -> Sut {
        match op {
            Op::Claim(a, reqs) => {
                let reqs = reqs
                    .into_iter()
                    .map(|r| ClaimRequest {
                        path: PathBuf::from(r.path),
                        symbols: r.syms.iter().map(|s| s.to_string()).collect(),
                        intent: r.intent,
                        ttl_seconds: None,
                        plan_revision: None,
                    })
                    .collect();
                sut.0.claim(&agent(a), reqs);
            }
            Op::Release(a, paths) => {
                sut.0.release(
                    &agent(a),
                    &paths.iter().map(PathBuf::from).collect::<Vec<_>>(),
                );
            }
            Op::ReleaseAll(a) => {
                sut.0.release_all_for(&agent(a));
            }
        }
        sut
    }

    fn check_invariants(sut: &Sut, model: &Scope) {
        let real = real_scope(&sut.0);

        // 1. Safety: no two agents ever hold overlapping Edit scopes.
        for ((a, p, scope), intent) in &real {
            if *intent != ClaimIntent::Edit {
                continue;
            }
            for ((a2, p2, scope2), intent2) in &real {
                if a == a2 || p != p2 || *intent2 != ClaimIntent::Edit {
                    continue;
                }
                assert!(
                    scope != scope2 && scope != FILE_LEVEL && scope2 != FILE_LEVEL,
                    "write-write overlap: {a}:{scope} and {a2}:{scope2} both Edit {p}\nreal={real:#?}"
                );
            }
        }

        // 2. Refinement: the real map equals the specification model.
        assert_eq!(
            &real, model,
            "real occupancy diverged from the specification"
        );

        // 3. The mirrored indexes agree.
        let s = sut.0.inner.lock();
        for (path, occ) in &s.by_file {
            assert!(
                !occ.agents.is_empty(),
                "empty FileOccupancy leaked for {path:?}"
            );
            for a in &occ.agents {
                assert!(
                    s.by_agent
                        .get(a)
                        .is_some_and(|cs| cs.iter().any(|c| &c.path == path)),
                    "by_file lists {a:?} on {path:?} but by_agent has no claim"
                );
            }
            for agents in occ.symbols.values() {
                assert!(agents.is_subset(&occ.agents));
            }
        }
        for (a, claims) in &s.by_agent {
            assert!(!claims.is_empty(), "empty by_agent bucket leaked for {a:?}");
            let mut seen = std::collections::HashSet::new();
            for c in claims {
                assert!(
                    s.by_file.get(&c.path).is_some_and(|o| o.agents.contains(a)),
                    "by_agent claim {c:?} has no by_file entry"
                );
                assert!(
                    seen.insert((c.path.clone(), c.symbols.clone())),
                    "duplicate claim row"
                );
            }
        }
    }
}

prop_state_machine! {
    #![proptest_config(ProptestConfig { cases: 512, max_shrink_iters: 5000, ..ProptestConfig::default() })]
    #[test]
    fn occupancy_refines_specification(sequential 1..30 => Sut);
}

/// Shrunk counterexample found by the state machine above: a file-level Read
/// by the symbol's own editor used to hide its symbol-level Edit.
#[test]
fn file_level_read_does_not_hide_a_symbol_edit() {
    let map = OccupancyMap::new();
    let req = |syms: &[&str], intent| ClaimRequest {
        path: PathBuf::from("zz_verif_regress.rs"),
        symbols: syms.iter().map(|s| s.to_string()).collect(),
        intent,
        ttl_seconds: None,
        plan_revision: None,
    };
    let (b, c) = (agent("b"), agent("c"));
    assert!(map
        .claim(&b, vec![req(&["s2"], ClaimIntent::Edit)])
        .conflicts
        .is_empty());
    assert!(map
        .claim(&b, vec![req(&[], ClaimIntent::Read)])
        .conflicts
        .is_empty());

    let r = map.claim(&c, vec![req(&[], ClaimIntent::Edit)]);
    assert!(
        r.granted.is_empty(),
        "file-level Edit granted over b's symbol Edit"
    );
    assert_eq!(r.conflicts.len(), 1);
    assert_eq!(r.conflicts[0].agent_id, b);
    assert_eq!(r.conflicts[0].intent, ClaimIntent::Edit);

    // And a reader is still warned about the live editor.
    let r = map.claim(&c, vec![req(&[], ClaimIntent::Read)]);
    assert_eq!(r.advisories.len(), 1);
}
