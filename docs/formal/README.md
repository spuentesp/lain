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

Each spec is small enough for exhaustive TLC; model-checks run on every
CI lane when a file in `docs/formal/` changes.

The two new specs (`RejoinProtocol.tla`, `SnapshotResidency.tla`)
intentionally model the **current** implementation at fine
granularity and use TLC to find counterexamples that confirm
suspected races. They do not pre-emptively fix anything. The
follow-up fix pass writes the Rust changes (variant (b)/(c) in
the spec language) and re-runs TLC with the same model, this
time expecting no counterexamples — at which point the spec
becomes the regression check.

## Property tests (Rust)

`I2`, `I4`, `I5`, `I6` are pinned by proptest cases in
`tests/contracts_properties/` (added per phase).
