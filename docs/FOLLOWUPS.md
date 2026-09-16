# Follow-ups

Tracked work that comes out of PRs already merged to `dev` but
was deliberately deferred to keep those PRs small. Each entry
points at the source PR, the section of the plan it came from,
and a one-line scope summary.

Last update: after PR #66 (M4 step 8 + annotations + CI enrichment)
merged to `dev`. Stays on `0.7.4-rc1`; release cut is separate
scope.

## From `docs/M4-step-8-plan.md` (PR #66 deferred)

### Cooperative cancellation token
- **Source:** `docs/M4-step-8-plan.md` §2 — plumb a
  `tokio_util::sync::CancellationToken` through every long-running
  phase (`RepoIndex::index`, `RepoSource::clone_into`,
  `Workspace::reload`, watcher, federation aggregate, MCP body
  collect).
- **Why deferred:** invasive refactor that touches every
  long-running phase. Doesn't fit the "easiest wins first"
  framing the user asked for.
- **Where to land:** next planning round. Should land BEFORE
  M4 can be marked ✅ — the design section requires this to
  pass the "blocked-indexer responsiveness test".
- **Acceptance:** TCP-RST mid-`/mcp` request returns control
  within budget; SIGINT triggers shutdown join within 30s;
  `cargo test --workspace` still passes.

### `spawn_blocking` isolation
- **Source:** `docs/M4-step-8-plan.md` §3 — new
  `src/server/ingest/blocking.rs` with an `offthread(cancel, f)`
  helper; batch the per-file hot loops.
- **Why deferred:** invasive — restructures every git / fs /
  parser call site.
- **Side benefit:** this PR will fill in the
  `PerRepoReadiness::outstanding_files` counter (currently
  always 0 — see `RepoIndex::outstanding_files` doc) and the
  live staleness resolver for `File`/`Symbol` targets (currently
  hard-coded `true` because the registry has no federation
  backend access).
- **Acceptance:** federation cold-boot wall-clock drops
  measurably on the canonical fixture; `RUST_LOG=trace` shows
  file-walk phases on blocking threads, not on the async
  runtime; no new panics, no lost cancellation.

## From the annotation layer (PR #66 deferred)

### Auto-include in `explain_symbol` / `get_blast_radius` markdown
- **Source:** PR #66 deferred items. The `summaries_for_targets`
  helper at `src/server/mcp/annotation_tools.rs` is in place; the
  wiring into the existing markdown bodies is the missing piece.
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

### Cross-repo annotation routing
- **Source:** PR #66 deferred items. `TargetSpec` already
  supports `kind: 'repo', repo_id: '...'`; `AnnotationRegistry`
  pins every row to the single registered repo so cross-repo
  targets silently fall into the wrong store.
- **Acceptance:** `add_annotation(target={kind:'repo', repo_id:'<registered>'})`
  lands in the right per-repo store; `list_annotations` returns
  it from a cross-repo query without filtering by repo; handoff
  notes cross federation boundaries.

### `tests/annotations_e2e.rs` and `tests/handoff_e2e.rs`
- **Source:** PR #66 deferred items. The unit tests in
  `src/server/annotations.rs` cover storage round-trip, body
  validation, filter, staleness, UTF-8 boundary truncation.
  The e2e tests ride on the same dispatcher wiring and were
  dropped as a smaller marginal addition.
- **Acceptance:** write→read→resolve→stale-detection flows
  exercised through the MCP dispatcher end-to-end (not just the
  storage layer); handoff flow exercised through
  register_agent → leave_handoff_note → unregister →
  re-register → get_pending_handoffs.

## Pre-existing, not introduced by PR #66

### Cold repo `last_indexed_commit` serializes as `Some("0")` instead of `None`
- **Source:** `db.get_last_commit()` in `src/server/graph.rs`
  returns `Some(string)` after the loader has run once, even
  when no commit was reached. Surfaces through
  `FederatedIndex::per_repo_readiness` and `get_capabilities`.
- **Acceptance:** `get_capabilities.repositories[].last_indexed_commit`
  is `null` until a successful index pass has reached a commit,
  then the actual commit string.

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
