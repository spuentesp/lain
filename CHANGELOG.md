# Changelog

All notable changes to LAIN are documented here. Versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **Built-in parsers for every supported language.** Go, Java, C, C++,
  C#, Ruby, Swift, Kotlin, Scala and PHP join Rust, Python and
  JavaScript/TypeScript, and Vue/Svelte `<script>` blocks are parsed in
  place. Each gets definitions and call/type edges with no language
  server installed; before, these languages depended entirely on an
  installed server, and even then produced no call edges. Queries are
  compiled once per language instead of once per file.
- **`lain setup --lsp none|detected|LANG,...`.** Setup detects languages
  from tracked files, reports each one's optional language server, and
  installs only the ones picked (interactively, or by this flag). Nothing
  is installed by default, and `--yes` does not imply any.

### Changed

- `get_health` lists only the languages in the repository, with the
  built-in parser and whether the optional server is installed, instead
  of every registered server marked "Missing".
- Calls outside any named definition (test callbacks, `__main__` blocks,
  RSpec blocks) are attributed to their file instead of being dropped.
- tree-sitter 0.22 → 0.25 and current grammars. Building from source
  needs Rust 1.89: the dependency tree already needed 1.88 (the README
  said 1.75), and the presence lock uses std file locks (1.89).

### Fixed

