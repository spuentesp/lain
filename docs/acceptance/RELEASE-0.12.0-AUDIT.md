# Release audit — v0.12.0 (zero-pending record)

Date: 2026-10-25. Base: `dev` @ the commit that merged PR #347. Release branch:
`release/v0.12.0`.

## Pending inventory

| Item | State | Evidence |
|---|---|---|
| Open issues | #328 only, deferred in writing | `gh issue list --state open`. #328 (`federation: root snapshots need a nonempty repos manifest`) is coordination scope, deferred since 0.10.0 and not in this release. |
| Open PRs | Not in this release, deferred in writing | #344 (`feat/service-refs`, phase 0 of a separate feature line) and the dependabot PRs #332–#343 and #305 (a major `jsonschema` migration). None is merged or validated here. |
| Known test flakes | none accepted | Full battery below ran clean on this tree. |
| Uncommitted WIP | none in the release worktree | `git status` clean. An old stash of `feat/per-pr-blast-radius` belongs to another line and is not touched. |
| Acceptance findings register | closed or scoped out | `docs/acceptance/RELEASE-0.11.0-AUDIT.md` scope-outs 1–5 carried forward. Scope-out 4 (macOS lane coverage for the load-stress test) still applies. |

## Verification on this tree

- `cargo test --workspace --no-fail-fast`: **94 targets, 3543 passed, 0 failed, 8 ignored**.
- `scripts/demo.sh` (capability suite): **166 passed, 2 skipped, 0 failed**.
- `scripts/check-release-version.py --tag v0.12.0`: **OK**.
- `cargo fmt --all -- --check`: clean.
- Local mutation gate (`scripts/mutation-gate.sh origin/dev`, file batches): every group exits 0 on the final code. The incremental mutation job was removed from CI in this release (it ran past its 60-minute limit on large diffs); the gate is a local pre-merge step.

## Performance and memory

- Saleor 3-repo federation, cold reindex: about 30 s on this release. Peak RSS measures about 2.7 GB on this host. Earlier figures of 1.4 to 1.7 GB could not be reproduced on the same code.
- The optional parse cache (`parse_cache_mb`) is off by default.

## Deferrals written down

1. #328 (federation root snapshot manifest): coordination scope; not in 0.12.0.
2. #344 and the dependabot PRs: separate lines or major migrations; not in 0.12.0.
3. Federation benchmark p99 target (100 ms, enforced with `LAIN_BENCH_ENFORCE=1`): measures 142 ms on this host. The normal suite passes; the target is the bar, not yet met.
4. Byte-identical federated graph across runs: content is deterministic; file bytes differ by node-index order and sync timestamps. Documented in `docs/FEDERATION.md`.
5. Scope-out 4 from 0.11.0 (macOS coverage for one load-stress test) carried forward unchanged.

## Process

Metadata bumped in `Cargo.toml`, `Cargo.lock`, `server.json`, `npm-shim/package.json` and `Formula/lain.rb` (version and URLs). The health-badge default stays on `v0.11.0` until v0.12.0 is published. The formula's `sha256` values are filled after publish from `SHA256SUMS`, not before.
