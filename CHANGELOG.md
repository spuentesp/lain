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

- **Relative `workspace_dir` paths in `repos.yaml` no longer
  produce a silent 0-file index.** Joining the workdir-relative
  `entry.path` onto a relative workspace produced paths like
  `./tauri/crates/tauri/src/lib.rs` that libgit2 could not resolve
  under the workdir and therefore treated as ignored, so the
  indexer reported zero tracked files per repo and the federation
  loaded empty graphs. `GitSensor::get_all_tracked_files` and
  `GitSensor::get_changed_files_since` now pass the workdir-
  relative path straight to `is_path_ignored`. Regression test:
  `get_all_tracked_files_works_with_relative_workspace_path`.
  Surfaces in the 2026-09-18 Tauri federation trial
  (`POSTMORTEM.md` Bug #1).

- **HTTP transport listener binds before the startup re-index is
  spawned.** `LainMcpServer::run_http` previously logged
  "Starting Lain MCP HTTP server" and then spawned the
  backgrounded startup re-index task before reaching
  `TcpListener::bind`. When the re-index hung, port 9999 never
  opened, no log line followed the "Starting" line, and clients
  got `connection refused`. The listener is now bound first and a
  "listening" log line fires immediately after `bind` returns, so
  a stuck startup task is distinguishable from a stuck bind. If
  the re-index does hang, the listener stays up and clients get
  the structured `warming_up` response from `dispatch_tool_call`;
  operators can poll `/health` to observe the stuck state. Root
  cause of the underlying hang (Bug #2) is still under
  investigation.

### Removed

- **`/tmp/lain-build` cleanup.** Deleted the duplicate trial build
  artifact (~1.2 GB). The repo at
  `data/agents/orca/workspaces/lain/detailing` is the canonical
  lain source; agent hook configs already point at the repo's
  `hooks/` directory and the repo's hook READMEs include a
  "Dynamic Dispatch Caveat" section that `/tmp/lain-build`'s
  copies lacked. Resolves postmortem open question #4 from the
  2026-09-18 Tauri federation trial.

### Tier-3 follow-ups

- **`NodeType::Synthetic` for hub nodes.** Hub nodes (`Hub:
  message_bus_publisher` etc.) are no longer typed as `Function`:
  they aren't functions, they have no source location, and walking
  into them via `Calls`/`Uses` produced nonsense answers. The new
  variant carries `NodeType::Synthetic` in the schema, the
  `presence_tools::symbol_weight` tier (1, with containers), and
  the `describe_schema` doc surface. Agents that previously saw
  hub nodes show up in `find_anchors` now see them filtered out by
  the node-type filter `type_filter=function` — the correct
  outcome, since heuristic evidence should be opt-in via
  `get_blast_radius(..., include_weak_edges=true)` or
  `explain_dispatch`, not by reading hub nodes as if they were
  real functions.

- **`dynamic_dispatch_sensor` regex precompilation.** The hot loop
  in `scan_workspace_dispatch` now reads from a `Lazy<Vec<CompiledDetector>>`
  rather than compiling 12 regexes per file per scan. On a
  multi-thousand-file workspace, that's ~120k regex compilations
  per scan avoided. The precompile also resolves the
  `&str → EdgeType` and `&str → confidence` lookups once at static
  init, leaving the per-file hot loop with seven lines and a
  single allocation (the `GraphEdge` per match). Patterns that
  fail to compile are silently dropped — the failure mode is
  "this family never matches", which is honest and easier to spot
  in test coverage than a panic at static-init time.

- **`assess_change` defaults to `include_weak_edges=true`.** The
  pre-edit risk verdict (low/medium/high) is meaningless if it
  can't see dynamic-dispatch callers. A `bus.publish` site with
  only heuristic callers previously reported `direct=0,
  transitive=0, risk=low` and the agent would ship the regression
  `explain_dispatch` was built to prevent. With `include_weak_edges=true`,
  heuristic callers are tagged with `~` and `[heuristic,
  conf=X.XX]` (the same format `get_blast_radius` already uses)
  so agents can tell them apart from type-resolved calls.
  `explain_dispatch` remains the diagnostic follow-up when the
  verdict surfaces heuristic callers (it explains which detector
  fired and whether runtime traces confirm them).

- **Rust trait-object + async-spawn detectors.** Two new
  detectors close the Tier-3 review point on Rust coverage that
  was missed in the original sensor surface: `rust_trait_object`
  matches `Box<dyn Trait>`, `Rc<dyn Trait>`, `Arc<dyn Trait>`
  (where `Trait` is a user-defined identifier, not `Any`) at
  confidence 0.6; `async_task_spawn` matches `tokio::spawn`,
  `async_std::task::spawn`, `smol::spawn`, `executor::spawn`,
  `workers::spawn` at confidence 0.5. Negative coverage pinned:
  bare `spawn(` without a runtime namespace prefix does NOT
  match (prose noise, not a dispatch surface). The two new
  detectors together fill the gap that the original Tier-3
  surface had on Rust — every other dynamic-dispatch family the
  agent might reasonably hit now produces a heuristic edge
  with a confidence score.

- **`assess_change` regression test for the heuristic-caller
  path.** A new unit test in
  `src/server/tools/handlers/semantic.rs` builds a fixture with
  one `BusTopic` heuristic edge pointing at the symbol under
  assessment and pins the contract that the output must
  contain the heuristic marker (`[heuristic` /
  `heuristic caller(s) included`), the confidence tag
  (`conf=0.70`), and a non-`low` risk verdict. Without this
  test, a future refactor could silently re-introduce the
  false-negative iter 14 closed. The test also caught a real
  fixture bug along the way: the `File` node representing the
  call site was never `upsert_node`'d, so `insert_edges_batch`
  silently dropped the edge as orphan and `get_edges_to`
  returned `[]` — a useful confirmation that the
  silent-drop-on-orphan behaviour is itself worth pinning in
  a follow-up test.

- **`explain_dispatch` tests no longer use the global store.**
  `RuntimeTraceStore::global()` is a process-wide `OnceLock` —
  once a test ingests a span into it, that span persists for the
  lifetime of the test process. The
  `verdict_distinguishes_runtime_confirmed_from_runtime_only`
  test was the only one using the global; it now constructs a
  fresh `RuntimeTraceStore::new(StoreConfig::default())` and
  calls `build_with_store` directly. Removed the now-unused
  `reset_global_for_tests` seam (`OnceLock` has no `take()`,
  the function was a no-op, and no caller existed).

- **`dynamic_eval` detector closes the Python + JS gap.**
  `eval()`, `exec()`, `pickle.loads()`, and `new Function()` are
  direct string-to-code surfaces; anything reachable from the
  loaded string is invisible to the static graph. New detector
  matches these patterns at confidence 0.4 (matches reflection
  tier) and tags them as `DynamicDispatch`. Two regression
  tests pin the contract: a Python `pickle.loads` fixture and
  a JS `eval` fixture — so a future Python-only tuning can't
  silently miss the same risk in a JS codebase.

- **`assess_change` surfaces heuristic-only honestly.** Pre-fix:
  `assess_change` on a symbol with zero static callers and one
  heuristic caller returned `risk=low` — the agent would treat
  that as safe, exactly the regression `explain_dispatch` was
  built to prevent. Post-fix: when the static graph is empty
  but the blast-radius output carries the `heuristic caller(s)
  included` marker, `assess_change` emits `risk=low* —
  heuristic-only (see ~ N heuristic caller(s) below)` instead.
  The asterisk is the agent-visible signal that the static
  graph is empty by blind-spot, not by absence of callers.
  Risk-tier ordering (`low*` → `medium` → `high`) is preserved
  so any consumer that matched on the bare `low` word still
  works. Two regression tests pin the new contract:
  `assess_change_heuristic_only_verdict_does_not_say_low` and
  the iter-17 `assess_change_surfaces_heuristic_callers_in_risk_verdict`.

- **`assess_change` truly-empty blast radius pins `risk=low`.**
  The risk-verdict tier relies on `count_bullets` to distinguish
  a truly-empty blast radius from a populated one. The old
  implementation matched any line whose trimmed prefix was
  `- `, which counted the section header itself (`- Direct
  dependents (0):`) as a bullet — a symbol with zero callers
  was therefore classified as `direct=1 / transitive=2 /
  risk=medium`. Tightening the bullet marker to two-space
  indent matches what `get_blast_radius` actually emits for the
  `affected_names` list. The new
  `assess_change_truly_empty_blast_radius_says_low` test pins
  the third vertex of the contract: `0 static + 0 heuristic =
  bare risk=low` (no asterisk, no heuristic marker). The
  iter-21 `low*` heuristic now lives strictly in the
  `0 static + N heuristic` vertex and cannot leak into the
  truly-safe path.

- **Negative-coverage regression tests for detector regexes.**
  Three tests pin what the heuristic regexes MUST NOT match
  so a future tightening can't silently start firing on
  innocent identifiers:
  - `identifier_prefixes_with_eval_or_exec_do_not_match` —
    `evaluate_query`, `execution_time`, `exec_summary`,
    `executable_path` (all common identifier forms that
    contain the eval/exec substrings) must not fire
    `dynamic_eval`.
  - `concrete_generic_types_do_not_match_rust_trait_object`
    — `Vec<MyStruct>`, `HashMap<String, MyStruct>`,
    `Box<MyStruct>` (concrete types without `dyn`) must
    not fire `rust_trait_object`.
  - `identifier_prefixes_in_dispatch_context_do_not_match` —
    the dynamic_eval version of test 1, in bus/handler.py
    shape. The comment-stripping case (commented-out
    `bus.publish(` still matches today) is intentionally
    NOT pinned — fixing it requires a tree-sitter pre-pass
    per file and is documented in the test as a future-PR
    concern.

- **`type_escape` detector for TypeScript `as any` / `as
  unknown as any` / `<any>`.** TypeScript's type escape
  hatches are the most common JS/TS-specific
  dynamic-dispatch surface we hadn't yet covered. After any
  of these casts, the value dispatches through the JavaScript
  prototype chain at runtime — the LSP can't follow, the
  static type system has been told to look the other way.
  Pinned at confidence 0.3 (lower than `serde_value`'s 0.4
  because `as any` is often a temporary workaround rather
  than an architectural dispatch surface — the user usually
  intends to remove it eventually). Three regression tests
  pin the contract: `typescript_as_any_emits_type_escape_edge`,
  `typescript_as_unknown_as_any_matches`, and the negative
  coverage `as_something_other_than_any_does_not_match`.

- **Mixed static + heuristic risk-tier pinned.** The fourth
  vertex of the `assess_change` risk-tier contract:
  `N static + N heuristic → regular tier (medium / high)`. The
  mixed case must NOT downgrade to either tier-3 special
  verdict — the static caller's presence keeps the risk tier
  on the regular scale; the heuristic evidence augments an
  already-existing caller surface but doesn't replace the
  count-based tier. Pinned by
  `assess_change_mixed_static_and_heuristic_uses_normal_tier`:
  verdict is medium or high (not bare `low` or `low*`), and
  the heuristic evidence still surfaces in the body so the
  agent knows the runtime target has more callers than the
  static graph shows.

- **`insert_edges_batch` dropped-edge count contract pinned.**
  The function silently drops edges whose endpoints are both
  missing (or whose source is missing), and the production
  caller `insert_edges_reporting` emits a `warn!` carrying the
  count. The function returns the count but no test was
  pinning that contract — iter 17 found the silent-drop
  behaviour the hard way when an `assess_change` fixture
  forgot to upsert one endpoint. Pinned by
  `insert_edges_batch_reports_dropped_count_for_orphan_edges`:
  - Source present, target missing: NOT dropped; held for
    the federation's `project_repo` drain.
  - Source missing, target present: dropped (orphan — we
    can't project a source we don't have).
  - Source missing, target missing: dropped (orphan).
  - Source present, target present: inserted normally.

- **`assess_change` heuristic-caller count pinned.** The
  `~ N heuristic caller(s) included` line is the agent's
  only signal that a static-graph-empty blast radius has
  more callers than the static graph shows. Agents rely on N
  to decide whether to follow up with `explain_dispatch`; if
  N is wrong, the agent under- or over-estimates the blast
  radius. Pinned by
  `assess_change_heuristic_caller_count_reflects_graph`:
  fixture with three distinct File callers on a single
  Function target asserts `~ 3 heuristic caller(s) included`
  appears verbatim in the output.

- **Combined negative-coverage sweep across all 16 detector
  patterns.** A single fixture exercises every detector's
  benign form — `publisher_count`, `dispatch_count`,
  `container_size`, `Kafka` type usage, `app_size`,
  `router_count`, bare `spawn`, `evaluation_metric`,
  user-defined `Anything` trait — and asserts none of them
  fire. Each detector already has its own positive +
  negative tests; this combined sweep catches a future
  regression that widens any single pattern (e.g. removes
  a `\b` anchor) without requiring one new test per
  detector.

- **Cross-tool contract pinned: assess_change `low*` implies
  explain_dispatch sees the heuristic.** When assess_change
  emits `risk=low* — heuristic-only`, the same symbol fed
  to explain_dispatch must surface the heuristic caller
  (verdict `heuristic_only` or `runtime_confirmed`). If the
  two tools ever decouple, the agent would silently get a
  `no_callers` verdict on a heuristic-only fixture and ship
  the regression Tier 3 was built to prevent. Pinned by
  `assess_change_low_star_implies_explain_dispatch_sees_heuristic`.

- **Minimal OTLP HTTP/JSON ingest adapter for `runtime_trace`.**
  Closes the deferred Tier-3 OTLP listener work without
  pulling in `tonic` + `opentelemetry-proto`. Covers the
  common production path: OTel collector → OTLP HTTP
  exporter → lain. `parse_otlp_json(payload: &[u8]) ->
  Vec<SpanRecord>` walks the official OTLP HTTP/JSON shape
  (`{"resourceSpans": [...]}` → `scopeSpans: [...]` →
  `spans[]`) and emits one `SpanRecord` per span. Trace/span
  IDs must be 32/16 hex chars respectively; malformed IDs
  return `OtlpParseError::{TraceId, SpanId}`. Unknown span
  kinds fall back to `Internal` rather than dropping the span.
  What's intentionally NOT included: gRPC adapter (heavy
  deps) and the HTTP server route (a follow-up can wire
  `parse_otlp_json` to a hyper handler when
  `LAIN_TRACE_RUNTIME=true` is set).

- **OTLP HTTP listener at `/v1/traces`.** Wires the
  `parse_otlp_json` parser into a real `hyper::server::conn::http1`
  listener that OTLP collectors can POST to. The listener
  binds its own `TcpListener` (separate from the MCP HTTP
  transport) so the runtime trace path doesn't share the MCP
  bearer-token auth layer. POST `/v1/traces` ingests each
  span into the store; returns 200 with
  `{"partialSuccess":{"acceptedSpans":N}}` on success, 400
  on a malformed payload, 404 for any other path/method.
  Activation is opt-in via `LAIN_TRACE_RUNTIME=true`; the
  `cli/server.rs` wiring is left as a follow-up so this PR
  ships the listener + parser contract without changing
  startup behaviour. Auth on the listener is intentionally
  absent — OTLP collectors don't ship bearer tokens;
  operators firewall the port instead.

## [0.7.4] — 2026-09-16

### Added

- **Cosign keyless signing for release artifacts.** Each platform
  tarball now ships a `<tarball>.cosign.bundle.json` alongside the
  existing SLSA build-provenance bundle, signed via `cosign sign-blob`
  using the same GitHub Actions OIDC identity. Verifiable with only
  the `cosign` CLI — no `gh`/GitHub API dependency. See
  [`docs/VERIFICATION.md`](docs/VERIFICATION.md) for the verify
  command.

### Removed

- **Deleted the `v0.7.0`–`v0.7.3` GitHub releases.** They predated the
  build-provenance/SBOM/checksum pipeline and shipped without those
  artifacts. The underlying git tags are untouched; only the GitHub
  Release objects (and their binary assets) were removed.

### Fixed

- **Windows path-format normalization.** All MCP tool responses that
  surface a file path (`claim_files`, `release_files`, `list_occupancy`,
  `my_claims`, `list_my_claims`, `detect_overlap`) now serialize paths
  in forward-slash form on every platform. Previously Windows clients
  received `"src\\a.rs"` where the wire contract required
  `"src/a.rs"`, breaking the contract and the `multi_agent_concurrency`
  integration tests. A new `crate::server::path_util::posix_string`
  helper is the canonical cross-platform path string renderer,
  mirroring the existing `graph_path` pattern.
- **Audit log JSONL is now platform-independent.** `AuditEvent.path`
  is written in forward-slash form regardless of host OS, so the
  `get_recent_activity` `path_glob` filter (built with `/`) matches on
  Windows as it does on Linux. The same fix applies to the
  `group_by: "path"` branch of `group_key` in `audit_tools.rs`. No
  in-process reader other than `read_audit_log` consumes the JSONL
  today, so the wire-format change is internal.
- **Removed two `#[cfg_attr(target_os = "windows", ignore)]` gates**
  on `claim_files_accepts_string_form_files` and
  `get_recent_activity_tool_groups_by_path` in `tests/presence.rs`.
  Both tests now run on Windows after the underlying fixes.
- **`Formula/lain.rb` v0.7.4-rc1 download URLs and sha256 sums now
  match the actual published release.** The formula declared
  `version "0.7.4-rc1"` but its `url`/`sha256` lines still pointed
  at the `v0.7.3` tarballs — meaning every supported Homebrew
  install fetched the old binary and the formula's own
  `assert_match "lain 0.7.4-rc1"` test would fail against it.
  Updated all three platform blocks (macOS arm64, Linux x86_64,
  Windows x86_64) to point at the real `v0.7.4-rc1` assets on the
  GitHub release. Hashes pulled from the per-asset `.sha256` files
  uploaded alongside the binaries.
- **v0.7.4-rc1 GitHub release is now flagged as `prerelease: true`.**
  The release was published without the pre-release flag set, so
  npm's `latest` dist-tag handling didn't differentiate it from a
  final release. Marked it via the GitHub releases API so any
  consumer keying off the flag (npm `next` vs `latest`, downstream
  tooling that hides pre-releases, etc.) gets the right signal.
- **`/mcp` HTTP body is now bounded at 4 MiB.** The handler used to
  do `req.collect().await?` with no size cap, so a single oversized
  POST could exhaust server memory. Added a Content-Length
  precheck (returns 413 immediately if the header advertises a body
  > 4 MiB) and wrapped the stream in `http_body_util::Limited` so
  chunked-encoded bodies without Content-Length hit the same 413
  cap. The 4 MiB cap is generous for any legitimate MCP tool-call
  payload we accept.
- **`tests/e2e/federation_dashboard_e2e.sh` and
  `tests/e2e/multiplayer-hooks.sh` no longer leak response files
  on failure.** Both scripts used `mktemp` (no `-p`), so the
  response file landed in `/tmp` and survived any non-zero exit
  before the explicit `rm -f "$TMP"`. Switched to
  `mktemp -p "${WORKDIR}"` (resp. `$TMPDIR`) so the existing
  EXIT trap's `rm -rf` catches them on every exit path.
- **`fuzz/fuzz_targets/path_canonicalize.rs` now compiles.**
  `canonical_claim_path` in `src/server/presence.rs` was declared
  `fn` (crate-private) so the fuzz target's
  `use lain::server::presence::canonical_claim_path;` failed.
  Made it `pub`; the function is documented and used by 4
  internal call sites, so exposing it as part of the public API
  surface is intentional.
- **`agent-contract` CI job now skips cleanly on fork PRs.**
  GitHub downgrades `GITHUB_TOKEN` to read-only for
  `pull_request` events from forks, so the job's
  `gh api ... statuses/...` POST would 403 every fork-PR run.
  Added a fork guard to the `if:` (plus a comment pointing at
  the separate cross-repo status publishing problem for the
  branch-protection check itself).
- **`ci.yml` version-drift extractor now accepts pre-release
  tags.** The regex `v[0-9]+\.[0-9]+\.[0-9]+` would silently
  produce an empty match for `v0.7.4-rc1` (or any future
  `-rcN`/`-beta.N`). Extended to
  `v[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?` so the same extractor
  keeps working across stable and pre-release tags.
- **`npm-shim/package-lock.json` regenerated to match `package.json`.**
  The package metadata was at `0.7.4-rc1` but the lockfile's
  top-level `version` + `packages[""].version` were still
  `0.7.3`. Ran `npm install` in `npm-shim/`; no transitive deps
  changed (the package has no production deps), only the
  metadata aligned.
- **`docs/BRANCHING.md`, `docs/CII_OWNER_ATTESTATIONS.md`,
  `docs/SUPPLY_CHAIN.md`, and `docs/VULNS.md` refreshed.** Four
  stale claims surfaced by automated review on this PR:
  the BRANCHING `if: ${{ env.full-battery }}` line that was
  actually inline `github.ref`/`github.base_ref`; the CII
  attestation's false "default crypto provider aws-lc-rs"
  claim (Cargo.toml still uses reqwest's `rustls-tls` feature
  which pulls ring) and "all inputs are bounded" claim (now
  true with the `/mcp` cap above); the SUPPLY_CHAIN example
  that claimed `version-check` exposes an `epoch` output (it
  doesn't — that path was silently dropped, see release.yml
  for the actual per-build-job local computation); and
  VULNS.md's bucket-D which still listed `bincode@1.3.3` as
  unfixed when PR #57 had already migrated it to 2.0.x.
- **`tests/multi_agent_concurrency.rs`** replaces eleven raw
  `Some("src/...")` literal assertions with a local
  `path_components_eq` helper, matching the existing helper in
  `tests/feat_suite.rs`. These tests now run unmodified on every
  platform.
- **Workspace discovery no longer hijacks on dev/test runs.** When
  the parent process is `cargo test` / `cargo run`, its cwd is the
  project containing the `lain` binary itself, so the
  parent-process-cwd walk-up used to land on the source tree and
  `lain mcp` was asked to re-index the entire project being tested
  — the `oneshot_discovers_workspace_from_cwd` regression test
  timed out at 60 s. `find_git_workspace_root_resolved` now skips
  the parent-cwd candidate when the running binary lives inside
  the git root it resolved to, falling through to the process's
  own cwd. Real agent harnesses (Kimi, Claude Code, a plain
  shell) put the binary in a plugin dir or on `$PATH`, so the
  filter never fires for them.
- **Recorder `--ready-timeout-ms` flag.** The SPA recorder
  (`tests/js/record_spa_demo.js`) previously hard-coded a 600_000
  ms cap on its `waitForReady` poll. Cold-cache CI hosts occasionally
  exceeded that; the only escape was editing the source. The flag
  is now a CLI arg (default unchanged at 600_000). A regression
  test (`tests/js/recorder_cli.test.js`) pins the parser shape.
- **Dev SPA override via `LAIN_DEV_SPA_DIR`.** The Command Center
  SPA was `include_bytes!`'d at compile time, so every JS/CSS edit
  required `cargo build`. Setting `LAIN_DEV_SPA_DIR=<path>` now
  flips the assets module to read each file from disk on demand
  (one env-var lookup + one `is_dir` check per request). Edit
  `app.js` / `styles.css` / `index.html`, save, refresh the
  browser — no rebuild. Production builds leave the env var unset
  and the contract tests still pass. Workflow script:
  `scripts/dev-spa.sh`.
- **Hot-reload writer starvation on macOS — speculative `yield_now`
  mitigation.** `set_workspace_stress_visible_to_shared_lock`
  in `tests/hot_reload.rs` was gated on macOS because the reader
  occasionally collapsed to a single distinct count. The writer
  task did 100 synchronous `set_workspace` calls with no
  `.await` between them; on macOS's kqueue-based scheduler the
  writer monopolized a worker thread for the burst and the reader
  woke up only after the writer finished. Inserting
  `tokio::task::yield_now().await` between writes closes the
  starvation window on every platform. The test still passes on
  Linux (behavior-neutral change); the macOS gate stays until a
  real macOS runner confirms the mitigation removes the flake.
- **Cold-boot "Node not found for handle" race in
  `feat_negative_paths_end_to_end` — promoted from flake to hard
  gate.** The symptom was a ~20–25% intermittent failure on
  `feat_negative_paths_end_to_end` that surfaced two bugs stacked
  on top of each other:
  1. *Root cause (test fixture):* `tests/feat_negative_paths.rs::
     boot_server` declared three `tempfile::TempDir` values as
     locals; on return, `Drop` ran `remove_dir_all` while the
     spawned `lain server` child was still serving requests. The
     watcher's first `index_forced` then re-walked an empty
     `get_all_tracked_files()` and `prune_orphans` wiped the
     per-repo graph. Switched to `TempDir::keep()` so the dirs
     outlive the fixture.
  2. *Real but secondary cold-boot race (library code):* the
     per-repo `RepoIndex` and the federation backend did not share
     an "indexed" signal, so a tool call landing in the cold-boot
     window could see an empty per-repo graph even after
     `index()` returned. Added `RepoIndex::indexed_signal` (a
     `tokio::sync::Notify` fired after every successful
     `index()` / `index_forced()`) and a 200 ms bounded wait in the
     MCP dispatcher when `ctx.graph` is empty, with a fail-through
     to the existing federation fallback. Also switched
     `tests/common/mod.rs::wait_for_repo_index` from
     `tools_call_text` to `tools_call_envelope` so it can poll
     through the cold-boot window instead of panicking on
     `isError=true`.
  Reliability: `feat_negative_paths_end_to_end` was ~75% baseline,
  25/25 after both fixes (verified locally on commit `3436a51`).
  `feat_negative_paths_end_to_end` is now treated as a hard-gate
  test in CI — no special `#[ignore]` or runner-level tolerance.

### Investigated (no change)

- **CI `cargo build --bin lain` step is correctly unconditional.**
  The parked-bug inventory note flagged this step as a candidate
  for an `if: matrix.os == 'windows-latest'` guard. It cannot —
  the step exists because `tests/use_cases/battery_*` (specifically
  `battery_cli.rs` and `battery_success_metrics.rs`) invoke the
  binary as a subprocess, and those tests run on all three OS
  matrices (Linux, macOS, Windows), not just Windows. No code
  change; the workflow is left as-is.
- **macOS FSEvents config-watcher latency — documented as resolved by gate.**
  `config_watcher_triggers_reload_on_repos_yaml_modify` and
  `config_watcher_triggers_reload_on_workspaces_yaml_modify` were
  gated with `#[cfg_attr(target_os = "macos", ignore)]` because
  FSEvents coalescing latency is unbounded within any reasonable
  CI budget. The project's CI saga documented the iteration:
  5 s → 15 s → 30 s → gate (commits `0a341b5` → `6adc721` →
  `e29c4dc` → `7af23fd`). The only durable Linux-untested
  alternative — switching to `notify::PollWatcher` on macOS — is
  out of scope for this plan because it requires macOS hardware
  to verify. The gates stay; the rationale is documented for the
  next reader.

## [0.6.2] — 2026-08-28

### Fixed

- **Docs sweep — close 13 drift rows from the audit.** `docs/quickstart-tools.md`
  drops the non-existent `export_graph_json` heading (was 270) and adds
  a *Tools documented elsewhere* section linking to the 28 tools that
  live in `FEDERATION.md` / `multiplayer.md` / `hot-reload.md` /
  `command-center.md`; the canonical surface is 67 tools, so this page
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

### Fixed

- **Kimi integration (the headline fix).** `lain mcp` now reads the
  parent agent's cwd via `/proc/$PPID/cwd` on Linux and walks up for
  `.git` from there, falling back to the process's own cwd. Kimi's
  plugin security model pins the MCP subprocess cwd to the plugin
  root, so under 0.6.0 a naive `{"command":"lain","args":["mcp"]}`
  config resolved to the plugin directory instead of the project.
  With 0.6.1 the same config works under Kimi without any wrapper
  script. macOS is unsupported in either path. (Linux only.)

- **`src/cli/kimi_plugin_wrapper.sh`** rewritten to insert
  `--workspace <git_root>` *after* the `mcp` subcommand, because
  clap parses `--workspace` as a flag on `mcp`, not on the top-level
  binary. The earlier sentinel-rewrite form produced
  `lain --workspace <path> mcp`, which clap rejected with
  `unexpected argument '--workspace' found`. The wrapper is no longer
  required for Kimi; it remains in source as a fallback for users
  pinned to the 0.6.0 binary.

### Added

- **Federation per-repository readiness aggregation (M4 step 8).**
  New `PerRepoReadiness` DTO and `FederatedIndex::per_repo_readiness()`
  snapshot expose every repo's `state`, `indexed_signal`,
  `last_indexed_commit`, `last_indexed_at_unix_ms`, `outstanding_files`,
  and `staleness` (mapping `RepoHealth` to the existing
  `CapabilityState`). `get_capabilities` now includes these deep
  per-repo fields alongside the existing `repositories[].capabilities`
  shape; the aggregate `capabilities` and `SchemaVersion` are
  unchanged so old clients keep parsing. Federation-aggregate tools
  (`search_org`, `get_cross_repo_blast_radius`) were already gated
  via the central `gate_federated_tool_call`; this commit layers the
  snapshot path and the per-repo wire shape. New
  `tests/federation_readiness.rs` (7 tests) pins the contract.
- **Agent-side annotation + handoff layer (5 new MCP tools).** New
  per-repo SQLite storage at `<state_dir>/annotations/<repo>.sqlite`
  backing `add_annotation`, `list_annotations`, `resolve_annotation`,
  `leave_handoff_note`, and `get_pending_handoffs`. Live-staleness
  pass on `list_annotations` re-checks each open row's target
  against the live graph and marks rows with missing targets as
  `status: "stale"`. Schema dump regen (via `cargo run -- schema
  dump`) advertises the 5 new entries; diff is exactly the new
  tools. `LainServer` gains `annotations: Arc<AnnotationRegistry>`
  + `annotations()` accessor + `federation_repos()` helper.
- `cli::workspace::parent_process_cwd()` and a new
  `find_git_workspace_root_resolved()` policy that prefers the parent
  cwd over the process cwd. `find_git_workspace_root()` is the public
  wrapper that wires this in; the existing `Some(p)` / `None` ergonomics
  are preserved.

### Changed

- **`lain-health-badge` PR comment is enriched.** The sticky comment
  now leads with a `Capability readiness: ...` line from
  `get_capabilities`, lists open annotations per file (first 3
  rows + a "more..." link to the underlying `list_annotations` MCP
  call), and adds a "Previous-run delta" section that calls
  `explain_symbol` for every modified (not just added) function in
  the PR against the base ref's previous commit. All three
  additions are best-effort — a failed MCP call must not fail the
  badge itself. No new `action.yml` inputs.

### Fixed

- **`tests/feat_negative_paths.rs` baseline compile error.** The
  recent merge to `dev` (4c885c3) added `.keep()` calls that
  consumed `TempDir`s but the function's return type still
  expected `TempDir`. Replaced with `.path().to_path_buf()` so the
  same `PathBuf` is derived without moving the `TempDir`. Without
  this fix `cargo build --workspace --all-targets` failed on the
  branch baseline.

- `cli::mcp::resolve_workspaces()` and a strict variant that errors
  when no workspace can be resolved. `resolve_workspaces_strict()`
  backs `run_mcp` so a Kimi-style cwd-pinned spawn fails fast with a
  message that names the four ways to fix it (`--workspace PATH`,
  `LAIN_WORKSPACE`, run inside a clone, or pass the wrapper script
  on 0.6.0).

- Multi-workspace delegation: when `resolve_workspaces_strict()` finds
  more than one workspace, `run_mcp` synthesizes a temp `repos.yaml`
  and delegates to `run_server --transport stdio`, giving the agent
  the same federation surface as `lain server` without having to
  author the config itself.

### Docs

- README: explicit Kimi note explaining the native `/proc/$PPID/cwd`
  path on 0.6.1 and the wrapper as a 0.6.0 fallback.
- README + `docs/command-center.md`: chromium-captured Command Center
  screenshots on the Overview / Repos / Tools tabs.
- `docs/TECHNICAL.md`: workspace-resolution policy documented as a
  numbered list, matching the new `cli::mcp::resolve_workspaces`
  order.

### Verified

- `cargo test --release`: 41 test binaries, ~970 tests passing,
  0 failed, 2 ignored (semantic_search path: ONNX model not loaded).
- `scripts/demo.sh --quick`: 111/111 ground-truth fixture assertions
  pass.
- Federation smoke test against three real repos on disk
  (`pii-sentinel`, `free-pmo`, `qap-metaheuristics`): 63 tools,
  3/3 ready, `find_anchors repo_id=pii-sentinel` returns 5 real
  anchors.

## [0.6.0] — 2026-08-20

Initial public release. Federation `lain server` + single-repo
`lain mcp` + Command Center SPA. See `README.md` and the docs
index in `docs/INDEX.md`.
