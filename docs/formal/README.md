# TLA+ specs for the contract-federation invariants

This directory hosts TLA+ models that back the invariants in
`docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md`.

## Tooling

TLC (the model checker) is at `tools/tla/tlc` — a thin wrapper around
`tla2tools.jar` (downloaded from `tlaplus/tlaplus` v1.8.0). Java 25+ required.

```
./tools/tla/tlc docs/formal/CoverageClaim.tla
./tools/tla/tlc docs/formal/IndexGeneration.tla
./tools/tla/tlc -deadlock docs/formal/RejoinProtocol.tla
./tools/tla/tlc -deadlock docs/formal/SnapshotResidency.tla
```

## Specs

| Spec | Invariant(s) | Notes |
|------|--------------|-------|
| `CoverageClaim.tla` | I3 (verdict soundness) | 2–3 repos; exhaustively model-checks |
| `IndexGeneration.tla` | I7 (index generation consistency) + liveness | RwLock + dirty flag + snapshot swap |
| `RejoinProtocol.tla` | convergence / no-lost-update / I7 (§9.1) | Targets `federated_index.rs` `rejoin_contracts_if_dirty` at fine granularity. **Variant (a) — current code — is expected to surface counterexamples** for at least the no-lost-update and I7 invariants; the follow-up fix pass uses variant (b) clear-before-read or variant (c) epoch-stamped atomic publish. |
| `SnapshotResidency.tla` | no-eviction-of-held / cap-bound / single-flight (§9.2) | Targets `snapshots/manager.rs` `from_snapshot_with_wait_ms` / `install_resident` / `try_evict_one_lru_unheld` / `HoldGuard`. **Variant (a) — current code — is expected to surface counterexamples** for all three invariants; the follow-up fix pass uses variant (b) `held: AtomicUsize` or variant (c) single-flight builder per id. |

Each spec is small enough for exhaustive TLC. **TLC does not run in CI**
(an earlier version of this file said it did; it never did). Run
`make formal` (`scripts/check-formal.sh`) locally: it checks every row of
[`MANIFEST`](MANIFEST) against its expected outcome, `pass` or
`fail` (a documented counterexample).

The two new specs (`RejoinProtocol.tla`, `SnapshotResidency.tla`)
intentionally model the **current** implementation at fine
granularity and use TLC to find counterexamples that confirm
suspected races. They do not pre-emptively fix anything. The
follow-up fix pass writes the Rust changes (variant (b)/(c) in
the spec language) and re-runs TLC with the same model, this
time expecting no counterexamples — at which point the spec
becomes the regression check.

## Specs added with the verification suite

| Spec | Models | Outcome |
|------|--------|---------|
| `ReadinessLifecycle.tla` | `ReadinessHandle` driven by overlapping indexing passes | pre-fix: a late `ready()` opens the gate over a mutating pass. Fixed by `PassGuard` + terminal cancel. |
| `ReloadBus.tla` | reload signal path (producers, bounded broadcast, one rebuild loop) | pre-fix: subscribing inside `spawn` loses a request. Fixed: subscribe first. |
| `SnapshotInstallHold.tla` | `install_resident` → `HoldGuard::new` window | pre-fix: entry evictable between insert and hold. Fixed: `install_resident_held`. |
| `FsLeaseGuard.tla` | `presence_lock::try_lock` acquire | pre-fix: scan-then-create, all acquirers win. Fixed: `O_EXCL` guard. Residual (`_Stall`): a guard holder frozen > `GUARD_TTL`. |
| `JobRegistry.tla` | background-job registry (cap, task panic, persist/restore) | three independent pre-fix defects (cap race, panic leaks a slot, orphaned `Running` after restart), each switchable; fixed in `job_store.rs`. |
| `FsLease.tla` | alternative: fixed-path `O_EXCL` lease with rename-based steal/release | Exact without expiry; with expiry a 10-step residual remains (needs fencing tokens). Kept to document why it was not adopted: a non-owner's rename-and-verify release can move a stranger's live lock. |

## Beyond TLA+: other deterministic tooling

| Tool | Command | What it checks |
|------|---------|----------------|
| proptest state machines | `make proptest` | `ReadinessHandle`, `ReloadBus` status, `OccupancyMap` (refines the claim spec; found the file-level-Read/symbol-Edit bug), annotation list contract, glob matcher vs. its recursive definition |
| loom | `make loom` | every interleaving of the *real* readiness/reload/`DirtyFlag`/snapshot-residency code. Uses `--cfg lain_loom` (not `loom`: tokio reacts to that name). Production types come from `src/sync.rs`. Negative controls prove each model catches the bug it targets. |
| Kani | `make kani` | readiness publication rule, `constant_time_eq` ≡ `==`, civil-date arithmetic ranges |
| Miri | `make miri` | the raw-pointer thread-local in `sensors/util.rs` |
| cargo-mutants | `make mutants` | whether the tests above actually detect injected bugs |

None of these run in CI yet.

## Property tests (Rust)

`I2`, `I4`, `I5`, `I6` are pinned by proptest cases in
`tests/contracts_properties/` (added per phase).
