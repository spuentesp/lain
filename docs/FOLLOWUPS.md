# Follow-ups

Tracked work that comes out of PRs already merged to `dev` but
was deliberately deferred to keep those PRs small. Each entry
points at the source PR, the section of the plan it came from,
and a one-line scope summary.

Last update: 2026-09-16, refreshed against HEAD (past PR #66, #72,
#74, #75, #77, #88, plus PR A's cancellation work on
`feat/m4-cancellation-token` and PR B's spawn-blocking work on
`feat/m4-spawn-blocking`). Stays on `0.7.4-rc1`; release cut is
separate scope.

Four items originally logged here have been resolved and removed
from this file:

- **Cross-repo annotation routing** — fixed in `33f9373
  fix(annotations+readiness): address Copilot review findings`.
- **Cold repo `last_indexed_commit` serializing as `Some("0")`** —
  fixed in `6db4354 fix(federation): null last_indexed_commit until a
  successful index pass`.
- **Cooperative cancellation token** — fixed in PR A
  (`feat/m4-cancellation-token`, #88). Server-owned
  `CancellationToken` in `LifecycleInfo`; plumbed through every
  long-running phase; `await_startup_reindex` `select!`s
  `build_core_memory` against the token; stdio startup uses
  `cancel() + JoinHandle::await` within the existing 5-second
  budget; HTTP startup now retains its `JoinHandle`. New
  `index_cancelled` problem code.
- **`spawn_blocking` isolation** — fixed in PR B
  (`feat/m4-spawn-blocking`). New `src/server/ingest/blocking.rs`
  with `offthread(cancel, f)`. Libgit2 calls in
  `build_core_memory` (`get_latest_commit_info`,
  `get_changed_files_since`, `get_all_tracked_files`,
  `analyze_co_changes`) routed through the blocking-thread pool;
  `PerRepoReadiness::outstanding_files` wired through the
  federation watcher's receiver loop (inotify callback
  `fetch_add`s, receiver loop `fetch_sub`s).

The remaining M4 work — tree-sitter per-file work, ONNX inference,
and LSP subprocess calls — is open as a follow-up:

### Tree-sitter / ONNX / LSP-bridge migration (deferred)

- **Source:** AGENT_UX_ROADMAP.md Milestone 4 design §"Index
  execution and consistency" calls for routing all sync work
  through `spawn_blocking`. PR B migrated the libgit2 layer
  (the highest-impact per-pipeline calls); tree-sitter and ONNX
  inference are CPU-bound and the same pattern applies, but the
  LSP-bridge API is async (not sync) so its migration needs a
  different shape — exposing a sync subprocess entry point or
  wrapping the async call in `spawn_blocking` differently from
  the GitSensor pattern.
- **Status:** confirmed 2026-09-16, none of the three call sites
  (`scan_file_structure`'s tree-sitter calls at
  `src/server/ingest/scan.rs:195,204,260,398`; the
  `NlpEmbedder::embed`/`embed_batch` calls at
  `src/server/ingest/ingestion.rs:395,438` and `src/server/nlp.rs:219,246`;
  and the LSP multiplexer calls at
  `src/server/ingest/scan.rs:108,119`) are offthread-routed.
- **Why deferred:** the LSP-bridge migration in particular
  requires restructuring lsp-bridge's `async fn` API to expose a
  sync subprocess-call entry point. Tree-sitter and ONNX are
  simpler — drop the same `offthread` callsite shape — but
  they're per-file work that runs once per index, so the wall-clock
  win is smaller than the libgit2 layer PR B already landed.
- **Where to land:** separate PR off `dev` (or the next milestone
  pass). Acceptance: cold-boot wall-clock drops measurably on
  the canonical fixture; `RUST_LOG=trace` shows file-walk and
  tree-sitter phases on blocking threads, not on the async
  runtime.

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
