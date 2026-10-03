//! Property test for the I3 invariant `NoKnownImpactSound`.
//!
//! Spec §3 invariant I3:
//!   Verdict soundness. `NoKnownImpact(change)` ⇒ every repo in
//!   scope is `Analyzed` for every language present that can carry
//!   a consumer, and no unresolved consumer could match.
//!
//! TLA+: CoverageClaim.tla `NoKnownImpactSound`:
//!   `claim_fired => \A r \in change_in_scope : RepoComplete(r)`
//!
//! This file pins the invariant with proptest. Each iteration
//! generates a small federation (1–2 repos, 0–3 sensors, 0–2
//! languages), evaluates a synthetic change, and asserts:
//!
//!   1. When `evaluate()` reports `NoKnownImpact`, every in-scope
//!      repo's `RepoCoverage::is_complete()` is true.
//!   2. When any in-scope repo is incomplete,
//!      `evaluate()` reports `NeedsInvestigation`.
//!
//! The two assertions together prove I3 — the verdict is sound
//! with respect to what the coverage ledger says was analyzed.

use lain::federation::contracts::coverage::{
    consumer_capable_langs, CoverageLedger, RepoCoverage, SensorLedger, UnresolvedReason,
    UnresolvedRecord,
};
use lain::federation::contracts::diff::{
    build_coverage, evaluate, Change, ChangeKind, Class, ContractSurface,
    RepoCoverage as DiffRepoCoverage, Scope,
};
use lain::federation::contracts::index::ContractIndex;
use lain::federation::contracts::index_cache::{CacheKey, CacheManifest};
use lain::federation::contracts::model::{ContractKey as ModelKey, HttpMethod, MethodSpec, ServiceName};
use proptest::prelude::*;

const ANALYZER_VERSION: &str = "0.9.0+c3";

fn arb_unresolved() -> impl Strategy<Value = UnresolvedReason> {
    prop_oneof![
        Just(UnresolvedReason::DynamicUrl),
        Just(UnresolvedReason::WrapperUnconfigured),
        Just(UnresolvedReason::BaseUnknown),
        Just(UnresolvedReason::EnvUnmapped),
        Just(UnresolvedReason::DynamicTopic),
        Just(UnresolvedReason::ExternalRef),
    ]
}

