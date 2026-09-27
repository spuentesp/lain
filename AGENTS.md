# AGENTS.md — for AI agents working in this Lain checkout

## Branch policy

Two-branch model:

- **`dev`** — integration. Default branch. PRs target `dev`.
  CI: fast lane (Ubuntu tests, lint, npm-shim, schema-drift,
  health-badge). ~5 min.
- **`main`** — release line. Receives changes only via reviewed
  PRs from `dev`, and only when cutting a new release.
  CI: full hardened battery (all OSes, capability suite, coverage,
  release contracts). ~20 min.

**Always open PRs against `dev`. Never target `main` directly.**

dev → main is **release-only**, not a routine merge:

1. Branch `release/v0.x.y` off `dev`.
2. Bump every release-metadata file (`Cargo.toml`, `Cargo.lock`,
   `server.json`, `npm-shim/package.json`, `Formula/lain.rb`).
   `scripts/check-release-version.py --tag v0.x.y` validates.
3. PR against `main` (1 review + green agent-contract).
4. After merge: `git push origin v0.x.y` triggers `release.yml`.
5. Fast-forward dev: `git switch dev && git merge --ff-only main`.

Full branching rules + protection settings: `docs/BRANCHING.md`.
PR flow: `CONTRIBUTING.md`.

## Releases and tags

- Tags are git tags (`v0.x.y`). The release workflow handles
  cross-platform builds, provenance + SBOM + SHA256SUMS, server.json
  sync, and npm publish.
- Pre-release tags (`v0.x.y-rcN`) publish under npm's `next`
  dist-tag, not `latest`.
- Don't push tags without a release PR.

## npm publishing — Trusted Publishing (OIDC) only

- **No NPM_TOKEN secret.** The release workflow authenticates via
  GitHub Actions OIDC.
- Configured at
  `https://www.npmjs.com/package/@spuentesp/lain-mcp/access`:
  - Provider: **GitHub Actions**
  - Repository: `spuentesp/lain`
  - Workflow filename: `release.yml` *(basename only)*
  - Environment: *(blank)*
  - Permissions: **`npm publish`** *(not `npm stage publish`)*
- The `id-token: write` permission on the publish job is what
  makes OIDC available; do not remove it.

## CI expectations

`ci.yml` is tiered: dev/PR-to-dev get the fast lane, main/PR-to-main
get the full battery. Plus a workflow-level `concurrency:` block
cancels superseded runs for the same ref.

| Job | dev / PR-to-dev | main / PR-to-main |
|---|---|---|
| `test` (Ubuntu) | ✓ | ✓ |
| `test-cross` (macOS + Windows) | — | ✓ |
| `lint`, `npm-shim`, `schema-drift`, `health-badge` | ✓ | ✓ |
| `capability` (demo.sh), `coverage`, `version-drift`, `action-contracts` | — | ✓ |

## Conventions for AI agents

- Open PRs against `dev`. Never target `main` directly.
- Don't bump versions in feature PRs — that's the release PR's job.
- Don't commit secrets, even test fixtures. Use `hooks/<agent>/` to
  claim files before editing.
- Don't push tags without a release PR.
- The OpenSSF Scorecard is the public supply-chain signal; current
  goal is **9+ composite**. Rolling state in `docs/SCORECARD.md`.

## Background

The upstream `v0.7.0` release tarball shipped a `lain` binary whose
`--version` reported `lain 0.6.1`. Root cause: `Cargo.toml`'s
`version` field was never bumped from `0.6.1` when the `v0.7.0`
git tag was cut, so any binary built from that tree carried the
old version string. Functionally the binary behaved as v0.7.0;
only the version string was wrong.

## Status (2026-09-02 → 2026-09-03)

1. **Local fix (commits `93d6344` + `ae2a527`):** bumped
   `Cargo.toml` to `0.7.0` on `fix/v0.7.0-version-bump`, merged
   to `main`, and rebuilt `~/.local/lain/lain` so `--version`
   correctly reports `lain 0.7.0`. Original tarball binary is
   kept as `~/.local/lain/lain.bak.0.6.1`.
2. **Upstream fix (tag `v0.7.1`):** bumped `Cargo.toml` to `0.7.1`
   and pushed the tag so the release workflow publishes corrected
   binaries to GitHub Releases. `server.json`'s top-level + nested
   `version` fields get updated automatically by the release
   workflow; `Formula/lain.rb` and `npm-shim/package.json` are out
   of scope for this fix.
3. **Installer fix (tag `v0.7.2`):** `install.sh` had a
   function-call-ordering bug — it invoked
   `apply_noninteractive_defaults` at the top of the file before
   defining the function further down. With `set -e`, that killed
   the script with `command not found` before any work happened,
   which is why this environment always installed Lain manually.
   `apply_noninteractive_defaults` is now defined above its call
   site, so `curl … | bash` and direct invocation both work.

After the `v0.7.1` workflow completed, the official tarballs at
<https://github.com/spuentesp/lain/releases/tag/v0.7.1> report
the correct version string. After the `v0.7.2` workflow
completes, fresh installs no longer hit the silent-exit bug.

## Re-installing the official tarball

`install.sh` from upstream will now install `lain 0.7.2` (or
newer). Fresh installs work end-to-end via `curl … | bash` or
direct invocation — no more manual install dance.

## CI badge

The repo runs its own `lain-health-badge` action on every pull
request — see `.github/actions/lain-health-badge/`. The action
ships as part of the v0.7.3 release and is referenced from
`spuentesp/monitor_dm_system` PR #117 as a consumer demo; this
note is the orientation for future agents landing changes.

## If upstream `Cargo.toml` on `main` is regressed to `0.6.1`

That would re-introduce the original packaging bug. The fix on
this branch (or its descendant commits) bumps `Cargo.toml` to
match each release tag. Verify with `git log --oneline --
Cargo.toml` before cutting a new tag.

## Federation schema bumps (2026-09-07 onwards)

The federation graph on disk has an explicit version header (`FEDERATION_GRAPH_VERSION` in `src/server/federation/graph_backend.rs`). When the on-disk format changes in a non-backward-compatible way:

1. Bump `FEDERATION_GRAPH_VERSION` in the same commit that introduces the change.
2. The loader refuses to read graphs with a different header, returning `LainError::FederationSchemaMismatch`.
3. The operator's recovery path is `lain reindex`, which backs up the old graph and rebuilds.
4. CHANGELOG entry must name the schema bump and the recovery command.

Do not silently migrate. The federated graph is regenerable from per-repo graphs, and silent migration risks hiding real corruption.
