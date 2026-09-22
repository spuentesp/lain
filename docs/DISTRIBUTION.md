# Distribution checklist

Pre-release checklist for the npm-published `@spuentesp/lain-mcp`
package. Run this in order when cutting a release tag. The checks preserve
lessons from the 2026-09-16 incident, when npm `latest` contained the old
launcher and the matching GitHub release had no `SHA256SUMS` asset.

## Current known issue

As of 2026-09-22, the Windows clean-room install repair is in
tree but not yet released. `release.yml::build-windows` packages
every `*.dll` from `target/x86_64-pc-windows-msvc/release/`
alongside `lain.exe`, and the build fails loudly if `DirectML.dll`
is missing. Release packaging and CI both call
`scripts/package-windows-release.sh`, so the release PR's Windows
job checks the archive contract against a real Cargo build before
the tag exists. `scripts/test_package_windows_release.py` covers
missing-DLL and missing-sidecar failures, while
`npm-shim/scripts/install.test.js` covers the install side. The
actual release that carries the fix is the next release PR — until
the next tag, the published artifact still ships `lain.exe` alone.
See [`FOLLOWUPS.md`](FOLLOWUPS.md).

## What the runtime needs

`npm-shim/scripts/runtime.js` (the rewritten launcher, since
`609f8db`) is the thing that actually fetches and verifies the
binary. It expects every release tarball to be paired with a
`SHA256SUMS` file on the same GitHub Release. Without that file,
`ensureBinary` fails the install and `npx @spuentesp/lain-mcp`
exits non-zero on a clean machine.

## Pre-tag checklist

Run these in order, **before** pushing the `v0.x.y` tag that
triggers `release.yml`.

- [ ] **Code state.** `main` HEAD contains the rewritten launcher
      (`grep ensureBinary npm-shim/bin/lain.js`) and the SHA256SUMS
      aggregator (`.github/workflows/release.yml::sha256sums`).
- [ ] **Windows archive contract.** Run
      `python3 scripts/test_package_windows_release.py`. On the release
      PR, the Windows `test-cross` job must also package the binaries from
      `target/debug` and confirm that `DirectML.dll` is present. The tagged
      build uses the same packaging script against `target/.../release`.
- [ ] **Tag is stable, not pre-release.** Pre-release tags
      (`v0.x.y-rcN`) route `npm publish` to `--tag next`, which
      leaves `latest` pinned at the previous stable. A user who
      runs `npm install @spuentesp/lain-mcp` without specifying a
      tag never sees an rc. `release.yml` detects the suffix and routes
      accordingly; verify by reading the diff in
      `npm-shim/package.json` after `sync-server-json` runs.
- [ ] **Versions are aligned.** Every release-metadata file lists
      the same `v0.x.y`: `Cargo.toml`, `Cargo.lock`, `server.json`,
      `npm-shim/package.json`, `Formula/lain.rb`. Run
      `python3 scripts/check-release-version.py --tag v0.x.y`
      before tagging — the script catches the same drift that
      tripped the 0.7.0 / 0.6.1 incident (a binary whose
      `--version` reported the previous version).
- [ ] **`npm pack --dry-run` includes the launcher.** Run
      `npm pack --dry-run` in `npm-shim/`; assert the file list
      contains `bin/lain.js`, `scripts/runtime.js`, and
      `scripts/install.js`. This is what `scripts/smoke-npm-publish.sh`
      pins automatically.
- [ ] **Launcher body has `ensureBinary`.** A future revert of
      `bin/lain.js` to the pre-`609f8db` stub would silently
      re-break the headline command. The smoke script greps for
      `ensureBinary` in the bundled `bin/lain.js` so a synthetic
      stub fails CI before a release PR lands.

## Post-tag checklist

These run automatically as part of the release workflow
(`.github/workflows/release.yml`), but the human who cuts the tag
should verify each completed step before announcing the release.

- [ ] **`build-{linux,macos-arm64,windows}` green.** Three
      per-target binaries, each with its own SBOM, signed
      provenance, and cosign keyless signature.
- [ ] **`sha256sums` job green.** A single `SHA256SUMS` file
      attached to the release, containing one line per binary.
      `gh release view v0.x.y --json assets` should list
      `SHA256SUMS` alongside the three tarballs.
- [ ] **`publish-server-json` green.** `server.json` on the
      release is regenerated from `docs/METADATA.toml` and the
      release tag; every `"version":` field (top-level AND
      `packages[].version`) is updated in lockstep. This was the
      drift PR #103 caught.
- [ ] **`publish-npm` green.** The package version on the public
      registry matches the tag. For a stable tag, `npm view
      @spuentesp/lain-mcp dist-tags.latest` shows the new version
      after the job completes.
- [ ] **`distribution-acceptance` workflow green.** The 3-OS
      distribution gate (`scripts/clean_room_mcp_check.py`)
      exercised against the freshly-published artifact returns
      "Operational" for `initialize` + `tools/list` +
      `get_capabilities`. This is the canary for the live
      published-package contract.

## After the release lands

- [ ] **`npx @spuentesp/lain-mcp@<tag> --version`** on a clean
      machine prints `lain <version>`. This is what
      `runtime.js::verifyBinary` already does; the smoke is a
      human-facing sanity check that the published launcher actually
      ran end to end (download → extract → chmod → exec).
- [ ] **Update current status.** Refresh `docs/FOLLOWUPS.md` and the milestone
      summary in `docs/AGENT_UX_ROADMAP.md` if the release changes published
      behavior or resolves a distribution defect.

## When things go wrong

`release.yml::publish-npm-retry` is the documented escape hatch
for an npm publish that fails after the binaries are already on
GitHub Releases. Trigger it via
`gh workflow run release.yml -f tag=v0.x.y`; it re-runs only the
npm publish job, leaving the build artifacts untouched. The
escape hatch exists because an earlier release published GitHub assets but
left npm `latest` behind. It repairs npm publication without rebuilding or
replacing already-published binaries.
