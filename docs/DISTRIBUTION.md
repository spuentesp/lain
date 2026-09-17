# Distribution checklist

Pre-release checklist for the npm-published `@spuentesp/lain-mcp`
package. Run this in order at the moment you cut a release tag;
each step is a thing that has gone wrong on the live `latest`
dist-tag at some point (see `docs/AGENT_UX_ROADMAP.md` for the
2026-09-16 "Regressed in production" finding — the package on npm
that day was the pre-`609f8db` launcher, and there was no
`SHA256SUMS` asset on the corresponding GitHub release).

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
- [ ] **Tag is stable, not pre-release.** Pre-release tags
      (`v0.x.y-rcN`) route `npm publish` to `--tag next`, which
      leaves `latest` pinned at the previous stable. A user who
      runs `npm install @spuentesp/lain-mcp` without specifying a
      tag never sees an rc. `release.yml` lines 538-541 detect the
      suffix and route accordingly; verify by reading the diff in
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
      "Regressed in production" finding.

## After the release lands

- [ ] **`npx @spuentesp/lain-mcp@<tag> --version`** on a clean
      machine prints `lain <version>`. This is what
      `runtime.js::verifyBinary` already does; the smoke is a
      human-facing sanity check that the published launcher actually
      ran end to end (download → extract → chmod → exec).
- [ ] **Mark the live finding resolved in
      `docs/AGENT_UX_ROADMAP.md`.** Move the M1 row from "🔴
      Regressed in production" to "✅ Done" with the new tag in
      the evidence column, and delete the callout block below the
      table. Until then, the table reflects the published state,
      not the in-tree code.

## When things go wrong

`release.yml::publish-npm-retry` is the documented escape hatch
for an npm publish that fails after the binaries are already on
GitHub Releases. Trigger it via
`gh workflow run release.yml -f tag=v0.x.y`; it re-runs only the
npm publish job, leaving the build artifacts untouched. The
escape hatch exists because `@spuentesp/lain-mcp` has been stuck
at 0.6.1 on the public registry since v0.6.2 — exactly the
incident this checklist is designed to prevent recurring.
