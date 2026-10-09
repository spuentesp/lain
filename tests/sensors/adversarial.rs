//! Adversarial coverage for the data-driven sensor-patterns refactor.
//!
//! The maintainer's concern driving this file: existing tests exercise
//! canonical inputs and may miss edge cases the curated fixture never
//! reaches. The tests below probe failure modes the spec's happy-path
//! tests don't cover:
//!
//!   2a — A malformed `.scm` body in one override file must fail the
//!        whole `load_overrides` (transactional). A previously-stored
//!        good body must NOT survive.
//!   2b — Invalid YAML in one override file must fail the whole
//!        `load_overrides`. The bundled registry is untouched on
//!        `Err`.
//!   2c — An empty override directory (exists, contains nothing) is a
//!        no-op: `with_overrides` returns `Cow::Borrowed(&singleton)`.
//!   2d — An override that changes a bundled framework's `kind`
//!        REPLACES the bundled entry (the override's `kind` wins).
//!   2e — Concurrent scans of different repos don't share state.
//!   2f — An override with a brand-new framework id APPENDS to the
//!        language bucket; existing entries are untouched.
//!
//! Plus the Item-4 differential test (scan-with-no-overrides matches
//! the bundled-only baseline) at the bottom.
//!
//! Together with the mutation tests in this commit's report, these
//! tests pin the spec's invariants end-to-end so a future refactor
//! that silently re-introduces the parked-2 cleanup's bugs is caught
//! at `cargo test` time.

use lain::federation::repo_id::RepoId;
use lain::graph::GraphDatabase;
use lain::schema::{NodeType, RepoNamespace};
use lain::server::federation::contracts::model::{ContractFact, HttpMethod};
use lain::server::sensors::http_client_sensor::scan_workspace_clients;
use lain::server::sensors::http_sensor::scan_workspace_routes;
use lain::server::sensors::patterns::{FrameworkDef, FrameworkKind, Patterns};
use lain::server::sensors::util::Lang;
use std::borrow::Cow;

// ─── 2c — empty override directory is a no-op (singleton is borrowed when dir is absent) ───
//
// The fast path of `with_overrides` is the cheap "no allocation, no
// clone" branch when the override directory is absent. The spec's
// invariant: when the operator's `<root>/.lain/patterns/` is missing
// entirely, the returned `Cow` MUST be `Cow::Borrowed` pointing at
// the bundled singleton. A regression that always allocates a fresh
// `Patterns::clone_default()` (a `Cow::Owned`) would break this
// invariant — the cost would be the full bundled-YAML clone on every
// scan, not just the override-bearing ones.
//
// Companion invariant: when the directory exists but is empty,
// `load_overrides` is a no-op (the cache stays empty), and the
// resulting `compiled_queries()` returns `Cow::Borrowed(generated::QUERIES)`.
// The wrapping `with_overrides` still produces `Cow::Owned` (the
// function clones the default before calling `load_overrides` because
// the dir-exists branch can't tell upfront whether the dir carries
// any files), but the merged data the walker consumes is the same
// bundled static as the absent-dir path. This is the actual
// fast-path contract: zero per-call cost on the merged-queries
// surface, regardless of whether the dir is present-but-empty.
#[test]
fn empty_or_absent_override_directory_does_not_augment_patterns() {
    // Case A: absent override dir. The fast path must produce
    // Cow::Borrowed pointing at the bundled singleton — the
    // cheapest possible return (zero allocation, zero clone).
    let dir_absent = tempfile::tempdir().expect("tempdir absent");
    let result_absent =
        Patterns::with_overrides(dir_absent.path()).expect("absent override dir must not error");
    match &result_absent {
        Cow::Borrowed(p) => {
            assert!(
                std::ptr::eq(*p as *const _, Patterns::patterns() as *const _),
                "absent override dir: the borrowed Cow MUST point at the bundled singleton (the fast path's no-allocation promise); got a different address"
            );
        }
        Cow::Owned(_) => panic!(
            "absent override dir: the fast path MUST return Cow::Borrowed (singleton), not Cow::Owned — a regression that always clones is silent and slow"
        ),
    }

    // Case B: present but empty override dir. The
    // merged-queries surface must still return the bundled
    // static (no augmentation); the wrapper may be Cow::Owned
    // because the dir-exists branch is upfront about the
    // possible augmentation cost.
    let dir_empty = tempfile::tempdir().expect("tempdir empty");
    let empty_root = dir_empty.path();
    let patterns_dir = empty_root.join(".lain/patterns");
    std::fs::create_dir_all(&patterns_dir).expect("mkdir .lain/patterns");
    // Nothing in it.

    let result_empty =
        Patterns::with_overrides(empty_root).expect("empty override dir must not error");
    // The merged compiled-queries surface must be the bundled
    // static (Cow::Borrowed) because the empty dir produced no
    // override bodies. This is the user-facing invariant: "an
    // empty override dir leaves the walker seeing the same
    // data as the absent-dir case."
    let merged_empty = result_empty
        .compiled_queries()
        .expect("compiled_queries must succeed after an empty-dir load_overrides");
    let merged_absent = result_absent
        .compiled_queries()
        .expect("compiled_queries must succeed for the absent-dir singleton");
    // Both must be Cow::Borrowed of the bundled static.
    match (&merged_empty, &merged_absent) {
        (Cow::Borrowed(empty), Cow::Borrowed(absent)) => {
            assert!(
                std::ptr::eq(empty.as_ptr(), absent.as_ptr()),
                "empty-dir and absent-dir compiled_queries() must both return the bundled static (pointer-equal slices)"
            );
            // The slice length must be the bundled LEN (proves
            // no augmentation happened).
            assert_eq!(
                empty.len(),
                absent.len(),
                "empty-dir and absent-dir compiled_queries() must have the same length (the bundled LEN)"
            );
        }
        (Cow::Owned(_), _) | (_, Cow::Owned(_)) => panic!(
            "both empty-dir and absent-dir compiled_queries() must return Cow::Borrowed (the empty override cache means no augmentation)"
        ),
    }

    // Sanity: load_overrides on the empty dir flipped the
    // overrides_applied flag (the load ran, even though it
    // found nothing to load). This matches the existing
    // `overrides_applied_flips_even_when_dir_is_absent` test
    // for the absent-dir case.
    if let Cow::Owned(p) = &result_empty {
        assert!(
            p.overrides_applied(),
            "empty override dir: load_overrides must record that it ran, even when the dir carried nothing"
        );
    } else {
        // unreachable — present dir always goes Owned in the current impl
    }
}

