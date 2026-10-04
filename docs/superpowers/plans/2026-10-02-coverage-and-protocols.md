# Plan: contract-coverage-and-protocols (TLA+ first, then Phase A → E)

**Spec:** `docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md`
**Branch:** `feat/contract-coverage-and-protocols` (parent `feat/contract-federation`)
**PR:** #272 (draft, against `dev`)
**Date:** 2026-10-02

## Order (revised per the updated spec §9)

The spec now requires three TLA+ models that **target existing code**, with TLC finding counterexamples and a follow-up commit fixing each. **No new code lands before the TLA+ confirms what's broken.**

1. **`RejoinProtocol.tla`** (modeled against `federated_index.rs`) — find counterexamples in `rejoin_contracts_if_dirty` / `rejoin_contracts` / `contracts_dirty` / `projection_lock` / `contract_index`. Fix on this branch as a small commit.
2. **`SnapshotResidency.tla`** (modeled against `snapshots/manager.rs`) — find counterexamples in `from_snapshot_with_wait_ms` / `install_resident` / `try_evict_one_lru_unheld` / `HoldGuard`. Fix on this branch as a small commit.
3. **`CoverageClaim.tla`** (extended with cache validity per spec §9.3) — gates Phase A. Verifies the `analyzer_version` bump requirement.
4. **Phase A** (coverage ledger + rule-1 fix) — uses `CoverageClaim.tla` (extended) as the formal-first mapping.
5. **Phase B → E** — per spec §5–§8, each with their own TLA+ extensions where the spec calls for them.

## Mapping: TLA+ ⇒ Rust (formal-first)

The TLA+ spec is the contract. The Rust implementation must mirror each action and respect each invariant. Every new function cites its TLA+ action in its doc-comment.

### RejoinProtocol.tla → `federated_index.rs`

Suspected bugs (to be confirmed by TLC):
- **Lost dirty flag.** `rejoin_contracts` reads inputs → joins → writes `Binds` → swaps `contract_index` → `contracts_dirty.store(false)`. A `mark_contracts_dirty` call landing mid-join is overwritten by the trailing clear.
- **Torn input.** Nodes (`get_node`) and edges (`all_edges`) are read in separate calls; a concurrent projection can land in between.
- **Two views of one generation.** `Binds` edges applied before `contract_index` swap.

| TLA+ state / action | Rust |
|---|---|
| `dirty: BOOLEAN` | `contracts_dirty: AtomicBool` |
| `index: Generation` | `contract_index: ArcSwap<ContractIndex>` |
| `binds: Binds` | `binds: Arc<Binds>` (in federated_index) |
| `ProjectNode(n)` / `ProjectEdge(e)` | the projection paths |
| `MarkDirty()` | `mark_contracts_dirty()` |
| `Rejoin()` | `rejoin_contracts_if_dirty()` |
| Variants checked | (a) current code, (b) clear-before-read-and-reset-on-error, (c) epoch-stamped atomic publish |
| Invariants | convergence, no-lost-update, I7 reader consistency |
| Liveness | dirty flag eventually cleared |

### SnapshotResidency.tla → `snapshots/manager.rs`

Suspected bugs (to be confirmed):
- `held: AtomicBool` should be a count — two concurrent holds on the same resident share it, first drop clears it, snapshot can leave `resident` while still in use.
- `install_resident` checks `len() < cap` and inserts under separate lock acquisitions — cap overrun.
- `try_evict_one_lru_unheld` selects + removes under separate locks — hold can land in between.
- No single-flight — two builders for one snapshot id both build.
- Condvar uses a different mutex from the state it guards — possible lost wakeup.

| TLA+ state / action | Rust |
|---|---|
| `resident: SUBSET SnapshotId` | the residency set (capped) |
| `held: [SnapshotId → Nat]` | the hold count (currently `AtomicBool` — bug) |
| `cap: Nat` | `cap` config |
| `Hold(s)` / `Release(s)` | `HoldGuard::new` / drop |
| `Install(s)` | `install_resident` |
| `Evict(s)` | `try_evict_one_lru_unheld` |
| Invariants | held never evicted, `|resident| ≤ cap`, at most one federation per id, busy only when all slots held |

### CoverageClaim.tla (extended) → Phase A

Existing model (state space: repos, languages, sensors, sensor outcomes, unresolved). Extended with:

