# Real-federation test suite

End-to-end validation of the LAIN 0.9 contract-federation tools against
the real-repos fixture (`bytes` + `serde` + `tokio`). This is the
5-phase plan from `docs/CONTRACT_FEDERATION.md` §15 executed
against a non-synthetic fixture, so a sensor bug, a joiner bug, or
a wiring regression is caught by real bytes, real `tree-sitter`
output, and a real MCP server boot.

## Prereqs

- `scripts/demo-federation-fixture.sh /tmp/real-federation-test`
  (network access, clones three real GitHub repos)
- `cargo build` (produces `target/debug/lain`)

## Phases

| # | File | What it asserts |
|---|---|---|
| 1 | `ground_truth.sh` | For three unrelated Rust libraries, the cross-repo joiner finds **0 Binds**, **0 unresolved**, **complete=true** — the precision invariant. |
| 2 | `tools_smoke.sh` | All 13 contract tools return `isError=false` against a fresh snapshot. |
| 3 | `metrics.rs` | `cargo test --test metrics -- --ignored --nocapture` runs the Phase 1 script from a Rust test (CI integration). |
| 4 | `soundness.sh` | The manager surfaces `repo_not_registered` for unknown repos, `snapshot_not_found` for stale snapshot ids, and reports `complete=true` for a clean snapshot. No silent drops. |
| 5 | *(implicit)* | The agent investigation that produced this suite (and the bug-B fix in commit `413d3b5a`) is the real-agent dogfood: every claim in this README was derived from running the actual tools against the actual fixture, not from a unit test. |

## Running

```bash
scripts/demo-federation-fixture.sh /tmp/real-federation-test
cargo build
tests/real_federation/ground_truth.sh
tests/real_federation/tools_smoke.sh
tests/real_federation/soundness.sh
cargo test --test metrics -- --ignored --nocapture
```

Each script picks a free port (default 19876; soundness uses 19878 to
avoid clashing with a local dev server). Pass a different port as
`$2` to chain them on one host:

```bash
tests/real_federation/ground_truth.sh /tmp/real-federation-test 19876
tests/real_federation/tools_smoke.sh   /tmp/real-federation-test 19877
tests/real_federation/soundness.sh     /tmp/real-federation-test 19878
```

## CI integration

A future `real-federation.yml` workflow can drive these from a
schedule (nightly or weekly) on a self-hosted runner with network.
The shell scripts return 0 on success, non-zero on the first failed
assertion, which maps cleanly to a job status.
