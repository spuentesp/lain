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
| Known test flakes | None accepted | The battery passed on the release tree (see Verification). |
| Uncommitted WIP | Not in this release | The main checkout's WIP (AGENTS.md, CONTRIBUTING.md, docs/BRANCHING.md, `.github/workflows/ci.yml`) is a dev-review policy change. It is preserved on `release/v0.13.0-wip-backup` and goes in its own PR, not in this release. |

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
