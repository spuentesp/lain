# Release audit — v0.9.1 (2026-10-10)

Step 0 of the release checklist (AGENTS.md): publishing is the LAST
step. This record is the evidence that nothing was half-done when the
release branch was cut.

## Zero-pending audit

| Check | Evidence |
|---|---|
| Open issues | **0** (program issues #291-#302 all closed with references; dependabot PRs closed with rationale) |
| Open PRs | **0** |
| Uncommitted WIP, any worktree | **none** (main checkout clean; agent worktrees and their merged branches removed) |
| Known test flakes | **none** — #295 (differential oracle: git rename-detection false negative, `--no-renames` fix + pinned seed + persisted regression seeds) and #296 (torn mid-reindex answers: reader quiesce + degraded banner + storm coalescing) fixed and merged |
| Findings register | §5-7 resolved (#292/#293/#294), §8-9 resolved (#295/#296), §15 resolved (21 CodeQL dismissals with justifications; dependabot triaged). The remaining honesty item found by the #296 agent — `run_sync` promising a full refresh it never did — fixed in the same batch |
| Battery green on the exact tree | tree `17d8f51b8a841fc24c826f9808e44703d4d15497` (byte-identical to the tree the battery ran on): `cargo fmt` clean; `cargo test --lib` exit 0 (~2500); acceptance single-repo 15/15; acceptance multi-repo 16/16; `scripts/demo.sh --quick` "Every capability check passed" |

## What this release contains

The agent-facing data honesty batch (#301): staleness markers that mean
what they say, schemas that match the runtime, machine-checkable impact
claims (`lain impact --format claims`), onboarding docs that match
reality, and the two test-reliability fixes. Plus the validation that
the claims feature flips the AFFECTED-protocol streak (0/5 runs → the
demo run emitted complete claims with no decoys).

## Post-publish steps (this checklist, step 6)

- fill Formula/lain.rb sha256 values from the published SHA256SUMS
- bump the health-badge action's lain-version default to v0.9.1
- fast-forward dev
