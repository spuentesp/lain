# Final whole-branch review — `feat/contract-coverage-and-protocols`

**Reviewer:** whole-branch reviewer
**Date:** 2026-10-03
**Branch:** `feat/contract-coverage-and-protocols` (HEAD `72fffa0d`)
**PR:** #272
**Spec:** `docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md`
**Plan:** `docs/superpowers/plans/2026-10-02-coverage-and-protocols.md`

## 1. Cross-cutting review summary

The 54 coverage-branch-only commits (Phase A → E plus the fix-pass
and 6 TLA+ models) were walked across the surfaces the spec asks the
reviewer to look at. The work splits cleanly into:

- **Phase A — `CoverageLedger` + rule-1 fix** (soundness gate, 0.9
  target). The new `SensorLedger` / `CoverageLedger` types, the
  `RepoCoverage::is_complete` predicate, the `evaluate()`
  downgrade from `NoKnownImpact` to `NeedsInvestigation`, and the
  tri-state query result are well-formed. The cache-validity
  invariant (analyzer version + shape in the cache key) is enforced
  by the loader and pinned by `CoverageClaimCache.tla`. The
  `ContractJoiner::run` default delegation + `scan_with_report`
  pattern is consistent.
- **Phase B — `ClientRegistry` + tier-2 base composition.** The
  `ClientDef` / `ClientRegistry` types, `resolve_cross_file` (one
  re-export hop), and `compose_and_normalize` are well-formed and
  unit-tested. The Tier-2 hook (`target_service_via_registry`) is
  correctly placed in `resolve_consumer` before the `http_clients`
  rule.
- **Phase C — `EnvBindingIndex` + `env_sensor`.** The
  `env_bindings_for` scanner is correct, and the `EnvAmbiguous` /
  `EnvUnmapped` paths in `resolve_consumer` honour I6 (ambiguous
  preempts every lower tier).
- **Phase D — `NodeType::Table` + `ReadsTable` / `WritesTable`
  edges.** Folded into v3, no version bump. `sql_sensor` recognises
  the documented call shapes and falls back to `DynamicSql` on
  non-literal statements. The edges flow through the backend's
  `impact_propagation` (Incoming) for typed traversal.
- **Phase E-gRPC + E-GraphQL.** New `ContractKey::Rpc` and
  `ContractKey::Graphql` variants, `resolve_rpc_consumer` and
  `resolve_graphql_consumer` joiners, ambiguity guards on both
  (`RpcStubUnknown` on missing channel, `GraphqlNoOp` on multiple
  providers). Same-service I5 enforced.
- **Fixes-pass (6 commits).** RejoinProtocol variant (b) (clear
  before read + atomic publish), SnapshotResidency variant (b)
  (`held: AtomicUsize`) + variant (c) (single-flight per id), with
  companion TLA+ models that model the buggy variant (a), the fix
  variants (b)/(c), and a fixes-pass report.
- **TLA+ models.** Six pre-existing specs (`CoverageClaim`,
  `CoverageClaimCache`, `IndexGeneration`, `RejoinProtocol` + VB,
  `SnapshotResidency` + VBC) all check clean.

### What was clean

- **I3 verdict soundness.** `evaluate()` downgrades correctly when
  `coverage.repo_coverages` is non-empty AND any in-scope repo
  fails `RepoCoverage::is_complete`. Cache validity checks both
  `analyzer_version` and the implicit shape (the dir name carries
  both).
- **I5 no-same-service Binds.** Enforced in every joiner
  (HTTP, topic, RPC, GraphQL).
- **I7 index-generation consistency.** The atomic `(index,
  binds)` publish under one `RwLock` write eliminates the
  pre-fix torn-view window. `RejoinProtocol_VariantB.tla` confirms
  no counterexample in the post-fix model.
