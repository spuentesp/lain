# TLA+ specs for the contract-federation invariants

This directory hosts TLA+ models that back the invariants in
`docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md`.

## Tooling

TLC (the model checker) is at `tools/tla/tlc` — a thin wrapper around
`tla2tools.jar` (downloaded from `tlaplus/tlaplus` v1.8.0). Java 25+ required.

```
./tools/tla/tlc docs/formal/CoverageClaim.tla
./tools/tla/tlc docs/formal/IndexGeneration.tla
```

## Specs

| Spec | Invariant(s) | Notes |
|------|--------------|-------|
| `CoverageClaim.tla` | I3 (verdict soundness) | 2–3 repos; exhaustively model-checks |
| `IndexGeneration.tla` | I7 (index generation consistency) + liveness | RwLock + dirty flag + snapshot swap |

Each spec is small enough for exhaustive TLC; model-checks run on every
CI lane when a file in `docs/formal/` changes.

## Property tests (Rust)

`I2`, `I4`, `I5`, `I6` are pinned by proptest cases in
`tests/contracts_properties/` (added per phase).
