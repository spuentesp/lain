# Follow-ups

Tracked work that comes out of PRs already merged to `dev` but
was deliberately deferred to keep those PRs small. Each entry
points at the source PR, the section of the plan it came from,
and a one-line scope summary.

Last update: 2026-09-16, refreshed against HEAD (past PR #66, #72,
#74, #75, #77). Stays on `0.7.4-rc1`; release cut is separate scope.

Two items originally logged here after PR #66 are now resolved and
have been removed from this file:

- **Cross-repo annotation routing** — fixed in `33f9373
  fix(annotations+readiness): address Copilot review findings`.
  `target_to_repo` in `src/server/mcp/annotation_tools.rs` now
  resolves `AnnotationTarget::Repo { repo_id }` against the
  federation registry and rejects unknown repos, instead of pinning
  every row to the single registered repo.
- **Cold repo `last_indexed_commit` serializing as `Some("0")`** —
  fixed in `6db4354 fix(federation): null last_indexed_commit until a
  successful index pass`. `FederatedIndex::per_repo_readiness` now
  gates `last_indexed_commit` and the wall-clock stamp on
  `indexed_signal`, returning `None` until a real index pass lands —
  see the comment at `src/server/federation/federated_index.rs:304`,
  which cites this file's old entry #6 as its acceptance criterion.

The two `docs/M4-step-8-plan.md` items below are still open; that
plan file itself was never committed to `dev` (PR #66 merged only its
implementation, not the planning doc), so there is no in-tree or
git-history copy to link back to — the acceptance criteria captured
here are the only surviving record of that plan's scope.

## From `docs/M4-step-8-plan.md` (no longer in the tree; PR #66 deferred)

### Cooperative cancellation token
- **Source:** the original `docs/M4-step-8-plan.md` §2 — plumb a
  `tokio_util::sync::CancellationToken` through every long-running
  phase (`RepoIndex::index`, `RepoSource::clone_into`,
  `Workspace::reload`, watcher, federation aggregate, MCP body
  collect).
- **Still not started:** confirmed 2026-09-16, no
  `CancellationToken` anywhere in `src/`.
- **Why deferred:** invasive refactor that touches every
  long-running phase. Doesn't fit the "easiest wins first"
  framing the user asked for.
- **Where to land:** next planning round. Should land BEFORE
  M4 can be marked ✅ — `docs/AGENT_UX_ROADMAP.md` milestone 4
  stays 🟡 specifically on this and the item below.
- **Acceptance:** TCP-RST mid-`/mcp` request returns control
  within budget; SIGINT triggers shutdown join within 30s;
  `cargo test --workspace` still passes.

### `spawn_blocking` isolation
- **Source:** the original `docs/M4-step-8-plan.md` §3 — new
  `src/server/ingest/blocking.rs` with an `offthread(cancel, f)`
  helper; batch the per-file hot loops.
- **Still not started:** confirmed 2026-09-16, no `blocking.rs` or
  `offthread` in `src/server/ingest/`.
- **Why deferred:** invasive — restructures every git / fs /
  parser call site.
- **Side benefit:** this PR will fill in the
  `PerRepoReadiness::outstanding_files` counter — confirmed
  2026-09-16 it is still always 0 (`AtomicU64::new(0)` in
  `src/server/federation/repo_index.rs`, never incremented).
- **Acceptance:** federation cold-boot wall-clock drops
  measurably on the canonical fixture; `RUST_LOG=trace` shows
  file-walk phases on blocking threads, not on the async
  runtime; no new panics, no lost cancellation.

## From the annotation layer (PR #66 deferred)

### Auto-include in `explain_symbol` / `get_blast_radius` markdown
- **Source:** PR #66 deferred items. The `summaries_for_targets`
  helper at `src/server/mcp/annotation_tools.rs` is in place; the
  wiring into the existing markdown bodies is the missing piece.
- **Still not started:** confirmed 2026-09-16, no `Open annotations`
  / `open_annotations` text anywhere in `src/`.
- **Why deferred:** the existing markdown bodies have many
  callers (the human-facing UI + several test fixtures pinning
  the prose shape); a follow-up that just adds the new section
  preserves the existing wire contract.
- **Acceptance:** `explain_symbol` markdown grows an
  `### Open annotations` section when any open annotation exists
  for the resolved symbol; `get_blast_radius` includes
  `open_annotations: [AnnotationSummary]` for visited symbols;
  existing UI tests still pass (the new section is appended,
  not inserted into the middle).

### `tests/annotations_e2e.rs` and `tests/handoff_e2e.rs`
- **Source:** PR #66 deferred items. The unit tests in
  `src/server/annotations.rs` cover storage round-trip, body
  validation, filter, staleness, UTF-8 boundary truncation.
  The e2e tests ride on the same dispatcher wiring and were
  dropped as a smaller marginal addition.
- **Still not started:** confirmed 2026-09-16, neither file
  exists under `tests/`.
- **Acceptance:** write→read→resolve→stale-detection flows
  exercised through the MCP dispatcher end-to-end (not just the
  storage layer); handoff flow exercised through
  register_agent → leave_handoff_note → unregister →
  re-register → get_pending_handoffs.

## Release flow

### Next release cut (separate scope)
- **Source:** `AGENTS.md` release policy. Per
  `docs/BRANCHING.md`, `dev → main` is release-only.
- **Trigger:** user decision. Either cut `v0.7.4` (rc1 is
  already solid on dev — only one PR has landed since) or jump
  straight to `v0.8.0` given the M4 step 8 + annotations + CI
  enrichment surface area.
- **Acceptance:** branch `release/v0.x.y` off dev; PR against
  main with admin bypass + green agent-contract; tag push
  triggers `release.yml`; fast-forward dev to main after
  merge.

## How to use this file

When a follow-up is taken on, cut a branch from `dev`, link it
back to the relevant entry above in the PR body, and update the
status line ("pending" → "in flight" → "done" with PR link) when
the work lands.