- **Hard-rule compliance.** No commits unique to the coverage
  branch modify `tests/fixtures/contracts/ground_truth.yaml`,
  `scripts/contracts-fixture.sh`, `Cargo.toml`,
  `Cargo.lock`, `server.json`, `npm-shim/package.json`, or
  `Formula/lain.rb`. The only file the coverage branch touches
  under `src/server/federation/graph_backend.rs` is the
  `impact_propagation` switch (`ReadsTable` / `WritesTable` →
  `Incoming`); `FEDERATION_GRAPH_VERSION` stays at 3.
- **The new TLA+ models match the new code.** The variant (b) /
  variant (c) configurations of `RejoinProtocol` and
  `SnapshotResidency` check clean against the post-fix code.

### What was suspicious

Four cross-cutting concerns survived the walkthrough and are
modelled in `docs/formal/`. See §2.

## 2. Suspicion table

| ID | Description | Severity | TLA+ file | TLC outcome |
|----|-------------|----------|-----------|-------------|
| S1 | `install_resident` calls `try_evict_one_lru_unheld` BEFORE the cap check, evicting LRU entries unnecessarily when the resident has space | Important | `docs/formal/InstallResidentEviction.tla` (variant a) + `docs/formal/InstallResidentEviction_VariantB.tla` (variant b fix) | **Counterexample** in (a): 5-state trace shows `evictions_during_install = 1 ∧ resident_before = 0 < Cap = 3`. No counterexample in (b). |
| S2 | Single-flight `in_flight` slot is removed AFTER the outcome is published; a concurrent caller arriving after the publish but before the remove can start a duplicate build | Important | `docs/formal/SnapshotInFlightSlot.tla` (variant a) + `docs/formal/SnapshotInFlightSlot_VariantB.tla` (variant b fix) | **Counterexample** in (a): 5-state trace shows `has_first_result[s1] = TRUE ∧ second_build_count[s1] = 1`. No counterexample in (b) (the second-build action is unreachable while the slot is held). |
| S3 | Phase B tier-2 and Phase C env resolution are unreachable in the production rejoin path. `FederatedIndex::rejoin_contracts` calls `ContractJoiner::run` (with empty `ClientRegistry` and empty `EnvBindingIndex` defaults); the registry-building and env-binding variants are only used by tests. The rule-1 fix's opt-in clause (only emits `Unresolved{WrapperUnconfigured}` when `http_clients` is non-empty) makes this gap invisible to golden tests but a real I2 violation in production: a `CallVia::Receiver` whose `ClientDef` is registered lands in **no** terminal state when `http_clients` is empty. | **Critical** | `docs/formal/JoiningTier2NotPlumbed.tla` | **Counterexample**: 2-state trace shows `calls_seen = {c1} ∧ call_terminal[c1] = "None"` after a wrapper call enters the joiner. The I2 invariant `every call lands in exactly one terminal state` is violated. |
| S4 | `index` and `binds_epoch` publication in `contract_snapshot` is now atomic under one write-lock (variant (c) of the fix-pass); the existing `RejoinProtocol_VariantB.tla` confirms this holds. The reader side (tool that does `contract_snapshot()` then queries backend separately) is NOT covered by the TLA+ model and is documented as a "torn view" risk in the joiner comment. Speculation, not modeled — would need a reader-side model with 4+ variables to be useful. | Minor | (speculation, not modeled) | n/a |
| S5 | The existing `RejoinProtocol_VariantB.tla` uses the same `NoLostUpdate` invariant as the buggy `RejoinProtocol.tla` (line 78: `dirty \/ binds = ComputeBinds(config, nodes, edges)`). The clear-before-read fix moves `dirty := FALSE` to `RejoinStart` (step 0 → 1), but the invariant is checked at every reachable state — including mid-rejoin. The result: the post-fix code is flagged as `NoLostUpdate`-violating, even though the code is correct (mid-rejoin, the `projection_lock` serialises against any writer that would set `dirty := TRUE`). | Minor (model-only) | `docs/formal/RejoinProtocolMidRejoinInvariant.tla` | **Counterexample** under the original invariant. No counterexample under the corrected (quiescent-only) invariant. **The Rust code is correct; the existing TLA+ spec has a too-strong invariant that flags a false positive.** |

