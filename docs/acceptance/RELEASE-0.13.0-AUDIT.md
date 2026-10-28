# Release audit — v0.13.0 (zero-pending record)

Date: 2026-10-27. Base: `dev` at `9c12422a` (includes #364). Release branch:
`release/v0.13.0`. Scope: everything merged to `dev` since `v0.12.1`, which
includes the bus sensors (#354) and the fixes and features of #355–#364.

## Pending inventory

| Item | State | Why it is out of this release |
|---|---|---|
| Issue #328 (root snapshots need a nonempty repos manifest) | Deferred in writing | Proposes a `SnapshotManifest` and rejects root snapshots with no repos. Its design is not in `dev`. #356 closes the symptom another way: a root snapshot with no `repos` pins every configured repo at HEAD. The manifest and its `origin` field go to a later release. |
| PR #344 (service references, environment scope, `list_service_refs`) | Deferred in writing | Rebased onto `dev` with a conflict fix and a documentation fix, but its live CI still had failing checks at the last run and it was not re-verified. It is the input for the service-discovery work in Parallax, so it goes first in the next release. |
| PR #353 (post-release v0.12.1: formula checksums, badge default) | Deferred to post-release | AGENTS.md step 6: fill the formula `sha256` and bump the badge default after this release's assets are published. It cannot land before this release. |
| Dependabot PRs #305, #332–#343 | Deferred in writing | Major-version and toolchain bumps (`jsonschema` 0.29→0.60, `ort` rc.13→rc.15, `inventory` 0.2→0.3, `tree-sitter-swift`, GitHub Actions). Each needs its own change and test pass. |
| Acceptance findings register, open items (`docs/acceptance/FINDINGS-2026-10-11.md`) | Scoped out | Runtime-only coupling, scale beyond the tested band, and the README reframe. These are documented limits, not defects in this release. |
| Known test flakes | One, recorded | `mcp_list_claims_adapts_to_lain_claims` (`tests/coordination_proxy.rs`) failed once on the CI run of `main` at `4f9c28d2` (job `Cargo Test (ubuntu-latest)`, panic `left: 0, right: 1`, assertion at line 318). The rerun of the failed job passed on the same commit. Local repro: 0 failures in 40 sequential runs and 0 in 40 runs at 8-way concurrency. Cause not identified. The fake `zoanthrope` is an MCP stdio stub with a 10 s deadline in `CoordinationProxy::mcp_call`; the suspected cause is a timing race on a loaded runner. Tracked as a follow-up; it does not change this release's code. |
| Uncommitted WIP | Included | The dev-review policy (AGENTS.md, CONTRIBUTING.md, docs/BRANCHING.md) and the CI change that runs the capability job on main only are part of this release. The live `dev` protection still requires one approval; changing that setting is tracked separately. The WIP is preserved on `release/v0.13.0-wip-backup`. |

## Verification (on the exact release tree)

- `cargo test --workspace --no-fail-fast`: 3662 passed, 0 failed, 8 ignored.
- `scripts/demo.sh`: 165 passed, 2 skipped, 0 failed (exit 0).
- `scripts/check-release-version.py --tag v0.13.0`: OK.

## Process

Version bumped to `0.13.0` in `Cargo.toml`, `Cargo.lock`, `server.json`,
`npm-shim/package.json` and `Formula/lain.rb`. The health-badge default stays on
`v0.12.1` until 0.13.0 is published. Formula `sha256` values are filled after
publish. No federation schema bump: `FEDERATION_GRAPH_VERSION` stays at 4. The
index build stamp is a sidecar file, so a graph written by another build
rebuilds once on its own.
