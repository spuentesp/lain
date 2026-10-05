//! Model-based check of `AnnotationStore::list_with_staleness`.
//!
//! Specification: the result is the *reclassified* list (rows whose target
//! no longer exists read as `stale`), in `created_at DESC, id ASC` order,
//! restricted to the requested status, truncated to `limit`. Filtering and
//! truncation therefore commute with reclassification — a caller asking for
//! `open` rows must get the first `limit` open rows, not "the first `limit`
//! rows of any status, minus whatever isn't open".
use super::*;
use proptest::prelude::*;
use std::collections::HashSet;

fn store() -> (tempfile::TempDir, AnnotationStore) {
    let tmp = tempfile::tempdir().unwrap();
    let s = AnnotationStore::open(&tmp.path().join("ann.sqlite")).unwrap();
    (tmp, s)
}

fn add(store: &AnnotationStore, symbol: &str) -> String {
    let a = AddAnnotationInputs {
        target: AnnotationTarget::Symbol {
            symbol: symbol.into(),
        },
        kind: AnnotationKind::Note,
        body: "b".into(),
        author: AgentId("alice".into()),
        refs: vec![],
    }
    .into_annotation();
    store.add(&a).unwrap();
    a.id
}

fn status_of(s: Option<AnnotationStatus>) -> Option<&'static str> {
    s.map(|s| s.as_str())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn status_filter_commutes_with_limit(
        rows in prop::collection::vec((any::<bool>(), any::<bool>()), 0..25), // (exists, resolved)
        want in prop::option::of(prop_oneof![
            Just(AnnotationStatus::Open), Just(AnnotationStatus::Stale), Just(AnnotationStatus::Resolved)
        ]),
        limit in 1u32..8,
    ) {
        let (_tmp, store) = store();
        let me = AgentId("alice".into());
        let mut gone: HashSet<String> = HashSet::new();
        for (i, (exists, resolved)) in rows.iter().enumerate() {
            let sym = format!("sym{i}");
            let id = add(&store, &sym);
            if !exists { gone.insert(sym); }
            if *resolved { store.resolve(&id, &me).unwrap(); }
        }
        let exists = |t: &AnnotationTarget| match t {
            AnnotationTarget::Symbol { symbol } => !gone.contains(symbol),
            _ => true,
        };

        // Reference: the full reclassified list, then filter, then truncate.
        let all = store
            .list_with_staleness(&ListQuery {
                filter: &ListFilter { limit: Some(1000), ..Default::default() },
                exists: &exists,
            })
            .unwrap();
        let expected: Vec<String> = all
            .iter()
            .filter(|a| status_of(want).map_or(true, |w| a.status == w))
            .take(limit as usize)
            .map(|a| a.id.clone())
            .collect();

        let got: Vec<String> = store
            .list_with_staleness(&ListQuery {
                filter: &ListFilter { status: want, limit: Some(limit), ..Default::default() },
                exists: &exists,
            })
            .unwrap()
            .into_iter()
            .map(|a| a.id)
            .collect();

        prop_assert_eq!(got, expected, "status filter {:?} limit {}", want, limit);
    }
}

/// The shrunk shape: newer resolved rows must not crowd older open rows out
/// of an `open` query.
#[test]
fn open_rows_are_not_hidden_behind_newer_resolved_rows() {
    let (_tmp, store) = store();
    let me = AgentId("alice".into());
    let old_open = add(&store, "old_open");
    std::thread::sleep(std::time::Duration::from_millis(3));
    for i in 0..3 {
        let id = add(&store, &format!("resolved{i}"));
        store.resolve(&id, &me).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
    let got = store
        .list_with_staleness(&ListQuery {
            filter: &ListFilter {
                status: Some(AnnotationStatus::Open),
                limit: Some(2),
                ..Default::default()
            },
            exists: &|_| true,
        })
        .unwrap();
    assert_eq!(
        got.iter().map(|a| a.id.clone()).collect::<Vec<_>>(),
        vec![old_open]
    );
}

// ---- persistence: what is written is what is read back, across a reopen ------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Any valid annotation survives write -> close -> reopen -> read unchanged.
    #[test]
    fn annotations_round_trip_through_the_sqlite_file(
        body in "\\PC{1,300}",
        author in "[a-z]{1,8}",
        symbol in "[A-Za-z_:]{1,20}",
        nrefs in 0usize..4,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ann.sqlite");
        let refs: Vec<AnnotationTarget> = (0..nrefs)
            .map(|i| AnnotationTarget::Symbol { symbol: format!("r{i}") })
            .collect();
        let a = AddAnnotationInputs {
            target: AnnotationTarget::Symbol { symbol },
            kind: AnnotationKind::Note,
            body,
            author: AgentId(author),
            refs,
        }
        .into_annotation();
        {
            let s = AnnotationStore::open(&path).unwrap();
            s.add(&a).unwrap();
        }
        let s = AnnotationStore::open(&path).unwrap();
        prop_assert_eq!(s.get(&a.id).unwrap(), Some(a));
    }
}

/// A file that is not a database (truncated, overwritten) is an error the
/// caller can handle, never a panic, and never silently treated as empty data
/// that a later write would paper over.
#[test]
fn a_corrupt_annotation_file_is_an_error_not_a_panic() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("ann.sqlite");
    std::fs::write(
        &path,
        b"this is not a sqlite database, just text padding ".repeat(50),
    )
    .unwrap();
    let r = std::panic::catch_unwind(|| AnnotationStore::open(&path).and_then(|s| s.get("x")));
    let r = r.expect("opening a corrupt file panicked");
    assert!(r.is_err(), "a corrupt file must be reported: {r:?}");
}