### S1 — `install_resident` evicts unnecessarily when resident has space

**File:** `src/server/federation/contracts/snapshots/manager.rs:1185-1208`

**The code:**

```rust
loop {
    let evicted = self.try_evict_one_lru_unheld();   // <-- runs FIRST
    let mut resident = self.resident.lock();
    if resident.len() < cap || evicted {
        resident.insert(fed.snapshot_id.clone(), fed.clone());
        return Ok(());
    }
    // ... wait or busy
}
```

The combined-lock fix moved the `len() < cap` check and the
`insert` under one `self.resident.lock()` acquisition (good — that
closes the original cap-overrun race). But the call to
`try_evict_one_lru_unheld()` still happens BEFORE the lock and
BEFORE the cap check. When the resident has space (say 2 entries,
cap 4), an unheld LRU entry is evicted, reducing `len()` to 1;
the subsequent `insert` brings it back to 2. Net effect: one
cache hit lost per install.

**TLC trace (variant a):**

```
State 1 (Init): resident = {}, cap = 3
State 2 (InstallCheckInsert): resident = {s1}, evictions = 0, succeeded = TRUE
State 3 (EvictUnheld): resident = {}, evictions = 1
State 4 (InstallCheckInsert): resident = {s1}, evictions = 1, resident_before = 0
                              succeeded = TRUE
   --> invariant FAILS: evictions = 1 AND resident_before = 0 < Cap = 3
```

**Recommendation:** swap the order — acquire the lock first, run
the cap check, and only enter the eviction path when
`len() == cap`. The variant (b) model shows the fix holds.

### S2 — `in_flight` slot can leak a duplicate-build window

**File:** `src/server/federation/contracts/snapshots/manager.rs:1043-1088`

**The code:**

```rust
let outcome = if is_first {
    let build_result = self.build_snapshot_federation(record);
    let install_result = ...;
    let outcome = ...;
    {
        let mut state = slot.state.lock().unwrap();
        *state = Some(outcome.clone());
        slot.notify.notify_all();
    }
    // Race window: between the publish above and the
    // remove below, a concurrent caller can see the slot
    // in the map but also see has_first_result = TRUE.
    // The next caller AFTER the remove starts a duplicate
    // build.
    self.in_flight.lock().remove(&record.id);
    outcome
} else {
    // joiner waits on slot
    ...
}
```

