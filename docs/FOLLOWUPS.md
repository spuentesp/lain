# Follow-ups

Tracked work that comes out of PRs already merged to `dev` but
was deliberately deferred to keep those PRs small. Each entry
points at the source PR, the section of the plan it came from,
and a one-line scope summary.

Last update: 2026-09-19, refreshed against HEAD past the
2026-09-19 batch (PR #168 review-fix sweep, PR #169 / #170
`get_health` Bug #2 banner, PR #171 / #172 OTLP resolver
wiring). Closes the eight issues from the cross-agent code
review of PRs #151 / #153 / #154 / #165 — see the "Closed this
cycle" section below for the list. Stays on `0.7.4-rc1`;
release cut is separate scope.

Eight items from the 2026-09-19 review cycle were resolved and
removed from this file. They never landed here as open entries
because the review surfaced them fresh, but documenting them so
the next reviewer can see the cycle's full surface:

- **OTLP listener parsed spans but never wrote to the store**
  (PR #154 `acceptedSpans` lie) — fixed in PR #168: split
  response into `receivedSpans` / `storedSpans`; end-to-end
  test now pins both fields instead of just the 200 status.
- **`git_busy_since_nanos` atomic was dead state** (PR #165
  initialized to 0, never written) — fixed in PR #168: watchdog
  publishes a `SystemTime` nanosecond timestamp on free→held
  via CAS, clears on held→free, clears on exit.
- **`parse_otlp_json` doc/code mismatch** on span-kind
  fallback — fixed in PR #168.
- **`as i64` cast on `end_unix_nanos`** could silently wrap —
  fixed in PR #168: `u64::parse` + `i64::try_from`, returns
  `OtlpParseError::EndTime` on overflow.
- **`hex_len` accepted uppercase hex** — fixed in PR #168:
  OTLP IDs are lowercase per spec, uppercase is rejected and
  surfaced as a parse error. New `parse_rejects_uppercase_hex_ids`
  test pins the contract.
- **`get_all_tracked_files_works_with_relative_workspace_path`
  `set_current_dir` race** — fixed in PR #168: process-static
  `CWD_LOCK: Mutex<()>` serializes the test against any future
  cwd-manipulating test.
- **"Git sensor busy" `LainError::Other` string duplicated 4×**
  — fixed in PR #168: centralised in `git_sensor_busy_error()`
  helper in `src/server/ingest/ingestion.rs`.
- **OTLP listener tests used multi-thread tokio runtime**
  while the Bug #2 watchdog tests used `current_thread` —
  fixed in PR #168: aligned all three OTLP listener tests to
  `current_thread`.

Closed this cycle (six items that landed in this batch but
weren't previously tracked here):

- **Bug #2 hang not visible to MCP clients** — fixed in
  PR #169 / #170. `ToolContext.git_busy_since_unix_nanos`
  carries the live atomic; `get_health` emits a
  `⚠ Bug #2: GitSensor mutex held for {N}s — ...` banner when
  non-zero. Two regression tests pin both branches.
- **OTLP listener returned `storedSpans: 0` for every payload**
  — fixed in PR #171 / #172. New `SpanResolver` type alias
  + `no_resolver()` helper; `cli::server::run` wires
  `graph.find_node_by_name(span.name)` for the bound
  single-repo graph. Federation-aware resolution that walks
  every registered repo's graph and returns namespaced global
  ids is the next step — see the new entry below.

Five items originally logged here have been resolved and removed
from this file:

- **Cross-repo annotation routing** — fixed in `33f9373
  fix(annotations+readiness): address Copilot review findings`.
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
  subprocess calls onto `spawn_blocking`, same treatment the
  libgit2 calls got in PR `feat/m4-spawn-blocking` (#90) and
  the tree-sitter / ONNX calls got in
  `feat/m4-spawn-blocking-followup` (#98).
- **Upstream pin:** `lsp-bridge = "0.2"` (crates.io), upstream
  repo `ciresnave/lsp-bridge`. The async-only API on the
  `LspBridge` type is the blocker — see `bridge.rs:224`
  (`find_references`), `bridge.rs:717` (`get_document_symbols`),
  and `bridge.rs:159` (`open_document`). All three are
  `pub async fn` returning futures because the bridge drives the
  LSP child over stdio via Tokio.
- **Status:** **deferred — upstream blocker.** Our wrappers
  (`src/server/lsp.rs::get_references`,
  `::get_document_symbols_hierarchical`) call
  `bridge.find_references(...)` and `bridge.get_document_symbols(...)`
  directly, so the only way to expose a sync entry point that
  works on a blocking-thread is upstream in `lsp-bridge`.
  Wrapping the async call in `Handle::block_on` from inside
  `spawn_blocking` is the anti-pattern this whole initiative
  was designed to avoid — it would block a blocking-thread on
  the async runtime.
- **Where we land the upstream work:** the `ciresnave/lsp-bridge`
  repo. We need a sync variant of the three hot-path methods
  (`find_references_blocking`,
  `get_document_symbols_blocking`, and probably
  `open_document_blocking`) that internally drives its own
  Tokio runtime scoped to the blocking thread, OR exposes the
  raw stdio handle so a `spawn_blocking` closure on this side
  can drive it directly. The latter is cleaner.
- **What we did from this repo:** PR `feat/m4-lsp-cancel-aware`
  (#101) adds a `tokio::select!` race between each LSP `await`
  and the cancel token (`scan.rs:147-163` for `get_references`,
  `scan.rs:181-188` for `get_document_symbols_hierarchical`).
  When shutdown lands mid-scan, the LSP round-trip is abandoned
  promptly instead of waiting for the child to answer. That's
  a real cancellation-latency improvement — `RUST_LOG=trace`
  shows the LSP child stops being driven as soon as the token
  fires — but it does NOT move LSP onto the blocking-thread
  pool.
- **Migration on this side after upstream lands:** replace each
  call site (`scan.rs:161`, `scan.rs:186`, `ingestion.rs:826`,
  plus the inner `bridge.find_references` / `bridge.get_document_symbols`
  call sites in `src/server/lsp.rs:399` and `src/server/lsp.rs:318`)
  with the `_blocking` variant wrapped in
  `tokio::task::spawn_blocking(move || ...)`. The
  cancel-aware `tokio::select!` race moves to the *spawn* site,
  not the LSP round-trip itself.
- **Until then:** the LSP calls stay on the Tokio runtime, gated
  by the cancel-aware `tokio::select!`.

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

## From the runtime-trace / OTLP path (PR #171 / #172 deferred)

### Federation-aware OTLP resolver
- **Source:** PR #171 wired the OTLP listener's resolver to
  `graph.find_node_by_name(span.name)` against the bound
  single-repo graph. PR #172's CHANGELOG explicitly defers the
  federation case as a follow-up.
- **Status:** pending. Multi-repo federation calls land at the
  same listener but resolve against the staging placeholder,
  which is the same answer they got pre-fix — no regression, no
  improvement either.
- **What needs to happen:** build a federation-aware resolver
  closure that, for each span, walks every registered repo's
  `find_node_by_name`. Returned node_ids must be in **namespaced
  global form** (`repo_id:Kind:path:name`), not the local form
  the per-repo `GraphDatabase` uses, so `RuntimeTraceStore::ingest`
  mints a correctly-namespaced `RuntimeCall` edge. The local →
  global id mapping is `FederatedIndex::local_to_global` and
  is already wired; the missing piece is the resolver shape that
  walks it.
- **Where to land:** new `runtime_trace::federation_resolver`
  module that takes `Arc<FederatedIndex>` and returns a
  `SpanResolver`. `cli::server::run` swaps the
  single-graph resolver for this one when federation mode is
  active. StdIO single-repo mode keeps the simpler
  `find_node_by_name` resolver.
- **Acceptance:** federation-mode OTLP listener mints
  cross-repo `RuntimeCall` edges that `explain_dispatch` can
  surface. Span attribute `code.namespace` + `code.function`
  (the semconv keys the existing tests use) is the resolver
  input; the repo lookup can use `code.repo` or the OTLP
  resource's `service.name` attribute if either is present.

## From Bug #2 root-cause investigation (PR #174 follow-up)

### Production sidecar implementation
- **Source:** PR #174 prototype findings (`docs/notes/2026-09-19-sidecar-prototype-bench.md`,
  decision: GO) and architecture design (`docs/notes/2026-09-19-sidecar-architecture.md`).
- **Status:** in flight (Milestone 1 branch `feat/sidecar-proto-handshake`).
- **What needs to happen:** 8-milestone roadmap:
  1. Protocol version constant & handshake validation (`sidecar_proto.rs`).
  2. Child daemon hardening (`PR_SET_PDEATHSIG`, canonical paths, signals).
  3. Client supervisor (`SidecarGitSensor`, 3-in-30s respawn budget, timeouts).
  4. Unified `AnyGitSensor` abstraction & config flag.
  5. Plumb `AnyGitSensor` through `ingestion.rs`, watcher, and federation.
  6. Federation health reporting (`get_health` reporting `git_sensor.kind`).
  7. Binary distribution & packaging (release workflow, Homebrew, npm-shim).
  8. Soak testing & default flip to Sidecar.
- **Acceptance:** Simulated child hang does not block parent threads; child is auto-killed
  and respawned; all existing integration tests pass in both modes.

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
