# OpenSSF Scorecard plan

Live snapshot fetched from
`https://api.securityscorecards.dev/projects/github.com/spuentesp/lain`
on 2026-09-16. **Overall score: 7.9/10** (up from 5.0 on 2026-09-14).

## Current state (all 18 checks, live 2026-09-16)

| Check | Score | Notes |
|---|---:|---|
| Dependency-Update-Tool | 10 | Dependabot detected |
| Maintained | 10 | active commit/issue history |
| Binary-Artifacts | 10 | no binaries in repo |
| Dangerous-Workflow | 10 | no risky patterns |
| Token-Permissions | 10 | every workflow uses top-level `permissions: {}` + least-privilege per-job grants (verified 2026-09-16 across all `.github/workflows/*.yml`) |
| Security-Policy | 10 | `SECURITY.md` covers contact channel, supported versions, response SLAs, coordinated disclosure |
| Pinned-Dependencies | 10 | all GitHub Actions pinned by full commit SHA (verified 2026-09-16, no `@vN`-only refs found) |
| License | 10 | LICENSE present (MIT) |
| Fuzzing | 10 | `fuzz-nightly.yml` wired |
| CI-Tests | 10 | wired to every PR via `ci.yml` |
| SAST | 10 | CodeQL runs on every push/PR |
| Packaging | 10 | published |
| Vulnerabilities | 7 | rolling triage in [`docs/VULNS.md`](VULNS.md) — that file is the live source, not this one |
| CII-Best-Practices | 5 | "passing" badge live (`bestpractices.dev/projects/14660`, confirmed 2026-09-16) — the mid-tier "silver"/"gold" levels are unclaimed, not doc-heavy triage anymore |
| Branch-Protection | 5 | see root cause below |
| Contributors | 3 | single maintainer — structural, not fixable |
| Code-Review | 0 | see root cause below |
| Signed-Releases | 2 | see root cause below — this repo's own prior claim about what the check wants was wrong |

## Root cause: Code-Review (0) and Branch-Protection (5)

**This repo's own prior docs claimed both went to 10 on 2026-09-14 —
that was wrong.** Confirmed live against GitHub on 2026-09-16:

- `required_approving_review_count: 1` is set on `main`.
- `enforce_admins: false` is also set on `main`.
- Every recent PR merged to `main` (checked #49 through #73) was
  merged by the repo admin with only bot `COMMENTED` reviews —
  never a human `APPROVED` review — because admin bypass means the
  approval requirement never actually applies to the person doing
  all the merging.

The branch-protection *setting* is real; it just has never been
*exercised*, because the sole maintainer is also the sole approver
and GitHub won't let an author approve their own PR. Turning on
`enforce_admins: true` would make this check meaningful, but it would
also block every future merge until a second approver (human or a
bot account configured to submit `APPROVED`, not just `COMMENTED`)
exists — that's a workflow decision for the maintainer, not something
to flip silently. Until that decision is made, `Code-Review` is
structurally capped at 0 for the same reason `Contributors` is capped
at 3: single-maintainer reality, not a bug to "tighten."

## Recommended next moves, ranked by value ÷ effort

### 1. Signed-Releases (2 → 10) — this repo's own prior write-up here was wrong

**Correction (2026-09-16):** this doc previously claimed Scorecard's
`Signed-Releases` check "does not recognize the SLSA attestation" and
specifically wants `.sig`/`.asc`/`.pem`/`.gpg` files, recommending a
`cosign sign-blob` addition to the release pipeline. That was checked
directly against [Scorecard's own check
documentation](https://github.com/ossf/scorecard/blob/main/docs/checks/internal/checks.yaml)
on 2026-09-16 and **is false**: the check explicitly recognizes
`*.sigstore`, `*.sigstore.json`, and gives the *maximum* score of 10
specifically when `*.intoto.jsonl` (SLSA provenance) is present. It
samples the 30 most recent releases per artifact and does not verify
the signatures, just checks for their presence.

`release.yml` already produces exactly `*.sigstore.json` and
`*.intoto.jsonl` per binary via `actions/attest-build-provenance` +
`scripts/export-release-provenance.py` (confirmed live at
lines 156/234/316) — **the pipeline is already fully compliant.**
The real reason the live score is 2/10, confirmed by checking actual
release assets on 2026-09-16:

| Release | Has `.sigstore.json` / `.intoto.jsonl`? |
|---|---|
| v0.7.4-rc1 | yes |
| v0.7.3 | no |
| v0.7.2 | no |
| v0.7.1 | no |
| v0.7.0 | no |

Only the single most recent release was cut after the provenance-export
step landed in the pipeline; every older release predates it. Scorecard
is correctly reporting that 1 of the last 5 releases is signed — it's
a release-history problem, not a pipeline gap.

**Do not add cosign or any other signing mechanism** — there is nothing
missing to build. Two real options, both a maintainer decision rather
than something to change unilaterally:

1. **Do nothing.** The ratio improves automatically as new releases
   are cut with the current pipeline and eventually dominate the
   30-release sample window. Slow, but zero risk.
2. **Delete the pre-provenance releases** (v0.7.0–v0.7.3) so the
   sample window only contains compliant releases. Faster, but
   deleting published GitHub releases is a destructive, hard-to-reverse
   action that could break anyone who downloaded from or scripted
   against those specific release tags — needs an explicit maintainer
   call, not something to do as part of a docs pass.

### 2. Branch-Protection / Code-Review — process decision, not a patch

See the root-cause section above. The only real fix is
`enforce_admins: true` plus a plan for how PRs get a second approval
on a single-maintainer repo. Flagging for the maintainer to decide;
not something to change unilaterally given it can block merges.

### 3. CII-Best-Practices (5 → higher) — doc-heavy

The "passing" tier is claimed. Silver/gold tiers require materially
more process documentation. Defer unless there's a reason to chase it.

### 4. Vulnerabilities — see `docs/VULNS.md`

That file is the live triage log; keep updates there, not here.

## Out of scope

- **Contributors** — score 3 is structural (one primary author); not
  something to "fix".
- **CII silver/gold** — heavy enough to be its own initiative.

## Branching implications for the score work

The `main` branch requires:
- 1 approving review on every PR (not currently enforced against
  admin merges — see root cause above)
- `lain/agent-contract` status check

The `dev` branch requires:
- `lain/agent-contract` status check only

See [`docs/BRANCHING.md`](BRANCHING.md) for the workflow.

## Maintenance notes

The `main protection (Scorecard visible)` GitHub repository ruleset
mirrors the classic protection on `main`: one approval requirement,
stale-review dismissal, the `lain/agent-contract` check, an
up-to-date branch, and restrictions on deletion and force pushes. It
retains the existing administrator bypass — see the root-cause section
above for why that matters to the score.

Scorecard can read the ruleset with its default token. No
`SCORECARD_TOKEN` secret is required. Private vulnerability reporting
is enabled; the reporting link is in [SECURITY.md](../SECURITY.md).

### Dependency pinning

Both JavaScript CI jobs use `npm ci` with committed lockfiles. Update
the appropriate lockfile when changing a package manifest; don't fall
back to `npm install` in CI.

The September 14, 2026 scan at commit `49e96b0` reported nine
`downloadThenRun` findings in these scripts:

| File | Reported lines | Input being parsed |
| --- | --- | --- |
| `scripts/demo.sh` | 232, 579, 647, 772, 805 | MCP JSON responses |
| `tests/e2e/federation_dashboard_e2e.sh` | 92 | Health JSON response |
| `tests/e2e/multiplayer-hooks.sh` | 67, 81 | MCP JSON responses |
| `tests/e2e/real-bench.sh` | 59 | Health JSON response |

These pipelines pass response data to fixed `python3 -c` code that
parses JSON. They don't execute the response as Python code.
Scorecard's shell scanner treats the download-to-interpreter pipeline
as execution, so these findings remain false positives. This note
doesn't suppress the check.

Run the OpenSSF Scorecard workflow after merging changes to refresh
the published results; the viewer may take additional time to update.
