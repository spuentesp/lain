# OpenSSF Scorecard plan

The badge currently shows **score 5.0** for `github.com/spuentesp/lain`,
fetched from `https://api.securityscorecards.dev/projects/github.com/spuentesp/lain`
on 2026-09-14. This document lists every check, what we scored, why,
and the ranked fix list.

## Current state (all 18 checks)

| Check | Score | Reason | Fix path |
|---|---:|---|---|
| Binary-Artifacts | 10 | no binaries in repo | n/a |
| Dangerous-Workflow | 10 | no risky patterns | n/a |
| Dependency-Update-Tool | 10 | Dependabot detected | n/a |
| License | 10 | LICENSE present (MIT) | n/a |
| Maintained | 10 | 30 commits + 2 issues in last 90 days | n/a |
| Token-Permissions | 9 | one workflow has excessive perms | tighten |
| Pinned-Dependencies | 7 | some deps not pinned by hash | tighten |
| CI-Tests | 6 | CI runs but not on every commit | wire |
| Security-Policy | 4 | policy file present but incomplete | expand |
| Contributors | 3 | single maintainer | n/a (project reality) |
| Branch-Protection | 0 | no branch protection on default branch | **done** — set 2026-09-14 |
| Code-Review | 0 | 0/20 approved changesets | **done** — 1 approval required |
| SAST | 0 | no SAST tool runs on every commit | **done** — CodeQL added |
| Signed-Releases | 0 | SLSA provenance present but not `.sig`/`.asc` | add cosign keyless |
| Fuzzing | 0 | not fuzzed | out of scope (multi-day) |
| Vulnerabilities | 0 | 18 known vulns | triage |
| CII-Best-Practices | 0 | no CII badge effort | out of scope (doc-heavy) |
| Packaging | -1 | not published as a package | deferred (publishing is version work) |

**Already fixed this session:**

- `Branch-Protection`: 0 → 10 (set via API on `main`, lighter rules on `dev`)
- `Code-Review`: 0 → 10 (1 approval required, stale-review dismiss on push)
- `SAST`: 0 → 10 (`.github/workflows/codeql.yml` runs CodeQL on every push to `main`/`dev` and on every PR)

**On the next scorecard run (weekly + push-to-main) the composite should
re-aggregate from these three going to 10.** Three zeros to 10s move the
composite by roughly +1.5 points.

## Recommended next moves, ranked by value ÷ effort

### 1. Signed-Releases (0 → 10) — ~30 min

`release.yml` already calls `actions/attest-build-provenance@v2.4.0`,
which produces a SLSA-style provenance attestation signed by GitHub's
OIDC token. Scorecard's `Signed-Releases` check looks for `.sig`,
`.asc`, `.pem`, or `.gpg` files attached to the release — it does not
recognize the SLSA attestation as a "signed release" artifact.

Cheapest path: add `cosign sign-blob` with keyless OIDC after the
provenance step. Output to `release/lain-${VER}-${TARGET}.tar.gz.sig`
and add it to the `softprops/action-gh-release` upload list. Scorecard
sees `.sig` → score 10.

This touches the release pipeline, so it should be its own PR with a
dry-run review before any new release ships.

### 2. Token-Permissions (9 → 10) — ~15 min

A workflow still has `permissions: write-all` or an unscoped `GITHUB_TOKEN`.
Find the offender with `grep -rn "write-all\|permissions: write" .github/workflows/`
and tighten.

### 3. Security-Policy (4 → 10) — ~30 min

`SECURITY.md` exists but scorecard gives partial credit. The check
wants: contact channel, supported versions, expected response time,
coordinated disclosure language. Rewrite to include all four sections.

### 4. Pinned-Dependencies (7 → 10) — ~30 min

`dependabot.yml` keeps GitHub Actions pinned by commit SHA already.
The deduction is for one or two `uses: foo/bar@vN` style references
without a SHA. Grep for `@v[0-9]` in `.github/workflows/` and pin.

### 5. CI-Tests (6 → 10) — small but configurable

The check wants test results published as a GitHub check. The current
`cargo test` jobs are already check runs; the deduction is probably
because the `federation-nightly.yml` workflow runs on a cron and isn't
wired to PR status. Either wire it to PR runs or document it as
out-of-scope for merge gating.

### 6. Vulnerabilities (0 → ?) — medium effort

18 detected vulnerabilities via OSV/Dependabot. Triage:

1. `cargo audit` (or `cargo deny`) locally to enumerate.
2. For each, classify: patched in upstream Cargo.lock / advisory-only
   / unmaintained crate that needs replacing.
3. Either bump or pin-with-advisory.

This is real engineering work and likely worth a dedicated PR per
class of fix.

### 7. CII-Best-Practices (0 → ?) — doc-heavy

CII Best Practices self-certification requires ~50 met criteria across
documentation, governance, and code. Heavy lift for a single-maintainer
project. Defer.

### 8. Fuzzing (0 → ?) — multi-day

`cargo fuzz` integration with a CI cron that uploads reproducer
artifacts. Useful for the parser layer in particular. Out of scope for
this session.

## Out of scope this session

- **Packaging** — score `-1` means the check can't run, because
  crates.io/npm don't list `spuentesp/lain`. Publishing involves
  version-number work, which you asked to defer.
- **Contributors** — score 3 is structural (one primary author); not
  something to "fix".
- **CII / Fuzzing** — both meaningful but heavy enough to be their own
  initiatives.

## Branching implications for the score work

The `main` branch now requires:
- 1 approving review on every PR (Code-Review +0)
- `lain/agent-contract` status check (Branch-Protection +0)

The `dev` branch requires:
- `lain/agent-contract` status check only

Both rules were applied 2026-09-14 via the GitHub API. See
[`docs/BRANCHING.md`](BRANCHING.md) for the workflow.

## Maintenance update (September 14, 2026)

The `main protection (Scorecard visible)` GitHub repository ruleset mirrors
the classic protection on `main`: one approval, stale-review dismissal,
the `lain/agent-contract` check, an up-to-date branch, and restrictions on
deletion and force pushes. It retains the existing administrator bypass.
Keep these settings consistent when changing branch protection.

Scorecard can read the ruleset with its default token. No `SCORECARD_TOKEN`
secret is required. Private vulnerability reporting is enabled; the
reporting link is in [SECURITY.md](../SECURITY.md).

### Dependency pinning

Both JavaScript CI jobs use `npm ci` with committed lockfiles. Update the
appropriate lockfile when changing a package manifest; don't fall back to
`npm install` in CI.

The September 14, 2026 scan at commit `49e96b0` reported nine
`downloadThenRun` findings in these scripts:

| File | Reported lines | Input being parsed |
| --- | --- | --- |
| `scripts/demo.sh` | 232, 579, 647, 772, 805 | MCP JSON responses |
| `tests/e2e/federation_dashboard_e2e.sh` | 92 | Health JSON response |
| `tests/e2e/multiplayer-hooks.sh` | 67, 81 | MCP JSON responses |
| `tests/e2e/real-bench.sh` | 59 | Health JSON response |

These pipelines pass response data to fixed `python3 -c` code that parses
JSON. They don't execute the response as Python code. Scorecard's shell
scanner treats the download-to-interpreter pipeline as execution, so these
findings remain false positives. This note doesn't suppress the check.

Run the OpenSSF Scorecard workflow after merging changes to refresh the
published results; the viewer may take additional time to update.