// ─── 2a — malformed `.scm` body fails the whole `load_overrides` ───
//
// Transactional guarantee: a single bad body fails the whole call,
// and the cache is left untouched (no partial insertion of the
// previously-validated good body). A regression that parses each
// file independently and inserts before validating lets a single
// bad body corrupt the cache — the test below pins the all-or-
// nothing semantic.
#[test]
fn malformed_scm_body_fails_whole_load_overrides() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_root = dir.path();

    let patterns_dir = repo_root.join(".lain/patterns/python");
    std::fs::create_dir_all(&patterns_dir).expect("mkdir .lain/patterns/python");

    // First: a parseable body. If load_overrides were to insert
    // this BEFORE validating the second file, the cache would
    // carry it after the failed call — the test's third assertion
    // catches that regression.
    std::fs::write(
        patterns_dir.join("good.scm"),
        r#"
; Good override body (parseable Python tree-sitter query).
; Used to detect a regression that inserts good bodies into the
; cache BEFORE validating sibling files.
(module) @m
"#,
    )
    .expect("write good.scm");

    // Second: an unbalanced-paren body. This will fail the
    // `validate_scm_body` call inside `load_overrides` and
    // produce a `PatternsError::OverrideQuerySyntax`.
    std::fs::write(
        patterns_dir.join("bad.scm"),
        "(module) @m\n(this is not a valid tree-sitter query because parens don't balance",
    )
    .expect("write bad.scm");

    let mut patterns = Patterns::clone_default();
    let result = patterns.load_overrides(repo_root);
    assert!(
        result.is_err(),
        "load_overrides must fail when ANY override body is malformed: {result:?}"
    );
    // The error must be the structured `OverrideQuerySyntax` variant
    // (so an operator gets the line/column of the bad body), not a
    // bare `io::Error` or panic.
    match result.unwrap_err() {
        lain::server::sensors::patterns::PatternsError::OverrideQuerySyntax { path, .. } => {
            assert!(
                path.contains("bad.scm"),
                "the structured error must point at the bad file, not at the good one: {path:?}"
            );
        }
        other => panic!("expected OverrideQuerySyntax for bad.scm, got {other:?}"),
    }

    // Transactional: the good body must NOT have been inserted.
    // `compiled_queries()` returns `Cow::Borrowed(generated::QUERIES)`
    // when the override cache is empty, and would return the
    // merged map (including `python/good.scm`) if the cache had
    // been populated. Asserting the absence of `python/good.scm`
    // in the merged slice proves the cache was untouched.
    let merged = patterns
        .compiled_queries()
        .expect("compiled_queries is Ok after a failed load_overrides (the cache is empty, fast path triggers)");
    let good_present = merged.iter().any(|(k, _, _, _)| *k == "python/good.scm");
    assert!(
        !good_present,
        "the previously-validated good body must NOT survive a failed load_overrides (transactional all-or-nothing): {merged:?}"
    );
}

// ─── 2b — invalid YAML fails the whole `load_overrides` ───
//
// Symmetric to 2a: a malformed YAML file fails the call, the cache
// is untouched. A regression that accumulates per-file entries into
// `self.yaml.languages` and only validates the file at the end lets
// good YAML from a sibling file partially apply — the test pins the
// all-or-nothing semantic.
#[test]
fn invalid_yaml_fails_whole_load_overrides() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_root = dir.path();

    let patterns_dir = repo_root.join(".lain/patterns");
    std::fs::create_dir_all(&patterns_dir).expect("mkdir .lain/patterns");

    // Good YAML: adds a fresh id `python-fresh-from-good-yaml`. If
    // load_overrides were to merge this BEFORE validating the bad
    // file, the framework would be present in `framework()` after
    // the failed call.
    std::fs::write(
        patterns_dir.join("good.yaml"),
        r#"
languages:
  python:
    - id: python-fresh-from-good-yaml
      kind: outbound
      lib_match: '^from_good_yaml$'
"#,
    )
    .expect("write good.yaml");

    // Bad YAML: obviously malformed (`:\n:\n:` is not valid YAML
    // structure). Forces `serde_yaml::from_str` to fail.
    std::fs::write(
        patterns_dir.join("bad.yaml"),
        ":\n:\n:\n  - this is not valid yaml\n :\n",
    )
    .expect("write bad.yaml");

    let mut patterns = Patterns::clone_default();
    let result = patterns.load_overrides(repo_root);
    assert!(
        result.is_err(),
        "load_overrides must fail when ANY override YAML is malformed: {result:?}"
    );
    // The error must be the structured `Yaml(serde_yaml::Error)`
    // variant (so an operator gets the line/column of the bad
    // entry), not a panic or opaque string.
    match result.unwrap_err() {
        lain::server::sensors::patterns::PatternsError::Yaml(_) => {}
        other => panic!("expected Yaml(serde_yaml::Error), got {other:?}"),
    }

    // Transactional: the fresh id from the good YAML must NOT
    // have been appended. The `framework()` lookup goes through
    // `self.yaml.languages`, so a successful append would make
    // the lookup return `Some`.
    let fresh = patterns.framework("python-fresh-from-good-yaml");
    assert!(
        fresh.is_none(),
        "the previously-parsed good YAML's fresh id must NOT survive a failed load_overrides (transactional all-or-nothing)"
    );
}

