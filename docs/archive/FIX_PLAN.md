# Fix plan — `fix/dogfooding-fixes`

Branch: `fix/dogfooding-fixes` (cut from `dev` @ `3105a665`)

## Triage

| # | Bug | Class | Why |
|---|---|---|---|
| B1 | per-call `oneshot` reindex | architectural | needs new state (cache / snapshot pinning) → TLA+ spec |
| B2 | `binary_lives_inside` symlink | mechanical | pure function + tests |
| B3 | zero `Calls` edges | indexer bug | investigate; if it's an LSP phase bug, fix it; if a config bug, fix it |
| B4 | 198 files no call edges | mechanical | `find_dead_code` already surfaces it; add to `get_health` |
| B5 | truncated `graph.bin` recovery | architectural | WAL → new state machine → TLA+ spec |
| B6 | 60s default timeout | mechanical | bump default to 600s; document |
| B7 | `head -N` pipe kills indexer | mechanical | add `RUST_LOG` warning when stderr is not a tty; document |
| B8 | test fixtures top anchors | indexer | filter `scripts/**` and `tests/**` from `find_anchors` default; add flag |
| B9 | `get_coupling_radar` arg name | mechanical | docs + lint script (already in `scripts/`) |
| B10 | `events.jsonl` not queryable | mechanical | promote `get_audit_log` from `social` to `core` (advertise it by default) |
| B11 | `describe_schema` advertises `Calls` not in `get_health` | mechanical | add `Calls: N` to edge-counts |

## TLA+ judgment

The project already models concurrency / invariants in `docs/formal/`:
- `IndexGeneration.tla` — I7 (one generation per tool call)
- `CoverageClaim.tla` — I3 (verdict soundness)
- `RejoinProtocol.tla`, `InstallResidentEviction.tla`, `SnapshotResidency.tla`, `SnapshotInFlightSlot.tla` — concurrency

**Lean/Dafny:** not used in the project. Don't introduce them for these bugs.
**TLA+:** warranted for the two architectural fixes only:
- **B1** — if the fix is "a snapshot cache shared across calls", we get a generation-consistency story identical to the federation's. Either extend `IndexGeneration.tla` or add `SingleRepoIndexGeneration.tla` for the single-repo case. (If the simpler fix is "connect to a persistent server", no new spec — the existing per-server index is already consistent.)
- **B5** — a WAL is a state machine with non-trivial invariants: write-ahead order, recovery correctness, partial-write handling, generation swap during recovery. New spec: `GraphWal.tla` with a `.cfg` running through TLC.

**Property tests** in `proptest` for the same invariants. TLA+ is the higher bar.

## This branch (in order)

Mechanical fixes first (smallest, lowest risk), then the indexer bug
that hides the real graph from the user.

1. **B11** — add `Calls: N` to `get_health` edge counts.
2. **B9** — fix the docs (one-line change + `scripts/check-*.py`).
3. **B6** — bump `LAIN_ONESHOT_TIMEOUT` default to 600s; document.
4. **B7** — `RUST_LOG=warn` warning when stderr is not a tty; document the footgun in the Quickstart.
5. **B10** — promote `get_audit_log` to the `core` package.
6. **B4** — add a `coverage.call_graph` field to `get_health` (B11 already sets this up; B4 surfaces it on the wrong path).
7. **B2** — fix the symlink check in `find_git_workspace_root`.
8. **B8** — filter `scripts/**` and `tests/**` from `find_anchors` default; add `--include-tests` flag.
9. **B3** — investigate why `Calls` is 0 with rust-analyzer installed; fix.

Each as a separate commit. CI must stay green.

## Deferred (separate branches)

- **B1** — `feat/oneshot-snapshot-cache` (or `feat/persistent-server-default`)
- **B5** — `feat/graph-wal` (TLA+ spec + impl + property tests)

Each with its own TLA+ spec in `docs/formal/` if it earns one.

## Verification

Each commit must:
- pass `cargo build`
- pass `cargo test` (the existing test suite)
- pass the relevant `scripts/check-*.py` lint scripts
- include a CHANGELOG entry under "Unreleased"
- include a regression test where the bug is non-trivial (B2, B3, B8, B11)