- Who-calls accuracy, measured on 30 symbols per language against a
  textual oracle with every disagreement reviewed by hand
  (`scripts/acceptance/breadth.py`): all 14 languages now agree. Fixed
  along the way:
  - tracked files matching a `.gitignore` pattern were never indexed
    (cJSON ignores `test`, dropping 40-odd tracked files), and edits to
    them were ignored by the watcher;
  - JavaScript `obj.method = function …` / `exports.fn = …` definitions
    (all of Express's `app`/`res` API) were not indexed;
  - Rust calls inside macros (`format!`, `assert_eq!`, `vec!`, …);
  - C/C++ calls inside `#define` bodies; `ns1::ns2::f()` and
    `ns::f<T>()` calls; definitions after an attribute macro on its own
    line (`CXXOPTS_NODISCARD`);
  - C# files with `#if` inside an expression parsed as one error and lost
    every method after it;
  - Swift implicit-member calls (`.basicAuth()`);
  - a method named like a builtin (`$this->assert(…)`) lost its calls;
  - Ruby core methods (`include?`, `each_value`, …) called on other
    objects linked to same-named repo methods;
  - a call never resolves to a module (`mod tangle;` hid `fn tangle`),
    and symbol tools prefer the symbol over a same-named module;
  - `get_call_sites` skipped any line starting with `fn ` as a Rust
    definition, including Python's `fn = guess_filename(v)`.
- Semantic search ranked a history-dependent subset of the code: the
  background embedding pass stopped after 20 symbols (one pass per build,
  budget 20), and the rest were embedded only as a side effect of
  queries, 200 at a time. The pass now covers every symbol (about 20 s
  for psf/requests), `get_capabilities` reports `semantic_search` as
  `warming_up` with `indexing.embeddings` progress until it finishes,
  and `search_code` notes a partial index in its answer.
- `lain server` indexed every repo before it started listening, so it
  answered nothing — not even `/health` — for seconds (rust-analyzer
  installed) to minutes (large federations). Indexing now runs in the
  background; `find_symbol`, `search_code`, `get_health` and
  `understand_repository` answer at once with a "still indexing" note,
  and graph tools return the existing `warming_up` response until the
  repo is ready.
- `get_audit_log` returned the whole audit window (40 MB on a long-lived
  install) and parsed it on every call. It now returns the most recent
  `limit` events (default 200), scanning from the end. Its arguments, and
  `get_recent_activity`'s, are now declared in the tool schema.
- The audit log is shared by every workspace on the machine, and each
  repo's `get_audit_log` listed every other repo's edits. Events now
  record their workspace, and both audit tools read only their own.
- `LspPool` clones did not share the round-robin counter their comment
  said they shared, so clones kept picking the same language server.
- Re-indexing a directory added a second copy of its `Namespace` node,
  splitting its edges and making `lain doctor` reject the saved graph as
  corrupt ("graph index does not match its nodes"). The copy is now
  refreshed in place, and graphs already on disk are repaired on load.
- `lain doctor` reported the graph stale right after `lain setup`, which
  writes `.mcp.json` into the repository. Only changes to files the index
  reads count now.
- A call through another object (`session.request(...)` inside requests'
  module-level `request()`) resolved to the caller itself and was
  dropped, so `Session.request` lost its main caller.
- `get_call_chain` searched from one arbitrary definition of an ambiguous
  name and answered "no path"; it now searches from all of them and says
  which one the path starts at.
- Kotlin: `!isDone()` / `-offset()` calls were missed (the grammar parses
  them as a call of the prefix expression).
- JavaScript/TypeScript: `export const useCart = defineStore(...)` (Pinia
  stores, composables, `styled.x`, slices) is indexed, so its callers are
  found.
- Presence claims could fail with "state-file lock not acquired" when
  several agents in one server contended: the polled lock let a waiter
  lose every race until the deadline. Threads of one process now queue
  fairly before polling it.
- A workspace reached through a symlink (every macOS temp dir: `/var` →
  `/private/var`, or a linked checkout) got absolute paths in every node
  id, so cross-repo edges never matched. Paths are now compared in
  canonical form when the literal prefix does not match.
- The git sidecar's socket path could exceed the Unix limit under a long
  `$TMPDIR`, and the sidecar died with only "exited prematurely". It
  falls back to `/tmp`.
- The sidecar and the stdio MCP probe are asked to exit before being
  killed; the immediate kill interrupted their exit (and truncated
  coverage profiles in CI).
- The Windows release packaging script passed a `D:\...` path to GNU
  tar, which read it as a remote host.
- `scripts/acceptance/run.py` checks the README's claims end to end
  against pinned open-source repositories — who-calls answers in every
  listed language, the single-repo flow, and the multi-repo flow.

- `lain mcp` minted node ids under a random per-process namespace: ids
  changed on every restart, and co-change edges (derived under the test
  namespace) were silently dropped, so the co-change radar, `find_related`
  partners and `explain_dispatch` co-change evidence were always empty.
  Ids now derive from the workspace path; old graphs are rebuilt once.
- Co-change partners were read in one direction only, hiding every
  partner whose path sorts before the queried file's.
- Federation: calls from a repo indexed before the repo it calls into
  never linked (cobra → pflag: 0 edges). A cross-repo link pass now runs
  once every repo is indexed, and on every start. Name-only links also
  require the calling repo to declare a dependency on the target in its
  manifests, removing links like pflag's `fmt.Println` → cobra.
- `lain workspaces create` wrote the workspace list over `repos.yaml`,
  deleting every repo; workspaces now go to `workspaces.yaml` beside it.
- `lain repos add` defaulted `--ref` to `main`; it now asks the remote
  for its default branch (the README's tokio-rs examples use `master`)
  and confirms what it added.
- A corrupt or foreign `graph.bin` panicked (`capacity overflow`) in
  `oneshot`, `query` and `doctor`; decoding is now bounded and the graph
  is rebuilt.
- `search_code` without an embedding model matched the whole query as
  one substring, so multi-word queries found nothing; it now matches by
  words (snake_case, camelCase, plurals) and shows symbol names.
- `understand_repository` reported `semantic_search: ready` with no model.
- `lain_intent` advertised `scopes` / `add_scopes` / `remove_scopes` as
  strings but required arrays, so schema-following calls failed.
- Unscoped `get_health` on a multi-repo server errored; it now reports
  the federation and each repo.
- `dict.update()` / `arr.push()`-style calls on other values no longer
  link to the one user-defined method of that name in another file;
  Python `@overload` stubs are not definitions; overloads in one
  Java/C#/Kotlin/Scala/Swift/C++ file receive calls from other files.
- `install.sh` under `curl | bash` never added lain to `PATH`; it now
  asks on the terminal (unchanged when there is none).
- `scripts/demo.sh` runs against the full tool profile and covers the
  intent tools; `make schema` names the binary.
- TypeScript is parsed with the TypeScript grammar; exported, method and
  arrow-function definitions are indexed in JS/TS.
- Python methods and decorated definitions are indexed.
- The first session on a new repository answered "no callers" for every
  symbol until restart (graph clones did not share their id/path
  indices).
- Rust string literals were never extracted for boundary detection
  (the shared `(string)` query does not match Rust's `string_literal`).

- **Intent & observability layer** (PRs 1–6 of
  `docs/archive/INTENT_AND_OBSERVABILITY_PLAN.md`). Two new MCP tools:
  `lain_intent` (declare / update per-agent goal + scopes) and
  `list_active_intents` (per-agent activity feed). New
  `POST /hook` endpoint that ingests tool-call observations from
  the agent's host-specific hook layer (Claude Code PostToolUse,
  AGY pre-edit, etc.). Three-level coordination engine that
  surfaces GREEN / YELLOW / RED with structured reasons and related
  activity. The `lain setup --agent claude` command now writes a
  three-sentence system prompt to `.lain/PROMPT.md` that the
  operator copies into their agent's startup-context file. New
  `scripts/e2e_full.sh` drives a real `lain server` through 46
  scenarios across intent lifecycle, activity observation,
  evaluation, persistence, cross-agent, error paths, and doc
  accuracy. Linearizability stress test
  (`tests/coordination_linearizability.rs`): 100 iterations × 4
  racers + N=10 + release/reclaim cycle, plus the AGY
  end-to-end harness (`scripts/agy_e2e.sh` → `verdict.json`).
  `unregister_agent` MCP tool added; releases every claim and
  drops the agent's intent + activity entries. Existing
  presence + occupancy snapshot file extended with
  `intents` / `activities` arrays (`#[serde(default)]` for
  backward-compat). Linearizability invariant preserved: RED
  always wins over YELLOW peer-reading, file-lock fail-closed
  continues to be the source of truth for exclusive ownership.

- **LSP cold-boot prewarm.** On every server start, each language
  server the workspace actually uses gets one warm-up
  `documentSymbol` call against a sentinel file (largest by
  mtime, bounded by `lsp_prewarm_max_files`) before `build_core_memory`
  reaches the scan batch. Cold-cache `rust-analyzer` / `clangd` no
  longer trip the runtime 1 s circuit breaker on the first real
  call (introduced in #112), so they stay out of tree-sitter-only
  fallback. New `IngestionConfig` knobs: `lsp_prewarm_timeout_secs`
  (default 30), `lsp_prewarm_max_files` (50), `lsp_prewarm_opt_out`
  (false). Readiness.phase advances through a new `PrewarmingLsp`
  state so operators see warm-up progress via `get_capabilities`
  and the Command Center. The prewarm path is fully isolated from
  the runtime circuit breaker — a slow / failing prewarm never
  marks a binary `unavailable`. Per-language opt-out via
  `lsp_prewarm_skip_extensions` (Vec<String>; default empty) for
  monorepos that mix languages whose LSPs are intentionally
  unavailable.

- **Batched `install_language_server` with auto-detect.** The
  existing single-install tool now accepts a batched `extensions`
  array, plus a special `"auto"` entry that expands to every
  language the workspace's tracked files use. Response carries a
  per-extension outcome (`installed`, `already_installed`,
  `unknown_ext`, `no_install_cmd`, `failed`) so an agent can
  decide whether to retry without parsing free-text errors.
  Idempotent: any binary already on PATH reports
  `already_installed` without spawning a duplicate install
  command. Backward compatible — passing `{language: "rust"}`
  works exactly as before. The `"auto"` entry now returns a
  typed `LainError::Config` instead of silently empty when the
  workspace isn't a git repository.

- **Semantic-default tool profile.** `tools/list` now returns the
  curated 14-tool semantic surface by default — the M5 bootstrap
  (`understand_repository`), the M6 high-level Agent API
  (`find_symbol`, `get_context`, `find_related`, `assess_change`,
  `search_code`), readiness + self-discovery (`get_health`,
  `get_capabilities`), and the multiplayer essentials
  (`register_agent`, `heartbeat`, `claim_files`, `release_files`,
  `get_world_state`) — with `get_agent_strategy` kept as an
  on-demand escape hatch to the full list. Federation, workspace,
  and server-status families are visible when in their respective
  modes. The legacy 79-tool surface is reachable via
  `LAIN_TOOL_PROFILE=full`. Active profile is exposed through
  `get_capabilities.tool_profile` so agents self-discover which
  filter is in effect at startup. The on-disk
  `docs/tool-schema.json` is unchanged: schema-drift CI still
  validates the fully-populated shape, only the runtime wire
  shrinks.

- **LSP prewarm visibility + operator knobs.** `GET /health` now
  includes `lsp_prewarm: { binary: { status, ms?, reason? } }` —
  one entry per LSP binary that ran warm-up, with status
  `warmed` / `timed_out` / `failed` / `skipped_no_sentinel` /
  `skipped_unavailable` (snake_case). `lain doctor --json` reports
  the active `tool_profile.{name, advertised_count}` and the
  resolved `lsp_prewarm.{timeout_secs, max_files, opt_out,
  env_opt_out}`. Federation and workspace modes are detected from
  disk (`repos.yaml` multi-repo / `workspaces.yaml` present), so
  the offline `doctor.json` snapshot matches what the live server
  reports on `tools/list`. Workspace-aware advertised_count is
  exact under all modes: the workspace handle is plumbed from
  `LainMcpServer` into `ToolContext` so `get_capabilities`
  reflects the live wire; doctor reads `workspaces.yaml` from disk
  to compute the same count offline.

- **README TL;DR + quickstart-tools callouts.** README's "See
  it run" lists cold-boot prewarm; "What can AI Agents ask LAIN?"
  gained sections 7 (prewarm) and 8 (curated profile); "TL;DR —
  Install in 30 Seconds" gained an `Operator knobs` table covering
  `LAIN_TOOL_PROFILE`, `LAIN_LSP_PREWARM`, and the three prewarm
  tunables with `lain doctor --json` as the diagnostic surface.
  `docs/quickstart-tools.md` was updated alongside.

- **Dynamic-dispatch mitigation (three tiers).** Tree-sitter + LSP
  cannot follow dynamic dispatch (message buses, DI containers,
  schema-driven routers, reflection). The static graph returned
  empty `Calls`/`Uses` lists for any caller routed through those
  patterns, and `get_blast_radius` reported "no dependents found"
  even when the application depended on the function. Three new
  layers close the gap:

  - **Tier 1 — protocol composition.** `get_agent_strategy` now
    teaches the agent to compose `get_blast_radius` with
    `get_coupling_radar`, `find_anchors`, `trace_dependency`, and
    `explain_dispatch` rather than trust a single tool. The
    pre-commit hook (`hooks/claude-code/pre-commit.sh`) now runs
    `LAIN_SMOKE_CMD` (with `LAIN_SMOKE_TIMEOUT_SECS`) when set,
    so the smoke gate is the catch-all for changes the static
    graph cannot reason about. New `docs/dynamic-boundaries.md`
    template for per-repo registration of message buses, DI
    containers, and routers.

  - **Tier 2 — heuristic edges.** New `EdgeType` variants
    `DynamicDispatch`, `BusTopic`, `RouteMatches`. New
    `EdgeProvenance { Static, Heuristic { detector, confidence },
    Runtime { trace_id, last_seen_unix } }` carried on every
    `GraphEdge`. New `dynamic_dispatch_sensor` walks the
    workspace and emits heuristic edges from matching files to
    deterministic `Hub:<detector>` synthetic nodes, with per-
    detector confidence constants (message_bus=0.7,
    container=0.6, schema_router=0.5, reflection=0.4).
    `get_blast_radius` gained `include_weak_edges` and honours
    `LAIN_HEURISTIC_MIN_CONFIDENCE` (default 0.5). Below-threshold
    edges stay out of the default view.

  - **Tier 3 — runtime synthesis.** New `explain_dispatch` tool
    (in the curated 15-tool `Semantic` profile) returns
    `{verdict, static_callers, heuristic_callers, runtime_callers,
    co_change_partners}` per symbol. The `verdict` field
    (`static_only`, `heuristic_only`, `runtime_only`,
    `runtime_confirmed`, `insufficient_evidence`) is the single
    string an agent should route on. The `runtime_trace`
    module (`src/server/runtime_trace/`) carries the
    `RuntimeTraceStore` API surface; the OTLP gRPC adapter
    itself is deferred to a follow-up PR.

- **Backfill CLI.** `lain hooks backfill-heuristics
  --workspace <path> [--graph <path>] [--dry-run]` re-runs the
  heuristic sensor against an existing `.lain/graph.bin` without
  a full re-index of static edges. Idempotent: deterministic UUID
  v5 identifiers mean re-runs add zero new edges. Use after
  upgrading lain that predates Tier 2.

- **Awareness doc coverage.** All twelve agent awareness files
  under `hooks/` (`claude`, `claude-code`, `cursor`, `codex`,
  `cline`, `copilot`, `gemini`, `kimi`, `agy`, `windsurf`,
  `opencode`, plus `kimi/skills/lain/SKILL.md`) gained a
  "Dynamic Dispatch Caveat" section that links back to
  `docs/dynamic-dispatch.md` in the lain repo.

- **Umbrella doc.** `docs/dynamic-dispatch.md` describes the
  three tiers end-to-end, documents the new env vars
  (`LAIN_HEURISTIC_MIN_CONFIDENCE`, `LAIN_TRACE_TTL_SECS`,
  `LAIN_TRACE_MAX_EDGES`), and lists what the mitigation does
  not solve. Linked from `docs/INDEX.md`.

- **Polished.** Removed dead `_force_use` helper from
  `explain_dispatch`; tightened `NodeType` import scope;
  `resolve_node` rejects empty handles explicitly so the new
  empty-name `File` nodes (source of heuristic edges) cannot
  be silently matched by `find_node_by_name("")`.

- **OTLP listener startup hook.** `cli/server.rs` now binds the
  runtime-trace OTLP HTTP listener at startup, gated by
  `LAIN_TRACE_RUNTIME=true` and a configurable
  `LAIN_TRACE_OTLP_PORT` (default 4318, the OTLP/HTTP convention).
  The listener runs on its own `TcpListener`, completely separate
  from the MCP transport, so trace ingestion never shares a port
  with the agent-facing API and never competes for MCP request
  budget. Bind failure is downgraded to a startup warning — the
  server still comes up with tracing disabled rather than failing
  the boot — so an OTLP port collision on a host running other
  observability tooling no longer wedges the MCP entry point.

- **`get_blast_radius` empty-graph contract pinned.** A fresh,
  never-indexed graph now produces a structured `NotFound` from
  `get_blast_radius` rather than a confident "no callers" report.
  The handler walks incoming edges, so a zero-node graph would
  otherwise take the BFS loop's early-exit and report a symbol as
  "isolated" — wrong, because the symbol is missing, not
  unconnected. `resolve_node` already detects the empty-graph
  branch and points the agent at `get_health` and federation
  fallback; the new test in `tools::handlers::impact` pins that
  contract so a future refactor that drops the branch fails loud
  instead of silently regressing.

- **`explain_dispatch` NotFound contract pinned.** A missing
  symbol now surfaces as `NotFound` from `explain_dispatch`
  rather than a confident `verdict: "insufficient_evidence"`
  report. `explain_dispatch` resolves the symbol up-front via
  `resolve_node`, so a future refactor that replaces the
  early-return with a fall-through to `build_with_store` would
  emit a confident "no callers" answer for a symbol that doesn't
  exist at all. The new test in
  `tools::handlers::explain_dispatch` pins the up-front-resolve
  contract so that regression fails at `cargo test` time.

- **Bug #2 sidecar prototype (decision: GO).** Throwaway
  prototype that hosts `git2::Repository` in a child process
  and answers libgit2 calls over a Unix domain socket
  (`src/bin/lain-git-sidecar.rs` + `src/bin/sidecar_bench.rs`).
  Wire protocol in `src/sidecar_proto.rs`. Benchmark at
  1000 iters on the lain repo itself: **average p95 IPC
  overhead = 10.3 µs** across the 5 `GitSensor` methods called
  from `build_core_memory`'s offthread closures. Verdict from the
  plan's decision tree: **GO** (< 500 µs avg p95). Findings
  benchmark write-up (retired from the current docs; preserved in Git history).
  Full architecture design (the production shape, schema
  versioning, child lifecycle, federation health surface) at
  the sidecar architecture record (retired from the current docs; preserved in
  Git history). Next cycle:
  implement `SidecarGitSensor` and the `AnyGitSensor` enum as
  drop-in replacements for `Arc<Mutex<GitSensor>>`, default
  mode stays `InProcess` until soak-tested.

- **Bug #2 sidecar protocol versioning & handshake (Milestone 1).**
  Added `PROTOCOL_VERSION: u32 = 1` constant and mandatory
  `Request::Handshake` / `Response::HandshakeAck` / `Response::HandshakeNack`
  frames to `src/sidecar_proto.rs`. `lain-git-sidecar` now requires
  the handshake as its initial message on connection and rejects
  mismatched protocol versions before processing operational git
  requests. Fixed `lain-git-sidecar` listener loop to cleanly exit
  on `Request::Shutdown` so parent process wait completes immediately.

- **Bug #2 sidecar daemon hardening (Milestone 2).**
  Hardened `lain-git-sidecar` with Linux `PR_SET_PDEATHSIG` (auto-exit
  on parent termination to prevent zombie child processes), async-signal-safe
  socket cleanup on `SIGINT` / `SIGTERM`, RAII `SocketCleaner` guard,
  and canonical path resolution via `dunce::canonicalize`.

- **Bug #2 sidecar client supervisor & lifecycle management (Milestone 3).**
  Added `SidecarGitSensor` and supervisor engine (`src/sidecar.rs`),
  implementing transparent child process spawning, per-call timeouts
  (2s), force-kill on hang, automatic crash recovery with a single retry,
  rolling respawn budget (max 3 retries in 30s) returning `LainError::Unavailable`
  on exhaustion, and diagnostic `SidecarHealth` reporting. End-to-end lifecycle
  suite (`tests/sidecar_lifecycle.rs`) validates normal operations, auto-recovery
  on `kill -9`, and budget exhaustion without server deadlock. Added `Request::IsIgnored`
  to complete the full 7-method `GitSensor` surface.

- **Federation drain contract pinned.** An edge whose source is
  in the local index but whose target is missing now reliably
  ends up in `take_pending_external_edges` instead of being
  counted as dropped. The companion to
  `insert_edges_batch_reports_dropped_count_for_orphan_edges`
  pins the positive arm — the federation's `project_repo`
  drains that queue after the intra-repo edge pass to emit the
  edge to the federated backend, so silently dropping it would
  break cross-repo projection with no operator-visible signal
  (the dropped counter would not move). Also asserts the queue
  starts empty (so a leaking earlier test cannot pollute this
  one) and that `take_pending_external_edges` drains
  (`take_*`, not `peek_*`) so a second call returns empty.

- **Bug #2 hang watchdog.** `LainServer` now spawns a tokio task
  that probes the parking_lot `GitSensor` mutex with `try_lock`
  every 5 s. If the mutex has been continuously held for longer
  than `LAIN_GIT_SENSOR_BUSY_THRESHOLD_SECS` (default 30 s, well
  below the 60 s `index_timeout()` budget), the watchdog emits a
  single `tracing::warn!` per hold with elapsed time and a
  pointer to `scripts/debug-hung-server.sh`. Companion to the
  `try_lock` mitigation in `build_core_memory` — the mitigation
  breaks the cascade; the watchdog surfaces the hang earlier so
  operators see it at ~30 s instead of waiting the full 5-minute
  budget. Threshold is env-tunable. The watchdog honors the
  server-owned cancellation token and clears its shared atomic
  on exit so a server restart doesn't carry a stale timestamp.
  Two regression tests pin both branches (`_warns_when_mutex_held_past_threshold`
  and `_stays_silent_when_threshold_not_crossed`), and the loop
  is extracted into a generic `run_git_sensor_watchdog` free
  function so it can be tested with `Mutex<()>` without
  constructing a real `GitSensor` on disk.

- **OpenSSF Scorecard `Signed-Releases` status documented.** The
  `docs/SCORECARD.md` plan now reflects that `release.yml`
  already runs `cosign sign-blob --yes --bundle ...` with
  keyless OIDC on every per-platform build (PR #84) and
  attaches the bundle to the GitHub Release alongside the SLSA
  provenance, in-toto attestation, SBOM, and SHA256 side-file.
  The remaining 2/10 score on `Signed-Releases` is a
  file-extension mismatch — Scorecard looks for
  `*.sig`/`*.asc`/`*.pem`/`*.gpg`, but cosign v3 writes
  `*.cosign.bundle.json`. The signature is real and
  verifiable; only the filename extension is wrong. Two paths
  forward are documented (extract the raw signature with
  `jq -r '.messageSignature.content' | base64 -d > tarball.sig`,
  or switch to cosign v2 with `--output-signature`); both
  touch `release.yml` and should land as their own PR with a
  dry-run review before the next release ships.

- **Bug #2 hang now visible in `get_health`.** The parking_lot
  `GitSensor` watchdog spawned by `IngestHandle::start_git_sensor_watchdog`
  publishes a wall-clock nanosecond timestamp on the free→held
  transition (CAS, so only the first observer wins) and clears
  it on the held→free transition. Until now that atomic was
  observable only via `tracing::warn!`, which a stdio MCP client
  can't surface. `ToolContext` now carries the live atomic
  (cloned via `LainMcpServer::with_server`), and `get_health`
  appends a `⚠ Bug #2: GitSensor mutex held for {N}s — ...`
  banner whenever it's non-zero. Alertmanager can match the
  literal `Bug #2` without parsing prose; dashboards can
  graph the elapsed-seconds field via a regex. New helper
  `ToolExecutor::git_busy_since_banner` is unit-tested for
  both the free-mutex (`None`) and held-mutex (banner with
  elapsed seconds) branches, plus the NTP-step / future-timestamp
  clamp.

- **OTLP listener now mints runtime edges.** `POST /v1/traces`
  on the runtime-trace listener previously returned
  `receivedSpans: N, storedSpans: 0` for every payload —
  parsed, validated, then dropped because the listener had no
  way to map spans to graph nodes. The new `SpanResolver` type
  alias and `no_resolver()` helper let callers plug in any
  span→node_id policy; `cli::server::run` wires
  `graph.find_node_by_name(span.name)` for the bound single-repo
  graph. When the listener runs with `no_resolver()` (tests,
  sidecar executors without a graph) `storedSpans` stays at 0
  honestly; when the caller supplies a real resolver, parent /
  child span pairs whose names both resolve mint a
  `RuntimeCall` edge that `explain_dispatch` then surfaces.
  Federation-aware resolution that walks every registered
  repo's graph and returns namespaced global ids is a
  follow-up — multi-repo federation calls land here today
  and resolve against the staging placeholder, which is the
  same answer they got before.

- **`explain_symbol` / `get_blast_radius` append `### Open
  annotations`.** The PR #66 carry-over from `FOLLOWUPS.md`
  is now end-to-end pinned. Both tools look up open annotations
  targeting the resolved symbol via
  `open_annotations_for_symbol` (federation-mode only, by
  design — the dedicated `add_annotation` / `list_annotations`
  MCP tools already route by repo). When at least one open
  annotation matches, the markdown body grows a
  `### Open annotations` section listing each entry's
  `[@author, date, kind=…] body_excerpt`. When none match, the
  section is omitted entirely (`format_open_annotations_section`
  returns an empty string for `&[]`). The end-to-end test in
  `tests/annotations_e2e.rs` walks the JSON-RPC dispatcher
  through all three branches (no-annotation / open-annotation /
  resolved-annotation) so the contract can't regress
  silently.

### Fixed

- **Relative workspace paths in `repos.yaml`.** Prevented silent 0-file indexing
  when workspace paths are relative.
- **HTTP listener startup ordering.** `TcpListener::bind` runs before spawning
  the background startup re-index task so the port binds immediately.
- **Federation cold-start loop.** Cycle-detection guard suppresses repeated
  `index_forced` self-trigger loops during cold-start.
- **Coordination lock race.** `with_shared_presence` fails closed on lock
  timeout instead of proceeding unlocked.

### Removed

- Cleaned up obsolete build artifacts and temporary files.

## [0.7.4] — 2026-09-16

### Added

- **Cosign keyless signing.** Platform release tarballs include
  `<tarball>.cosign.bundle.json` signed via GitHub Actions OIDC identity.
- **Dev SPA override.** `LAIN_DEV_SPA_DIR` allows serving Command Center assets
  from disk without rebuilding.
- **Recorder CLI flag.** Added `--ready-timeout-ms` flag to the SPA demo recorder.

### Removed

- **Deleted unverified releases.** Removed `v0.7.0`–`v0.7.3` GitHub release
  assets that predated provenance, SBOM, and checksum signing (tags preserved).

### Fixed

- **Cross-platform path normalization.** MCP tool responses and `AuditEvent.path`
  consistently serialize paths with forward slashes across all platforms.
- **Release packaging alignment.** Updated `Formula/lain.rb` asset URLs and
  sha256 sums to match published release assets, and marked `v0.7.4-rc1` as
  prerelease in GitHub Releases.
- **Bounded MCP request body.** Capped `/mcp` POST body at 4 MiB, returning HTTP
  413 for oversized payloads.
- **Workspace discovery in tests.** Avoids parent-process walk-up hijacking when
  tests run inside `cargo test` / `cargo run`.
- **Test robustness and leak fixes.** Fixed temporary file leaks in e2e scripts,
  made `canonical_claim_path` public for fuzz tests, and prevented fork-PR 403s
  in CI.
- **Cold-boot race.** Resolved "Node not found for handle" startup race via
  `RepoIndex::indexed_signal` and bounded wait.
- **Hot-reload starvation mitigation.** Added cooperative yield in hot-reload
  stress tests on macOS.

## [0.6.2] — 2026-08-28

### Fixed

- **Docs sweep — close 13 drift rows from the audit.** `docs/quickstart-tools.md`
  drops the non-existent `export_graph_json` heading (was 270) and adds
  a *Tools documented elsewhere* section linking to the 28 tools that
  live in `FEDERATION.md` / `multiplayer.md` / `hot-reload.md` /
  `command-center.md`; the canonical surface is 83 tools, so this page
  now covers 39 of them, not all of them. `docs/TECHNICAL.md` swaps a
  `curl … export_graph_json` example for `describe_schema`, and notes
  that `lain schema dump` is the wire-format authority. `docs/ARCHITECTURE.md`
  swaps the "1500 lines" guesstimate for "~1230 lines" (`app.js` is
  1234). `docs/multiplayer.md` corrects the multiplayer tool count from
  "8 new MCP tools" to 14 (8 inline + the 5 listed in their sections)
  and disambiguates the `world_state` envelope field from the
  `get_world_state` MCP tool. `docs/USER_MANUAL.md` and `docs/quickstart-tools.md`
  clarify that `semantic_search` is *filtered from `tools/list`* when no
  NLP model is loaded (66 of 67 advertised), not advertised with an
  "unavailable" answer. `README.md` flips the mermaid agent label from
  Cursor to Agy/Codex (Cursor has no full hook script in `hooks/`) and
  rewords the `lain ask` row. `docs/hot-reload.md` polls every 2 s (was
  every second). `docs/wish-list.md` refreshes the `61 / 63 / 64` tool
  counts to match the canonical 67. `index.html`/`theme.css`/etc. were
  not touched; no code changes shipped.

- **D-H3 tool-arg consistency.** The `get_repo_info` MCP tool's required
  argument is renamed from `id` to `repo_id`. The old name was confusing
  alongside sibling tools that use `agent_id` / `session_token`, and
  already aligned with `get_cross_repo_blast_radius_for_repo`. Callers
  must update their request bodies; the `docs/FEDERATION.md` reference
  page and `scripts/demo.sh` are updated alongside, and a
  `tool_args_for_caller_identity_are_named_consistently` regression test
  pins the surface so the next drift fails loudly.

- **D-L3 demo.sh binary freshness.** `scripts/demo.sh` now prints the
  binary's version *and* mtime on startup, and warns and exits 2 when
  any source file (`Cargo.toml`, `Cargo.lock`, `src/**/*.rs`) is newer
  than the binary. Previously `--quick` and `--no-build` skipped the
  build but still ran `target/release/lain`, so a demo could silently
  measure a stale binary and report it as current. New flags:
  `--force-build` (rebuild even under `--quick` / `--no-build`) and
  `--allow-stale` (skip the check). The comparison lives in a sourced
  helper, `scripts/demo-freshness.sh`, covered by
  `tests/demo_sh_freshness.sh`.

## [0.6.1] — 2026-08-28

### Added

- **Federation per-repository readiness.** Exposed per-repository readiness
  and staleness metrics via `PerRepoReadiness` and `get_capabilities`.
- **Agent annotations & handoff layer.** Added SQLite-backed MCP tools
  `add_annotation`, `list_annotations`, `resolve_annotation`,
  `leave_handoff_note`, and `get_pending_handoffs`.

### Changed

- **Enriched health badge comment.** PR comment includes capability readiness,
  open annotations, and previous-run deltas.

### Fixed

- **Kimi parent-process discovery.** `lain mcp` resolves workspace by reading
  `/proc/$PPID/cwd` on Linux.
- **Strict workspace resolution.** `resolve_workspaces_strict` provides clear
  remediation errors when no workspace is found, and auto-delegates
  multi-workspace setups to federation mode.
- **Plugin wrapper flag order.** Corrected `--workspace` argument placement in
  `kimi_plugin_wrapper.sh`.

## [0.6.0] — 2026-08-20

Initial public release. Federation `lain server` + single-repo `lain mcp` +
Command Center SPA.