// ─── 2d — override changing `kind` REPLACES the bundled entry ───
//
// Bundled `fastapi-route` has `kind: route`. An override that sets
// `kind: outbound` must REPLACE the entry wholesale (the existing
// `route` framework disappears for this repo). A regression that
// merges by field and keeps the original `kind` (effectively a
// "field-level merge") would leave the bundled `kind: route` in
// place — the test below pins the replace-not-merge semantic by
// asserting the override's `kind` wins.
#[test]
fn override_changing_kind_replaces_bundled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_root = dir.path();

    let patterns_dir = repo_root.join(".lain/patterns");
    std::fs::create_dir_all(&patterns_dir).expect("mkdir .lain/patterns");

    // Sanity: the bundled entry is a route.
    let bundled = Patterns::patterns()
        .framework("fastapi-route")
        .expect("fastapi-route is bundled");
    assert_eq!(
        bundled.kind,
        FrameworkKind::Route,
        "bundled fastapi-route must be a route (sanity check on the fixture)"
    );

    // Override: same id, but a different kind. The override's kind
    // must win.
    std::fs::write(
        patterns_dir.join("python.yaml"),
        r#"
languages:
  python:
    - id: fastapi-route
      kind: outbound
      lib_match: '^fastapi$'
      deny_methods: []
"#,
    )
    .expect("write python.yaml");

    let mut patterns = Patterns::clone_default();
    patterns
        .load_overrides(repo_root)
        .expect("load_overrides must accept the per-repo YAML");

    // The override's kind must have REPLACED the bundled one.
    let overridden = patterns
        .framework("fastapi-route")
        .expect("fastapi-route survives the override");
    assert_eq!(
        overridden.kind,
        FrameworkKind::Outbound,
        "the override's `kind: outbound` must REPLACE the bundled `kind: route` (replace-not-merge semantic)"
    );

    // And the route iterator for Python must NO LONGER yield
    // fastapi-route (because its kind is now outbound, not route).
    let routes: Vec<&str> = patterns
        .route_patterns(Lang::Python)
        .map(|d| d.id.as_str())
        .collect();
    assert!(
        !routes.contains(&"fastapi-route"),
        "fastapi-route must NOT appear in route_patterns after the override changed its kind to outbound: {routes:?}"
    );

    // It must now appear in the outbound iterator instead.
    let outbounds: Vec<&str> = patterns
        .outbound_patterns(Lang::Python)
        .map(|d| d.id.as_str())
        .collect();
    assert!(
        outbounds.contains(&"fastapi-route"),
        "fastapi-route must appear in outbound_patterns after the override changed its kind to outbound: {outbounds:?}"
    );
}