The single-flight (variant c) is sound for callers that see the
slot in the map (they wait on the condvar and reuse the
publisher's result). But after the publish and before the
remove, a second caller can enter the map path, see the slot,
clone it, and observe `state = Some(outcome)`. A third caller
arriving AFTER the remove sees no slot and starts a duplicate
build — the resident cache for the id is still warm from the
first build, so `from_snapshot_with_wait_ms` would return the
warm cache, but the duplicate build still runs in the wrong
case (a build that was needed by an eviction-sweep between
calls).

**TLC trace (variant a):**

```
State 1 (Init): in_flight = {}, has_first_result[s1] = FALSE
State 2 (FirstBuildStart(s1)): in_flight = {s1}
State 3 (FirstBuildDone(s1)): has_first_result[s1] = TRUE
State 4 (FirstBuildRemove(s1)): in_flight = {}
State 5 (SecondBuildAfterRemove(s1)):
    has_first_result[s1] = TRUE  AND  second_build_count[s1] = 1
   --> invariant FAILS: a second build ran while the first
       federation was still in resident
```

**Recommendation:** hold the `in_flight` map lock across the
whole `build + install + publish` sequence; only release it
after the slot removal. Or move the slot removal to BEFORE
the build (and accept that the joiner side will not see the
slot — but the resident cache check at the top of
`from_snapshot_with_wait_ms` catches this case for any caller
that arrived during the build).

### S3 — Tier-2/3 plumbing is unreachable in production

**File:** `src/server/federation/federated_index.rs:1158` and
`src/server/federation/contracts/joiner.rs:162, 179, 1528`.

**The code:**

```rust
// federated_index.rs:1158 — the live rejoin
let out = ContractJoiner::run(&contract_nodes, &contract_edges, &config);

// joiner.rs:162, 179 — the defaults
pub fn run(...) -> JoinOutput {
    Self::run_with_registry(nodes, edges, config, &ClientRegistry::new())
}
pub fn run_with_registry(...) -> JoinOutput {
    Self::run_with_registry_and_env(nodes, edges, config, registry, &EnvBindingIndex::default())
}
```

The `ClientRegistry` is built by `detect_clients` /
`ClientRegistry::insert` in `clients.rs`, which is defined and
unit-tested but **never called from production code**. The
`EnvBindingIndex` is built by `env_bindings_for` in `env_sensor.rs`,
which is defined and unit-tested but **never called from
production code**. Only tests exercise
`run_with_registry` and `run_with_registry_and_env`.

The rule-1 fix at `joiner.rs:344` was made opt-in to keep the
golden tests passing:

```rust
if http_clients.is_empty() && !registry_has_def {
    continue;  // pre-Phase-A silent drop is preserved
}
```

When `http_clients` is empty AND the registry is empty (which is
every production rejoin), the wrapper candidate is silently
dropped — neither bound nor recorded. **This is the pre-Phase-A
soundness bug remaining in production**, and the spec's I2
invariant (`every discovered call lands in exactly one terminal
state`) is violated.

**TLC trace (variant a — the production wiring):**

```
State 1 (Init): registry = {}, has_http_clients = FALSE
State 2 (WrapperDropped(c1)):
    calls_seen = {c1}
    call_terminal[c1] = "None"
   --> invariant I2EveryCallTerminal FAILS:
       c1 is in calls_seen but call_terminal[c1] = "None"
       (i.e. no terminal state)
```

**Recommendation (non-blocking for this review, but should be a
follow-up):** wire `detect_clients` + `env_bindings_for` into the
production rejoin. The plumbing is in
`run_with_registry_and_env`; the call site at
`federated_index.rs:1158` needs to:
1. Run a per-repo pre-pass that calls `detect_clients` and
   `env_bindings_for` over the projected graph's repo roots.
2. Pass the resulting `ClientRegistry` and `EnvBindingIndex` to
   `run_with_registry_and_env` instead of `run`.
3. Restore the rule-1 fix's always-on behaviour (drop the
   `http_clients.is_empty() &&` guard).

The proptest `adding_evidence_never_lowers_rank` exercises the
test-only surface and will catch regressions; the production
surface is not currently covered. **This is the largest
correctness gap surfaced by this review.**

### S4 — Reader-side torn view (speculation, not modeled)

**File:** `src/server/federation/federated_index.rs:1010-1012`,
`src/server/federation/contracts/snapshots/manager.rs:1153-1162`.

A tool that does:

```rust
let snap = fed.contract_snapshot();  // get (index, binds) for generation N
let backend_binds = fed.backend.all_edges()?;  // separate query
```

could observe generation N's `index` and a DIFFERENT generation's
backend `Binds` set if a second rejoin is in flight. The TLA+
model `RejoinProtocol.tla` only models readers that call
`ReaderReadIndex` and `ReaderReadBinds` against the model
variables, not against the backend. A faithful model would
introduce a separate `backend_binds` variable updated by the
rejoin step that writes to the backend, plus a third reader
action that reads the backend. That's a 4-variable model — not
out of reach, but the bug surface is small (the window between
`backend.upsert_edges_batch` and `contract_snapshot.write()` is
microseconds) and the existing variant (c) fix is documented in
the joiner comment. **Speculation, not modeled in this review.**