| TLA+ (extended) | Rust |
|---|---|
| `cache_key: CacheKey = (analyzer_version, repo_id, commit_id)` | `CacheKey` already exists; add `analyzer_version: SemVer` |
| `CacheValid(c) ↔ c.analyzer_version = current_version` | `is_valid(key)` check |
| `InvalidateAllCaches` (triggered by `analyzer_version++`) | bump + clear |
| `analyzer_version' ≠ analyzer_version ⇒ ledger presence required` | the rule "ledger presence is part of cache validity" |

### Per-call resolution state machine (deferred to Phase B)

I2 / I5 / I6 are pipeline invariants (every call lands in one terminal state; no self-bind; total-order precedence). These will get TLA+ models in Phase B once the wrapper-resolution state machine exists in code. For now, the spec keeps them as property tests.

## Phases

### Phase A — Coverage ledger + rule-1 fix (soundness; 0.9 gate)

Per spec §4. Implemented *after* the cache-validity extension to CoverageClaim.tla passes.

```rust
pub struct SensorLedger {
    pub files_seen: usize,
    pub files_analyzed: usize,
    pub files_skipped: Vec<SkipRecord>,
    pub emitted: usize,
    pub unresolved: Vec<UnresolvedRecord>,
    pub error: Option<String>,
}

pub struct RepoCoverage {
    pub ledger: BTreeMap<SensorId, BTreeMap<Lang, SensorLedger>>,
    pub languages_present: BTreeSet<Lang>,
    pub sensor_counts: SensorCounts,                  // derived; kept for back-compat
    pub cache_key: CacheKey,                          // analyzer_version + repo_id + commit_id
    pub error: Option<String>,
}

impl RepoCoverage {
    pub fn is_complete(&self, supported_langs: &[Lang]) -> bool { ... }
    pub fn cache_valid(&self, current_version: SemVer) -> bool { ... }
}
```

Mechanics (formal-first — each cites the TLA+ action):
- `walk_workspace` returns `Vec<FileRecord>` — every file classified once.
- `Sensor::scan_with_report(...)` default-delegates to `scan` (TLA+: `Reindex`).
- `run_all` returns `(SensorCounts, CoverageLedger)` (TLA+: `RepoComplete`).
- Cache validity (TLA+: `CacheValid`) — analyzer_version bump invalidates cache.
- Verdict downgrade: `NoKnownImpact` → `NeedsInvestigation` on incomplete coverage (TLA+: `NoKnownImpactSound`).
- Rule-1 fix: wrapper candidates → `ConsumerTarget::Unresolved{WrapperUnconfigured}`.

Acceptance (from spec §4):
- Fixture repo containing a language with no sensor ⇒ `not_analyzed`, `NeedsInvestigation`.
- Corrupt/oversized/unreadable file ⇒ in `files_skipped`.
- Wrapper call with no config ⇒ in `unresolved`.
- Snapshot round-trip preserves the ledger AND the cache_key.
- `pr13_hermetic_precision_recall_over_t1_fixture` stays at 1.000 × 6.
- Each golden test that flips from `NoKnownImpact` → `NeedsInvestigation` is called out in the commit message.

### Phase B — Wrapper & base-URL resolution (stretch)

Per spec §5. Adds the per-call resolution state machine. Phase B is when I2 / I5 / I6 get their TLA+ model.

### Phase C — Env aliases

Per spec §6.

### Phase D — SQL tables

Per spec §7. `NodeType::Table`, `EdgeType::ReadsTable`, `EdgeType::WritesTable` folded into v3.

### Phase E — gRPC then GraphQL

Per spec §8. `ContractKey::Rpc/Graphql` folded into v3.

## Delivery cadence

- **All on this branch, stacked on PR #272** (per user: "MERGE TO THIS PR. NOT TO DEV.").
- Each fix / phase lands as commits on this branch.
- Each landing: `cargo test`, `clippy --all-targets` 0/0, fmt, fresh-named ci-probe + acceptance harness before "done".
- Bugs found en route (TLC counterexamples, golden flips) fixed in place.
- Hard-rules compliance: no schema version bumps, no fixture edits, no `Cargo.toml [dependencies]` runtime version bumps. CHANGELOG + `lain reindex` cover the v3-fold-in.

## Tooling

- TLA+: `tools/tla/tlc` wrapper, `tla2tools.jar` v1.8.0. CI runs `./tools/tla/tlc docs/formal/*.tla` when `docs/formal/` changes.
- New parser crates (tree-sitter-proto, graphql-parser, sql-parser) — license/vuln policy check via `docs/VULNS.md` before adoption.

## What's not in this plan

- TLA+ for I1, I2, I4, I5, I6 (until Phase B) — proptest cases per spec §9.
- Sensor-phase ordering — write-locked swap, model would be trivial.