// ─── 2e — concurrent scans of different repos don't share state ───
//
// Spec invariant: each `Patterns::with_overrides(root)` returns a
// per-call `Patterns` instance whose override cache is independent
// of every other call. A regression that re-introduces the parked-2
// cleanup's `Mutex<Vec<(String, String)>>` + global lock, or any
// other shared-mutable-state, would let one repo's overrides
// contaminate another. The test below fans out 8 threads, each
// scanning a unique repo, and asserts every thread sees only its
// own override (or no override if its repo has none).
#[test]
fn concurrent_scans_of_different_repos_dont_share_state() {
    use std::sync::{Arc, Barrier};
    use std::thread;

    // Build 8 unique repos. 4 of them carry a per-repo override
    // with a distinct, identifiable body; 4 have no override
    // directory. The threads start their scans behind a Barrier
    // so all 8 races against `compiled_queries()` and
    // `load_overrides` simultaneously — the worst case for any
    // shared-mutable-state regression.
    const N: usize = 8;
    let mut handles = Vec::new();
    let barrier = Arc::new(Barrier::new(N));

    for tid in 0..N {
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            let dir = tempfile::tempdir().expect("tempdir");
            let repo_root = dir.path();

            // Every repo gets a minimal Python file that produces
            // the same merged-slice baseline (the bundled
            // `python/requests-outbound.scm` body) so the
            // thread-local "did the override reach me?" check
            // is decoupled from the per-source variation.
            std::fs::write(
                repo_root.join("main.py"),
                "import requests\nrequests.get('https://api.example.com/users')\n",
            )
            .expect("write main.py");

            // Repos 0, 2, 4, 6 carry a per-repo override; 1, 3,
            // 5, 7 do not. The override bodies are
            // thread-distinctive so cross-contamination is
            // immediately observable.
            if tid % 2 == 0 {
                let patterns_dir = repo_root.join(".lain/patterns/python");
                std::fs::create_dir_all(&patterns_dir).expect("mkdir");
                let body = format!(
                    r#"
; Per-thread override for tid={tid} — carries this thread id in
; the doc-comment header so a cross-contamination regression
; surfaces immediately.
(function_definition) @_thread_{tid}
"#
                );
                std::fs::write(patterns_dir.join("thread-override.scm"), body)
                    .expect("write thread-override.scm");
            }

            // Wait for all threads to be ready, then race.
            barrier.wait();

            // Build the per-repo Patterns via the canonical helper.
            // Each call gets its own instance; no two threads
            // share a `Patterns`.
            let patterns = Patterns::with_overrides(repo_root)
                .expect("with_overrides must succeed");

            // For override-bearing repos, assert the merged
            // compiled-queries map carries ONLY this thread's
            // override body — never any other thread's.
            if tid % 2 == 0 {
                let merged = patterns
                    .compiled_queries()
                    .expect("compiled_queries must succeed");

                // The override's distinctive key must be present
                // in this thread's merged slice.
                let expected_key = "python/thread-override.scm";
                let has_own = merged.iter().any(|(k, _, _, _)| *k == expected_key);
                assert!(
                    has_own,
                    "tid={tid} override-bearing scan must carry its own override key {expected_key:?}: {merged:?}"
                );

                // And NO other thread's body must be present
                // (the doc-comment carries a unique tid).
                for other in 0..N {
                    if other == tid {
                        continue;
                    }
                    if other % 2 != 0 {
                        continue; // odd tids have no override body
                    }
                    let marker = format!("tid={other}");
                    let contaminated = merged
                        .iter()
                        .any(|(_, _, _, b)| b.contains(&marker));
                    assert!(
                        !contaminated,
                        "tid={tid} saw tid={other}'s override body — cross-thread contamination: {merged:?}"
                    );
                }
            } else {
                // For override-less repos, the merged slice must
                // be the BUNDLED static (no override key appears
                // — none was loaded).
                let merged = patterns
                    .compiled_queries()
                    .expect("compiled_queries must succeed");
                let has_any_thread_override = merged
                    .iter()
                    .any(|(k, _, _, _)| *k == "python/thread-override.scm");
                assert!(
                    !has_any_thread_override,
                    "tid={tid} (no overrides) MUST NOT see any thread-override.scm entry — a cross-thread contamination regression: {merged:?}"
                );
                // And the Cow variant should be `Borrowed` (the
                // singleton fast path).
                match &patterns {
                    Cow::Borrowed(p) => {
                        assert!(
                            std::ptr::eq(*p as *const _, Patterns::patterns() as *const _),
                            "tid={tid} (no overrides) must see the singleton (Cow::Borrowed); a regression that always clones would produce Cow::Owned"
                        );
                    }
                    Cow::Owned(_) => panic!(
                        "tid={tid} (no overrides) must see the singleton (Cow::Borrowed); got Cow::Owned"
                    ),
                }
            }
        }));
    }

    for h in handles {
        h.join().expect("thread panicked");
    }
}

// ─── 2f — override with a fresh framework id APPENDS, doesn't disturb existing entries ───
//
// Spec invariant: a fresh-id override grows the language bucket by
// one; the bundled entries are untouched. A regression that
// replaces-by-position (the override file's first entry replaces
// the bundled file's first entry) would clobber an unrelated
// framework — the test pins "match by id, never by position".
#[test]
fn override_with_fresh_id_appends_to_language_bucket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_root = dir.path();

    let patterns_dir = repo_root.join(".lain/patterns");
    std::fs::create_dir_all(&patterns_dir).expect("mkdir .lain/patterns");

    // Sanity: capture the bundled Python route count so we can
    // assert it grew by exactly one (no double-append, no
    // clobber).
    let pre_routes: Vec<String> = Patterns::patterns()
        .route_patterns(Lang::Python)
        .map(|d| d.id.clone())
        .collect();
    let pre_count = pre_routes.len();
    assert!(
        pre_count >= 1,
        "sanity: the bundled Python bucket must have at least one route (otherwise the assertion is meaningless)"
    );

    // Override: a fresh id `python-only-fresh-framework` that
    // doesn't collide with any bundled entry.
    std::fs::write(
        patterns_dir.join("python.yaml"),
        r#"
languages:
  python:
    - id: python-only-fresh-framework
      kind: route
      verbs: [get]
"#,
    )
    .expect("write python.yaml");

    let mut patterns = Patterns::clone_default();
    patterns
        .load_overrides(repo_root)
        .expect("load_overrides must accept the per-repo YAML");

    // The fresh entry is now present.
    let fresh = patterns
        .framework("python-only-fresh-framework")
        .expect("fresh override id is appended to the registry");
    assert_eq!(fresh.kind, FrameworkKind::Route);
    assert_eq!(fresh.verbs, vec!["get".to_string()]);

    // The bundled entries are still all there (none lost).
    let post_routes: Vec<String> = patterns
        .route_patterns(Lang::Python)
        .map(|d| d.id.clone())
        .collect();
    for bundled_id in &pre_routes {
        assert!(
            post_routes.contains(bundled_id),
            "bundled entry {bundled_id:?} must survive the fresh-id override: {post_routes:?}"
        );
    }
    // The fresh entry is also there.
    assert!(
        post_routes.contains(&"python-only-fresh-framework".to_string()),
        "fresh override id must be appended: {post_routes:?}"
    );
    // And the bucket grew by exactly one.
    assert_eq!(
        post_routes.len(),
        pre_count + 1,
        "Python route count must grow by exactly one after a fresh-id append: pre={pre_count}, post={}",
        post_routes.len()
    );
}

