# Agent UX Roadmap

> Goal: make LAIN feel like infrastructure an agent can assume exists.
>
> A developer should be able to install it, connect an MCP client, and get useful repository intelligence with almost no configuration. An agent should be able to discover what LAIN knows, choose the right capability, and recover from partial readiness without human intervention.

## Status (2026-09-17)

What's actually landed on `dev`, checked against the code — not aspirational.
Each milestone section below repeats its own status inline. Full history is
`git log`; this table is a snapshot, not an archive.

| # | Milestone | Status | Evidence / what's missing |
|---|---|---|---|
| 1 | Frictionless distribution | 🔴 Regressed in production *(code is ready; published artifact is not)* | The in-tree launcher at `npm-shim/scripts/runtime.js` (`609f8db`) verifies checksums, caches by version+target, honors `LAIN_VERSION`, and reuses cache offline. The bundled `npm-shim/bin/lain.js` calls `ensureBinary()` from `runtime.js`. **However**, the published `@spuentesp/lain-mcp@latest` on npm is still `0.6.1` (confirmed 2026-09-16), whose tarball predates the rewrite; that published binary has no knowledge of `LAIN_CACHE_DIR` / `LAIN_VERSION` and exits 1 on a clean machine. The fix is a publishing decision (cut a stable release from a commit that contains `609f8db`, which is now on `dev`, then `npm dist-tag add @spuentesp/lain-mcp@<v> latest`). See [`docs/DISTRIBUTION.md`](DISTRIBUTION.md) for the pre-release checklist that catches this class of regression. |
| 2 | `lain setup` | ✅ Done | `src/cli/setup.rs` (`7132baa`): detects repo/languages, resolves or offers to install the optional embedding model, configures one MCP client, verifies with a real `initialize`+`tools/list` round trip. `generic` (writes `.mcp.json`) and `claude-code` (shells to `claude mcp add`) adapters only — Codex/Cursor/VS Code/Continue are Milestone 8's job. |
| 3 | `lain doctor` | ✅ Done | `lain doctor` / `lain capabilities --json` / `lain status --json`, sharing one readiness model (`src/server/readiness.rs`). |
| 4 | Zero-config MCP startup | ✅ Done | All 10 implementation-sequence steps landed (`5a4b99c` … `b558a1b`): mandatory tool classification, the central dispatch gate, phase/progress instrumentation, backgrounded startup re-index, multi-thread runtime, watcher-ready handoff, final overlay reconciliation, a stdio `capabilities_changed` push notification, per-repository federation readiness gating (`gate_for_dispatch` resolves a tool call's target repo(s) and gates on their own `RepoHealth` instead of the one process-global handle; `get_capabilities` reports both per-repo state and a worst-of aggregate), and step 10's close-out: there was no separate old blocking-startup helper left to delete (steps 5-6 rewrote `run_stdio`/`run_http` in place rather than adding a parallel path), so step 10 added the structural test the roadmap calls for instead — `each_transport_has_exactly_one_startup_index_entry_point` pins exactly one startup-index entry point per transport by reading `handler.rs`'s own source. PR #66 adds the deep per-repo fields on `get_capabilities` (`indexed_signal`, `last_indexed_commit`, `last_indexed_at_unix_ms`, `outstanding_files`, `staleness`) and lands the agent-side annotation + handoff layer (5 new MCP tools, per-repo SQLite storage, live-staleness pass, `LainServer.annotations()` accessor) plus the CI-side enrichment of `lain-health-badge`. Both design-side carry-overs from `FOLLOWUPS.md` have landed: server-owned `CancellationToken` in `LifecycleInfo` (PR `feat/m4-cancellation-token`, #88) plumbed through `build_core_memory`, `index_one_repo`, `sync_volatile_overlay`, `process_change_locked`, both watchers, the NLP prewarm task (`child_token`), and the stdio/HTTP startup tasks; and the sync indexing work (PR `feat/m4-spawn-blocking`) now routes the libgit2 calls (`get_latest_commit_info`, `get_changed_files_since`, `get_all_tracked_files`, `analyze_co_changes`) through a new `src/server/ingest/blocking.rs::offthread(cancel, f)` helper onto the blocking-thread pool. `PerRepoReadiness::outstanding_files` is now wired through the federation watcher's receiver loop (the inotify callback `fetch_add`s, the receiver loop `fetch_sub`s) so `get_capabilities` reports real back-pressure instead of always 0. `await_startup_reindex` races `build_core_memory` against `cancel.cancelled()` via `tokio::select!`, the stdio startup task uses `cancel_token.cancel()` + `JoinHandle::await` within the existing 5-second budget (replacing `AbortHandle::abort()`), and the HTTP transport now retains the `JoinHandle` it previously dropped. New problem code `index_cancelled` distinguishes cooperative shutdown from `index_failed`. Tree-sitter, ONNX, and LSP-subprocess migration remain as future work (LSP-bridge's API is async, so it requires a different migration pattern); both can land as follow-ups without touching the milestone's existing acceptance criteria. |
| 5 | Agent bootstrap context | ✅ Done | `understand_repository` shipped in PR `feat/m5-understand-repository` (#92). Single MCP call returning a stable JSON payload with `repository / architecture / capabilities / recommended_actions` sections; lists M6 semantic tools as `available: false` until M6 lands so an agent doesn't call a missing tool. M6 subsequently flipped those flags in `feat/m5-m6-flags` (#103). |
| 6 | Semantic Agent API | ✅ Done | Five new MCP tools registered via the inventory pattern: `find_symbol` ("Where is X?" — graph name index, optional `path_hint` + `type_filter`, single-match produces a `Use this:` shortcut), `get_context` ("Explain X" — composes `explain_symbol` + `get_call_sites` + `trace_dependency` + `get_code_snippet` under stable section headers `## Definition`, `## Callers`, `## Callees`, `## Source`), `find_related` ("What is connected to X?" — composes `trace_dependency` + `get_coupling_radar` + optional `semantic_search` section that degrades gracefully without an NLP model), `assess_change` ("What breaks if I change X?" — composes `get_blast_radius` + `get_call_sites` + `find_untested_functions` + `get_coupling_radar`, capped with a one-line `risk` verdict), and `search_code` ("Find code that does Y" — mode-dispatched between `lexical` and `semantic`; `auto` default tries semantic first and falls back to lexical, recording the fallback in the response). Markdown output (matches existing tool surface); readiness requirements wired through `definitions::readiness_requirement` (`find_symbol` + `search_code` → `GraphIndependent`; `get_context` + `find_related` + `assess_change` → `GraphRequired`; `search_code` semantic path returns typed `LainError::Unavailable` when no model is loaded). Low-level tools remain accessible (the milestone explicitly forbids removal); 7 unit tests cover single/ambiguous/no-match find, type_filter narrowing, lexical search_code happy/sad paths, and assess_change on a missing symbol. `docs/tool-schema.json` regenerated: 73 → 78 tools. **Annotation markdown wiring (the `docs/FOLLOWUPS.md` carry-over)** landed in PR `feat/ux-pendings-round-1` (`8cedfc0`) — `explain_symbol` and `get_blast_radius` now append an `### Open annotations` section when open annotations target the symbol, omitting it entirely otherwise. |
| 7 | Capability discovery | ✅ Done | `get_capabilities`, folded into Milestone 4's central gate work. |
| 8 | First-class client recipes | ✅ Done | Four new MCP-client adapters registered under `lain setup --agent <name>`: `codex` (delegates to `codex mcp add` when the CLI is on `PATH`, falls back to a direct TOML edit of `$CODEX_HOME/config.toml` otherwise), `cursor` (writes `~/.cursor/mcp.json` preserving every other `mcpServers` entry), `vscode` (writes `.vscode/mcp.json` if it exists, otherwise the user-scoped `Code/User/mcp.json` resolved via `dirs::config_dir()`), and `continue` (writes `~/.continue/config.json` under `experimental.modelContextProtocolServers`, deduplicating any existing `lain` entry). The dispatch gate at `setup.rs:744-748` accepts all six `--agent` values. The interactive prompt at `setup.rs:622` lists all six options. Each adapter follows the same idempotent + preserve-other-settings + atomic-write contract that the existing `generic` and `claude-code` adapters already pin. Unit tests in `setup::tests` cover the file-write fallback paths (4 adapters × 3 cases = 12 tests). Real-CLI end-to-end coverage lives in `scripts/test_client_recipes.sh`, which CI runs against installed editor binaries on the relevant runners — the unit tests pin the file shape; the CI script pins the editor's actual config storage. `docs/COOKBOOK.md` has a per-adapter recipe; `README.md`'s "Connecting Your AI Agent" section points each adapter at `lain setup --agent <name>`. |
| 9 | Distribution acceptance CI | ✅ Done | `.github/workflows/distribution-acceptance.yml` (`496938c`): 3-OS matrix, three lanes (public `npx` user path, forced-install automation path, release-gate against downloaded GitHub Release assets), all driving `scripts/clean_room_mcp_check.py` through the full `initialize → tools/list → get_capabilities → structural query` sequence against *published* state. Currently red against the live `latest` npm package — see the callout below the table; that is this gate correctly catching a real problem, not a bug in the gate. **Local pre-publish smoke added in PR `feat/ux-pendings-round-1`** (`scripts/smoke-npm-publish.sh`) catches the same class of regression before tagging a release — `npm pack --dry-run` content, `bin/lain.js` body has `ensureBinary()`, version-drift between `Cargo.toml` and `npm-shim/package.json`, and the launcher in the tarball matches the in-tree file byte-for-byte. |

> **🔴 Live finding (2026-09-17), unresolved:** the in-tree code is ready
> (commit `609f8db` rewrote `npm-shim/scripts/runtime.js` and the bundled
> `bin/lain.js` calls `ensureBinary()`), but the **published** state on npm
> is still `0.6.1` — that tarball predates the rewrite, has no
> `LAIN_CACHE_DIR` / `LAIN_VERSION` support, performs no checksum
> verification, and exits 1 on a clean machine. The `v0.6.1` GitHub
> release also has **no `SHA256SUMS` asset** at all. Net effect: `npx
> @spuentesp/lain-mcp` on a clean machine today installs nothing and
> exits 1 — the exact headline command this whole roadmap is built
> around fails for every stranger.
>
> **What flips the row above to "✅ Done":** a stable release cut from
> a commit on `main` that contains `609f8db` (already there), with
> `npm publish` succeeding and `latest` moving to the new version.
> The release workflow has a documented `publish-npm-retry`
> escape hatch (`gh workflow run release.yml -f tag=v0.x.y`) for
> the case where the first publish attempt fails after binaries
> are already on GitHub Releases. See
> [`docs/DISTRIBUTION.md`](DISTRIBUTION.md) for the pre-release
> checklist that catches this class of regression, and the
> "After the release lands" section for the post-publish sanity
> checks (`npx @spuentesp/lain-mcp@<tag> --version` on a clean
> machine, then move the M1 row to "✅ Done" in this table).

## Product outcome

The target experience is intentionally boring:

```bash
npx @spuentesp/lain-mcp mcp
```

or, after installation:

```bash
lain setup
lain doctor
lain mcp
```

The user should not need to know that LAIN is written in Rust, where the graph is stored, which language server backs a capability, whether an embedding model is installed, or which of dozens of MCP tools should be called first.

The product promise is:

1. **Install without compiling.**
2. **Detect the repository automatically.**
3. **Expose useful structural intelligence immediately.**
4. **Degrade gracefully while optional capabilities warm up.**
5. **Give agents a small semantic interface instead of forcing tool archaeology.**
6. **Make health, freshness, and readiness machine-readable.**
7. **Keep advanced internals available without making them the default UX.**

---

## UX principles

### 1. Progressive disclosure

The default path should contain the fewest concepts possible. Advanced users can still reach federation, custom models, custom LSPs, graph operations, and low-level tools.

Default:

```text
install → setup → agent connects → useful answer
```

Advanced:

```text
custom install → explicit config → federation → low-level graph/LSP tools
```

### 2. Zero dead ends

Every failure should answer three questions:

- What happened?
- What still works?
- What can the user or agent do next?

Bad:

```text
semantic search unavailable
```

Better:

```text
Semantic search is warming up.
Structural search, symbol lookup, call graph, and blast radius are ready.
Run `lain doctor --fix` to install the optional model now.
```

### 3. Agent-readable first, human-readable always

Anything that describes runtime state should have a stable structured form. Human CLI output can remain attractive, but JSON must be available for automation.

```bash
lain doctor --json
lain capabilities --json
lain status --json
```

### 4. Fast path before perfect path

Structural intelligence should become usable before optional enrichment finishes. AST/tree-sitter, Git, and cached graph data should not be blocked by embeddings or slow language-server startup.

### 5. One canonical path

There can be multiple installation mechanisms, but the docs should recommend one. There can be many MCP clients, but setup should present one consistent configuration model.

---

# Milestone 1 — Frictionless distribution

**Status: 🟡 Partial.** The npm launcher (`npm-shim/scripts/runtime.js`, `609f8db`) verifies checksums, caches by version+target, and honors `LAIN_VERSION`. What's missing is the clean-room CI proof (Milestone 9) that `npx @spuentesp/lain-mcp --version` actually works on a bare machine.

## User story

> I want to try LAIN in an arbitrary repository without installing Rust or reading installation documentation.

## Target UX

Preferred:

```bash
npx @spuentesp/lain-mcp mcp
```

Installed form:

```bash
npm install -g @spuentesp/lain-mcp
lain --version
```

Native packages remain valid secondary paths:

```bash
brew install spuentesp/tap/lain
cargo install lain
```

## Design

The npm package should be a thin native launcher, not an alternate implementation.

```text
npx
 │
 ▼
@spuentesp/lain-mcp
 │
 ├─ detect OS + architecture
 ├─ resolve LAIN version
 ├─ find cached binary
 ├─ verify checksum
 ├─ download release asset if missing
 └─ exec native binary
```

### Release artifacts

Preserve the existing versioned `.tar.gz` archive contract on GitHub Releases (`{version}` excludes the leading `v`):

```text
lain-{version}-x86_64-unknown-linux-gnu.tar.gz
lain-{version}-aarch64-apple-darwin.tar.gz
lain-{version}-x86_64-pc-windows-msvc.tar.gz
SHA256SUMS
```

Archives contain `lain` at the root, or `lain.exe` on Windows. Add `SHA256SUMS` covering the exact published archives. The initial supported targets are Linux x64, macOS arm64, and Windows x64. Linux arm64 and macOS x64 remain deferred until release builds, launcher mappings, and clean-room installation/MCP tests exist.

Keep the npm installer and Homebrew formula aligned with these filenames. Any future naming or archive-format change must update release packaging, both consumers, and installation tests together before publication.

### Launcher behavior

Cache binaries under an OS-appropriate per-user cache directory. The launcher should:

- never require administrator privileges;
- verify SHA-256 before execution;
- use an atomic download/rename flow;
- support `LAIN_VERSION` for pinning: an explicit value takes precedence over the npm package version; otherwise use the exact npm package version, never an implicit latest-release lookup;
- support offline reuse of an already cached binary;
- print download progress only when attached to a TTY;
- keep MCP stdout protocol-clean by sending diagnostics to stderr.

Forward explicit CLI arguments unchanged; the canonical MCP invocation includes `mcp`. Bare invocation may retain native help behavior. Cache entries must include version and target. Require tag, Cargo metadata, package metadata, archive name, and executable `--version` to agree for each release; the explicit version override selects a separately verified release.

## Acceptance criteria

- Clean Linux x64, macOS arm64, and Windows x64 runners can execute `npx @spuentesp/lain-mcp --version`.
- The exact command `npx @spuentesp/lain-mcp mcp` passes an MCP initialize/tools-list round trip with protocol-clean stdout.
- No Rust toolchain is required.
- A second invocation uses the cache and performs no network download.
- Corrupt or mismatched checksums fail closed with a useful error.
- Release CI proves every advertised artifact boots.

---

# Milestone 2 — `lain setup`: guided, modern onboarding

**Status: ✅ Done** (for the `generic` and `claude-code` adapters). `src/cli/setup.rs`, commit `7132baa`. Detection, model auto-install, idempotent re-run with backup, `--dry-run`/`--print-config`, and a real MCP verification round trip are all implemented and tested (`tests/setup_e2e.rs`). Codex/Cursor/VS Code/Continue adapters are Milestone 8's remaining scope.

## User story

> I cloned a repository. Configure LAIN for my agent without making me understand MCP internals.

## Target UX

```text
$ lain setup

  LAIN setup

  Repository        ✓ ~/src/my-project
  Languages         ✓ Rust, TypeScript
  Structural index  ✓ ready
  Git history       ✓ ready
  Semantic search   ○ optional model not installed

  Choose an agent
  › Claude Code
    Codex
    Cursor
    VS Code
    Continue
    Generic MCP

  Configuration     ✓ written
  Connection        ✓ verified

  Ready. Ask your agent:
  “What is the blast radius of changing validate_token?”
```

Non-interactive forms:

```bash
lain setup --agent claude-code
lain setup --agent codex
lain setup --agent cursor
lain setup --agent generic --json
```

## Design

`setup` should be idempotent. Re-running it must inspect and update rather than duplicate configuration.

### Detection phase

Detect:

- Git root;
- languages and frameworks;
- existing `.lain` state;
- installed language servers;
- existing agent configuration;
- semantic model availability;
- whether the repository is already indexed.

### Configuration adapters

Implement client-specific adapters behind one interface:

```text
AgentAdapter
 ├─ detect()
 ├─ config_path()
 ├─ read()
 ├─ plan_changes()
 ├─ apply()
 └─ verify()
```

Adapters should exist for supported clients, while `generic` prints or writes standard MCP JSON.

### Safety

Before modifying a client config:

- parse it structurally;
- preserve unrelated settings;
- show a concise plan in interactive mode;
- create a timestamped backup when modifying a user-owned file;
- expose `--dry-run`;
- expose `--print-config` for users who do not want automatic writes.

## Acceptance criteria

- Re-running setup causes no duplicate MCP entries.
- Every adapter has fixture tests for empty, existing, malformed, and already-configured files.
- `--dry-run` performs no writes.
- Setup can succeed even when semantic search is unavailable.
- The final verification starts LAIN and performs an MCP initialize/tools-list round trip.

---

# Milestone 3 — `lain doctor`: one trustworthy diagnosis surface

**Status: ✅ Done.** `lain doctor` / `lain capabilities --json` / `lain status --json` all exist and share the readiness model in `src/server/readiness.rs`.

`doctor` already exists. The UX milestone is to make it the single authoritative answer to “is LAIN usable here?”

## Target UX

```text
$ lain doctor

  LAIN 0.x.x                 agent-ready

  Core
  ✓ Repository     ~/src/my-project
  ✓ Git            2.5x
  ✓ Graph          current @ 8d5a6fc
  ✓ MCP stdio      healthy

  Intelligence
  ✓ Symbols        ready
  ✓ Call graph      ready
  ✓ Git co-change   ready
  ○ Semantic        optional model not installed

  Integrations
  ✓ Claude Code     configured
  - Cursor          not configured

  Agent-ready: YES
```

Machine form:

```bash
lain doctor --json
```

Suggested schema:

```json
{
  "schema_version": 1,
  "server_version": "0.x.x",
  "agent_ready": true,
  "repository": {
    "root": "/repo",
    "head": "8d5a6fc",
    "indexed_commit": "8d5a6fc",
    "working_tree_overlay": true
  },
  "capabilities": {
    "symbols": { "state": "ready", "optional": false },
    "call_graph": { "state": "ready", "optional": false },
    "git_history": { "state": "ready", "optional": false },
    "semantic_search": { "state": "unavailable_optional", "optional": true }
  },
  "integrations": {
    "claude_code": "configured"
  },
  "problems": []
}
```

### `--fix`

Where the repair is deterministic and safe:

```bash
lain doctor --fix
```

Examples:

- recreate missing cache directories;
- remove stale session files;
- repair a generated MCP entry;
- download an optional model after confirmation;
- trigger a stale index refresh.

Never silently install unrelated system packages or mutate source files.

## Acceptance criteria

- `doctor --json` includes integer `schema_version` independently of `server_version`; incompatible schema changes increment it. Additive fields are permitted and consumers ignore unknown fields.
- Exit codes distinguish ready, degraded-but-usable, and unusable.
- Every reported problem includes a remediation action where possible.
- MCP stdout remains clean when doctor logic is reused during startup.

---

# Milestone 4 — Zero-config MCP startup

**Status: 🟡 Implementation sequence done, design not fully closed.** All 10 steps of the implementation sequence below are landed (`5a4b99c` … `b558a1b`) — see "Status" at the top of this document for the full breakdown. PR #66 layered `PerRepoReadiness` + `FederatedIndex::per_repo_readiness()` on top of the existing federation gate, and added the agent-side annotation + handoff layer (5 new MCP tools, per-repo SQLite storage) plus the CI-side enrichment of `lain-health-badge` (top-of-comment readiness line, per-file annotations, previous-run delta). What remains is outside the numbered sequence but required by this milestone's own design: a real cooperative cancellation token through the indexing phases, and `spawn_blocking` isolation of the pipeline's synchronous work (the design section above requires this design to "pass the blocked-indexer responsiveness test," which hasn't been built).

## User story

> My agent starts LAIN from somewhere inside the repository. LAIN figures out the rest.

## Target lifecycle

```text
MCP process starts
      │
      ▼
find nearest .git root
      │
      ▼
start MCP transport
      │
      ├─ initialize + tools/list → available immediately
      │
      ▼
inspect existing graph
      │
      ├─ graph current → graph tools ready
      │
      └─ graph stale/missing
                │
                ├─ graph tools return structured warming_up
                ├─ health/capability tools remain available
                └─ refresh in background → ready
```

The MCP handshake must not wait for repository indexing. A client can initialize,
list tools, and inspect health/readiness while LAIN builds the graph. The first
successful graph query must not require a separate `lain init` step.

Repository-root resolution is the only pre-transport prerequisite. If no repository
can be resolved, LAIN writes one remediation message to stderr, writes nothing to
stdout, and exits with code 2. `doctor --json` and `status --json` represent the same
condition with problem code `repository_not_found`; an MCP tool cannot report it
because no repository-scoped server is constructed.

### Loading behavior

Starting the protocol early does **not** authorize tools to query a graph while it
is being rebuilt. Until a complete index is ready, graph-dependent tools return a
stable structured loading result instead of hanging, returning partial answers, or
failing with an opaque error:

```json
{
  "state": "warming_up",
  "message": "LAIN is indexing this repository.",
  "progress": {
    "files_completed": 420,
    "files_total": 1100
  },
  "retry_after_ms": 3000,
  "available_tools": ["get_health", "get_capabilities"]
}
```

The MCP dispatcher enforces this gate centrally. Health, capability,
server-status, and other explicitly graph-independent operations remain callable;
graph-dependent operations become callable only after the indexing state transitions
to `ready`. Tool descriptions must tell agents that a `warming_up` response is
retryable and point them to the capability tool for current progress.

The initial implementation gates graph tools for both missing and stale indexes.
Serving an old graph while a new graph is built is explicitly outside this
milestone.

### Readiness model

Capabilities have these explicit states:

```text
ready
warming_up
stale_usable
unavailable_optional
unavailable_error
```

Doctor, bootstrap, discovery, and CLI status share the same capability keys (`symbols`, `call_graph`, `git_history`, `semantic_search`) and objects with required `state` and `optional` fields. The only optional diagnostic fields in schema version 1 are `reason`, `remediation`, and `retry_after_ms`; omit them when they do not apply.

| State | Available behavior | Agent action |
|---|---|---|
| `ready` | Capability can answer from current data. | Call normally. |
| `warming_up` | Capability cannot answer yet; the MCP protocol and other ready capabilities remain usable. | Use a ready alternative or retry after the suggested delay. |
| `stale_usable` | Capability can answer from an intact, immutable stale snapshot with explicit freshness metadata. A graph being rebuilt in place never qualifies. | Use when freshness permits; request refresh otherwise. |
| `unavailable_optional` | An optional dependency is absent or disabled; this capability cannot answer. | Fall back or request optional setup. |
| `unavailable_error` | A failure prevents this capability from answering. | Use an alternative and follow remediation. |

`agent_ready` is true when required capabilities (`optional: false`) are all `ready` or `stale_usable` and MCP transport is healthy. Doctor exits 0 when required capabilities are current and no capability has an error or is warming/stale; absent optional dependencies alone do not change that. It exits 1 for usable but degraded state and 2 when required capabilities or transport are unusable. Test all states, transitions, and aggregate/exit-code mappings.

### Startup rules

- Find the nearest Git root by walking upward.
- Start the MCP transport before waiting for repository indexing.
- Keep `initialize`, `tools/list`, and graph-independent health/readiness operations responsive while indexing runs.
- Gate graph-dependent tools centrally until a complete usable graph is available.
- Return a structured `warming_up` result with progress and retry guidance; never expose a partially rebuilt graph.
- Use a current cached graph immediately when one is available.
- Treat stale serving as optional: only serve a stale graph when it remains an intact snapshot isolated from the graph being rebuilt.
- Apply the working-tree overlay before claiming freshness.
- Reindex stale or missing graphs in the background after the MCP transport starts.
- Never block structural tools on optional semantic initialization.
- Emit readiness notifications/events when capabilities transition to ready.

### Authoritative startup state

Startup state has one owner and one snapshot type. Do not derive a second startup
state independently inside `doctor`, individual tools, or transport code. The Rust
type is `IndexLifecycleSnapshot` in `src/server/readiness.rs` and contains:

```text
IndexLifecycleSnapshot
├─ sequence: u64                  monotonically increases on every transition
├─ attempt_id: u64                monotonically increases for each new index attempt
├─ state: warming_up | ready | unavailable_error
├─ phase: discovering | scanning | resolving | enriching | persisting | overlay
├─ started_at_unix_ms: integer
├─ completed_at_unix_ms: integer or null
├─ target_commit: string or null
├─ indexed_commit: string or null
├─ files_total: integer or null
├─ files_completed: integer
├─ files_failed: integer
├─ retry_after_ms: integer or null
├─ problem: Problem or null        blocking terminal problem only
└─ warnings: array of Problem      sorted by code, then message

Problem
├─ code: stable machine-readable string
├─ message: human-readable string
├─ remediation: human-readable next action
└─ retryable: boolean
```

Store the snapshot behind one shared synchronization boundary so readers cannot
combine fields from different transitions. The server, tool executor, capability
projection, status output, and health output all receive the same shared handle.
Progress updates increment `sequence`; timestamps use UTC Unix milliseconds; absent
or not-yet-known values serialize as `null`, never as invented zero values.

Allowed state transitions are:

```text
process start
    → warming_up/discovering
    → warming_up/scanning
    → warming_up/resolving
    → warming_up/enriching
    → warming_up/persisting
    → warming_up/overlay
    → ready

any warming_up phase
    → unavailable_error

warming_up/*
    → warming_up/discovering with attempt_id + 1
      only when HEAD changes before ready is published
```

Phases follow the displayed order. The coordinator skips a phase only when its
planned work count is zero. It may not move backward within an attempt. A `HEAD` change starts a new
attempt instead of moving the current attempt back to `scanning`.

`ready` and `unavailable_error` are terminal for the startup attempt. A later
explicit refresh creates a new attempt with a higher `attempt_id` and `sequence`
and transitions back to `warming_up`; it never mutates the old attempt's identity
in place. Invalid transitions fail tests and log an internal error. Only the
indexing coordinator may change lifecycle state. Workers report progress to the coordinator but do not write
the shared snapshot directly.

For capability projection during startup:

- MCP transport becomes healthy when the stdio loop or HTTP listener is serving.
  A successful `initialize` records client-specific handshake health but does not
  mutate repository readiness for other clients.
- `git_history` is `ready` after the repository opens and `HEAD` is readable.
- `symbols` and `call_graph` remain `warming_up` until base indexing, persistence,
  and initial working-tree overlay reconciliation all succeed.
- `semantic_search` is `warming_up` only when a model is configured and its required
  index work is in progress. Without a configured model it is
  `unavailable_optional`, never `unavailable_error`.
- `agent_ready` remains false while either required structural capability is
  `warming_up` or `unavailable_error`.

### MCP method and tool behavior

Protocol methods are never subject to the graph gate:

- `initialize`;
- `notifications/initialized`;
- `ping`;
- `tools/list`.

`tools/list` always returns the stable advertised surface. Tools do not disappear
and reappear as readiness changes because many MCP clients cache this response.

Every advertised tool must declare exactly one readiness requirement in its
canonical definition:

```text
protocol_only       no tools use this; reserved for MCP methods
graph_independent   callable during startup
graph_required      requires symbols/call graph readiness
semantic_required   requires semantic readiness and graph readiness
```

There is no implicit permissive default. Schema generation and tests fail when a
tool lacks a classification or names an unknown requirement. The initial
`graph_independent` set is deliberately small: capability discovery, `get_health`,
and `get_server_status`. A graph-independent diagnostic may read synchronized
counters or metadata from a graph under construction, but it must label them as
intermediate and must not return repository-intelligence conclusions from them.
Additional tools may join this set only with a test proving they do not depend on a
complete graph or overlay to produce a correct answer.

The central MCP `tools/call` dispatcher evaluates the declared requirement before
calling any handler. Individual handlers must not implement their own startup
checks. During warm-up:

- `graph_independent` tools dispatch normally;
- `graph_required` and `semantic_required` tools return the loading result without
  entering their handlers;
- semantic tools return `unavailable_optional` instead when no model is configured;
- unknown tools continue to use the normal unknown-tool error path.

The loading result is a successful MCP tool result (`isError: false`) because
warm-up is an expected retryable state, not a failed request. Return the object in
`structuredContent` and mirror the same JSON in a text content item for clients
that do not render structured content:

```json
{
  "schema_version": 1,
  "attempt_id": 1,
  "sequence": 7,
  "state": "warming_up",
  "capability": "symbols",
  "message": "LAIN is indexing this repository.",
  "progress": {
    "phase": "scanning",
    "files_completed": 420,
    "files_total": 1100,
    "files_failed": 0
  },
  "retry_after_ms": 3000,
  "next_action": {
    "tool": "get_capabilities",
    "arguments": {}
  }
}
```

Fields and ordering in the mirrored JSON are deterministic. In schema version 1,
`retry_after_ms` is always 3000; changing that contract requires an explicit schema
decision rather than an undocumented environment knob. Agents may poll more slowly;
the server does not require a call to acknowledge the transition.

Gated calls return immediately. The server does not queue, hold, or replay the
original tool call after readiness changes; the client must issue a new call. This
prevents abandoned client requests from accumulating during a long index.

When startup ends in `unavailable_error`, gated tools return `isError: true` with
the same stable envelope plus `problem`. The response must say what still works and
provide a concrete remediation. It must not include a Rust debug representation or
backtrace unless verbose diagnostics were explicitly requested.

Startup uses these stable problem codes:

| Code | Trigger | Retryable | Required remediation |
|---|---|---:|---|
| `repository_not_found` | No Git root can be resolved. | no | Run inside a Git repository or pass `--workspace`. |
| `graph_corrupt` | Persisted graph cannot be decoded or validated. | no | Run `lain doctor --fix` to quarantine it and rebuild. |
| `index_startup_timeout` | One attempt exceeds the configured startup timeout. | yes | Retry or increase `--reindex-timeout`. |
| `index_no_usable_files` | Eligible source files exist but every scheduled file fails all supported parsers. | yes | Inspect verbose diagnostics, repair the parser/toolchain issue, and retry. |
| `index_failed` | Scan, resolve, or enrichment returns a non-file-specific failure. | yes | Inspect verbose diagnostics and retry. |
| `persistence_failed` | The complete graph cannot be written atomically. | yes | Check `.lain` permissions/free space and retry. |
| `overlay_reconciliation_failed` | Initial dirty-worktree reconciliation fails. | yes | Check language tooling or use verbose diagnostics, then retry. |
| `watcher_start_failed` | Source watcher cannot establish its readiness barrier. | yes | Check OS watcher limits/permissions and retry. |

Unsupported and intentionally ignored files are excluded before `files_total` is
fixed. A scheduled file that fails all supported parsing paths increments
`files_failed`. Some failed files do not disable the whole repository: when at least
one eligible file indexes successfully and graph invariants pass, structural
capabilities may become `ready` with an `index_files_failed` warning containing the
exact failed count. When eligible files exist and none index successfully, startup
ends with `index_no_usable_files`. A repository with zero eligible source files is a
valid empty index and may become `ready`. Optional semantic-model failure affects
only `semantic_search` and does not use a required structural problem code.

For `graph_corrupt`, `lain doctor --fix` renames the unreadable file to
`graph.bin.corrupt-<UTC timestamp>` in the same directory, reports the recovery
path, and starts a clean rebuild. It never deletes or overwrites the corrupt file.
Without `--fix`, diagnosis is read-only and startup stays `unavailable_error`.

### Index execution and consistency

The background task continues using the current incremental graph writer, but
no graph-dependent handler may run until the whole startup attempt succeeds. Per-
batch writes and intermediate persistence are implementation progress, not a usable
snapshot. `indexed_commit` does not advance unless the complete pass satisfies the
existing full-pass rules.

Background indexing must also be execution-isolated from protocol I/O. The current
`lain mcp` entry point uses a single-thread Tokio runtime. Change it to a
multi-thread runtime with `max(2, available_parallelism)` worker threads. Moving
indexing into `tokio::spawn` on the old runtime alone does not satisfy this milestone because
blocking Git, parser, LSP, model, or filesystem work could still starve
`initialize`, `ping`, or `tools/list`. Route synchronous Git, parser, model, and
filesystem work through `spawn_blocking`; keep async LSP I/O on Tokio tasks. The
index coordinator and every blocking child handle are server-owned, cancellable,
and joined. This design must pass the blocked-indexer responsiveness test below.

The coordinator performs these steps in order:

1. Resolve and validate the repository root before constructing repository state.
2. Construct the MCP handler and lifecycle snapshot with `warming_up/discovering`.
3. Construct the transport and handler without awaiting repository indexing.
4. Spawn exactly one startup indexing task on the isolated execution lane; retain
   its cancellation handle and join handle in server-owned lifecycle state.
5. Enter the MCP serving loop immediately. No startup-index future is awaited on the
   protocol I/O path.
6. Read `HEAD`, the persisted indexed commit, and the intended file count.
7. If the persisted graph is already current, skip scanning but still reconcile the
   initial working-tree overlay before transitioning to `ready`.
8. Otherwise run scan, resolve, enrich, and persistence phases while publishing
   monotonic progress.
9. Reconcile the working-tree overlay. A successful base index without this step is
   not `ready`.
10. Start or confirm the source watcher and commit-sync worker.
11. Re-read `HEAD`. If it changed during startup, remain `warming_up` and immediately
    index toward the new commit; do not briefly publish `ready` for the superseded
    commit.
12. Transition once to `ready` only when `indexed_commit == HEAD`, required graph
    invariants pass, persistence succeeded, and the overlay represents the current
    working tree.

Indexing and watcher-driven mutation must continue to use the existing repository
change lock. Events arriving during the startup pass must be queued or followed by
the final reconciliation in step 9; no event may be silently lost in the handoff
between initial reconciliation and watcher activation. The implementation must add
a test-visible watcher-ready barrier rather than depending on sleeps.

The existing startup timeout remains a bound on one attempt. On timeout, cancel and
join the indexing task, persist only progress that satisfies existing persistence
invariants, and transition to `unavailable_error` with code
`index_startup_timeout`. Do not leave a detached indexer mutating the graph after
the error is published. A normal MCP shutdown also cancels and joins the startup
task and stops watcher/commit-sync workers before process exit.

Cancellation is cooperative as well as task-level. The scan loop checks a shared
cancellation token before scheduling a batch, after each completed batch, before
each persistence operation, and between resolve, enrich, persistence, and overlay
phases. Once cancellation is observed, no new graph write may begin. The coordinator
waits for any already-held graph write lock to finish before publishing
`unavailable_error` or completing shutdown. Tests use injected barriers and tokens;
production code must not depend on timing sleeps.

Because the current writer mutates the graph incrementally, a failed or timed-out
attempt is never exposed as `stale_usable`. The next process may reuse the persisted
progress to continue indexing, but graph tools remain gated until a complete pass
succeeds.

### Current, stale, and missing graph policy

This milestone implements exactly these policies:

| Initial graph | Startup behavior | Graph-tool behavior |
|---|---|---|
| Current at `HEAD` | Validate and reconcile overlay in background. | `warming_up` until reconciliation completes, then `ready`. |
| Stale | Refresh in background using existing incremental persistence. | `warming_up`; do not serve the graph during mutation. |
| Missing | Build in background. | `warming_up`. |
| Corrupt/unreadable | Record typed failure; do not silently replace unless an existing safe recovery rule authorizes it. | `unavailable_error`. |
| Refresh failed/timed out | Cancel and join the attempt; retain valid persisted progress for a future retry. | `unavailable_error`. |

`stale_usable` remains part of the shared capability schema for intact snapshots,
but startup does not emit it in this milestone. Supporting it later requires a
separate shadow-build-and-atomic-swap design. That optimization must not be partially
implemented through the live incremental writer.

### Multiple repositories

Each repository owns its own lifecycle snapshot. Repository-scoped tools gate only
on the selected repository. A tool spanning several repositories may run only when
every repository in its resolved input set satisfies its declared requirement; if
not, its loading response lists the blocking repository IDs in deterministic sorted
order.

Capability and federation-health responses include both per-repository state and an
aggregate. Aggregate required capabilities are `ready` only when every configured
repository required by the active workspace is ready. Optional semantic absence in
one or more repositories does not make the aggregate unusable. A failed repository
does not prevent health or capability calls for ready repositories.

### Progress and notifications

Progress is derived from actual scheduled source files:

- `files_total` is fixed when a scan attempt is planned;
- `files_completed` counts successful and failed file attempts and never decreases;
- `files_failed` is required, counts failed file attempts, never decreases, and is
  always less than or equal to `files_completed`;
- phase changes and meaningful progress updates increment `sequence`;
- snapshot progress is published once per completed ingestion batch;
- notifications are emitted at most four times per second, coalescing intermediate
  progress and retaining the newest snapshot.

On every externally visible state transition, emit one advisory MCP notification:

```text
notifications/lain/capabilities_changed
```

Its payload is the same capability snapshot returned by discovery. Clients are not
required to support the custom notification, so polling `get_capabilities` remains
the canonical fallback. Notification delivery failure is logged to stderr and does
not affect indexing state or MCP stdout.

### Implementation sequence

Land Milestone 4 as a series of reviewable commits or PRs. Each step must leave the
tree green and must not introduce a second temporary state model:

1. Add lifecycle and problem DTOs, transition validation, serialization fixtures,
   and capability projection tests.
2. Add mandatory readiness classification to every canonical tool definition and
   validate the complete generated tool surface.
3. Add central dispatch gating and loading/error response fixtures while retaining
   the existing blocking startup.
4. Instrument indexing phases and monotonic file progress through the coordinator.
5. Replace the pre-transport await with a server-owned background task for stdio;
   add cancellation and joining on every exit path.
6. Apply the same lifecycle helper to HTTP transport so behavior cannot drift.
7. Add the watcher-ready handoff and final `HEAD`/overlay reconciliation.
8. Add per-repository federation state and deterministic multi-repository gating.
9. Add capability-change notifications and verify polling-only clients behave
   identically.
10. Remove the old blocking-startup helper and tests only after all replacement
    contract tests pass; update documentation and the tool-schema snapshot in the
    same change.

No step may leave both the old awaited indexer and the new background coordinator
reachable in production. Search-based structural tests must assert that there is
one startup-index entry point for each transport.

Required ownership and code placement:

| Concern | Canonical location | Rule |
|---|---|---|
| Lifecycle DTOs, transition validation, capability projection | `src/server/readiness.rs` | No transport or CLI-specific formatting. |
| Shared lifecycle handle on the server/tool context | `src/server/ingest/server.rs` and `src/server/tools/registry.rs` | All consumers clone the same handle. |
| Index phase/progress reporting | `src/server/ingest/ingestion.rs` | Report through a coordinator-owned progress sink; never format MCP responses here. |
| Tool readiness requirement | `src/server/tools/definitions.rs` and all special definitions in `src/server/mcp/definitions.rs` | Exactly one requirement per advertised tool. |
| Central gate and MCP response construction | `src/server/mcp/handler.rs` | One gate before every tool handler path. |
| Background task ownership and transport startup | `src/server/mcp/handler.rs` | Shared helper used by stdio and HTTP. |
| Capability MCP tool | `src/server/mcp/definitions.rs` plus its handler | Thin serialization of the shared projection. |
| CLI JSON/human projections | `src/cli/doctor.rs` and new `src/cli/capabilities.rs` / `src/cli/status.rs` | Consume shared DTOs; do not recompute readiness. |
| Contract tests | `tests/mcp_cold_start.rs`, new readiness unit tests, and schema fixtures | No sleep-based readiness assertions. |

If implementation reveals that a listed file is no longer canonical because of a
landed refactor, update this ownership table in the same PR that changes the target;
do not silently place a second implementation elsewhere.

### Explicit non-goals

Milestone 4 does not:

- serve stale graph queries while a replacement graph is being built;
- expose partial results from the live incremental writer;
- dynamically add or remove tools from `tools/list`;
- queue and replay graph-tool calls made during warm-up;
- make optional semantic initialization a prerequisite for structural readiness;
- automatically delete corrupt graphs;
- require clients to understand custom notifications.

These exclusions are final for this milestone. Any later proposal that changes one
requires its own design, acceptance criteria, and issue; it is not a missing tail of
the startup implementation.

## Acceptance criteria

- Starting `lain mcp` inside a nested repository directory works.
- In an integration test where the indexer is held at an injected barrier,
  `initialize`, `ping`, and `tools/list` each complete within 2 seconds on every CI
  platform; existing warm-path performance budgets remain unchanged.
- An immediate graph-dependent call during cold indexing returns structured `warming_up` state with retry guidance, not a partial answer.
- Health/capability discovery remains callable during indexing and reports deterministic progress.
- Graph-dependent tools transition to normal answers only after the complete index is ready.
- An indexing failure transitions required graph capabilities to `unavailable_error` with remediation.
- On the committed tiny cold-start fixture, a current cached graph reaches `ready`
  within 5 seconds and its first structural call remains within the existing tool
  performance budget.
- A missing semantic model does not fail MCP startup.
- A stale graph is gated as `warming_up` for this milestone and is never dispatched
  as `stale_usable` through the live incremental writer.
- MCP stdout remains protocol-clean throughout startup and indexing.
- Every advertised tool has one validated readiness requirement, and all gated
  calls pass through the central dispatcher.
- Progress counters are monotonic and state transitions follow the allowed state
  machine under concurrent polling.
- A `HEAD` change during startup is indexed before `ready` is published.
- Timeout, indexing failure, corrupt graph, and shutdown paths leave no detached
  indexing or watcher tasks.
- Cancellation tests prove that no graph write begins after cancellation is
  observed and that process shutdown joins the coordinator within 5 seconds when
  workers are at cooperative cancellation points.
- Single-repository and federation tests cover current, stale, missing, failed, and
  mixed-readiness repositories.
- Loading and failure responses have byte-stable JSON fixtures with no ANSI escapes.
- The former cold-start assertion is replaced: an immediate structural call may
  return `warming_up`, and the same call must return a normal non-empty answer after
  readiness transitions to `ready`.

---

# Milestone 5 — Agent bootstrap context

**Status: ⬜ Not started.** `understand_repository` does not exist anywhere in the codebase.

## Problem

A new agent should not spend several calls learning what repository it is in, which LAIN capabilities are available, and which tools are appropriate.

## Primary MCP tool

```text
understand_repository
```

`understand_repository` is the public semantic tool name. Internally it may compose
a bootstrap-context builder, but `bootstrap_repository_context` must not be exposed
as a second public alias because that recreates tool-selection ambiguity.

Example response:

```json
{
  "schema_version": 1,
  "server_version": "0.x.x",
  "repository": {
    "name": "lain",
    "languages": ["Rust", "JavaScript"],
    "head": "8d5a6fc",
    "dirty": false
  },
  "architecture": {
    "entry_points": ["src/main.rs"],
    "anchors": ["Server", "GraphDatabase", "Indexer"],
    "important_paths": ["src/server", "src/server/graph.rs", "tests/use_cases"]
  },
  "capabilities": {
    "symbols": { "state": "ready", "optional": false },
    "call_graph": { "state": "ready", "optional": false },
    "git_history": { "state": "ready", "optional": false },
    "semantic_search": { "state": "warming_up", "optional": true }
  },
  "recommended_actions": [
    {
      "intent": "understand a symbol",
      "tool": "get_context"
    },
    {
      "intent": "assess a change",
      "tool": "assess_change"
    }
  ]
}
```

The payload should respect a token/size budget:

```json
{ "budget_tokens": 3000 }
```

## Design constraints

- deterministic ordering;
- stable top-level schema;
- no giant source dumps;
- include freshness and confidence metadata;
- favor repository anchors, entry points, and architectural boundaries;
- use semantic enrichment only when already available.

## Acceptance criteria

- A fresh MCP client can orient itself with one call.
- The tool stays useful when semantic search is absent.
- Responses remain bounded and deterministic enough for regression tests.

---

# Milestone 6 — A small semantic Agent API

**Status: 🟢 Implemented.** The five semantic tools (`find_symbol`,
`get_context`, `find_related`, `assess_change`, `search_code`)
landed via `feat/m6-semantic-agent-api` (#95); the
`recommended_actions` map in `understand_repository` flipped
their availability flags via `feat/m5-m6-flags` (#103). The
wire-level filter that makes them the default `tools/list`
response (`LAIN_TOOL_PROFILE=semantic` by default, `=full`
opt-out) shipped as a follow-up. ("Semantic" here means
intent-organized, not embedding-based — don't confuse it with
the semantic-search *model*, which Milestone 2's `lain setup`
already auto-installs.)

The semantics layer is also explicit about NOT removing the
79-tool detailed surface — `LAIN_TOOL_PROFILE=full` opts back
in to everything, and `get_agent_strategy` is kept inside the
default semantic surface as an escape hatch that returns the
full markdown documentation on demand. Operationally that
means a default install feels like 14 tools and an opt-out
flip recovers the full 79; an LLM agent that doesn't self-
discover `get_agent_strategy` will still have the curated
high-level surface to lean on.

## Problem

LAIN's detailed MCP surface is powerful, but a large tool list increases selection cost and makes client behavior less predictable.

Do not remove the low-level tools. Add a preferred semantic layer.

## Proposed primary tools

```text
understand_repository
find_symbol
get_context
find_related
assess_change
search_code
query_graph
get_capabilities
get_health
```

Possible intent mapping:

| Agent intent | Primary tool | Internal composition |
|---|---|---|
| “Where is X?” | `find_symbol` | symbol index, aliases, paths |
| “Explain X” | `get_context` | symbol, callers, callees, source, docs |
| “What is connected to X?” | `find_related` | graph edges, co-change, semantic links |
| “What breaks if I change X?” | `assess_change` | blast radius, call sites, tests, co-change |
| “Find code that does Y” | `search_code` | lexical + structural + optional semantic |
| “Understand this repo” | `understand_repository` | bootstrap/anchors/entry points |
| Advanced graph question | `query_graph` | existing graph query surface |
| Fast readiness check | `get_capabilities` | shared capability snapshot only |
| Full diagnosis | `get_health` | capability snapshot plus problems and remediation |

### Tool descriptions matter

Each primary tool description should tell the model:

- when to use it;
- when not to use it;
- expected cost;
- whether it needs semantic readiness;
- which low-level alternatives exist.

### Composition over duplication

Primary tools should orchestrate existing internal capabilities instead of introducing parallel indexing or duplicate business logic.

## Acceptance criteria

- The primary tool set covers common repository navigation and change-analysis tasks.
- Existing specialized tools remain accessible.
- Agent benchmark prompts select the semantic tool more reliably than before.
- Tool schemas remain concise enough for MCP clients with limited context budgets.

---

# Milestone 7 — Capability discovery and readiness

**Status: ✅ Done.** `get_capabilities` exists and is load-bearing in Milestone 4's central gate and notification work.

Introduce one cheap MCP tool named `get_capabilities` that an agent can call before
planning. This is the canonical polling target used by warm-up responses. Do not add
an unnamed endpoint, resource alias, or second discovery tool.

```json
{
  "schema_version": 1,
  "server_version": "0.x.x",
  "repository": "my-project",
  "capabilities": {
    "symbols": { "state": "ready", "optional": false },
    "call_graph": { "state": "ready", "optional": false },
    "git_history": { "state": "ready", "optional": false },
    "semantic_search": {
      "state": "warming_up",
      "optional": true
    }
  },
  "freshness": {
    "head": "abc123",
    "indexed_commit": "abc123",
    "working_tree_overlay": true
  }
}
```

`get_capabilities` performs no Git subprocess, graph traversal, model load, index
mutation, or network call. It serializes the current shared readiness snapshot and
is safe to call frequently. `get_health` remains the more expensive diagnostic
surface and adds problems, remediation, installation checks, and detailed counts.

Expose the same capability model through `lain capabilities --json`; expose
repository freshness, indexing progress, and aggregate readiness through
`lain status --json`. Both CLI responses include `schema_version` and
`server_version` and reuse the core state computation. These are proposed commands,
not existing CLI guarantees.

## Acceptance criteria

- Agents never need to infer readiness from error strings.
- `get_capabilities` is present in `tools/list`, classified
  `graph_independent`, and stays callable throughout indexing.
- Repeated calls without a transition return byte-equivalent structured payloads
  except for fields explicitly documented as clocks.
- Capability state transitions are testable.
- Freshness is explicit.
- Optional capability absence is distinguishable from failure.
- CLI capability and status fixtures cover ready, warming, stale, absent optional dependency, and failed required dependency states. Validate JSON without ANSI escapes, shared schema fields, and doctor-compatible readiness exit codes.

---

# Milestone 8 — First-class client recipes

**Status: ⬜ Not started** (1 of 6 clients partially covered as a side effect of Milestone 2). `claude-code` has an adapter (`lain setup --agent claude-code`, install + auto-config, no CI verification yet); Codex/Cursor/VS Code/Continue have none.

Support a small set of integrations well rather than documenting dozens poorly.

Initial matrix:

| Client | Install | Auto-config | Verified in CI/manual contract |
|---|---:|---:|---:|
| Claude Code | yes | yes | yes |
| Codex | yes | yes | yes |
| Cursor | yes | yes | yes |
| VS Code | yes | yes | yes |
| Continue | yes | yes | yes |
| Generic MCP | yes | config output | yes |

Every integration page should fit this shape:

```text
1. Install LAIN
2. Run `lain setup --agent <name>`
3. Restart client if required
4. Ask one known-good question
```

Avoid client-specific product logic inside the core server. Client adapters belong at the configuration boundary.

---

# Milestone 9 — Distribution acceptance CI

**Status: ✅ Done** (`.github/workflows/distribution-acceptance.yml`, `496938c`) — and it is currently red against the live published package for a real reason, not a false positive. See the callout in the "Status" table at the top of this document.

The real acceptance test is not “Cargo tests pass.” It is “a stranger can install LAIN in a clean environment and an MCP client can talk to it.”

Add a clean-room matrix:

```text
Linux x64
macOS arm64
Windows x64
```

For each target:

1. install only Node where required for the npx path;
2. run the public install command with `CI` and `LAIN_FORCE_INSTALL` unset in that subprocess, exercising the normal user path;
3. verify `lain --version`;
4. create/clone a tiny fixture repository;
5. run `lain doctor --json`;
6. launch MCP stdio using exactly `npx @spuentesp/lain-mcp mcp`;
7. perform `initialize`;
8. list tools;
9. call capability discovery;
10. if required structural capabilities are `warming_up`, poll capability discovery
    no faster than `retry_after_ms` until `ready` or the test's explicit index budget
    expires;
11. call one structural query and require a normal non-loading answer;
12. assert protocol-clean stdout.

The current npm installer skips downloads when `CI` is set unless `LAIN_FORCE_INSTALL=1`. Add a separate automation lane with both `CI=true` and `LAIN_FORCE_INSTALL=1`; retain the unforced user lane above. Use fresh cache directories in both lanes, require an actual downloaded binary, and then verify offline cache reuse. Do not treat a skipped postinstall as a successful installation.

Add a separate release-gate job that downloads exactly the published release artifacts rather than workspace-built binaries.

## Acceptance criteria

A release cannot be considered distribution-green unless the clean-room path passes on every advertised platform.

---

# Visual and interaction language

The CLI should feel calm and intentional rather than verbose.

## Human output

Use:

- short headings;
- aligned status rows;
- `✓`, `○`, `!`, `×` where terminals support Unicode;
- a `NO_COLOR` fallback;
- one recommended next action;
- no animated spinner when stdout/stderr is not a TTY.

Example:

```text
  LAIN setup

  Repository        ✓ ~/src/lain
  Structural graph  ✓ current
  Semantic model    ○ optional
  Claude Code       ✓ configured

  Ready.
```

## Structured output

Every important operational command should support JSON without ANSI escapes:

```bash
lain setup --json
lain doctor --json
lain capabilities --json
lain status --json
```

## Errors

Prefer typed remediation:

```text
× Could not update Claude Code configuration
  ~/.config/claude/config.json contains invalid JSON.

  Nothing was changed.
  Fix the file and run:
    lain setup --agent claude-code
```

Avoid stack traces by default. Offer `--verbose` / `RUST_LOG` for diagnostics.

---

# Suggested implementation order

> Trust and visibility signals (README badges: CI, Scorecard, SafeSkill,
> MSRV, SBOM, provenance, and the pending Agent Contract badge) are
> tracked separately in [`docs/badge_rollout_plan.md`](badge_rollout_plan.md)
> rather than as a numbered phase here — that work is independently
> shippable and orthogonal to the milestones below.

## Phase A — Distribution foundation

1. Release artifact naming and checksums.
2. npm launcher with platform detection and cache.
3. clean-room `npx ... --version` CI.

**Why first:** every other UX improvement is easier to adopt once installation is trivial.

## Phase B — Trust and onboarding

4. Normalize structured capability/readiness state.
5. Modernize `doctor`, including safe corrupt-graph quarantine, and implement `capabilities --json` / `status --json` around that state.
6. Add `get_capabilities` and mandatory readiness classification for every MCP tool.
7. Add central graph-tool gating, background startup indexing, progress, and clean shutdown.
8. Add federation readiness aggregation and capability transition notifications.
9. Implement `setup` core and generic MCP adapter.
10. Add client adapters incrementally.

**Why second:** setup should consume a reliable health/readiness model instead of inventing a parallel one.

## Phase C — Agent-native interface

11. Implement `understand_repository` context bootstrap.
12. Implement the remaining semantic primary tool layer.
13. Complete tool descriptions and recommendation metadata.
14. Add agent selection benchmarks.

**Why third:** simplify the agent surface after the server can reliably report what is ready.

## Phase D — Polish and release gates

15. Add optional background model acquisition UX.
16. Add cross-platform clean-room MCP contract tests.
17. Update Quickstart to the new canonical path.

---

# Proposed issue breakdown

Keep implementation PRs small and independently shippable.

1. **Release: publish native binaries and SHA256SUMS**
2. **npm: native launcher with verified binary cache**
3. **CI: clean-room launcher smoke tests**
4. **Core: structured capability/readiness model**
5. **CLI: redesign `lain doctor` + stable JSON + safe corrupt-graph quarantine** — `--fix` renames, never deletes, before rebuilding.
6. **CLI: implement `lain capabilities --json` and `lain status --json`** — test all capability states, schema version, exit codes, freshness, indexing progress, required failures, and agreement with doctor.
7. **MCP: add `get_capabilities` and classify every advertised tool** — no production behavior changes yet; schema validation rejects missing classifications.
8. **MCP: central readiness gate and stable warm-up/error envelopes** — retain blocking startup until the gate and fixtures are complete.
9. **Index: lifecycle coordinator with phases and monotonic progress** — one shared state handle; no transport-specific state.
10. **MCP: start stdio/HTTP transport before indexing** — isolate indexing from protocol I/O; cancel and join it on every exit path.
11. **Index: deterministic watcher handoff and final HEAD/overlay reconciliation** — add a readiness barrier; do not use sleeps.
12. **Federation: per-repository readiness and deterministic aggregate gating** — mixed-ready repositories remain diagnosable.
13. **MCP: capability transition notifications** — polling remains canonical; notification failure never changes readiness.
14. **CLI: implement idempotent `lain setup` framework**
15. **Setup: generic MCP adapter**
16. **Setup: Claude Code adapter**
17. **Setup: Codex adapter**
18. **Setup: Cursor / VS Code / Continue adapters**
19. **MCP: `understand_repository` context tool**
20. **MCP: remaining primary semantic agent API**
21. **Bench: agent tool-selection benchmark suite**
22. **CI: end-to-end MCP distribution acceptance matrix**
23. **Docs: replace install-first quickstart with setup-first onboarding**

Dependencies are strict: 4 → 5 → 6 → 7 → 8 → 9 → 10 → 11 → 12 → 13;
setup issues 14–18 depend on 5–7 and 13; semantic issues 19–21 depend on 7 and
12; the final distribution gate 22 depends on all runtime and setup issues it
exercises; documentation issue 23 lands last. Distribution issues 1–3 may
run in parallel with 4–13.

Each issue must define UX transcripts, implementation constraints, fixtures, and
acceptance tests before coding. An issue is not complete while it leaves a temporary
code path, ignored test, unclassified tool, unhandled shutdown path, undocumented
schema field, stale generated schema, or follow-up TODO required for its stated
contract.

---

# Definition of done for the overall initiative

LAIN reaches the intended UX bar when a developer on a clean supported machine can:

```bash
cd existing-repository
npx @spuentesp/lain-mcp setup --agent <supported-agent>
```

and then ask the agent a repository question without manually installing Rust, editing MCP JSON, prebuilding an index, configuring an embedding model, or learning LAIN's internal tool catalog.

The connected agent can independently determine:

- where the repository is;
- what LAIN capabilities are ready;
- whether its knowledge is fresh;
- which high-level tool to call;
- what fallback remains if an optional subsystem is unavailable.

That is the product boundary: **powerful internals, nearly invisible setup.**
