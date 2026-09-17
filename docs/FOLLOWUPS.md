# Follow-ups

Tracked work that comes out of PRs already merged to `dev` but
was deliberately deferred to keep those PRs small. Each entry
points at the source PR, the section of the plan it came from,
and a one-line scope summary.

Last update: 2026-09-16, refreshed against HEAD (past PR #66, #72,
#74, #75, #77). Stays on `0.7.4-rc1`; release cut is separate scope.

Four items originally logged here have been resolved and removed
from this file:

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
- **Cooperative cancellation token** — fixed in PR A
  (`feat/m4-cancellation-token`, #88). Server-owned
  `CancellationToken` in `LifecycleInfo`; plumbed through every
  long-running phase; `await_startup_reindex` `select!`s
  `build_core_memory` against the token; stdio startup uses
  `cancel() + JoinHandle::await` within the existing 5-second
  budget; HTTP startup now retains its `JoinHandle`. New
  `index_cancelled` problem code.
- **`spawn_blocking` isolation (libgit2 portion)** — fixed in PR B
  (`feat/m4-spawn-blocking`, #90). New `src/server/ingest/blocking.rs`
  with `offthread(cancel, f)`. Libgit2 calls in
  `build_core_memory` (`get_latest_commit_info`,
  `get_changed_files_since`, `get_all_tracked_files`,
  `analyze_co_changes`) routed through the blocking-thread pool;
  `PerRepoReadiness::outstanding_files` wired through the
  federation watcher's receiver loop (inotify callback
  `fetch_add`s, receiver loop `fetch_sub`s).

The third `docs/M4-step-8-plan.md` item (tree-sitter / ONNX
migration) was originally deferred to a follow-up; PR E
(`feat/m4-spawn-blocking-followup`) lands the tree-sitter and
ONNX portions. The LSP subprocess portion remains open as the
only outstanding piece of the original M4 execution-isolation
design.

## From `docs/M4-step-8-plan.md` (no longer in the tree; PR #66 deferred)

### Cooperative cancellation token
- **Source:** the original `docs/M4-step-8-plan.md` §2 — plumb a
  `tokio_util::sync::CancellationToken` through every long-running
  phase (`RepoIndex::index`, `RepoSource::clone_into`,
  `Workspace::reload`, watcher, federation aggregate, MCP body
  collect).
- **Status:** **resolved in PR A** (`feat/m4-cancellation-token`,
  #88). Server-owned `CancellationToken` in `LifecycleInfo`;
  plumbed through every long-running phase; `await_startup_reindex`
  `select!`s `build_core_memory` against the token; stdio startup
  uses `cancel() + JoinHandle::await` within the existing 5-second
  budget; HTTP startup now retains its `JoinHandle`. New
  `index_cancelled` problem code. Acceptance met: TCP-RST
  mid-`/mcp` returns control within budget; SIGINT triggers
  shutdown join within 30s; `cargo test --workspace` passes.

### `spawn_blocking` isolation
- **Source:** the original `docs/M4-step-8-plan.md` §3 — new
  `src/server/ingest/blocking.rs` with an `offthread(cancel, f)`
  helper; batch the per-file hot loops.
- **Status:** **libgit2 + `outstanding_files` resolved in PR B**
  (`feat/m4-spawn-blocking`, #90). Tree-sitter + ONNX resolved in
  PR E (`feat/m4-spawn-blocking-followup`). LSP subprocess calls
  remain open — see the dedicated entry below.
- **Acceptance met for resolved portions:** libgit2 calls
  (`get_latest_commit_info`, `get_changed_files_since`,
  `get_all_tracked_files`, `analyze_co_changes`) route through
  `offthread`; tree-sitter `extract_definitions` /
  `extract_refs` / `extract_strings` route through `offthread`
  via a batched helper (`extract_tree_sitter_file`); ONNX
  `NlpEmbedder::embed` calls in the NLP prewarm and lazy
  enrichment paths route through `offthread`; `outstanding_files`
  is wired through the federation watcher's receiver loop;
  `cargo test --workspace` passes (891 lib + ~200 integration).

### Tree-sitter / ONNX migration
- **Source:** the original `docs/M4-step-8-plan.md` §3 — tree-sitter
  per-file calls (`scan.rs:195,204,260,398`), ONNX inference
  (`nlp.rs:219,246`, `ingestion.rs:395,438`).
- **Status:** **resolved in PR E** (`feat/m4-spawn-blocking-followup`).
  `scan_file_structure` now batches all four tree-sitter calls into
  one `offthread(cancel, ...)` call returning the
  `TreeSitterFile { defs, static_refs, pattern_refs }` aggregate;
  `add_tree_sitter_definitions` (the LSP-fallback path) is now
  `async` and runs its `extract_definitions` via `offthread`;
  the NLP prewarm and lazy-enrichment loops both route
  `NlpEmbedder::embed` through `offthread`. `apply_attribute_labels`
  now takes a pre-computed `&[SymbolDef]` (sync helper); the
  offthread boundary lives at the caller side. 3 unit tests in
  `scan::offthread_tests`, 3 in `ingestion::nlp_offthread_tests`.

### LSP subprocess calls
- **Source:** the original `docs/M4-step-8-plan.md` §3 — LSP
  subprocess calls (`scan.rs:108,119`, `ingestion.rs:639-657`).
- **Status:** open. `lsp-bridge`'s `LspMultiplexer::get_references`
  / `get_document_symbols_hierarchical` are `async fn` returning
  futures, not `Send + 'static`-friendly in the same way as the
  libgit2 calls. Routing through `spawn_blocking` requires either
  exposing a sync subprocess entry point in `lsp-bridge`
  (upstream changes) or wrapping the async call in a
  `spawn_blocking` block-on (which blocks a blocking-thread on
  the async runtime — the anti-pattern this whole initiative was
  designed to avoid).
- **Where to land:** separate PR after the upstream lsp-bridge
  sync entry-point lands (or via a different migration shape).
- **Acceptance:** federation cold-boot wall-clock drops
  measurably on the canonical fixture; `RUST_LOG=trace` shows
  LSP phases on blocking threads, not on the async runtime; no
  new panics, no lost cancellation.

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