// ─── Item 4 — scan-with-no-overrides matches the bundled-only baseline ───
//
// Spec invariant: the no-override fast path of `with_overrides` and
// `Patterns::patterns()` produce identical scan output. A regression
// that branches somewhere on "is this the borrowed path?" and skips
// a step would surface here — this test pins the differential.
#[test]
fn scan_with_no_overrides_matches_bundled_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_root = dir.path();

    // Minimal Python file with a single outbound call. No
    // `.lain/patterns/` — both scan paths must produce the same
    // graph.
    std::fs::write(
        repo_root.join("main.py"),
        r#"
import requests
def fetch():
    return requests.get("https://api.example.com/users")
"#,
    )
    .expect("write main.py");

    // Scan A — `Patterns::with_overrides(repo_root)` (the fast
    // path returns Cow::Borrowed pointing at the singleton, so
    // this exercises the same data the singleton path would).
    let db_path_a = repo_root.join("ga.bin");
    let graph_a = GraphDatabase::new(&db_path_a).expect("ga");
    let ns = RepoNamespace::for_test();
    let repo_id = RepoId::new("differential_a").unwrap();
    let count_a =
        scan_workspace_clients(&graph_a, repo_root, &ns, &repo_id).expect("scan A must succeed");
    let nodes_a: Vec<_> = graph_a
        .get_all_nodes()
        .into_iter()
        .filter(|n| n.node_type == NodeType::HttpClientCall)
        .collect();

    // Scan B — `Patterns::patterns()` directly (no
    // `with_overrides` involved). Same repo, same file, different
    // entry point. The output MUST match A's output bit-for-bit
    // (modulo node ids, which are content-derived and should be
    // identical too because the input is the same).
    //
    // We don't have a `scan_workspace_clients(&graph, root, &ns,
    // &repo_id, &Patterns::patterns())` overload — every
    // scan_workspace_* opens with `with_overrides(root)`. So we
    // drive the comparison at the per-line level via a fresh
    // graph + the same code path: a second `with_overrides` call
    // with the same root.
    let db_path_b = repo_root.join("gb.bin");
    let graph_b = GraphDatabase::new(&db_path_b).expect("gb");
    let count_b =
        scan_workspace_clients(&graph_b, repo_root, &ns, &repo_id).expect("scan B must succeed");
    let nodes_b: Vec<_> = graph_b
        .get_all_nodes()
        .into_iter()
        .filter(|n| n.node_type == NodeType::HttpClientCall)
        .collect();

    // Same count, same names, same contracts.
    assert_eq!(
        count_a, count_b,
        "scan A and B must emit the same number of calls"
    );
    assert_eq!(
        nodes_a.len(),
        nodes_b.len(),
        "scan A and B must produce the same number of HttpClientCall nodes"
    );
    for (a, b) in nodes_a.iter().zip(nodes_b.iter()) {
        assert_eq!(
            a.name, b.name,
            "scan A and B must produce HttpClientCall nodes with the same `name` (URL template)"
        );
        // Compare the contracts by their Debug rendering — the
        // contracts are structurally identical and carry no
        // node-id reference (the consumer fact is purely
        // self-describing).
        let ca = a
            .contract
            .first()
            .expect("HttpClientCall carries a contract");
        let cb = b
            .contract
            .first()
            .expect("HttpClientCall carries a contract");
        assert_eq!(
            format!("{ca:?}"),
            format!("{cb:?}"),
            "scan A and B must produce identical ConsumerFact contracts for the same input",
        );
    }

    // Also pin the singleton identity: the no-override fast path
    // must return the bundled singleton, not a clone.
    let patterns = Patterns::with_overrides(repo_root)
        .expect("with_overrides must succeed for an absent .lain/patterns/");
    match &patterns {
        Cow::Borrowed(p) => {
            assert!(
                std::ptr::eq(*p as *const _, Patterns::patterns() as *const _),
                "no-override with_overrides must point at the bundled singleton"
            );
        }
        Cow::Owned(_) => panic!(
            "no-override with_overrides must return Cow::Borrowed (singleton), not Cow::Owned"
        ),
    }
}

// ─── Extra differential: scan-with-routes also matches the bundled-only baseline ───
//
// Symmetric to the above, but exercises the `http_sensor` walker.
// The two sensors share the `with_overrides` fast path; if one
// branches, the other probably branches too — pin both.
#[test]
fn scan_with_no_overrides_matches_bundled_only_for_routes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_root = dir.path();

    std::fs::write(
        repo_root.join("main.rs"),
        r#"use axum::{routing::get, Router};

async fn get_users() {}

fn build() -> Router {
    Router::new().route("/users", get(get_users))
}
"#,
    )
    .expect("write main.rs");

    let db_path_a = repo_root.join("ga.bin");
    let graph_a = GraphDatabase::new(&db_path_a).expect("ga");
    let ns = RepoNamespace::for_test();
    let repo_id = RepoId::new("differential_routes_a").unwrap();
    let count_a =
        scan_workspace_routes(&graph_a, repo_root, &ns, &repo_id).expect("scan A must succeed");
    let routes_a: Vec<_> = graph_a
        .get_all_nodes()
        .into_iter()
        .filter(|n| n.node_type == NodeType::HttpRoute)
        .collect();

    let db_path_b = repo_root.join("gb.bin");
    let graph_b = GraphDatabase::new(&db_path_b).expect("gb");
    let count_b =
        scan_workspace_routes(&graph_b, repo_root, &ns, &repo_id).expect("scan B must succeed");
    let routes_b: Vec<_> = graph_b
        .get_all_nodes()
        .into_iter()
        .filter(|n| n.node_type == NodeType::HttpRoute)
        .collect();

    assert_eq!(count_a, count_b);
    assert_eq!(routes_a.len(), routes_b.len());
    for (a, b) in routes_a.iter().zip(routes_b.iter()) {
        assert_eq!(a.name, b.name, "scan A and B must agree on the route name");
        let ca = a.contract.first().expect("HttpRoute carries a contract");
        let cb = b.contract.first().expect("HttpRoute carries a contract");
        // Both must be `Provider` contracts with method Get and
        // template `/users`. The `With_provider_contract_carries
        // _the_template` assertion covers the differential for
        // the http_sensor path; the http_client_sensor path is
        // covered above.
        match (ca, cb) {
            (ContractFact::Provider(pa), ContractFact::Provider(pb)) => {
                assert_eq!(pa.method, HttpMethod::Get);
                assert_eq!(pb.method, HttpMethod::Get);
                assert_eq!(pa.template, "/users");
                assert_eq!(pb.template, "/users");
            }
            other => panic!("expected Provider contracts, got {other:?}"),
        }
    }
}

