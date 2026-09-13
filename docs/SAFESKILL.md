# SafeSkill scoring

The SafeSkill scoring workflow scans PRs and `main` with the pinned
`skillsafe@0.2.9` CLI, saves its JSON findings as an Actions artifact, and shows
the score in the job summary. It preserves the CLI's failure status (including
scores below 40); scan failures are not reported as successful checks.

Use `npx --yes skillsafe@0.2.9 scan . --json` locally. The `spuentesp/lain`
argument uses `npm pack`, which fails because this Rust repository has no root
`package.json`. No findings are suppressed to raise the score.

CLI scans do not update the public listing. On pushes to `main`, weekly, and
manual runs on `main`, a separate job requests a forced hosted rescan and waits
for completion. PRs do not publish their results to the default-branch listing.
The hosted service scans the current default branch, not necessarily the exact
commit that triggered a run, and can use a different scanner version.

Public report: <https://safeskill.dev/scan/spuentesp-lain>.
API and scanner documentation: <https://safeskill.dev/docs>.

SafeSkill focuses on supported JavaScript/TypeScript and text content, including
the npm launcher and agent instructions. Its score is not a Rust security audit.
Review individual findings in context; native launchers necessarily perform
filesystem and child-process operations.