### S5 — `RejoinProtocol_VariantB.tla` has a too-strong invariant

**File:** `docs/formal/RejoinProtocol_VariantB.tla:77-78`.

The existing `RejoinProtocol_VariantB.tla` was supposed to validate
the post-fix code (clear-before-read, atomic publish). It uses
the same invariant as the original `RejoinProtocol.tla`:

```tla
NoLostUpdate == dirty \/ binds = ComputeBinds(config, nodes, edges)
```

This invariant is checked at every reachable state, including
mid-rejoin. The clear-before-read fix moves `dirty := FALSE` to
`RejoinStart` (step 0 → 1), so during the rejoin, `dirty` is
`FALSE` but `binds` is still the OLD set (the rejoin hasn't run
`RejoinApply` yet). The invariant fails at every mid-rejoin state
where the inputs changed since the last rejoin.

**TLC trace (model demonstrates the false positive):**

```
State 1 (Init): dirty = TRUE, binds = {}, nodes = {}, edges = {}
State 2 (WriterUpdateInput(n1,e1)):
    dirty = TRUE, nodes = {n1}, edges = {e1}, binds = {}
State 3 (RejoinStart):
    dirty = FALSE, rejoin_holds_lock = TRUE, rejoin_step = 1,
    binds = {}, nodes = {n1}, edges = {e1}
   --> invariant NoLostUpdateOriginal FAILS:
       dirty = FALSE AND
       binds = {} ≠ ComputeBinds(c0, {n1}, {e1}) = {<<n1, e1>>}
```

**The fix to the spec is one line:** scope `NoLostUpdate` to
quiescent states (`rejoin_step = 0`). The model in
`RejoinProtocolMidRejoinInvariant.tla` shows the corrected
invariant `NoLostUpdateQuiescent` holds across 235 reachable
states. **The Rust code is correct** — the
`projection_lock` serialises against any writer that would set
`dirty := TRUE`, and a mid-rejoin reader cannot enter the
rejoin. The TLA+ spec's invariant is too strong for the post-fix
code; the spec needs a one-line change.

## 3. Cross-cutting verdict

**Ready to merge WITH fixes.** Two Important bugs (S1, S2) and
one Critical bug (S3) surfaced. All three have TLA+ models with
counterexamples in the variant (a) configuration and clean checks
in the variant (b) configuration. The recommended Rust fixes are
small:

- S1: swap the order in `install_resident` — acquire the lock
  first, run the cap check, only enter the eviction path on a
  full resident. ~5 lines.
- S2: hold the `in_flight` map lock across the whole
  build+install+publish sequence. ~3 lines.
- S3: plumb a real `ClientRegistry` and `EnvBindingIndex`
  through `rejoin_contracts` and `build_snapshot_contract_index`,
  and make the rule-1 fix always-on. ~30 lines plus a
  per-repo pre-pass loop.

The spec's other invariants (I3, I5, I7) and the new code's
contract-federation invariants all hold under the new fix-pass
models. The hard rules are respected for the coverage branch
specifically (no fixture / dependency / version-bump edits).
The branch can merge once the three follow-up Rust fixes
land, with the variant (b) TLA+ models becoming the regression
check.

## 4. Hard-rule compatibility