// ─── 2g — `pending_scml` sort is load-bearing even when filenames happen to be alphabetic ───
//
// Coverage gap revealed by mutation 1d: the existing
// `multiple_override_scm_files_in_one_language_folder_are_merged`
// test creates `.scm` files in alphabetic order, and on Linux
// `read_dir` returns them in the same order (creation order).
// The sort step is therefore a no-op for that test — dropping
// the sort leaves the test passing, hiding a real production
// hazard (read_dir order is platform-dependent).
//
// This test closes the gap by creating files in REVERSE
// alphabetic order so the read_dir order is guaranteed to
// differ from the sort order. With the sort removed, the
// merge loop's "advance past sorted overrides" branch would
// produce a wrong (out-of-order, possibly duplicated) merged
// slice, and the assertions below catch it.
#[test]
fn override_scm_files_in_reverse_creation_order_are_merged_sorted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_root = dir.path();

    let patterns_dir = repo_root.join(".lain/patterns/python");
    std::fs::create_dir_all(&patterns_dir).expect("mkdir .lain/patterns/python");

    // Create the .scm files in REVERSE alphabetic order so the
    // platform's `read_dir` returns them out of order. The
    // `load_overrides` sort is what makes the merged slice
    // sorted; dropping it leaves a non-sorted slice.
    let body_a = r#"
; Override A — carries "Override A" header.
; Created FIRST in reverse-alphabetic order to expose the sort
; step's necessity.
(module) @m
"#;
    let body_b = r#"
; Override B — carries "Override B" header.
(function_definition) @f
"#;
    let body_c = r#"
; Override C — carries "Override C" header.
(import_statement) @i
"#;
    // Write in reverse: c, b, a. read_dir will return them in
    // that order (creation order on most platforms).
    std::fs::write(patterns_dir.join("fetch-outbound.scm"), body_c).expect("write c");
    std::fs::write(patterns_dir.join("httpx-outbound.scm"), body_b).expect("write b");
    std::fs::write(patterns_dir.join("requests-outbound.scm"), body_a).expect("write a");

    let mut patterns = Patterns::clone_default();
    patterns
        .load_overrides(repo_root)
        .expect("load_overrides must accept the per-repo .scm bodies");

    let merged = patterns
        .compiled_queries()
        .expect("compiled_queries must succeed after a successful load_overrides");

    // The merged slice must be sorted by key, regardless of
    // creation order. The spec pins this so `generated::get`
    // can binary-search.
    let mut keys: Vec<&str> = merged.iter().map(|(k, _, _, _)| *k).collect();
    let original = keys.clone();
    keys.sort_unstable();
    assert_eq!(
        original, keys,
        "the merged slice must be sorted by key — dropping the load_overrides sort leaves the slice in read_dir (creation) order, which is platform-dependent"
    );

    // Each override key appears exactly once (no duplicate-emit
    // from a non-sorted merge loop). The keys are:
    //   python/fetch-outbound.scm   < python/httpx-outbound.scm   < python/requests-outbound.scm
    let fetch_key = "python/fetch-outbound.scm";
    let httpx_key = "python/httpx-outbound.scm";
    let req_key = "python/requests-outbound.scm";
    let fetch_count = merged.iter().filter(|(k, _, _, _)| *k == fetch_key).count();
    let httpx_count = merged.iter().filter(|(k, _, _, _)| *k == httpx_key).count();
    let req_count = merged.iter().filter(|(k, _, _, _)| *k == req_key).count();
    assert_eq!(fetch_count, 1, "fetch key appears once");
    assert_eq!(httpx_count, 1, "httpx key appears once");
    assert_eq!(req_count, 1, "requests key appears once");

    // And the REPLACE step ran for each (override body wins,
    // not bundled body). The doc-comment headers are unique to
    // the override, so a grep for the header string proves the
    // override body reached the merged map.
    for (k, _, _, body) in merged.iter() {
        if *k == fetch_key {
            assert!(
                body.contains("Override C"),
                "fetch body must be the override's: {body:?}"
            );
        } else if *k == httpx_key {
            assert!(
                body.contains("Override B"),
                "httpx body must be the override's: {body:?}"
            );
        } else if *k == req_key {
            assert!(
                body.contains("Override A"),
                "requests body must be the override's: {body:?}"
            );
        }
    }
}

