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
  feature work; PRs to `dev` require one approval and the fast CI lane.
- **`main`** is the protected release line. PRs from `dev` → `main`
  require one approving review and the full cross-platform CI lane.

## Why this shape

The previous model — push directly to `main` — was fine when there was
one maintainer and no audit pressure. It stopped being fine once we
added the OpenSSF Scorecard badge: Scorecard's `Code-Review` and
`Branch-Protection` checks were both at 0, and the badge reflected
that. We turned on:

- `main` requires one approval, `lain/agent-contract`, lint, and the
  Ubuntu/macOS/Windows Cargo tests.
- `dev` requires one approval, `lain/agent-contract`, lint, and the Ubuntu
  Cargo test.
- Force pushes and direct deletion are blocked on both.

**These settings alone did not move the score to 10.** Confirmed
live against `api.securityscorecards.dev` on 2026-09-16:
`Branch-Protection` is 5 and `Code-Review` is still 0, because
`enforce_admins` is `false` on `main` — every PR so far has been
merged by the repo admin with only bot `COMMENTED` reviews, never a
human `APPROVED` one, so the 1-approval requirement has never
actually been exercised. See [`docs/SCORECARD.md`](SCORECARD.md) for
the full root-cause writeup; this is a single-maintainer structural
limit, not a misconfiguration to patch quietly.

## Branch protection rules

Last verified through the GitHub API on 2026-09-20.

### `main`

| Setting | Value |
|---|---|
| Required approving reviews | 1 |
| Dismiss stale reviews on push | yes |
| Require status checks | `lain/agent-contract`, lint, Ubuntu/macOS/Windows Cargo tests |
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
| Required approving reviews | 1 |
| Require status checks | `lain/agent-contract`, lint, Ubuntu Cargo tests |
| Require branches up to date before merge | yes |
| Allow force pushes | no |
| Allow deletions | no |

`dev` is still the faster integration lane, but branch protection now requires
one approval as well as the fast-lane checks. `main` adds the cross-platform
test contexts and is reserved for release PRs.

## What counts as "agent contract"

The `lain/agent-contract` status is published by the `agent-contract`
job inside [`.github/workflows/ci.yml`](../.github/workflows/ci.yml), not
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

# Open a PR targeting dev. Once approval, CI, and the rollup are green,
# merge.
gh pr create --base dev --head feature/some-name

# To release: branch from dev, update every release-metadata file, and
# open the release PR against main.
git switch dev
git pull --ff-only
git switch -c release/v0.x.y
python3 scripts/check-release-version.py --tag v0.x.y
gh pr create --base main --head release/v0.x.y \
  --title 'release: v0.x.y' --body '...' --label release
```

After the release PR merges, push the version tag so `release.yml` publishes
the artifacts, then fast-forward `dev` to `main`. Urgent fixes still land on
`dev` first; `main` receives changes only through a release PR.

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
gets the full hardened battery. The gating is per-job via inline
`if:` conditions reading `github.ref` and `github.base_ref` directly
— job-level `if:` can't read workflow `env`, so a shared
`FULL_BATTERY` env var isn't an option. Heavy jobs use:

```yaml
if: github.ref == 'refs/heads/main' || (github.event_name == 'pull_request' && github.base_ref == 'main')
```

to scope themselves to push-to-main and PR-to-main only:

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
