# Release audit — v0.11.0 (zero-pending record)

**Date:** 2026-10-17
**Tree:** `dev` @ `026d4283` (= `chore/sync-dev-v0.10.0` after absorbing
`origin/dev` @ `6c392158` — the twin of our sync base — via merge
`7bb8549b`, auto-merged clean except CHANGELOG; plus the demo readiness
fix `026d4283`).
**Status at this audit:** the pending list is **clean**. Nothing here is
merged to `main` or tagged — per the process rule at the bottom, the tag
happens only after the owner's explicit go.

This is the AGENTS.md step-0 evidence, written down.

## Pending inventory

| Item | State | Evidence |
|---|---|---|
| Open issues | **1** — #328, deferred in writing | `gh issue list --state open` → only #328 (`federation: root snapshots need a nonempty repos manifest`), the WIP patch preserved from the 0.10.0 audit; coordination scope, not this release |
| Open PRs | **1** — #305, deferred in writing | `gh pr list --state open` → only the dependabot `jsonschema` bump; deferral quote carried below |
| Red CI on main / dev | **none** | remotes unchanged by this work; the 32 commits are local-unpushed and will get their first CI run on the release PR (this is stated, not hidden) |
| Uncommitted WIP in any worktree | **none** | main checkout clean; feature worktrees pruned after each unit merged; only the host-owned `lain-release-010` worktree remains, untouched |
| Known test flakes left unfixed | **none** | this cycle eradicated the known class: `hot_reload::set_workspace_stress_visible_to_shared_lock` reproduced under 8×20 load and fixed (writer/reader handshake + stall watchdog, 20/20 green under load); `shared_daemon_outlives_the_first_call_and_exits_when_idle` and `failure_modes::request_reload_handles_corrupt_yaml` hardened (69 / 198 no-repro attempts, 20/20 and 23/23 green under load). Two documented residuals are scoped out below, not left flaky |
| Acceptance findings registers | closed / scoped out in writing | `FINDINGS-2026-10-10.md` housekeeping RESOLVED; `FINDINGS-2026-10-11.md` §Open items scope-outs inherited unchanged (quoted below). This cycle's own findings (mutation-gate contamination, demo scale-gate race, `query-language.md` drift) were found and closed before this audit |
| Acceptance suites + demo.sh + full battery on this exact tree | **green** | see "Acceptance evidence" below |
| Release metadata consistent | **pending step 3** | bumps land on `release/v0.11.0`; `scripts/check-release-version.py --tag v0.11.0` is the step-3 gate, re-run evidence goes in the release PR |

## Acceptance evidence (tree `026d4283`)

- **`scripts/demo.sh` (capability suite): `checks: 166 passed, 0 failed,
  2 skipped`, exit 0 — on a COLD run** (fresh `WORK`, no index). The audit
  itself found and fixed the last gap here: the scale section asserted
  answers while the big-repo index was still `warming_up` (symbols reach
  `ready` ≈ 2.2 min after boot; `/health` is up in seconds). Fix
  `026d4283` makes the scale checks poll the capability gate's own
  contract (`get_capabilities`) before asserting content, with a loud
  skip if readiness never comes. Deterministic in cold and warm runs.
- **Full battery:** `cargo test --workspace` green (the run before this
  file was written; doc-tests phase clean).
- **Parallax-required expansions verified end-to-end** on the real
  3-repo Saleor fixture (acceptance §7 of
  `docs/superpowers/plans/2026-10-14-parallax-requirements.md`):
  0 `graphql_no_op` items for the storefront; `graphql:query:product`
  bound to `saleor-api` with `bound_consumers: 22`; 0 of 499 GraphQL keys
  with >1 provider; 825 GraphQL consumers resolved in total. The
  `storefront → saleor-api` arrow exists.
- **Differential oracle** for the query engine: 16/16 (node set + edge
  set + exact paths), RED-before/GREEN-after recorded; mutation gate:
  22/22 mutants caught on a real diff, and its red demo proved the gate
  fails when it must (dead-code survivors → exit 1) after the
  copied-scratch-tree contamination was found and removed.
- **Formal:** `scripts/check-formal.sh` 39 specs, 0 unexpected —
  including `ConsumerBinding` (pass) and `ConsumerBinding_Unchecked`
  (counterexample as documented) wired into `MANIFEST` this cycle.

## Deferrals carried in writing (unchanged from 0.10.0)

1. **PR #305** — `deps: bump jsonschema from 0.29.1 to 0.58.6`
   (dependabot). Not landing: 2 required checks fail and the jump is a
   major API surface change needing its own migration work. Same
   deferral text as the 0.10.0 audit applies.
2. **Issue #328** — `federation: root snapshots need a nonempty repos
   manifest` (WIP patch preserved in the issue). Coordination scope,
   explicitly not this release.

## Scope-outs (written down, not half-done)

Inherited from `docs/acceptance/FINDINGS-2026-10-11.md` §Open items:

1. Runtime-only coupling (service-mesh DNS, external registries,
   generated clients from private packages) — documented blind-spot class.
2. Scale beyond the tested band (500+ repos) — untested.
3. **README/positioning reframe** to the topology-server frame — naming
   work, product copy follows. Documentation/positioning scope, unchanged.

New this cycle:

4. **macOS lane coverage** — `hot_reload`'s load-stress test carries
   `#[cfg_attr(target_os = "macos", ignore)]`; lifting it needs one green
   macOS run (the lane runs on `main`). A coverage gap, not a false pass.
5. **Cross-process port TOCTOU in test fixtures** — the in-process
   `free_port` registry removes intra-binary collisions; a colliding
   external allocation now fails loudly with `EADDRINUSE` instead of
   flaking. Residual risk is bounded and loud.
6. **`list_unresolved` `candidates` for GraphQL consumers** lists every
   HTTP endpoint (name-grammar artifact) — cosmetic, documented in the
   query-engine truth-up, out of scope by design.

## What ships (v0.11.0)

Features required by Parallax (engine side): `services[].provides_graphql`,
full GraphQL bindings (`check_binding` with `graphql:op:field` keys +
`bindings_entry` YAML + `op_mismatch`/`field_mismatch` reasons),
`edges`/`paths` in `query_graph` results (documented 5-level tie-break),
query-engine truth-up (`find.id`, `{"not": …}` wire form, honest labels,
dead `meta.plan` removed). Fixes: ill-formed binding key pairs are a
config error (same-kind HTTP template bridges stay supported),
`config_hash` stability, `query-language.md` reconciled to the engine,
flake eradication, hermetic git-fixture tests, incremental mutation gate
in CI, `ConsumerBinding.tla` synchronized with the binding resolver.

## Process rule

Steps 1–2 of the release procedure are done (this record). Step 3
(`release/v0.11.0` branch + metadata bumps + `check-release-version.py`)
and everything after (PR to `main`, tag `v0.11.0`, publish, post-release
formula/badge updates) proceed only on the owner's explicit go.