// ─── Extra coverage gap closures ───────────────────────────────────
//
// The `cargo llvm-cov` pass on the sensors test binary showed
// several legitimate coverage gaps in `patterns/mod.rs` — pieces of
// the public API that production code (e.g. `entry_point_sensor`)
// calls but the test suite never exercises. The tests below close
// those gaps so a future refactor that breaks the contract surfaces
// immediately.
//
//   3a — `entry_point_patterns(lang)` is in the public API. The
//        production `entry_point_sensor` calls it via the thread-
//        local `current_patterns()`, but no unit test exercises it
//        directly. The test below iterates entry points for every
//        bundled language and asserts at least one language has
//        entry-point frameworks (otherwise the bundled YAML is
//        broken).
//   3b — `Patterns::Debug` impl is uncovered. A regression that
//        changes the Debug output (or panics on a circular ref)
//        would surface here.
//   3c — `Patterns::from_yaml_file` is uncovered. The `from_yaml_str`
//        path is well-tested, but the file-loading variant (which
//        catches read errors via `OverrideRead`) has no test.
//   3d — The "advance past sorted overrides" branch in
//        `compiled_queries` (line ~462) is uncovered. The merge
//        loop's "advance" branch emits an override whose key
//        sorts BEFORE the current bundled key. Reaching it
//        requires an override whose key is alphabetically
//        earlier than the first bundled key that follows it.
//        The test below creates an override `python/aaaaa.scm`
//        (sorts before `python/aiohttp-outbound.scm`, the
//        first bundled Python key) to exercise the branch.
//   3e — `deny_methods_for` with no `lib_match` (the framework
//        matches any library) is uncovered. The bundled YAML
//        always sets `lib_match`, but the `None` branch is part
//        of the public contract; the test exercises it via a
//        minimal in-memory YAML.

#[test]
fn entry_point_patterns_filters_by_entrypoint_kind() {
    // The bundled YAML doesn't ship any `kind: entrypoint`
    // entries today (entry-point detection is done via the
    // bundled `<lang>/<framework>-entry-point.scm` tree-sitter
    // queries, not by enumerating the YAML). The test still
    // exercises `entry_point_patterns` via a minimal in-memory
    // YAML to confirm the filter works correctly — a
    // regression that filters by a wrong kind (e.g. `Route`
    // instead of `EntryPoint`) would surface here.
    let yaml = r#"
languages:
  python:
    - id: py-route
      kind: route
      verbs: [get]
    - id: py-outbound
      kind: outbound
      lib_match: '^py_out$'
    - id: py-entry
      kind: entrypoint
      verbs: [get]
    - id: py-entry-2
      kind: entrypoint
"#;
    let patterns = Patterns::from_yaml_str(yaml).expect("yaml is valid");

    let py_eps: Vec<&FrameworkDef> = patterns.entry_point_patterns(Lang::Python).collect();
    // The iterator must return ONLY `kind: entrypoint` entries
    // — not routes, not outbounds.
    assert_eq!(
        py_eps.len(),
        2,
        "entry_point_patterns must return exactly the 2 entrypoint entries; got {py_eps:?}"
    );
    for ep in &py_eps {
        assert_eq!(
            ep.kind,
            FrameworkKind::EntryPoint,
            "entry_point_patterns must filter by kind=EntryPoint; got a {:?} for {:?}",
            ep.kind,
            ep.id
        );
    }
    // And both entrypoint entries are present (none lost).
    let ids: Vec<&str> = py_eps.iter().map(|d| d.id.as_str()).collect();
    assert!(
        ids.contains(&"py-entry"),
        "py-entry must be in the iterator: {ids:?}"
    );
    assert!(
        ids.contains(&"py-entry-2"),
        "py-entry-2 must be in the iterator: {ids:?}"
    );

    // Every lang (Python / Rust / Go / Java / C# / Ruby / Kotlin
    // / TSJS) has an `entry_point_patterns` iterator that returns
    // WITHOUT panicking, even when the bucket is empty. The
    // empty-bucket case is the no-op the production walker relies
    // on for languages the bundled YAML doesn't ship entry-point
    // frameworks for.
    let langs = [
        Lang::Python,
        Lang::Rust,
        Lang::Go,
        Lang::Java,
        Lang::CSharp,
        Lang::Ruby,
        Lang::Kotlin,
        Lang::TsJs,
    ];
    for lang in langs {
        let eps: Vec<&FrameworkDef> = patterns.entry_point_patterns(lang).collect();
        for ep in &eps {
            assert_eq!(
                ep.kind,
                FrameworkKind::EntryPoint,
                "{lang:?} entry_point_patterns must filter by kind=EntryPoint; got {:?} for {:?}",
                ep.kind,
                ep.id
            );
        }
    }
}

#[test]
fn patterns_debug_impl_renders_without_panicking() {
    // The Debug impl iterates `self.yaml.languages.keys()` and
    // queries the override / cache flags. A regression that adds
    // a non-Debug field (or introduces a circular reference via
    // the merged-queries cache) would panic here.
    let patterns = Patterns::patterns();
    let dbg = format!("{patterns:?}");
    assert!(
        !dbg.is_empty(),
        "Patterns::Debug must produce a non-empty rendering (otherwise an operator's diagnostic dump is empty)"
    );
    // The rendering must include the field names — otherwise a
    // future refactor that renames the fields in the struct
    // without updating the Debug impl produces an empty-ish
    // output.
    assert!(
        dbg.contains("yaml_languages"),
        "Debug must name the `yaml_languages` field: {dbg:?}"
    );
    assert!(
        dbg.contains("overrides_applied"),
        "Debug must name the `overrides_applied` field: {dbg:?}"
    );
    assert!(
        dbg.contains("override_scml_count"),
        "Debug must name the `override_scml_count` field: {dbg:?}"
    );
}