/// Truncating a real database mid-file must not panic on open or read.
#[test]
fn a_truncated_annotation_database_never_panics() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("ann.sqlite");
    let id = {
        let s = AnnotationStore::open(&path).unwrap();
        let a = AddAnnotationInputs {
            target: AnnotationTarget::Symbol { symbol: "f".into() },
            kind: AnnotationKind::Note,
            body: "hello".into(),
            author: AgentId("a".into()),
            refs: vec![],
        }
        .into_annotation();
        s.add(&a).unwrap();
        a.id
    };
    let full = std::fs::read(&path).unwrap();
    for cut in [0, 1, 100, full.len() / 2, full.len().saturating_sub(1)] {
        std::fs::write(&path, &full[..cut]).unwrap();
        let id = id.clone();
        let p = path.clone();
        let r = std::panic::catch_unwind(move || {
            if let Ok(s) = AnnotationStore::open(&p) {
                let _ = s.get(&id);
            }
        });
        assert!(r.is_ok(), "panic with the file cut to {cut} bytes");
    }
}

// ---- state machine: the store refines a status model, across reopen ---------------------

mod lifecycle {
    use super::*;
    use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};

    /// Model: ids in creation order with their status and resolver.
    #[derive(Clone, Debug, Default)]
    pub struct Model {
        rows: Vec<(usize, Option<String>)>, // (seq, resolved_by)
    }

    #[derive(Clone, Debug)]
    pub enum Op {
        Add,
        Resolve(usize, bool), // row index (mod len), which agent
        Reopen,               // close and reopen the sqlite file
        Check,
    }

    pub struct Ref;
    impl ReferenceStateMachine for Ref {
        type State = Model;
        type Transition = Op;
        fn init_state() -> BoxedStrategy<Model> {
            Just(Model::default()).boxed()
        }
        fn transitions(m: &Model) -> BoxedStrategy<Op> {
            let mut ops = vec![Just(Op::Add).boxed(), Just(Op::Reopen).boxed()];
            if !m.rows.is_empty() {
                ops.push(
                    (0usize..64, any::<bool>())
                        .prop_map(|(i, b)| Op::Resolve(i, b))
                        .boxed(),
                );
                ops.push(Just(Op::Check).boxed());
            }
            proptest::strategy::Union::new(ops).boxed()
        }
        fn apply(mut m: Model, op: &Op) -> Model {
            match op {
                Op::Add => {
                    let n = m.rows.len();
                    m.rows.push((n, None));
                }
                Op::Resolve(i, b) => {
                    let n = m.rows.len();
                    let row = &mut m.rows[i % n];
                    if row.1.is_none() {
                        row.1 = Some(if *b { "alice" } else { "bob" }.to_string());
                    }
                }
                Op::Reopen | Op::Check => {}
            }
            m
        }
    }

    pub struct Sut {
        _tmp: tempfile::TempDir,
        path: std::path::PathBuf,
        store: AnnotationStore,
        ids: Vec<String>,
        resolved: std::collections::HashSet<usize>,
    }

    fn assert_matches(sut: &Sut, m: &Model) {
        for (i, (_, resolved_by)) in m.rows.iter().enumerate() {
            let a = sut.store.get(&sut.ids[i]).unwrap().expect("row exists");
            match resolved_by {
                None => {
                    assert_eq!(a.status, "open");
                    assert!(a.resolved_by.is_none() && a.resolved_at_unix_ms.is_none());
                }
                Some(by) => {
                    assert_eq!(a.status, "resolved");
                    assert_eq!(
                        a.resolved_by.as_ref().map(|x| x.0.as_str()),
                        Some(by.as_str())
                    );
                    assert!(a.resolved_at_unix_ms.is_some());
                }
            }
        }
    }

    impl StateMachineTest for Sut {
        type SystemUnderTest = Sut;
        type Reference = Ref;
        fn init_test(_: &Model) -> Sut {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("a.sqlite");
            let store = AnnotationStore::open(&path).unwrap();
            Sut {
                _tmp: tmp,
                path,
                store,
                ids: vec![],
                resolved: Default::default(),
            }
        }
        fn apply(mut sut: Sut, _m: &Model, op: Op) -> Sut {
            match op {
                Op::Add => {
                    let id = add(&sut.store, &format!("s{}", sut.ids.len()));
                    sut.ids.push(id);
                }
                Op::Resolve(i, b) => {
                    if sut.ids.is_empty() {
                        return sut; // shrinking can drop the Add this depended on
                    }
                    let idx = i % sut.ids.len();
                    let by = AgentId(if b { "alice" } else { "bob" }.into());
                    let already = !sut.resolved.insert(idx);
                    let r = sut.store.resolve(&sut.ids[idx], &by);
                    // Resolving twice is refused, never silently re-attributed.
                    assert_eq!(r.is_err(), already, "resolve outcome vs model: {r:?}");
                }
                Op::Reopen => {
                    sut.store = AnnotationStore::open(&sut.path).unwrap();
                }
                Op::Check => {}
            }
            sut
        }
        fn check_invariants(sut: &Sut, m: &Model) {
            assert_matches(sut, m);
            let all = sut
                .store
                .list_with_staleness(&ListQuery {
                    filter: &ListFilter {
                        limit: Some(1000),
                        ..Default::default()
                    },
                    exists: &|_| true,
                })
                .unwrap();
            assert_eq!(
                all.len(),
                m.rows.len(),
                "list must contain every row exactly once"
            );
        }
    }

    prop_state_machine! {
        #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]
        #[test]
        fn annotation_store_refines_the_status_model(sequential 1..30 => Sut);
    }
}
