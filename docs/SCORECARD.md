# OpenSSF Scorecard plan

Live snapshot fetched from
`https://api.securityscorecards.dev/projects/github.com/spuentesp/lain`
on 2026-09-19. **Overall score: 7.9/10** (unchanged since 2026-09-16;
the only movements since the last refresh are documentation and
test pins; no behavior-changing security work landed in this
window).

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
| Signed-Releases | 2 | SLSA provenance attestation present; Scorecard wants `.sig`/`.asc` — only 1 of 5 recent releases has one |

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

### 1. Signed-Releases (2 → 10) — partial in PR #84, follow-up pending

`release.yml` now runs `cosign sign-blob --yes --bundle ...` with
keyless OIDC on every per-platform build job (wired in PR #84,
live at lines 195–201, 282–288, 378–384). The bundle
(`.cosign.bundle.json`) is attached to each GitHub release along
with the SLSA provenance bundle (`.sigstore.json`), the in-toto
attestation (`.intoto.jsonl`), the per-binary CycloneDX SBOM, and
the per-binary SHA256 side-file. All three tarball uploads now
have a fully-fidelity provenance chain verifiable with the
`cosign verify-blob` CLI alone.

What is still missing for Scorecard's 10/10: Scorecard's
`Signed-Releases` check looks for files matching `*.sig`,
`*.asc`, `*.pem`, or `*.gpg` extensions. The cosign v3 bundle
uses `.cosign.bundle.json`, which Scorecard does not recognise
as a "signed release artifact" despite the underlying signature
being valid. Hence the stuck-at-2 score (the `passing` check
catches that *something* signed exists, not the *extension* of
that file).

Two concrete paths forward; both touch the release pipeline and
should land as their own PR with a dry-run review before the next
release ships:

a. **Extract the raw signature from the cosign v3 bundle.**
   After `cosign sign-blob --bundle ...`, run
   `jq -r '.messageSignature.content' | base64 -d > <tarball>.sig`
   and add `<tarball>.sig` to the `softprops/action-gh-release`
   files list. `jq` is already a runner dependency (used in
   `release.yml` line 72 for the npm retry branch). Cheapest
   option; keeps the existing keyless flow; touches ~9 lines
   per platform × 3 platforms = ~30 lines plus the upload list.

b. **Switch to cosign v2 `--output-signature` / `--output-certificate`.**
   Pin the cosign installer to a v2.x release; the deprecated
   flags produce a `.sig` file directly. Loses no functionality
   but locks the workflow to a tool version that's EOL upstream.

Either path unblocks Scorecard's 10/10 on `Signed-Releases`
(plus the implicit downstream: every other check that references
"this repo signs releases" goes green). It is not done in this
iter because modifying `release.yml` deserves a dry-run review
on the next release tag — out of scope for a docs pass.

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
