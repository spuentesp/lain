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
