# Follow-ups

Tracked work that comes out of PRs already merged to `dev` but
was deliberately deferred to keep those PRs small. Each entry
points at the source PR, the section of the plan it came from,
and a one-line scope summary.

Last update: 2026-09-16, refreshed against HEAD (past PR #66, #72,
#74, #75, #77, #88, plus PR A's cancellation work on
`feat/m4-cancellation-token`, PR B's spawn-blocking work on
`feat/m4-spawn-blocking`, PR E's tree-sitter+ONNX migration on
`feat/m4-spawn-blocking-followup`, and the LSP cancel-aware
work on `feat/m4-lsp-cancel-aware`). Stays on `0.7.4-rc1`;
release cut is separate scope.

Five items originally logged here have been resolved and removed
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
- **`spawn_blocking` isolation (libgit2 portion)** — fixed in PR B
  (`feat/m4-spawn-blocking`). New `src/server/ingest/blocking.rs`
  with `offthread(cancel, f)`. Libgit2 calls in
  `build_core_memory` (`get_latest_commit_info`,
  `get_changed_files_since`, `get_all_tracked_files`,
  `analyze_co_changes`) routed through the blocking-thread pool;
  `PerRepoReadiness::outstanding_files` wired through the
  federation watcher's receiver loop (inotify callback
  `fetch_add`s, receiver loop `fetch_sub`s).
- **Tree-sitter + ONNX migration** — fixed in PR E
  (`feat/m4-spawn-blocking-followup`). `scan_file_structure`
  batches the four tree-sitter calls into one
  `extract_tree_sitter_file` wrapped in `offthread`; the NLP
  prewarm and lazy-enrichment loops both route
  `NlpEmbedder::embed` through `offthread`. 6 new unit tests.

The only remaining M4 work is the LSP subprocess calls:

### LSP subprocess calls (deferred — upstream blocker)

- **Source:** AGENT_UX_ROADMAP.md Milestone 4 design §"Index
  execution and consistency" calls for routing the LSP
  subprocess calls (`scan.rs:108,119`, `ingestion.rs:639-657`)
  through `spawn_blocking`.
- **Status:** **deferred — upstream blocker.** `lsp-bridge`
  0.2's `LspMultiplexer::get_references` /
  `get_document_symbols_hierarchical` are `async fn` returning
  futures; the only way to expose a sync subprocess entry
  point is upstream in `lsp-bridge`. Wrapping the async call
  in `Handle::block_on` from inside `spawn_blocking` is the
  anti-pattern this whole initiative was designed to avoid
  (it would block a blocking-thread on the async runtime).
- **What we did from this repo:** PR
  `feat/m4-lsp-cancel-aware` adds a `tokio::select!` race
  between the LSP `await` and the cancel token. When shutdown
  lands mid-scan, the LSP round-trip is abandoned promptly
  instead of waiting for the child to answer. That's a real
  cancellation-latency improvement — `RUST_LOG=trace` shows
  the LSP child stops being driven as soon as the token
  fires — but it does NOT move LSP onto the blocking-thread
  pool.
- **Where to land:** separate upstream PR in `lsp-bridge`
  adding a sync `get_references_blocking` /
  `get_document_symbols_blocking` API on `LspMultiplexer`.
  Once that's released, this repo's migration is a small
  call-site change. Until then, the LSP calls stay on the
  Tokio runtime, gated by the cancel-aware `tokio::select!`.

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