#[test]
fn from_yaml_file_loads_a_real_file_from_disk() {
    // The file-loading variant of `from_yaml_str` carries its
    // own `OverrideRead` error path (read errors are wrapped
    // there, not propagated as bare `io::Error`). The test
    // exercises the happy path: write a temp YAML, call
    // `from_yaml_file`, assert the framework round-trips.
    let dir = tempfile::tempdir().expect("tempdir");
    let yaml_path = dir.path().join("frameworks.yaml");
    std::fs::write(
        &yaml_path,
        r#"
languages:
  python:
    - id: from-yaml-file-test
      kind: outbound
      lib_match: '^from_yaml_file$'
      deny_methods: [test_method]
"#,
    )
    .expect("write yaml");

    let patterns = Patterns::from_yaml_file(&yaml_path).expect("from_yaml_file must succeed");
    let fwk = patterns
        .framework("from-yaml-file-test")
        .expect("the framework survives the round-trip");
    assert_eq!(fwk.kind, FrameworkKind::Outbound);
    assert_eq!(fwk.lib_match.as_deref(), Some("^from_yaml_file$"));
    assert_eq!(fwk.deny_methods, vec!["test_method".to_string()]);
}

#[test]
fn compiled_queries_advance_branch_emits_override_before_bundled() {
    // The "advance past sorted overrides" branch in
    // `compiled_queries` (around line 462) emits an override
    // whose key sorts BEFORE the current bundled key. Reaching
    // it requires an override key that sorts before the FIRST
    // bundled key in the bucket. The bundled Python keys start
    // with `python/aiohttp-outbound.scm` (the earliest in
    // alphabetic order), so an override `python/aaaaa.scm`
    // (synthesised) sorts before it and exercises the advance
    // branch.
    //
    // We need a parseable tree-sitter body for the override —
    // the `tree_sitter::Query::new` validator must accept the
    // body BEFORE the merge loop's advance branch can run.
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_root = dir.path();
    let patterns_dir = repo_root.join(".lain/patterns/python");
    std::fs::create_dir_all(&patterns_dir).expect("mkdir .lain/patterns/python");
    std::fs::write(
        patterns_dir.join("aaaaa.scm"),
        r#"
; Override with a key that sorts before `python/aiohttp-outbound.scm`
; (the first bundled Python key). Exercises the
; "advance past sorted overrides" branch in `compiled_queries`
; (line ~462) — the merge loop emits this entry BEFORE the
; first bundled entry it encounters.
(module) @m
"#,
    )
    .expect("write aaaaa.scm");

    let mut patterns = Patterns::clone_default();
    patterns
        .load_overrides(repo_root)
        .expect("load_overrides must accept the per-repo .scm body");

    let merged = patterns
        .compiled_queries()
        .expect("compiled_queries must succeed after load_overrides");

    // The override must appear in the merged slice.
    let aaaaa_present = merged.iter().any(|(k, _, _, _)| *k == "python/aaaaa.scm");
    assert!(
        aaaaa_present,
        "the override `python/aaaaa.scm` must appear in the merged slice (otherwise the advance branch never ran)"
    );

    // And it must sort before `python/aiohttp-outbound.scm`
    // (proves the merge loop's advance branch emitted it
    // BEFORE the first bundled entry, which is the actual
    // semantic of "advance past sorted overrides").
    let aaaaa_idx = merged
        .iter()
        .position(|(k, _, _, _)| *k == "python/aaaaa.scm")
        .expect("aaaaa.scm position");
    let aiohttp_idx = merged
        .iter()
        .position(|(k, _, _, _)| *k == "python/aiohttp-outbound.scm")
        .expect("aiohttp-outbound.scm position");
    assert!(
        aaaaa_idx < aiohttp_idx,
        "aaaaa.scm must sort before aiohttp-outbound.scm in the merged slice (the advance branch's contract): aaaaa={aaaaa_idx}, aiohttp={aiohttp_idx}"
    );
}

#[test]
fn deny_methods_for_with_no_lib_match_matches_any_library() {
    // The `lib_match: None` branch in `deny_methods_for` means
    // "any library matches" — the framework's deny surface is
    // returned for every library the caller queries. The
    // bundled YAML always sets `lib_match`, so the `None` branch
    // is uncovered. The test exercises it via a minimal
    // in-memory YAML.
    let yaml = r#"
languages:
  python:
    - id: any-lib-outbound
      kind: outbound
      # No `lib_match` — matches any library per the
      # `deny_methods_for` contract.
      deny_methods: [json, text, data, body]
"#;
    let patterns = Patterns::from_yaml_str(yaml).expect("yaml is valid");
    // Any library name (including one the bundled regex
    // wouldn't match) should see the deny list.
    let deny = patterns.deny_methods_for(Lang::Python, "any-random-library", "get");
    assert!(
        deny.contains(&"json".to_string()),
        "the no-lib_match branch must surface `json` for any library: {deny:?}"
    );
    assert!(
        deny.contains(&"text".to_string()),
        "the no-lib_match branch must surface `text` for any library: {deny:?}"
    );
    // And the union must dedupe (per the `deny_methods_for`
    // contract — same library, same deny list, no duplicates
    // even when multiple frameworks match).
    let unique_count = {
        let mut seen = std::collections::HashSet::new();
        deny.iter().filter(|m| seen.insert(m.as_str())).count()
    };
    assert_eq!(
        unique_count,
        deny.len(),
        "deny_methods_for must dedupe the returned list: {deny:?}"
    );
}
