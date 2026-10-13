# Release audit — v0.10.0 (zero-pending record)

**Date:** 2026-10-13
**Branch:** `release/v0.10.0`, cut from `origin/dev` @ `3b0922a0`
(`chore: sync dev with v0.9.1 release state (#321)`)
**Status at this audit:** the pending list is **clean**. Nothing here is
merged or tagged — per the process rule at the bottom, the tag happens
only after the owner's explicit go.

This is the AGENTS.md step-0 evidence, written down.

## Pending inventory

| Item | State | Evidence |
|---|---|---|
| Open issues | **0** (as of 2026-10-13) | `gh issue list --state open` empty; the acceptance-program issues (#294–#302) are all closed |
| Open PRs | **1** — #305, deferred in writing | see quote below |
| Red CI on main / dev | **none** | see CI evidence below |
| Uncommitted WIP in any worktree | **none** | four worktrees checked (`orca/lain`, `lain-proxy`, `lain-sync`, `lain-release-010`); only a local untracked build dir `fuzz/target/` in `orca/lain` — ignored again by this branch's `.gitignore` restore |
| Known test flakes left unfixed | **none** | #295 (GitSensor differential proptest seed) and #296 (load-sensitive `find_anchors`/coupling-radar) both fixed in 0.9.1 and closed; `proptest-regressions/server/git_verification.txt` pins the reproductions |
| Acceptance findings registers | closed / scoped out in writing | `FINDINGS-2026-10-10.md` housekeeping block is marked RESOLVED (CodeQL dismissals, dependabot triage); `FINDINGS-2026-10-11.md` §Open items holds three deliberate scope-outs (below) |
| Release metadata consistent | **yes** | `scripts/check-release-version.py --tag v0.10.0` → `OK: release metadata matches 0.10.0` |

## PR #305 — open, deferred in writing

`deps: bump jsonschema from 0.29.1 to 0.58.6` (dependabot). Owner's
deferral, quoted from
[PR #305](https://github.com/spuentesp/lain/pull/305#issuecomment-6133664297):

> **Deferral (v0.9.1 release audit):** not landing in this release — 2
> required checks fail and the jump 0.29.1 → 0.58.6 is a major API
> surface change, not a patch bump. Will be handled as its own change
> with the migration work it needs. Recorded here per the zero-pending
> rule (deferral in writing).

The same deferral covers v0.10.0: the migration work it needs has not
started, and it is not a release-blocking item.

## Acceptance register scope-outs (written down, not half-done)

From `docs/acceptance/FINDINGS-2026-10-11.md` §Open items — research
and copy work, explicitly not part of this release:

1. **Runtime-only coupling** (service-mesh DNS, external registries,
   generated clients from private packages) — a documented blind-spot
   class, not a defect in shipped behavior. Out of scope.
2. **Scale beyond the tested band** (500+ repos) — untested; the
   published verdicts are bounded by the tested band. Out of scope.
3. **README/positioning reframe** to the topology-server frame —
   product copy, not a fix. Deferred past the release; no behavior
   depends on it.

## CI evidence

- **Release base (dev tip `3b0922a0`)** — fast lane green:
  [CI run 38444215391](https://github.com/spuentesp/lain/actions/runs/38444215391)
  (`conclusion: success`, 17m33s, 2026-10-13) on exactly the commit
  this branch is cut from. Jobs green: Ubuntu `cargo test --workspace`
  (incl. the capability suite / `demo.sh`), lint + format + doc tests,
  npm-shim install tests, schema-drift, arch guardrails, agent-contract
  publish.
- **Full hardened battery** — green on main tip `a507e726` (#318):
  [CI run 38440263606](https://github.com/spuentesp/lain/actions/runs/38440263606)
  (`conclusion: success`, 35m37s). PR-to-main re-runs the full battery
  (test-cross macOS + Windows, coverage, version-drift, action
  contracts) on this branch; that run is the merge gate.
- **No red on either tip.** The one recent failure —
  [run 38440273998](https://github.com/spuentesp/lain/actions/runs/38440273998),
  `Cargo Test (ubuntu-latest)` on the #319 push — was superseded by the
  #321 sync, whose run is the green 38444215391 above.

## What this branch changed beyond the version bump

The v0.9.1 → v0.10.0 content (coordination delegation #319, nightlies
fixes #318, badge-parser fix #316, post-release metadata #317, sync
#321) is what `origin/dev` carries. While assembling this audit, four
**main-only live fixes were found missing on dev** — the dev sync
(#321) dropped main-only code files (only `fuzz/Cargo.lock` had been
restored, in `a4d4bbe0`). All four are restored on this branch,
verbatim, so the release cannot regress them and the PR-to-main
Windows lane is not red:

| Restored | Source | What it is |
|---|---|---|
| `tests/setup_e2e.rs` | `4c350035` (in the v0.9.1 release squash #315) | compare `.mcp.json` entries against `dunce::canonicalize` paths — `Path::canonicalize` yields `\\?\` paths on Windows and the windows-latest battery failed on it |
| `tests/e2e/real-bench.sh` | #318 | federation readiness gate (`READY_TIMEOUT`) + warm-up `retry_after_ms` — without it the Federation Nightly Benchmark goes red again |
| `.gitignore` (`/fuzz/target`) | #318 | "Ignore fuzz/target" from the same fix |
| `.github/workflows/ci.yml` | #316 | `version-drift` extractor reads only the `default:` line; a range scan picks `v0.8.0`/`v0.7.4` out of the doc comment first |

Not touched, by design: `.github/actions/lain-health-badge/action.yml`
— `lain-version` default stays `v0.9.1` (latest **published** release).
Bumping it pre-tag deadlocks the release gate.

## POST-PUBLISH steps (scheduled — do these only after the tag publishes)

1. Fill `Formula/lain.rb`'s three `sha256` values from the published
   v0.10.0 `SHA256SUMS` file (the formula ships `0000…0` placeholders
   with the `POST-PUBLISH:` comment in this PR).
2. Bump `.github/actions/lain-health-badge/action.yml`'s
   `lain-version` default `v0.9.1` → `v0.10.0`.

Both belong in one post-release PR (the #317 pattern). Doing either
**before** the tag breaks the release: pre-filled checksums produce a
formula that fails verification (artifacts are not bit-reproducible),
and a pre-bumped badge default 404s the badge job, which makes CI red
on the tagged commit and `release.yml` refuses to build it.

## Release-history lessons (why this audit is this strict)

1. **v0.8.0 shipped with a broken brew formula.** The formula's
   `sha256` values were filled from a local build before publish; the
   published tarballs hashed differently and `brew` verification
   failed. Now: placeholders + POST-PUBLISH fill only.
2. **Windows npm was broken until 0.9.1's #312.** `npx
   @spuentesp/lain-mcp` failed under Git Bash (the installer fed the
   release archive to Git's GNU tar, which aborted with `gzip: stdin:
   unexpected end of file`); distribution acceptance was red on every
   run from at least 2026-09-19 until the System32 bsdtar fix shipped.
3. **v0.9.1 shipped with known fixes pending.** The Federation Nightly
   Benchmark had been red since 2026-09-28 and its readiness-gate fix
   (#318) only landed after the release; the npm-shim Windows fix was
   itself noted as "reaches users with the next release". A release cut
   over a red nightly is how pending work becomes invisible.
4. **zoanthrope 0.1.0's binary was broken.** The coordination core is
   an external dependency now; its version matters. CI pins
   `@spuentesp/zoanthrope@0.1.3` (npm install, source-build fallback
   for platforms without a prebuilt asset) and `CI_REQUIRE_ZOANTHROPE=1`
   turns the battery's missing-binary skip into a hard failure, so a
   broken or absent zoanthrope can never pass silently again.

## Process rule (the one that prevents a repeat)

**Tag only after the owner's explicit go on a clean audit.** Publishing
is the last step, not the first: nothing is merged and nothing is
tagged until this pending list is clean *and* the owner has said go.
A release that ships with a list of "pendientes" is half-done work.
