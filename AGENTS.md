# AGENTS.md — for AI agents working in this Lain checkout

## Branch policy

Two-branch model:

- **`dev`** — integration. Default branch. PRs target `dev`.
  CI: fast lane (Ubuntu tests, lint, npm-shim, schema-drift,
  health-badge, capability suite). ~5 min.
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
| `capability` (demo.sh) | ✓ | ✓ |
| `coverage`, `version-drift`, `action-contracts` | — | ✓ |

## Conventions for AI agents

- Open PRs against `dev`. Never target `main` directly.
- Don't bump versions in feature PRs — that's the release PR's job.
- Don't commit secrets, even test fixtures. Use `hooks/<agent>/` to
  claim files before editing.
- Don't push tags without a release PR.
- The OpenSSF Scorecard is the public supply-chain signal; current
  goal is **9+ composite**. Rolling state in `docs/SCORECARD.md`.

## Background

The pre-2026-09-15 `AGENTS.md` was a release-tracking note about
a v0.7.0 incident where the upstream tarball shipped a binary
whose `--version` reported `0.6.1` (a missed `Cargo.toml` bump).
That incident was resolved by PR #43. This file now supersedes
that note with the current branching, CI, and release policy.