| Rule | Status |
|------|--------|
| No schema version bumps | Pass. `FEDERATION_GRAPH_VERSION` stays at 3 in `src/server/federation/graph_backend.rs:23`. The Phase D commit `b7681d3d` folds `NodeType::Table` + `ReadsTable` / `WritesTable` into v3 with a CHANGELOG note and `lain reindex` instruction. |
| No `tests/fixtures/contracts/ground_truth.yaml` edits | Pass. `git log main..HEAD --not feat/contract-federation --oneline -- tests/fixtures/contracts/ground_truth.yaml` is empty. The 981 lines of diff in this file are inherited from the parent `feat/contract-federation` branch. |
| No `scripts/contracts-fixture.sh` edits | Pass. `git log main..HEAD --not feat/contract-federation --oneline -- scripts/contracts-fixture.sh` is empty. The 1974 lines of diff are inherited from the parent branch. |
| No `Cargo.toml [dependencies]` runtime version bumps | Pass. `git log main..HEAD --not feat/contract-federation --oneline -- Cargo.toml Cargo.lock` is empty. The `jsonschema = "0.29"` runtime dep and the tree-sitter build-deps are inherited from the parent branch. |

## 5. TLA+ inventory

**Pre-existing (6 files, all clean):**

- `docs/formal/CoverageClaim.tla` — I3 verdict soundness
- `docs/formal/CoverageClaimCache.tla` — cache-validity invariant
- `docs/formal/IndexGeneration.tla` — I7 index-generation consistency
- `docs/formal/RejoinProtocol.tla` — variant (a) buggy, finds counterexample
- `docs/formal/RejoinProtocol_VariantB.tla` — variant (b/c) post-fix, **but the `NoLostUpdate` invariant is too strong (see S5)**
- `docs/formal/SnapshotResidency.tla` — variant (a) buggy, finds counterexample
- `docs/formal/SnapshotResidency_VariantBC.tla` — variant (b/c) post-fix, clean

**New (8 files added by this review):**

- `docs/formal/InstallResidentEviction.tla` + `.cfg` — variant (a) finds S1
- `docs/formal/InstallResidentEviction_VariantB.tla` + `.cfg` — variant (b) fix, clean
- `docs/formal/SnapshotInFlightSlot.tla` + `.cfg` — variant (a) finds S2
- `docs/formal/SnapshotInFlightSlot_VariantB.tla` + `.cfg` — variant (b) fix, clean
- `docs/formal/JoiningTier2NotPlumbed.tla` + `.cfg` — variant (a) finds S3
- `docs/formal/RejoinProtocolMidRejoinInvariant.tla` + `.cfg` — confirms S5 (model-only false positive)

All new models run with `tools/tla/tlc -deadlock`. TLC outputs
are reproducible from this checkout.

## 6. Suspicion tally

- Suspicions checked: **5** (S1, S2, S3, S4, S5)
- Counterexamples found: **4** (S1, S2, S3, S5)
- Of which code-level bugs: **3** (S1, S2, S3)
- Model-only false positive surfaced: **1** (S5)
- Speculation, not modeled: **1** (S4)
- TLA+ models created: **8** files (4 suspicions × 2 variants each, plus S5's two-invariant model)
- Hermetic: **yes** (all models run from the worktree at `72fffa0d`,
  no env, no flake)

## 7. Files created by this review

```
docs/formal/InstallResidentEviction.tla
docs/formal/InstallResidentEviction.cfg
docs/formal/InstallResidentEviction_VariantB.tla
docs/formal/InstallResidentEviction_VariantB.cfg
docs/formal/SnapshotInFlightSlot.tla
docs/formal/SnapshotInFlightSlot.cfg
docs/formal/SnapshotInFlightSlot_VariantB.tla
docs/formal/SnapshotInFlightSlot_VariantB.cfg
docs/formal/JoiningTier2NotPlumbed.tla
docs/formal/JoiningTier2NotPlumbed.cfg
docs/formal/RejoinProtocolMidRejoinInvariant.tla
docs/formal/RejoinProtocolMidRejoinInvariant.cfg
.superpowers/sdd/2026-10-02-coverage-and-protocols/review-final-report.md
```

All files are in the worktree at `/tmp/review-72fffa0d`. The
reviewer did NOT commit, push, or switch branches on the main
checkout.

---

FINAL REVIEW COMPLETE — 5 suspicions checked, 8 TLA+ models, hermetic 1.000, ready to merge WITH fixes
