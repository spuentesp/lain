# OpenSSF Scorecard status

Live snapshot fetched from
`https://api.securityscorecards.dev/projects/github.com/spuentesp/lain` on
2026-09-20. The latest published scan is dated 2026-09-17.

**Overall score: 7.9/10.**

## Current checks

| Check | Score | Current interpretation |
|---|---:|---|
| Dependency-Update-Tool | 10 | Dependabot detected |
| Maintained | 10 | active commit and issue history |
| Binary-Artifacts | 10 | no committed binaries detected |
| Dangerous-Workflow | 10 | no dangerous workflow patterns detected |
| Token-Permissions | 10 | workflow tokens follow least privilege |
| Security-Policy | 10 | `SECURITY.md` detected |
| License | 10 | MIT license detected |
| Fuzzing | 10 | fuzzing workflow detected |
| CI-Tests | 10 | CI observed on merged PRs |
| Packaging | 10 | packaging workflow detected |
| Pinned-Dependencies | 9 | Scorecard still detects at least one dependency not pinned by hash |
| SAST | 9 | CodeQL detected, but not observed on every commit in the scan window |
| Vulnerabilities | 7 | three current advisory ids; see [`VULNS.md`](VULNS.md) |
| CII-Best-Practices | 5 | passing badge claimed; silver/gold unclaimed |
| Branch-Protection | 5 | protection is not maximal on development and release branches |
| Signed-Releases | 4 | two of the last five releases contain recognized signed artifacts |
| Contributors | 3 | one contributing organization |
| Code-Review | 0 | no approved changesets observed in the scan window |

## Actionable work

### Dependency and SAST regressions

`Pinned-Dependencies` and `SAST` previously read 10 but now read 9. Before
changing workflows, inspect the Scorecard finding details from the next scan to
identify the exact unpinned dependency and commits without SAST coverage. Do
not weaken or churn correctly SHA-pinned actions based only on the aggregate
score.

### Signed releases

Release jobs publish cosign v3 bundles (`*.cosign.bundle.json`) and GitHub SLSA
attestations, but Scorecard recognizes only some recent release artifacts. The
score has improved from 2 to 4 as signed releases enter the five-release
window.

If raising this check remains a priority, validate one of these in a release PR:

1. Extract and upload a raw `*.sig` from each cosign bundle while retaining the
   bundle and provenance artifacts.
2. Confirm from Scorecard's current documentation that the chosen file is
   recognized before changing the release pipeline.

Any release-workflow change needs the normal dry-run review and must preserve
OIDC trusted publishing.

### Branch protection and code review

Live protection queried on 2026-09-20 requires one approval on both `dev` and
`main`; administrators can still bypass enforcement. A single maintainer cannot
approve their own PR, so `Code-Review` remains structurally constrained unless
another trusted approver participates. Changing administrator enforcement
without an approval path can block every merge and is a maintainer process
decision, not a documentation fix.

### Vulnerabilities

Keep package-level triage in [`VULNS.md`](VULNS.md). Do not duplicate a static
advisory inventory here.

## Non-actionable or low-value gaps

- `Contributors` reflects the current contributor population.
- CII silver/gold requires additional project-process work; pursue it only when
  that process is useful independently of the score.

## Maintenance

Refresh this file from the API rather than copying an older table. Scorecard is
a lagging external measurement, so record both the fetch date and the scan date.
The public workflow may take time to incorporate newly published releases or
branch-protection changes.
