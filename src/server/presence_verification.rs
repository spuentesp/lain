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

// ---- the pieces the model above does not reach --------------------------------------------

mod pieces {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as O};
    use std::sync::Arc;

    fn agent(s: &str) -> AgentId {
        AgentId(s.into())
    }

    fn req(path: &str, syms: &[&str], intent: ClaimIntent) -> ClaimRequest {
        ClaimRequest {
            path: path.into(),
            symbols: syms.iter().map(|s| s.to_string()).collect(),
            intent,
            ttl_seconds: None,
            plan_revision: None,
        }
    }

    #[test]
    fn any_symbol_intent_ignores_file_level_and_prefers_edit() {
        let a = agent("a");
        let mut f = FileOccupancy::default();
        assert_eq!(f.any_symbol_intent(&a), None, "no claims");
        // A file-level claim is not a symbol claim.
        f.intents
            .entry(FILE_LEVEL.into())
            .or_default()
            .insert(a.clone(), ClaimIntent::Edit);
        assert_eq!(f.any_symbol_intent(&a), None, "file-level only");
        f.intents
            .entry("s1".into())
            .or_default()
            .insert(a.clone(), ClaimIntent::Read);
        assert_eq!(
            f.any_symbol_intent(&a),
            Some(ClaimIntent::Read),
            "all symbol claims are reads"
        );
        f.intents
            .entry("s2".into())
            .or_default()
            .insert(a.clone(), ClaimIntent::Edit);
        assert_eq!(
            f.any_symbol_intent(&a),
            Some(ClaimIntent::Edit),
            "one edit wins"
        );
        // Another agent's claims are not this agent's.
        assert_eq!(f.any_symbol_intent(&agent("b")), None);
    }

    #[test]
    fn last_touched_is_per_scope_and_per_agent() {
        let (a, b) = (agent("a"), agent("b"));
        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1234);
        let mut f = FileOccupancy::default();
        assert_eq!(f.last_touched_for(&a, "s"), None);
        f.last_touched
            .entry("s".into())
            .or_default()
            .insert(a.clone(), t);
        assert_eq!(f.last_touched_for(&a, "s"), Some(t));
        assert_eq!(f.last_touched_for(&b, "s"), None);
        assert_eq!(f.last_touched_for(&a, "other"), None);
    }

    #[test]
    fn debug_reports_the_map_sizes() {
        let m = OccupancyMap::new();
        let _ = m.claim(
            &agent("a"),
            vec![
                req("zz_dbg1.rs", &[], ClaimIntent::Edit),
                req("zz_dbg2.rs", &[], ClaimIntent::Read),
            ],
        );
        let s = format!("{m:?}");
        assert!(s.contains("OccupancyMap"), "{s}");
        assert!(s.contains("files: 2") && s.contains("agents: 1"), "{s}");
    }

    #[test]
    fn persist_callback_fires_on_mutation_and_can_be_swapped_and_restored() {
        let m = Arc::new(OccupancyMap::new());
        let orig = Arc::new(AtomicUsize::new(0));
        let o2 = orig.clone();
        m.set_persist_callback(move || {
            o2.fetch_add(1, O::SeqCst);
        });
        let a = agent("a");
        let _ = m.claim(&a, vec![req("zz_pc1.rs", &[], ClaimIntent::Edit)]);
        let after_claim = orig.load(O::SeqCst);
        assert!(
            after_claim >= 1,
            "a mutation must fire the persist callback"
        );

        // Swap in a capturing callback: the original stops firing, the capture records.
        let dir = tempfile::tempdir().unwrap();
        let cell = Arc::new(parking_lot::Mutex::new(None));
        let prev = m
            .swap_persist_capture(
                cell.clone(),
                dir.path().join("presence.json"),
                Arc::new(PresenceRegistry::new()),
                m.clone(),
                Arc::new(crate::server::intent::IntentRegistry::new()),
                Arc::new(crate::server::activity::ActivityTracker::new()),
            )
            .expect("the previous callback is returned");
        let _ = m.claim(&a, vec![req("zz_pc2.rs", &[], ClaimIntent::Edit)]);
        assert_eq!(
            orig.load(O::SeqCst),
            after_claim,
            "the replaced callback must not fire"
        );
        assert!(
            matches!(&*cell.lock(), Some(Ok(()))),
            "the capture records the save result"
        );

        // Restore: the original fires again.
        m.restore_persist_callback(prev);
        let _ = m.claim(&a, vec![req("zz_pc3.rs", &[], ClaimIntent::Edit)]);
        assert!(
            orig.load(O::SeqCst) > after_claim,
            "the restored callback must fire"
        );
    }

    #[test]
    fn claim_roots_keep_the_workspace_first_and_deduplicate() {
        let ws = tempfile::tempdir().unwrap();
        let m = OccupancyMap::new();
        assert!(m.claim_roots_snapshot().is_empty());
        m.add_claim_roots(&[
            PathBuf::from("/repo/a"),
            PathBuf::from("/repo/b"),
            PathBuf::from("/repo/a"),
        ]);
        assert_eq!(
            m.claim_roots_snapshot(),
            vec![PathBuf::from("/repo/a"), PathBuf::from("/repo/b")]
        );
        m.add_claim_roots(&[]);
        assert_eq!(m.claim_roots_snapshot().len(), 2);
        m.set_workspace_root(ws.path());
        let roots = m.claim_roots_snapshot();
        let canon = lexical_normalize(&std::fs::canonicalize(ws.path()).unwrap());
        assert_eq!(roots[0], canon, "the workspace anchors first");
        assert_eq!(roots.len(), 3);
        // Setting it again moves it to the front without duplicating it.
        m.add_claim_roots(&[canon.clone()]);
        m.set_workspace_root(ws.path());
        let roots = m.claim_roots_snapshot();
        assert_eq!(roots.iter().filter(|r| **r == canon).count(), 1);
        assert_eq!(roots[0], canon);
    }

    #[test]
    fn a_reader_is_advised_of_each_editor_and_the_symbols_they_hold() {
        let m = OccupancyMap::new();
        let (a, c, b) = (agent("a"), agent("c"), agent("b"));
        let p = "zz_adv.rs";
        let _ = m.claim(&a, vec![req(p, &["s1"], ClaimIntent::Edit)]);
        let _ = m.claim(&c, vec![req(p, &["s2"], ClaimIntent::Edit)]);
        let _ = m.claim(&agent("d"), vec![req(p, &["s3"], ClaimIntent::Read)]); // a reader: no advisory
        let r = m.claim(&b, vec![req(p, &[], ClaimIntent::Read)]);
        assert!(r.conflicts.is_empty(), "a read never conflicts");
        assert_eq!(r.granted.len(), 1);
        let mut adv = r.advisories.clone();
        adv.sort_by(|x, y| x.agent_id.0.cmp(&y.agent_id.0));
        assert_eq!(adv.len(), 2, "one advisory per editing agent: {adv:?}");
        assert_eq!(adv[0].agent_id, a);
        assert_eq!(
            adv[0].symbols,
            vec!["s1".to_string()],
            "c's s2 is not a's symbol"
        );
        assert_eq!(adv[0].intent, ClaimIntent::Edit);
        assert_eq!(adv[1].agent_id, c);
        assert_eq!(adv[1].symbols, vec!["s2".to_string()]);
        // The reader's own claims never advise itself.
        let again = m.claim(&a, vec![req(p, &[], ClaimIntent::Read)]);
        assert!(again.advisories.iter().all(|e| e.agent_id != a));
    }

    /// A declaration always wins over a guess: an inferred claim is marked as such,
    /// an explicit one clears the mark, and a later guess never downgrades it.
    #[test]
    fn inferred_marker_is_set_by_guesses_cleared_by_declarations_and_never_re_set() {
        let m = OccupancyMap::new();
        let (x, y) = (agent("x"), agent("y"));
        let probe = |path: &str| -> bool {
            let r = m.claim(&y, vec![req(path, &[], ClaimIntent::Edit)]);
            assert_eq!(r.conflicts.len(), 1, "y must collide with x on {path}");
            r.conflicts[0].inferred
        };
        // A guess is marked.
        let _ = m.claim_inferred(&x, vec![req("zz_inf1.rs", &[], ClaimIntent::Edit)]);
        assert!(
            probe("zz_inf1.rs"),
            "a guessed claim is reported as inferred"
        );
        // Declaring it clears the mark.
        let _ = m.claim(&x, vec![req("zz_inf1.rs", &[], ClaimIntent::Edit)]);
        assert!(
            !probe("zz_inf1.rs"),
            "an explicit claim clears the inferred mark"
        );
        // A guess on top of a declaration does not downgrade it.
        let _ = m.claim_inferred(&x, vec![req("zz_inf1.rs", &[], ClaimIntent::Edit)]);
        assert!(
            !probe("zz_inf1.rs"),
            "a guess must not downgrade a declared claim"
        );
        // A declaration first, never guessed: not inferred either.
        let _ = m.claim(&x, vec![req("zz_inf2.rs", &[], ClaimIntent::Edit)]);
        assert!(!probe("zz_inf2.rs"));
    }

    #[test]
    fn ttl_becomes_an_absolute_expiry_and_a_reclaim_replaces_instead_of_duplicating() {
        let m = OccupancyMap::new();
        let a = agent("a");
        let mut r = req("zz_ttl.rs", &[], ClaimIntent::Edit);
        r.ttl_seconds = Some(3600);
        let _ = m.claim(
            &a,
            vec![r.clone(), req("zz_ttl_none.rs", &[], ClaimIntent::Edit)],
        );
        let claims = m.list_for_agent(&a);
        let with_ttl = claims
            .iter()
            .find(|c| c.path == PathBuf::from("zz_ttl.rs"))
            .unwrap();
        let none = claims
            .iter()
            .find(|c| c.path == PathBuf::from("zz_ttl_none.rs"))
            .unwrap();
        assert_eq!(none.expires_at, None);
        let exp = with_ttl.expires_at.expect("a TTL claim carries an expiry");
        assert_eq!(
            exp.duration_since(with_ttl.claimed_at).unwrap(),
            std::time::Duration::from_secs(3600)
        );
        // Not yet due.
        assert!(m
            .expire_by_ttl()
            .iter()
            .all(|(who, p)| !(who == &a && p == &PathBuf::from("zz_ttl.rs"))));
        // Re-claiming the same scope replaces the row (no duplicates inflating the count).
        let _ = m.claim(&a, vec![r]);
        let n = m
            .list_for_agent(&a)
            .iter()
            .filter(|c| c.path == PathBuf::from("zz_ttl.rs"))
            .count();
        assert_eq!(n, 1, "a re-claim must replace, not append");
    }

    #[test]
    fn persist_fires_only_when_something_was_granted() {
        let m = OccupancyMap::new();
        let fired = Arc::new(AtomicUsize::new(0));
        let f2 = fired.clone();
        m.set_persist_callback(move || {
            f2.fetch_add(1, O::SeqCst);
        });
        let (a, b) = (agent("a"), agent("b"));
        let _ = m.claim(&a, vec![req("zz_pf.rs", &[], ClaimIntent::Edit)]);
        let after_grant = fired.load(O::SeqCst);
        assert!(after_grant >= 1);
        // b's edit is refused: nothing changed, so nothing to persist.
        let r = m.claim(&b, vec![req("zz_pf.rs", &[], ClaimIntent::Edit)]);
        assert!(r.granted.is_empty() && !r.conflicts.is_empty());
        assert_eq!(
            fired.load(O::SeqCst),
            after_grant,
            "a refused claim must not trigger a save"
        );
    }

    #[test]
    fn release_persists_only_when_something_was_released() {
        let m = OccupancyMap::new();
        let fired = Arc::new(AtomicUsize::new(0));
        let f2 = fired.clone();
        m.set_persist_callback(move || {
            f2.fetch_add(1, O::SeqCst);
        });
        let a = agent("a");
        let _ = m.claim(&a, vec![req("zz_rel.rs", &[], ClaimIntent::Edit)]);
        // Nothing held at this path: no change, no save.
        assert!(m.release(&a, &[PathBuf::from("zz_never.rs")]).is_empty());
        assert!(
            m.release(&agent("stranger"), &[PathBuf::from("zz_rel.rs")])
                .len()
                <= 1
        );
        let after_noop = fired.load(O::SeqCst);
        // Releasing a real claim saves exactly once more.
        assert_eq!(
            m.release(&a, &[PathBuf::from("zz_rel.rs")]),
            vec![PathBuf::from("zz_rel.rs")]
        );
        assert!(
            fired.load(O::SeqCst) > after_noop,
            "a real release must be persisted"
        );
        // The no-op release of a path nobody holds did not save.
        let before = fired.load(O::SeqCst);
        assert!(m.release(&a, &[PathBuf::from("zz_gone.rs")]).is_empty());
        assert_eq!(
            fired.load(O::SeqCst),
            before,
            "an empty release must not trigger a save"
        );
    }
}
