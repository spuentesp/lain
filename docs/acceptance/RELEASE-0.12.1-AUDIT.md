# Release audit — v0.12.1 (zero-pending record)

Date: 2026-10-26. Base: `dev` (includes PR #351). Release branch:
`release/v0.12.1`. No product source changed since v0.12.0.

## Pending inventory

| Item | State | Evidence |
|---|---|---|
| Open issues | #328 only, deferred in writing | `gh issue list --state open`. Coordination scope since 0.10.0. |
| Open PRs | Not in this release, deferred in writing | #344 (`feat/service-refs`, separate feature line) and dependabot #305, #332–#343. Major migrations or other lines. |
| Known test flakes | none accepted | The presence attribution flake (`e2e_attribution_via_real_child_pid`) was fixed in #349 and passes repeatedly. |
| Uncommitted WIP | none in the release worktree | `git status` clean. |
| Acceptance findings register | closed or scoped out | 0.11.0 scope-outs carried forward. |

## Verification on this tree

- `cargo test --workspace --no-fail-fast`: **94 targets, 3543 passed, 0 failed, 8 ignored**.
- `scripts/demo.sh`: **166 passed, 2 skipped, 0 failed** (exit 0).
- `scripts/check-release-version.py --tag v0.12.1`: OK.

## Deferrals written down

1. #328: coordination scope; not in 0.12.1.
2. #344 and the dependabot PRs: separate lines or major migrations; not in 0.12.1.
3. Byte-identical federated graph across runs: content is deterministic; file bytes differ by node-index order and sync timestamps. Documented in `docs/FEDERATION.md`.

## Process

Metadata bumped in `Cargo.toml`, `Cargo.lock`, `server.json`, `npm-shim/package.json` and `Formula/lain.rb`. The health-badge default stays on `v0.12.0` until v0.12.1 is published; the formula `sha256` values are filled after publish.
