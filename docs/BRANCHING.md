# Branching policy

LAIN uses a two-branch model: **`main` is the stable release line** and
**`dev` is the integration branch** where ongoing work accumulates.

## The shape

```text
feature/foo ──┐
              │
feature/bar ──┼──► dev ──► main
              │
hotfix/x ─────┘
```

- **Feature branches** (`feature/<name>`, `fix/<name>`, `chore/<name>`)
  branch off `dev` and target `dev` when opened as PRs.
- **`dev`** is the always-green integration branch. It receives all
  feature work; PRs to `dev` only need CI to pass.
- **`main`** is the protected release line. PRs from `dev` → `main`
  require one approving review and a green `lain/agent-contract`
  status.

## Why this shape

The previous model — push directly to `main` — was fine when there was
one maintainer and no audit pressure. It stopped being fine once we
added the OpenSSF Scorecard badge: Scorecard's `Code-Review` and
`Branch-Protection` checks were both at 0, and the badge reflected
that. Both went to 10 the day we turned on:

- `main` requires 1 approval + green `lain/agent-contract`.
- `dev` requires green `lain/agent-contract`.
- Force pushes and direct deletion are blocked on both.

## Branch protection rules

Configured 2026-09-14 via the GitHub API.

### `main`

| Setting | Value |
|---|---|
| Required approving reviews | 1 |
| Dismiss stale reviews on push | yes |
| Require status checks | `lain/agent-contract` |
| Require branches up to date before merge | yes |
| Require conversation resolution | no |
| Require signed commits | no |
| Require linear history | no |
| Allow force pushes | no |
| Allow deletions | no |
| Enforce on admins | no |

### `dev`

| Setting | Value |
|---|---|
| Required approving reviews | 0 (integration branch) |
| Require status checks | `lain/agent-contract` |
| Require branches up to date before merge | yes |
| Allow force pushes | no |
| Allow deletions | no |

`dev` deliberately has no review requirement — that's `main`'s gate.
A broken PR will be caught by the agent-contract rollup before it can
merge.

## What counts as "agent contract"

The `lain/agent-contract` status is published by the `agent-contract`
job inside [`.github/workflows/ci.yml`](.github/workflows/ci.yml), not
by a separate workflow. It runs as part of every CI invocation and
aggregates three sibling CI jobs:

- `Capability suite (demo.sh)` — scripts/demo.sh ground-truth checks
- `Tool schema matches docs/tool-schema.json` — schema-drift gate
- `npm-shim install tests` — npm launcher test suite

A passing agent-contract status means: tools/list is correct, the
ground-truth capability suite passes, and the npm launcher works.
That's the contract an MCP consumer cares about; the test matrix and
lint jobs are deliberately excluded from the rollup.

## Daily workflow

```bash
# Start a feature branch off dev (default branch for new work).
git switch dev
git pull --ff-only
git switch -c feature/some-name

# ... work, commit ...

# Open a PR targeting dev. Once CI is green and the rollup posts
# "Passing", merge.
gh pr create --base dev --head feature/some-name

# Periodically: cut a release from dev into main.
git switch dev
git pull --ff-only
git switch -c release/v0.x.y
gh pr create --base main --head release/v0.x.y \
  --title 'release: v0.x.y' --body '...' --label release
```

## Hotfix path

For an urgent fix that can't wait for `dev` to settle:

1. Branch off `main`: `git switch main && git switch -c hotfix/thing`.
2. Open a PR to `main`. It still needs 1 review + agent-contract.
3. After merging to `main`, cherry-pick or fast-forward `dev` to
   catch up: `git switch dev && git merge --ff-only main`.

## What this policy is *not*

- **Not Git Flow.** There's no `release/*` long-lived branch per
  version — `main` is always the next-to-be-released code; tagged
  commits *are* the releases.
- **Not trunk-based.** `main` is protected and receives changes only
  via reviewed PRs. Direct push is blocked.
- **Not immutable history.** Squash-merge and rebase-merge are both
  allowed. Force-pushes on protected branches are blocked.

## CI expectations per branch

`ci.yml` is tiered so the dev branch gets the fast lane and `main`
gets the full hardened battery. Job-level `if: ${{ env.full-battery }}`
conditions scope the heavy jobs to main-only pushes and PR-to-main
PRs:

| Job | dev / PR-to-dev | main / PR-to-main |
|---|---|---|
| `test` (Ubuntu) | ✓ | ✓ |
| `test-cross` (macOS + Windows) | — | ✓ |
| `lint`, `npm-shim`, `schema-drift`, `health-badge` | ✓ | ✓ |
| `capability` (demo.sh 113 ground-truth checks) | — | ✓ |
| `coverage` (llvm-cov) | — | ✓ |
| `version-drift`, `action-contracts` | — | ✓ |

A workflow-level `concurrency:` block cancels superseded runs for
the same ref, so rapid-fire commits to `dev` don't pile up.

This tiering keeps the dev → main signal cheap (~5 min on dev)
while preserving the full battery as the merge gate on main.

For the agent-facing summary, see the top-level `AGENTS.md`.
