# Five-cycle implementation and review plan

**All five implementation/review cycles are complete.**

Baseline: `8d5a6fc`; findings and evidence: [implementation audit](audit_2026_09_11.md).

The user requested documentation, a plan, fixes, and review, iterated five times. Each numbered cycle below includes implementation, relevant validation, a review of the resulting diff and adjacent behavior, and corrections prompted by that review. Changes stay local; publishing a release is outside this implementation task.

| Cycle | Implementation | Review and acceptance |
| --- | --- | --- |
| 1 | Repair Windows binary-path construction and the no-LSP namespace regression (findings 1, 6). | Exercise the previously skipped branch deterministically; review platform-specific bindings and regression assertions. |
| 2 | Serialize single-repo overlay reconciliation and order subscriber mutations correctly (findings 2, 3). | Repeated refresh, replacement/deletion, and concurrent sync must preserve owner/subscriber state and owned IDs. |
| 3 | Prune empty repositories and retract removed federation members (findings 4, 5). | Verify disk persistence, remaining repositories, edges, overlay ownership, and watcher lifecycle; fix related regressions. |
| 4 | Honor badge threshold/language configuration and install the requested binary version (findings 7, 8). | Mock external dependencies; cover empty language input, a custom threshold, and a requested version differing from latest. |
| 5 | Prevent release version drift, strengthen CI coverage, and review all changes together. | Run full Rust tests, formatting, CI Clippy, schema comparison, shell/JS checks and capability checks. Record platform/model limitations honestly. |

## Execution log

### Cycle 1

Status: complete.

Cycle 1 review: replaced the Windows-only mutation with `EXE_SUFFIX`; removed the invalid Arc mutation; exposed the existing per-multiplexer unavailable marker so integration fixtures can force fallback without global environment changes. Review found a second early-returning fallback test and made it deterministic as well. Matching-ID and fallback tests pass with the installed LSP still on PATH. Native Windows execution is unavailable on this Linux host.

### Cycle 2

Status: complete. Both public overlay entry points acquire the same async lock; internal reconciliation avoids recursive locking. Removals precede replacement inserts. Review also found that direct file changes retained renamed/empty/deleted symbols; the shared replacement path now retracts those entries.

Cycle 2 validation/review: all 18 watcher tests and the private lock-acquisition test pass. Both public entry points now wait for a held guard. Repeated refresh, rename, empty file, deletion and overlapping reconciliation are covered. The review corrected direct-update stale IDs as part of the same replacement mechanism.

### Cycle 3

Implementation: successful empty tracked-file results now prune the graph. Review caught an additional persistence omission in the single-repo deletion-only early return; it now saves before returning. Repo removal retracts projected nodes and incident edges, deactivates overlay publication, and stops the watcher. A weak watcher reference avoids retaining removed repos indefinitely. Membership changes serialize with projection, and queued external edges cannot recreate removed target repositories.

Validation: 115 federation unit tests passed (one pre-existing ignored test), including removal, retained peer state, on-disk backend state, delayed external edges, overlay ownership and watcher deactivation. The two-pipeline deletion regression also checks reopening the saved graph.

### Cycle 4

Implemented explicit composite input mapping and a direct, exact-version release download with an executable version check before installation. Empty language input now disables installation. Review found two adjacent contract failures: comma-separated language names were sent as one name, and failed MCP calls could produce a success badge. Languages now split correctly, and transport/error/empty responses fail health computation explicitly. Thresholds must be valid non-negative integers.

Validation: five offline tests execute the production scripts against mocked downloads and MCP calls, covering exact pinning, version mismatch rejection, custom threshold, empty/comma-separated languages, and MCP failure. Existing extraction tests still pass.

### Cycle 5

Status: complete. Add version/tag/binary gates and CI coverage, then review the complete diff and run the project checks.

Cycle 5 implementation/review: release jobs now reject tag/metadata mismatch before building and reject binary-version mismatch before publishing. The Rust consistency test now compares against `CARGO_PKG_VERSION`, covering the original Cargo-only regression. CI runs action/installer/release contract checks and the previously omitted JS style/recorder and npm launcher tests. Final review also wired the supplied publishing token into both badge publication steps and removed the invalid action-level permissions block (permissions remain the caller workflow's responsibility).

The final review checked mutex lifetimes and ordering, recursive-lock avoidance, subscriber event ordering, empty/deleted file persistence, removal versus projection, in-flight overlay publication, remaining-repository isolation, deterministic fallback tests, and version/input contracts. No further blocking findings were identified within this scope.

## Final finding disposition

| Original finding | Disposition and regression evidence |
| --- | --- |
| 1: Windows immutable binary path | Fixed with platform `EXE_SUFFIX`; binary-version test passes locally. Native Windows CI still needs to execute on the changed checkout. |
| 2: Sidecar loses replacement nodes | Fixed; repeated refresh, renamed function and empty file preserve owner/subscriber equality. |
| 3: Unpolled mutex future | Fixed; both entry points block behind a held guard; overlapping updates retract old IDs. |
| 4: Empty repository ghosts | Fixed in both pipelines, including persisted state after reopening. |
| 5: Removed repository remains queryable | Fixed; backend nodes/edges and owned overlay entries are removed, watchers stop, queued edges and in-flight scans cannot republish removed nodes. |
| 6: No-LSP namespace test panic | Fixed; both fallback regressions now run even when rust-analyzer is installed. |
| 7: Ignored badge inputs | Fixed; custom threshold, disabled LSP installation and comma-separated languages are exercised against production scripts. |
| 8: Ineffective version pin | Fixed; requested asset is downloaded directly and version-checked; mismatched binary is rejected before installation. |

Additional fixes from the reviews: direct overlay rename/empty/delete cleanup; single-repo deletion-only persistence; weak watcher ownership and deactivation; queued-edge resurrection prevention; error responses cannot produce success badges; release drift prevention; explicit publication-token forwarding.

## Final validation

- Full `cargo test --workspace`: **1,271 passed, 0 failed, 2 ignored**, 51 result blocks. Doc tests contain zero tests.
- The subsequently strengthened in-flight deactivation regression also passed separately.
- CI-equivalent Clippy and formatting checks passed; unrestricted Clippy is not claimed.
- Module resolution passed; no Rust module files were added.
- Schema dump matches the committed snapshot.
- Rebuilt-binary capability suite: **111 passed, 2 skipped** (model-dependent checks).
- Release guards: four regression tests passed; current tag/metadata/binary check passed.
- Badge contracts: five offline tests passed; eleven extraction checks passed.
- JavaScript tests: 55 passed. npm installer/launcher tests: 30 + 10 passed.
- Shell installer checks: 13 passed. Action scripts passed Bash syntax checks; workflow/action files parsed as YAML.

Native Windows/macOS execution, browser E2E and model-backed NLP checks were not available/run here. The changed Windows path uses a platform-provided constant and is covered by the existing three-OS CI matrix; that is not a substitute for reporting an actual native run. No release, push, or external publication was performed. The implementation remains in the working tree.

The synchronization changes deliberately serialize complete overlay reconciliations and backend projection/membership writes. This trades concurrent write throughput for consistent ownership and removal behavior; large-repository performance was not benchmarked in this task.

Closeout on 2026-09-12: after temporary build artifacts and logs were cleared from the environment, the binary was rebuilt and the pending capability check completed successfully. CI-equivalent Clippy, formatting, all nine Python contract tests, tag/metadata/binary consistency, and whitespace checks were also reconfirmed. The 1,271-test Rust result above is from the completed earlier run; the full suite was not unnecessarily repeated during closeout.