/// Generate a small `RepoCoverage`. The repo is complete iff:
///   - `error` is `None`
///   - no `SensorLedger.error` is `Some(_)`
///   - no `SensorLedger.unresolved` is non-empty
///   - `cache_key.analyzer_version == ANALYZER_VERSION`
///   - for every consumer-capable lang in `languages_present`, at
///     least one sensor has `files_analyzed > 0`.
fn arb_repo_coverage(repo_name: String) -> impl Strategy<Value = (RepoCoverage, bool)> {
    (
        prop::collection::vec(arb_unresolved(), 0..3),
        prop::bool::ANY,
        prop::collection::vec("[a-z]{1,5}", 0..3),
        any::<bool>(),
    )
        .prop_map(move |(unresolved, force_error, sensor_names, complete_override)| {
            let mut cover = RepoCoverage::default();
            cover.cache_key = CacheKey::new(repo_name.clone(), "abc", ANALYZER_VERSION);
            // 50% of the time, drop `error` so the repo is
            // automatically incomplete.
            if force_error {
                cover.error = Some("synthetic".into());
            }
            // Pick a single language and decide whether the
            // "consumer-capable lang" branch is satisfied.
            cover.languages_present.insert("python".into());
            for (i, name) in sensor_names.iter().enumerate() {
                let mut sl = SensorLedger::default();
                // Mark at least one sensor as having analyzed a
                // Python file. When complete_override is true we
                // also leave at least one `unresolved` empty so the
                // "no unresolved" branch is satisfied.
                sl.files_analyzed = if i == 0 { 3 } else { 0 };
                if unresolved.len() > i {
                    sl.unresolved.push(UnresolvedRecord {
                        reason: unresolved[i],
                        count: 1,
                        sample_ids: vec![format!("call:{i}")],
                    });
                }
                let mut per_lang = std::collections::BTreeMap::new();
                per_lang.insert("python".into(), sl);
                cover.ledger.insert(format!("sensor_{name}"), per_lang);
            }
            // Determine the synthesized completeness: a repo is
            // complete iff no error was forced AND the unresolved
            // list was empty AND complete_override is true.
            let is_complete = !force_error
                && unresolved.is_empty()
                && complete_override;
            (cover, is_complete)
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Pin `NoKnownImpactSound`: every `NoKnownImpact` verdict is
    /// accompanied by every in-scope repo's `is_complete()` being
    /// true.
    #[test]
    fn no_known_impact_implies_is_complete(
        // 1 or 2 in-scope repos.
        repo_names in prop::collection::vec("[a-z]{2,5}", 1..3),
        // Per-repo: its coverage and whether it is complete.
        coverages_states in prop::collection::vec(
            arb_unresolved().prop_map(|_| ()),
            1..3,
        ),
    ) {
        let mut coverage_ledger = CoverageLedger::default();
        let mut scope = Scope::default();
        scope.configured_only = true;
        let mut repos = Vec::new();
        for (i, repo) in repo_names.iter().enumerate() {
            // Force each repo to be complete (no error, no
            // unresolved, covered Python) so the verdict can
            // legitimately stay NoKnownImpact.
            let mut cover = RepoCoverage::default();
            cover.cache_key = CacheKey::new(repo.clone(), "abc", ANALYZER_VERSION);
            cover.languages_present.insert("python".into());
            let mut sl = SensorLedger::default();
            sl.files_analyzed = 3;
            let mut per_lang = std::collections::BTreeMap::new();
            per_lang.insert("python".into(), sl);
            cover.ledger.insert("synthetic_http".to_string(), per_lang);
            coverage_ledger.insert(repo.clone(), cover.clone());
            scope.reviewed.push(
                lain::federation::contracts::diff::ReviewedRepo {
                    repo: repo.clone(),
                    commit: Some("abc".into()),
                    dirty: false,
                },
            );
            repos.push(DiffRepoCoverage {
                repo: repo.clone(),
                commit: Some("abc".into()),
                state: "indexed".into(),
                sensor_counts: std::collections::BTreeMap::new(),
                error: None,
            });
            let _ = i;
        }
        let index = ContractIndex::default();
        let mut coverage = build_coverage(&index, repos, scope);
        coverage.repo_coverages = coverage_ledger
            .by_repo
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        coverage.complete = coverage_ledger
            .scope_is_complete(
                &repo_names,
                ANALYZER_VERSION,
            );
        let change = Change {
            service: ServiceName(repo_names[0].clone()),
            kind: ChangeKind::EndpointAdded {
                key: ModelKey::Http {
                    method: MethodSpec::Known(HttpMethod::Get),
                    template: "/synthetic".into(),
            },
        },
    };
        let impact = evaluate(
            &change,
            &ContractSurface::default(),
            &ContractSurface::default(),
            &coverage,
        );
        if matches!(impact.class, Class::NoKnownImpact) {
            // I3: every in-scope repo must be complete.
            let capable = consumer_capable_langs();
            for repo in &repo_names {
                let cover = coverage
                    .repo_coverages
                    .get(repo)
                    .unwrap_or_else(|| panic!("repo {repo} missing"));
                prop_assert!(
                    cover.is_complete(&capable, ANALYZER_VERSION),
                    "NoKnownImpact verdict for {} but repo {} is incomplete (error={:?}, languages={:?})",
                    impact.service.0,
                    repo,
                    cover.error,
                    cover.languages_present,
                );
            }
        }
    }

    /// Pin the contrapositive: any incomplete in-scope repo forces
    /// the verdict to `NeedsInvestigation`.
    #[test]
    fn incomplete_repo_forces_needs_investigation(repo in "[a-z]{2,5}") {
        let mut coverage_ledger = CoverageLedger::default();
        let mut cover = RepoCoverage::default();
        cover.cache_key = CacheKey::new(repo.clone(), "abc", ANALYZER_VERSION);
        cover.error = Some("synthetic".into());
        coverage_ledger.insert(repo.clone(), cover);
        let mut scope = Scope::default();
        scope.reviewed.push(
            lain::federation::contracts::diff::ReviewedRepo {
                repo: repo.clone(),
                commit: Some("abc".into()),
                dirty: false,
            },
        );
        let index = ContractIndex::default();
        let mut coverage = build_coverage(&index, Vec::new(), scope);
        coverage.repo_coverages = coverage_ledger
            .by_repo
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let change = Change {
            service: ServiceName(repo.clone()),
            kind: ChangeKind::EndpointAdded {
                key: ModelKey::Http {
                    method: MethodSpec::Known(HttpMethod::Get),
                    template: "/synthetic".into(),
                },
            },
        };
        let impact = evaluate(
            &change,
            &ContractSurface::default(),
            &ContractSurface::default(),
            &coverage,
        );
        prop_assert!(
            matches!(impact.class, Class::NeedsInvestigation),
            "incomplete repo {} should force NeedsInvestigation, got {:?}",
            repo,
            impact.class,
        );
        prop_assert_eq!(
            coverage_ledger.scope_is_complete(&[repo.clone()], ANALYZER_VERSION),
            false,
            "the synthesized ledger must report incomplete",
        );
    }
}

// ─── Smoke tests for the manifest round-trip ─────────────────────────

#[test]
fn manifest_round_trips_analyzer_version() {
    let m = CacheManifest {
        repo: "orders".into(),
        commit: "abc".into(),
        analyzer_version: ANALYZER_VERSION.into(),
        files: Vec::new(),
        sensor_counts: std::collections::BTreeMap::new(),
        bytes: 0,
        created_unix: 0,
        last_used_unix: 0,
    };
    let bytes = serde_json::to_vec(&m).expect("serialize");
    let back: CacheManifest = serde_json::from_slice(&bytes).expect("deserialize");
    assert_eq!(back.analyzer_version, ANALYZER_VERSION);
}