# Release audit — v0.15.0 (zero-pending record)

Date: 2026-11-10. Base: `dev` at `b7fd2935` (includes #400–#403). Release
branch: `release/v0.15.0`. Scope: everything merged to `dev` since
`v0.14.0`, which includes the answer-scope work, the startup cache, the
Rust gRPC contracts, the ORM relations, and the Parallax requirements
report.

## Pending inventory

| Item | State | Why it is out of this release |
|---|---|---|
| Issue #328 (root snapshots need a nonempty repos manifest) | Deferred in writing | Its `SnapshotManifest` design is not in `dev`. #356 closes the symptom another way: a root snapshot with no `repos` pins every configured repo at HEAD. The manifest and its `origin` field go to a later release. |
| Acceptance findings register, open items (`docs/acceptance/FINDINGS-2026-10-11.md`) | Scoped out | Runtime-only coupling, scale beyond the tested band, and the README reframe. These are documented limits, not defects in this release. |
| Dependabot PRs #391–#395, #398 | Deferred in writing | Major-version and toolchain bumps (`tokio` 1.53→1.54, `lru` 0.12→0.19, `notify` 6→8, `tree-sitter-scala`, `tokio-util`, GitHub Actions). Each needs its own change and test pass; none is a release blocker. |
| Known test flakes | One, recorded | `mcp_list_claims_adapts_to_lain_claims` (`tests/coordination_proxy.rs`) failed once on the CI run of `main` at `4f9c28d2` (panic `left: 0, right: 1`, assertion at line 318). The rerun passed on the same commit; local repro was 0 failures in 40 sequential runs and 0 in 40 runs at 8-way concurrency. Cause not identified. The fake `zoanthrope` is an MCP stdio stub with a 10 s deadline in `CoordinationProxy::mcp_call`; the suspected cause is a timing race on a loaded runner. It does not change this release's code. |
| Uncommitted WIP | Preserved, out of the release tree | Two worktrees carry uncommitted changes: `/data/agents/orca/lain` (`CHANGELOG.md`, `src/server/lsp.rs` on `feat/nat-http-contract`) and the `main` worktree (`Cargo.toml`, disabling `ort` on macOS). Neither is part of the `release/v0.15.0` tree, which is cut from `dev`. The `ort` change is the macOS linker workaround dropped in v0.14.0; `dev` keeps `ort` enabled, and `src/server/nlp.rs` imports it, so committing it as-is would break the build. |
| macOS ARM64 release target | Out of scope for this release, documented | `release.yml` builds Linux and Windows only; the macOS ARM64 target was dropped in v0.14.0 for an unresolvable ONNX Runtime linker error and has not returned. This release makes the published metadata match what actually ships (see below) rather than re-adding a broken target. |

## Defects found and fixed in this release

Two distribution defects were found while preparing v0.15.0. Both are
release-quality problems in what v0.14.0 published, and both are fixed
here.

### 1. `Formula/lain.rb` shipped checksums that did not match the artifacts

The v0.14.0 Formula pinned the sha256 values of the **v0.13.0** assets
(`901a393c…` for Linux, `16687aaa…` for Windows) while pointing its URLs
at the **v0.14.0** assets. Verified against the published release: the
v0.14.0 Linux tarball hashes to `1f3129e3f5a3a0075416291429da1f7c5015fa50e97dd5aee2a860dcc3c5923d`,
and the Windows tarball to `692cda2e43b4c567115556bbfcf7cd90b1dd8ef578e25fb53e350c05558493f5`.
`brew install lain` therefore failed checksum verification.

Root cause: the post-publish fill step (AGENTS.md step 6) was skipped for
v0.14.0. The v0.13.0 release had a working fill because
`post-release/v0.13.0` applied the published hashes; the equivalent
v0.14.0 sync branch only bumped the version and URLs.

Fix: `scripts/fill-formula-sha256.py` fills the Formula's `sha256` values
from a published release's `SHA256SUMS` (`--check` verifies without
writing), and `scripts/test_fill_formula_sha256.py` pins the behaviour in
CI. `scripts/post-flight-check.sh` now calls the script instead of
printing instructions a human must copy by hand, and
`docs/DISTRIBUTION.md` lists the fill in its "After the release lands"
checklist.

### 2. The health-badge default pointed at a stale release, failing CI on `main`

`.github/actions/lain-health-badge/action.yml` pinned `v0.13.0` while the
latest published release was `v0.14.0`. The `version-drift` job compares
the two, so every push to `main` failed — the last CI run on `main`
(`80889c07`) has exactly one failing job, `lain-version default tracks
latest release`.

Fix: the default moves to `v0.14.0` in this release PR. Per AGENTS.md
step 2 it must name a release that **exists** at merge time; `v0.15.0` is
not published yet, and pointing at it would 404 and deadlock the release
gate. It moves to `v0.15.0` in the post-publish commit, alongside the
Formula checksum fill.

### 3. Published metadata advertised a macOS binary that is never built

`Formula/lain.rb` carried an `on_macos` stanza pointing at a v0.14.0
aarch64 asset that was never published (`release.yml` has no macOS build
job), and `server.json` listed a `macos-aarch64` binary. Both now match
what the release actually builds. The same correction applies to
`scripts/post-flight-check.sh` (which failed every release while
expecting the macOS tarball) and the `docs/DISTRIBUTION.md` post-tag
checklist.

## Verification (on the exact release tree)

- `cargo test --workspace --no-fail-fast`: see the PR comment for the
  count on the release commit.
- `scripts/demo.sh`: see the PR comment.
- `scripts/check-release-version.py --tag v0.15.0`: OK.
- `scripts/test_fill_formula_sha256.py`: 6 passed.
- `python3 scripts/fill-formula-sha256.py --tag v0.15.0 --check`: fails
  with the placeholder hashes, as it must before the post-publish fill.

## Process

Version bumped to `0.15.0` in `Cargo.toml`, `Cargo.lock`, `server.json`,
`npm-shim/package.json` and `Formula/lain.rb`. The health-badge default
stays on `v0.14.0` until 0.15.0 is published. The Formula `sha256`
values are placeholders until then and are filled from the published
`SHA256SUMS` in the post-publish commit.

Federation schema: `FEDERATION_GRAPH_VERSION` advances from 4 to 5 (ORM
model/table/column relations). The loader refuses a graph written under
a different header with `LainError::FederationSchemaMismatch`; the
operator's recovery path is `lain reindex`, which backs up the old graph
and rebuilds it. The bump is named in the CHANGELOG entry for this
release, as AGENTS.md requires.
