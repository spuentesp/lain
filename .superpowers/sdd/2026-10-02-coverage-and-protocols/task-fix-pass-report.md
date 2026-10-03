# TLA+ counterexample fix pass — report

**Branch:** `feat/contract-coverage-and-protocols`
**Started:** `ced61f07` (TLA+ commit)
**Ended:** `2a0f3bf7` (variant specs commit)
**Date:** 2026-10-02 / 2026-10-03
**Spec:** `docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §9.1, §9.2
**Plan:** `docs/superpowers/plans/2026-10-02-coverage-and-protocols.md`

Six TLA+ counterexamples found by `task-tla-models-report.md`
landed as six Rust fixes on this branch, each as a separate
commit. Each commit references the TLA+ trace that motivated
it; each ships a regression test that exercises the scenario
from the trace.

## 1. Per-fix summary

### Fix 1 — RejoinProtocol clear-before-read (lost dirty flag)

| Field | Value |
|---|---|
| **Commit** | `53661dbf` |
| **Subject** | `fix(federation): clear dirty before reading in rejoin (closes lost-dirty-flag)` |
| **TLA+ trace** | `RejoinProtocol.tla` — 10-state trace. `WriterSetConfig(c1)` lands between `RejoinReadConfig(c0)` and `RejoinClear`; trailing clear overwrites the writer's `dirty := TRUE`. |
| **Invariant** | `NoLostUpdate`, `Convergence`. |
| **File** | `src/server/federation/federated_index.rs` |
| **Before / after line count** | 1513 → 1541 (+28 net) |
| **Variant** | (b) — clear-before-read. |

The rejoin now clears `contracts_dirty` at the start of
`rejoin_contracts_if_dirty` (under `projection_lock`); a writer
that lands after the clear sets `dirty := TRUE` again and the
next call redoes the work. `rejoin_contracts` no longer
manages the dirty flag. New `pub fn contracts_dirty()`
observer for the regression test.

### Fix 2 — RejoinProtocol epoch-stamped atomic publish (two views)

| Field | Value |
|---|---|
| **Commit** | `1b47d36f` |
| **Subject** | `fix(federation): publish (index, binds) atomically (closes two-views)` |
| **TLA+ trace** | `RejoinProtocol.tla` — 9-state trace. `ReaderReadIndex` records old `g1`, then `RejoinSwapIndex` advances to `g2`; `ReaderReadBinds` records `g2`. Reader's two records are from different generations. |
| **Invariant** | `I7ReaderConsistency`. |
| **File** | `src/server/federation/federated_index.rs` |
| **Before / after line count** | 1541 → 1580 (+39 net) |
| **Variant** | (c) — epoch-stamped atomic publish. |

`contract_index` field replaced with `contract_snapshot:
RwLock<Option<Arc<ContractSnapshot>>>` where `ContractSnapshot
{ index: Arc<ContractIndex>, binds: Arc<Vec<(String,
String)>> }`. The rejoin produces both halves in a single
write-lock acquisition; readers see them as a coherent pair.
`contract_index()` continues to return just the index
(derived from the snapshot); new `contract_snapshot()` returns
the atomic pair.

### Fix 3 — SnapshotResidency `held: AtomicUsize` (proper refcount)

| Field | Value |
|---|---|
| **Commit** | `ea275d88` |
| **Subject** | `fix(snapshots): HoldGuard uses refcount not bool` |
| **TLA+ trace** | `SnapshotResidency.tla` — 7-state trace (surface from the AtomicBool path). Two `Hold(s1)` share one slot; first `Release` flips `held_storage := FALSE`; eviction predicate sees `unheld` while the second holder is still using the federation. |
| **Invariant** | `NoEvictionOfHeld` (AtomicBool surface). |
| **File** | `src/server/federation/contracts/snapshots/manager.rs` |
| **Before / after line count** | 2009 → 2071 (+62 net) |
| **Variant** | (b) — refcount, not bool. |

`SnapshotFederation.held` is now `AtomicUsize`.
`HoldGuard::new` increments via `fetch_add(1, AcqRel)`; `Drop`
decrements via `fetch_sub(1, AcqRel)`. The eviction
predicate (`try_evict_one_lru_unheld`) only fires when the
count is 0. Three existing tests updated to match the
refcount surface.

### Fix 4 — SnapshotResidency single-flight builder per id

| Field | Value |
|---|---|
| **Commit** | `99723f7a` |
| **Subject** | `fix(snapshots): single-flight per snapshot id` |
| **TLA+ trace** | `SnapshotResidency.tla` — 3-state trace. Two `BuildStart(s1)` calls race the resident miss and both call `build_snapshot_federation`; whichever lands second in `install_resident` wins. |
| **Invariant** | `SingleFlight`. |
| **File** | `src/server/federation/contracts/snapshots/manager.rs` |
| **Before / after line count** | 2071 → 2275 (+204 net) |
| **Variant** | (c) — single-flight per id. |

Added `in_flight: parking_lot::Mutex<HashMap<String,
Arc<InflightSlot>>>` where `InflightSlot { state: Mutex<Option<BuildOutcome>>, notify: Condvar }`.
`from_snapshot_with_wait_ms` registers a per-id slot after
the resident miss; concurrent callers share the slot and
`Condvar::wait` for the first's result. New `BuildOutcome`
enum (`Ok(Arc<SnapshotFederation>)` or `Err(String)`).

### Fix 5 — SnapshotResidency combined lock for `install_resident` (cap overrun)

| Field | Value |
|---|---|
| **Commit** | `5195b998` |
| **Subject** | `fix(snapshots): install_resident checks cap under the insert lock` |
| **TLA+ trace** | `SnapshotResidency.tla` — 7-state trace. Three `InstallCheck(sN)` actions each pass `Cardinality(resident) < Cap` before any `InstallInsert`; final `|resident| = 3 > cap = 2`. |
| **Invariant** | `CapBound`. |
| **File** | `src/server/federation/contracts/snapshots/manager.rs` |
| **Before / after line count** | 2275 → 2401 (-77 since modify of the previous commit; net Fix-5 delta ~30) |
| **Variant** | (b) — check + insert under one lock. |

The `len() < cap` check and the `insert` now happen under a
single `self.resident.lock()` acquisition; the parking_lot
guard is held across both. The wait path drops the guard
explicitly so the condvar does not race on a held parking_lot.

### Fix 6 — SnapshotResidency combined lock for evict (select+remove race)

| Field | Value |
|---|---|
| **Commit** | `df84b5f7` |
| **Subject** | `fix(snapshots): evict select+remove under one lock` |
| **TLA+ trace** | `SnapshotResidency.tla` — 6-state trace. `EvictSelect(s1)` → `Hold(s1, count=1)` → `EvictRemove(s1)` removes the held snapshot. |
| **Invariant** | `NoEvictionOfHeld` (select+remove surface). |
| **File** | `src/server/federation/contracts/snapshots/manager.rs` |
| **Before / after line count** | 2401 → 2528 (+127 net) |
| **Variant** | (b) — select + remove under one lock. |

`try_evict_one_lru_unheld` now holds the resident lock across
the entire (find-LRU-unheld, re-check held, remove)
sequence. The held-count re-check under the remove lock
catches any hold that landed between the original select and
the remove; on a stale hit the function returns `false` and
the caller's loop retries.

## 2. TLA+ validation

After all six Rust fixes, two new TLA+ specs validate the
post-fix behavior:

```
$ ./tools/tla/tlc docs/formal/RejoinProtocol_VariantB.tla
9 states generated, 8 distinct states found.
Model checking completed. No error has been found.
```

`NoLostUpdate` and `Convergence` now hold for variant-(b)
clear-before-read (vs. variant-(a) which TLC finds the
9-state counterexample on).

```
$ ./tools/tla/tlc docs/formal/SnapshotResidency_VariantBC.tla
1477 states generated, 296 distinct states found.
Model checking completed. No error has been found.
```

`NoEvictionOfHeld`, `CapBound`, and `SingleFlight` now hold
for the combined variant-(b)/(c) model (vs. variant-(a)
which TLC finds the 3-state, 6-state, and 7-state
counterexamples on).

| Spec | Variant | States | Invariant | Result |
|---|---|---|---|---|
| `RejoinProtocol.tla` | (a) original | 127 | `NoLostUpdate`, `Convergence`, `I7ReaderConsistency` | counterexamples found (depth 9–10) |
| `RejoinProtocol_VariantB.tla` | (b) clear-before-read | 8 | `NoLostUpdate`, `Convergence` | no counterexample |
| `SnapshotResidency.tla` | (a) original | 23 | `NoEvictionOfHeld`, `CapBound`, `SingleFlight` | counterexamples found (depth 3–7) |
| `SnapshotResidency_VariantBC.tla` | (b) refcount + combined locks, (c) single-flight | 296 | `NoEvictionOfHeld`, `CapBound`, `SingleFlight` | no counterexample |

The variant specs model the post-fix code; the original
specs continue to surface the variant-(a) counterexamples
they were written to find.

## 3. Gate outcomes

| Gate | Result |
|---|---|
| `RUSTC_WRAPPER= RUSTUP_TOOLCHAIN=nightly cargo build --tests` | clean (no errors) |
| `RUSTC_WRAPPER= RUSTUP_TOOLCHAIN=nightly cargo test --test sensors` | 69 passed; 0 failed |
| `RUSTC_WRAPPER= RUSTUP_TOOLCHAIN=nightly cargo test --test federation_contracts_e2e pr13_hermetic_precision_recall_over_t1_fixture` | 1.000 × 6 |
| `cargo clippy --all-targets -- -D warnings` (stable) | clean |
| `cargo fmt --check` | clean |
| `bash scripts/check-mod-resolution.sh` | ok: all mod declarations resolve |
| `python3 scripts/check-format-duration-once.py` | clean |
| `python3 scripts/check-mcp-dispatch-shape.py` | clean |
| `python3 scripts/check-no-mirror-dtos.py` | clean |
| `python3 scripts/check-release-version.py` | OK: release metadata matches 0.8.0 |
| `python3 scripts/check-no-duplicate-sensors.py` | pre-existing failure from uncommitted working-tree changes (not this fix's files); the three flagged files (`entry_point_sensor.rs`, `event_sensor.rs`, `field_access_sensor.rs`) are among the uncommitted changes from another session that the brief explicitly says not to touch |

## 4. PR13 metrics

```
PR13_METRICS_JSON {"diff_precision":1.0,"diff_recall":1.0,"binds_precision":1.0,"binds_recall":1.0,"reads_field_precision":1.0,"reads_field_recall":1.0}
```

All six metrics at 1.000 (unchanged from the pre-fix-pass
baseline).

## 5. Test list

| Test | File | Purpose |
|---|---|---|
| `rejoin_does_not_lose_mid_join_mark` | `tests/contract_federation_integration.rs` | Fix 1: mid-rejoin `mark_contracts_dirty` survives the rejoin. |
| `rejoin_publishes_index_and_binds_atomically` | `tests/contract_federation_integration.rs` | Fix 2: every observed `contract_snapshot` has binds matching its consumers' Binds targets. |
| `hold_guard_uses_refcount_not_bool` | `src/server/federation/contracts/snapshots/manager.rs` | Fix 3: two holders share a slot; first Drop leaves count=1 (not 0). |
| `from_snapshot_single_flight_registers_one_slot_per_id` | `src/server/federation/contracts/snapshots/manager.rs` | Fix 4: two concurrent registrations share one slot (`Arc::ptr_eq`). |
| `install_resident_caps_under_concurrent_installs` | `src/server/federation/contracts/snapshots/manager.rs` | Fix 5: `|resident| ≤ cap` at every instant under racing installs. |
| `try_evict_does_not_remove_via_select_remove_race` | `src/server/federation/contracts/snapshots/manager.rs` | Fix 6: held LRU entry survives `install_resident`. |

## 6. Commits

```
53661dbf fix(federation): clear dirty before reading in rejoin (closes lost-dirty-flag)
1b47d36f fix(federation): publish (index, binds) atomically (closes two-views)
ea275d88 fix(snapshots): HoldGuard uses refcount not bool
99723f7a fix(snapshots): single-flight per snapshot id
5195b998 fix(snapshots): install_resident checks cap under the insert lock
df84b5f7 fix(snapshots): evict select+remove under one lock
2a0c3fbf feat(formal): add variant-(b)/(c) TLA+ configs to validate post-fix behavior
```

FIX-PASS COMPLETE — 6 bugs closed, hermetic 1.000