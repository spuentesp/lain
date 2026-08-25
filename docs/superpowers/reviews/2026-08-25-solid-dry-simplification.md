# Lain — SOLID/DRY/Simplification Report

**Date:** 2026-08-25
**Scope:** Whole codebase (`src/`, all 149 Rust files)
**Methodology:** Four parallel read-only reviews, one per subsystem (CLI/entry, server core/federation, MCP/query/overlay, ingest/NLP/LSP). Each finding was independently verified; cross-cutting duplications were confirmed in 2+ subsystems before being promoted to P0/P1.

**Repo stats (baseline):** 149 `.rs` files, 46,384 LoC source, 8,452 LoC tests across 35 test files. Largest files: `mcp/handler.rs` (3,204), `server/presence.rs` (2,065), `server/graph.rs` (1,768), `server/watcher.rs` (1,128), `tools/handlers/registry_impl.rs` (1,089).

---

## 1. Executive summary

The Lain codebase is **functional and well-tested at the unit level**, but is in the early stages of a maintainability problem that's common to MCP servers in active development: the **per-tool ceremony scales linearly with the number of tools**, while the underlying abstractions that would scale sub-linearly (`ToolHandler` trait, `inventory` registry, `RepoSource` trait, `SyncStatus`) exist but haven't been extended to cover the new surfaces. There is **no test gap that would block a refactor**; the refactors themselves are mechanical.

**Top three systemic issues, by leverage:**

1. **`mcp/handler.rs` is a 3,204-line god file** that owns both transports, three layers of state (`LainHandler`, `LainMcpServer`, `HandlerStatus`), and hand-rolled dispatch over 26 tools. Adding one tool currently requires 4–6 edits. The codebase already has the right abstraction (`ToolHandler` + `inventory`) in `tools/registry.rs`; the handler doesn't use it. **Single highest-leverage refactor.**

2. **Time, IO, and cache helpers are duplicated 4–5x each across the modules that need them.** `system_time_to_unix_secs` (5 copies), atomic file write (4), embedding-cache lookup (5), `find_git_workspace_root` (5), `LainError::Other` as the universal string-shaped fallback (~25 call sites). Each duplication is independently maintainable; the high count is the problem.

3. **The ingest pipeline is two parallel implementations** (`build_core_memory` for single workspace, `index_one_repo` for federation) that diverge in batch sizes, persistence strategy, and orphan-sweep gating. The comment at `ingest/ingestion.rs:415-425` documents a real-world regression where the federation path's `files.is_empty()` early return leaked deleted-file symbols permanently into the graph (3769 vs 3340 nodes observed on a long-lived vs fresh index of the same commit). Both paths should share a `Pipeline::run(...)` with a config struct.

The codebase has good bones: small, focused modules exist (`sentinel.rs`, `build_info.rs`, `glob_match.rs`, `audit.rs`, `revision_log.rs`, `auth.rs`, `refresh/mod.rs`), the `ToolHandler` + `inventory` pattern is exemplary (`tools/registry.rs`), and `RepoId`/`GlobalId` are well-designed opaque newtypes. The findings below are about extending these patterns to the rest of the surface, not replacing them.

---

## 2. Methodology

**Subsystems reviewed:**

| Subsystem | Files | LoC (est.) | Reviewer |
|---|---|---|---|
| CLI + entry points | 16 | ~3,850 | Agent #1 |
| Server core, federation, config | 28 | ~8,685 | Agent #2 |
| MCP, query, overlay, sensors, presence, reload, audit, auth | ~46 | ~21,800 | Agent #3 |
| Ingest, NLP, LSP, tree-sitter, toolchains, refresh | 15 | ~6,271 | Agent #4 |

**Principle lenses:**
- **S — Single Responsibility** — file/struct does one thing
- **O — Open/Closed** — adding a feature shouldn't require editing the registration site
- **L — Liskov Substitution** — trait impls honor the contract
- **I — Interface Segregation** — narrow, focused interfaces
- **D — Dependency Inversion** — depend on abstractions, not concretions
- **DRY** — duplicated logic, magic strings, magic numbers
- **YAGNI** — speculative generality, dead code, unreachable paths
- **Simplification** — ceremony that exists only to support ceremony

**Severity scale:**
- **P0** — significant maintainability hazard, fix soon
- **P1** — worth fixing, not blocking
- **P2** — nit / cleanup

Findings are grouped by **theme**, not by reviewer, so cross-cutting issues are visible. Each finding has location, impact, suggested fix, and severity.

---

## 3. P0 — Significant maintainability hazards

### P0-1 `mcp/handler.rs` is a 3,204-line god file with hand-rolled dispatch
**Category:** SRP / OCP
**Locations:**
- `src/server/mcp/handler.rs:367` (`handle_list_tools_request`)
- `src/server/mcp/handler.rs:424-1037` (`handle_call_tool_request` stdio match — 600+ lines)
- `src/server/mcp/handler.rs:1878-2285` (`handle_request` HTTP match — 400+ lines, parallel structure)
- `src/server/mcp/handler.rs:1082-1372` (4 near-identical `LainMcpServer` constructors)

**Impact:** Adding one MCP tool requires editing **at minimum** 4 places: (1) the tool runner, (2) the stdio `match` arm, (3) the HTTP `match` arm, (4) `ToolDef` in `mcp/definitions.rs`. The same presence-tool ladder is duplicated twice (stdio at 533–642, HTTP at 1945–2029); the same federation-tool ladder is duplicated twice (stdio at 648–1004, HTTP at 2035–2285); the same `get_server_status`/`request_reload`/`list_recent_projects` ladder is duplicated twice (stdio at 468–531, HTTP at 1884–1934). Drift between stdio and HTTP is already present (`jsonrpc_tool_result` vs `tool_text_result` envelopes differ on the error path).

The `ToolHandler` trait + `inventory::submit!(...)` registry in `tools/registry.rs:209-226` is **already the right pattern and is already used by `tools/handlers/registry_impl.rs`** — but `handler.rs` doesn't use it. Adding the same 24-line `inventory::submit!` block per tool is tedious but mechanical.

**Suggested fix:**
1. Make every tool a `ToolHandler` impl that returns `(ToolDefinition, dispatch fn)`.
2. Build a single `Vec<ToolEntry>` at startup from the inventory; both transports iterate the same table.
3. Extract arg-extraction helpers (`required_str_arg`, `optional_str_arg`, `required_u64_arg`) once and use from every tool.
4. Delete `handle_call_tool_request` and `handle_request` once the table is in place; replace with `let entry = self.registry.get(name)?; entry.dispatch(args)`.

**Estimated payoff:** ~1,500 LoC removed, 26 tool-add edits collapsed to 1.

---

### P0-2 `resolve_repos_config` defined twice with byte-identical bodies
**Category:** DRY (literal definition duplication — likely a bug)
**Locations:**
- `src/lib.rs:91-123`
- `src/cli/mod.rs:35-66`

Both `pub fn`s have the same signature `fn(&Path) -> PathBuf` and the same doc comment. `main.rs` calls the `cli::` one; the `lib.rs` one is presumably called from external consumers (the `lain::resolve_repos_config` re-export). Whichever gets edited last wins; the other silently goes out of sync.

**Suggested fix:** Delete one. Keep `lib.rs::resolve_repos_config` (canonical re-export surface) and have `cli/mod.rs` either `pub use crate::resolve_repos_config;` or call `crate::resolve_repos_config` directly.

**Resolved by:** plan `2026-08-25-helper-deduplication`, Task 1 (commit `b3521fd`).

---

### P0-3 `find_git_workspace_root` walk-up-for-`.git` duplicated 5x
**Category:** DRY (high-frequency, comment-documented duplication)
**Locations:**
- `src/cli/mcp.rs:83-98` (`find_git_workspace_root`)
- `src/cli/init.rs:71-84` (`find_git_workspace`)
- `src/cli/query.rs:219-228` (`walk_up_for_git`)
- `src/cli/oneshot.rs:220-233` (`find_git_workspace`)
- `src/cli/hooks.rs:559-580` (`find_workspace_root`, near-cousin)

Each has slightly different return shape (`Option` vs `Result<Option>`), error handling, and `canonicalize` behavior. Two of the five carry comments saying "mirrors" the others. A bug fix in one will be forgotten in the others.

**Suggested fix:** Single helper `pub fn find_git_workspace_root(start: Option<&Path>) -> Result<Option<PathBuf>>` in `cli/mod.rs` (or `cli/workspace.rs`). All five callers delegate.

**Resolved by:** plan `2026-08-25-helper-deduplication`, Tasks 2–3 (commits `fd2d9b4`, `bf7fc0c`, `8ec9321`, `8d36b32`, `ee93643`, `8f6d07f`).

---

### P0-4 Embedding cache lookup reimplemented 5x
**Category:** DRY / OCP
**Locations:**
- `src/server/query/executor.rs:303-323` (`get_node_embedding`)
- `src/server/tools/handlers/metrics.rs:519-539` (`get_embedding`)
- `src/server/tools/handlers/search.rs:79-143` (inlined inside `semantic_search`)
- `src/server/ingest/ingestion.rs:271-303` (one-shot inline)
- `src/server/ingest/jobs.rs:181-188` (one-shot inline)

Each copy re-encodes the same five-step logic: cache → stored embedding → on-demand embed → cache → (optional) persist back. The `volatile_embed_count < 200` cap (search.rs:91) and `MAX_EMBED_PER_PASS` (jobs.rs:65) are unrelated limits on the same operation. The `search.rs` version is the only one that writes the embedding back to the graph; the others silently re-embed on every cold restart.

**Suggested fix:** One method on the embedder: `fn get_or_compute(&self, node, workspace, cache: &EmbeddingCache, persist: bool) -> Option<Vec<f32>>`. Also worth caching the enriched text alongside the embedding (`HashMap<String, (Vec<f32>, String)>`) to fix `build_enriched_text` disk-read thrash (see P1-12).

---

### P0-5 `system_time_to_unix_secs` reimplemented 5x
**Category:** DRY
**Locations:**
- `src/server/federation/loader.rs:186-191` (returns `i64`)
- `src/server/mcp/presence_tools.rs:687-688` (returns `u64`)
- `src/server/mcp/federation_tools/server_status.rs:14-16` (returns `i64`)
- `src/server/config/recent_projects.rs:36-44` (`now_unix`, returns `i64`)
- `src/server/presence.rs:2059` (`system_time_now_unix`, returns `f64`)

**Suggested fix:** One helper in `src/server/mod.rs` (or a new `time.rs`) with all three return types and a `delta` variant. `recent_projects::now_unix` becomes `system_time_to_unix_secs(SystemTime::now())`.

**Resolved by:** plan `2026-08-25-helper-deduplication`, Tasks 4–5 (commits `fc5e433`, `d4dae9f`, `8e6c122`, `c84ec50`, `5f95ceb`, `2ed87d9`).

---

### P0-6 `LainError::Other(String)` is the de-facto catch-all
**Category:** Type design / DRY
**Locations:**
- `src/server/error.rs:62-63` (definition)
- ~25 call sites across `federation/loader.rs`, `federation/repo_index.rs`, `ingest/constructors.rs`, `ingest/server.rs`, `mcp/handler.rs`, `mcp/federation_tools/*`

`LainError` has 19 variants; 17 of them wrap `String` with no inner structure (`Git`, `Graph`, `Database`, `Lsp`, `Nlp`, `Mcp`, `Io`, `Serialization`, `NotFound`, `Unavailable`, `InvalidRepoId`, `InvalidGlobalId`, `Fatal`, `Config`, `Workspace`, `NotImplemented`, `Other`). Most "doesn't fit anywhere" cases funnel into `Other`. The variants `Git(err.message().to_string())` loses the `git2::ErrorCode` entirely.

**Suggested fix:** Two viable directions:
- **(Minimal)** Collapse all string-typed variants into `LainError::Message { category: ErrorCategory, msg: String }` with `ErrorCategory = Git | Graph | Lsp | …` (no per-variant messages, just categorization).
- **(Maximum signal)** Keep variants but add inner structure where it matters (`Git(git2::Error)`, `Io(std::io::Error)` via `#[from]`, `Lsp { lang: &'static str, msg: String }`).

Either way, also remove `LainError::NotImplemented` (Y2 — never constructed).

---

### P0-7 `graph.rs` is a 1,768-line god module
**Category:** SRP
**Location:** `src/server/graph.rs:1-1768`

Bundles: bincode persistence (1273-1316), co-change analysis (1163-1210), depth-from-main BFS (1111-1153), anchor scoring (940-1108), sub-graph extraction, BFS/traversal, freshness tracking, plus a `PetgraphBackend` lives on top of it. ~50 public methods, ~30 are `pub fn` only used inside tests (`query_nodes`, `get_neighbors`, `bfs_from`, `has_references_from`, `find_entry_points`).

Several of these are documented footguns (`find_node_by_name` returns the *first* sorted match — silently wrong for ambiguous names; `Clone` on `GraphNode` invites misuse; `upsert_nodes_batch(Vec<GraphNode>)` is dead but its name collides with the federation method of the same name).

**Suggested fix:** Split into `graph/mod.rs` (the `GraphDatabase` core + persistence) and `graph/co_change.rs`, `graph/anchors.rs`, `graph/depth.rs`. The co-change, anchor, and depth-from-main logic are independent concerns glued onto the same struct. Also delete `find_node_by_name` (use `find_all_nodes_by_name(...).into_iter().next()` explicitly at the call site), `insert_node` (alias for `upsert_node`), `upsert_nodes_batch(Vec<GraphNode>)` (dead), and `get_all_nodes` (alias for `all_nodes`).

---

### P0-8 `tracing` is not initialized in `lain mcp` — `warn!` calls are invisible
**Category:** Operational hazard / Simplification
**Locations:** Every `warn!` in `src/server/ingest/`, `src/server/treesitter.rs`, the federation paths.

**Impact:** `lain mcp` doesn't init tracing. Operators can't tell why a re-index silently dropped 37 files' worth of `Contains` edges. `get_health` says "Operational" because that's a different code path. The `RefreshOutcome` machinery was built to compensate for exactly this (mcp/handler.rs:1314-1340) but is only wired for the timeout case, not the per-batch silent drops.

**Suggested fix:** Initialize tracing in `LainServer::new` (cheap, just respects `RUST_LOG`). Add a `silent_drop_count: AtomicU64` counter on `LainServer` and surface it in `get_health` for the orphan-sweep and edge-drop classes.

---

### P0-9 Federation ingest pipeline duplicated 80% with single-workspace
**Category:** DRY / SRP
**Locations:**
- `src/server/ingest/ingestion.rs:20-361` (`build_core_memory`, single-workspace)
- `src/server/ingest/ingestion.rs:486-693` (`index_one_repo`, federation)

Same five stages (commit → file list → scan → resolve → orphan sweep → commit-set), different batch sizes, different orphan-sweep gating, different persistence strategies (`save_to_disk()` async vs `save_to_disk_sync()`), and the federation path lacks the `partial` timeout gate the single-workspace path has. The `sweep_orphans` fix for `files.is_empty()` deletion-only commits was originally added only on the federation side and had to be retrofitted (comment at ingestion.rs:415-425).

**Suggested fix:** Extract `IngestPipeline::run(files, db, lsp, git, limits, persistence) -> PipelineOutcome`. Both `build_core_memory` and `index_one_repo` become 10-line wrappers that hand it the right handles and a `PatternLimits` variant. The `PatternLimits::DEFAULT` vs `FEDERATION` distinction (resolve.rs:36-50) is the right place to encode the intentional divergence.

---

## 4. P1 — Worth fixing

### P1-1 RepoSource / WorkspaceSource are parallel-but-separate trait hierarchies
**Category:** DRY / OCP
**Locations:**
- `src/server/federation/repo_source.rs:9-22` (`RepoSource` trait)
- `src/server/federation/workspace.rs:172-180` (`WorkspaceSource` trait)

Same five methods, same `Arc<RwLock<SystemTime>>` field, same `WorkspaceDirSource` impl exists in *both* (one in each module, different ID types). The git-clone/fetch/reset triple is duplicated 3x (`LocalCloneSource::fetch` 65-91, `ShallowCloneSource::fetch` 125-152, `WorkspaceCloneSource::fetch` 287-314) — all 50 lines each, all hand-rolling `Command::new("git").arg("clone --quiet"...)`.

**Suggested fix:** Single trait `RepoSource` (with `id: &RepoId` + `local_path: &Path`), single `git_refresh(workspace, url, ref, shallow) -> Result` helper. Collapses ~200 LoC of near-identical code.

---

### P1-2 Tree-sitter extension dispatch hard-coded in 3 places; Go missing
**Category:** OCP / correctness
**Locations:**
- `src/server/treesitter.rs:89-114` (calls extraction)
- `src/server/treesitter.rs:250-258` (string literals)
- `src/server/treesitter.rs:293-299` (definitions)
- `src/server/lsp.rs:28-48` (LSP `LANGUAGE_MAP` — 19 entries)
- `src/server/watcher.rs:122-125` (`WATCHED_EXTENSIONS` — 19 entries)
- `src/server/toolchains.rs:274-292` (`default_markers` — 15 entries)

**Impact:** Adding Go (advertised in the LSP map and `default_markers`) requires touching 5 lists. `tree-sitter-go` is **not even in `Cargo.toml`** (Cargo.toml:99-101 only lists `tree-sitter-{rust,python,javascript}`); the tree-sitter path silently returns `vec![]` for `.go` files. A Go-only project with no working LSP gets *no* tree-sitter fallback — exactly the scenario `scan.rs:113-156` was written to cover. The same drift applies to Vue/Svelte (in `WATCHED_EXTENSIONS`, tree-sitter returns nothing) and Java/C#/Kotlin/Scala (advertised in LSP map, but `install_cmd: None` — `install_server` errors with no helpful message).

**Suggested fix:** Single source of truth: a `static LANGS: &[LangSpec]` table keyed by extension, recording `(tree_sitter: bool, lsp: Option<&'static LspConfig>, marker: Option<&'static str>, build_cmd: Option<&'static str>)`. The four lookup sites become filtered views of this table. Add a coverage test that walks `LANGS` and asserts every entry is consistent across the four lookup sites (and across `Cargo.toml`).

---

### P1-3 `LainMcpServer` constructors are three near-identical 12-field literal blocks
**Category:** Duplication / OCP
**Locations:**
- `src/server/mcp/handler.rs:1082-1098` (`new`)
- `src/server/mcp/handler.rs:1103-1119` (`with_federation`)
- `src/server/mcp/handler.rs:1131-1147` (`with_federation_and_workspaces`)

The 12-field `Self { executor, federation: ..., workspaces: ..., status_transport, status_port, status_started_at, status_last_sync_at: Arc::new(parking_lot::Mutex::new(now)), status_last_error: Arc::new(parking_lot::Mutex::new(None)), reload_bus, server, reindex_timeout }` block is repeated verbatim with only `federation`/`workspaces` flipping. Same `Arc::new(parking_lot::Mutex::new(None))` triplet in all three.

**Suggested fix:** Either `impl Default for LainMcpServer` with `executor` as the only required field, or a private `fn empty_status_slots(now: SystemTime) -> (...)` helper. Both blocks collapse to `Self { executor, federation: ..., ..Default::default() }`. Also worth folding `LainHandler` and `HandlerStatus` into one struct (they share 7 of 8 fields) — see P1-4.

---

### P1-4 `LainHandler` + `LainMcpServer` + `HandlerStatus` carry the same 7 optional fields three times
**Category:** SRP / ceremony
**Locations:**
- `src/server/mcp/handler.rs:300-333` (`LainHandler` fields)
- `src/server/mcp/handler.rs:1045-1080` (`LainMcpServer` fields)
- `src/server/mcp/handler.rs:447-456, 1249-1257, 1432-1441` (three `HandlerStatus` construction sites)

All three carry `federation`, `workspaces`, `reload_bus`, `server`, `status_*`. Each `with_*` builder adds an `if let Some` branch to two methods and clones `Arc<Mutex<>>` into `HandlerStatus`. Adding a new optional capability multiplies the boilerplate by ~10 lines.

**Suggested fix:** Build a `McpDeps` struct and pass it by `Arc` into both `LainHandler` and `LainMcpServer`. Or fold status into `LainServer` (it already owns `started_at`, `last_sync_at`, `last_error`) and pass `Option<Arc<LainServer>>` for the whole bundle.

---

### P1-5 Query language `match GraphOp` is duplicated between `execute` and `explain`
**Category:** SRP / OCP
**Locations:**
- `src/server/query/executor.rs:60-90` (`execute`)
- `src/server/query/executor.rs:380-437` (`explain`)

Two separate `match op` blocks enumerate the same seven `GraphOp` variants with different per-arm bodies. Adding a new op variant means touching both blocks, the enum (`spec.rs:620-629`), and the named-query constructors (`spec.rs:55-158`).

**Suggested fix:** Per-op trait: `trait GraphOpHandler { fn apply(&self, exec: &mut Executor) -> Result<...>; fn describe(&self) -> String; }` implemented by each variant. Both `execute` and `explain` iterate `op.iter().map(|o| o.apply(...))` and `op.iter().map(|o| o.describe())`.

---

### P1-6 Atomic file write duplicated 4x
**Category:** DRY
**Locations:**
- `src/cli/repos.rs:86-99` (`write_atomic`, two tmp-name conventions)
- `src/state.rs:65-80` (`ActiveWorkspace::save`, inline)
- `src/server/presence.rs:1515-1583, 1655-1656` (inline, twice)
- `src/server/graph.rs:1273-1316` (`save_to_disk` / `save_to_disk_sync`, async vs sync split)

**Suggested fix:** `pub fn write_file_atomic(path: &Path, bytes: impl AsRef<[u8]>) -> io::Result<()>` in `config/` (or new `cli/io.rs`). All callers use it. The graph's async/sync split can be one helper taking a `tokio::fs`/`std::fs` enum.

**Resolved by:** plan `2026-08-25-helper-deduplication`, Tasks 6–7 (commits `947132b`, `6d7a14a`, `d398e11`, `ef043e1`). The async/sync split is preserved as two named helpers (`write_file_atomic` sync + `tokio_write_file_atomic` async) per the brief's Global Constraint.

---

### P1-7 MCP-over-HTTP client implemented 3x
**Category:** DRY / SRP
**Locations:**
- `src/cli/hooks.rs:207-304` (typed `McpRequest`/`McpResponse`/`post_mcp`)
- `src/cli/oneshot.rs:108-129` (hand-framed `serde_json::json!({"jsonrpc":"2.0", "id":1, ...})`)
- `src/cli/doctor.rs:63-114` (`emit_tools_list_check` builds own URL + body)

**Suggested fix:** Extract `cli::mcp_client::post_tool_call(url, name, args) -> Result<Value>`. All three call sites use it.

**Resolved by:** plan `2026-08-25-helper-deduplication`, Task 8 (commits `4b3b24f`, `3dcee27`). HTTP only; `cli::oneshot.rs` stdio case deferred (different transport, not in scope).

---

### P1-8 Cross-encoder setup duplicated in two constructors
**Category:** Duplication
**Locations:**
- `src/server/ingest/constructors.rs:228-264` (`build_embedder_pair`, federation path)
- `src/server/ingest/constructors.rs:460-496` (same 16 lines inline in `LainServer::new`)

Same `let cross_dir = std::env::var("LAIN_CROSS_ENCODER").ok()...` setup. A fix to cross-encoder resolution rules has to be applied in both.

**Suggested fix:** Extend `build_embedder_pair` to be the only path; make the embedder arg optional. The single-workspace constructor's NlpEmbedder branch (constructors.rs:462-473) is the same logic already; only the cross-encoder block needs moving.

---

### P1-9 `cli/hooks.rs` is 946 lines and does 7 things
**Category:** SRP
**Location:** `src/cli/hooks.rs`

Module contents: clap subcommand enum (21-150), `HookSession` + session file I/O (152-205), `sanitize_agent_name` (168-185), MCP-over-HTTP client (207-314), TCP reachability probe with hand-rolled URL parser (516-548), filesystem presence lock orchestration (559-648, 721-785), `git rev-parse` (655-668).

**Suggested fix:** Split into `cli/hooks/mod.rs` (clap + dispatch), `cli/hooks/session.rs` (file I/O), `cli/hooks/mcp_client.rs` (HTTP client), `cli/hooks/filesystem_lock.rs` (presence_lock glue), `cli/hooks/git_ref.rs` (rev-parse). Move `cli/dispatch.rs` into one of these or rename to `cli/hooks_dispatch.rs` (the file is misnamed — see P2-1).

---

### P1-10 Dead code: `sync_volatile_overlay` / `process_change` / `run_background_sync` / `run_sliding_window`
**Category:** YAGNI
**Locations:**
- `src/server/ingest/ingestion.rs:363-400` (`sync_volatile_overlay` + `process_change`) — zero callers
- `src/server/ingest/jobs.rs:8-201` (`run_background_sync`, `run_sliding_window`) — zero callers

The watcher (`watcher.rs`) uses `LspMultiplexer::get_document_symbols_hierarchical` directly through `process_file` and bypasses this path entirely. `docs/TECHNICAL.md:392` references it in a diagram only.

**Suggested fix:** Delete both. Update `TECHNICAL.md` to point at `FileWatcher::process_file`. Delete `jobs.rs` entirely or fold the doc-comment nuggets into the watcher module.

---

### P1-11 LSP coverage honesty: 5 advertised languages have `install_cmd: None`
**Category:** YAGNI / honesty
**Location:** `src/server/lsp.rs:28-48`

`java`, `cs`, `swift`, `kt`, `scala` all advertise an `LspConfig` with `binary: "..."` and `install_cmd: None`. An agent calling `install_lsp kotlin-language-server` gets an unhelpful error. `omnisharp` is also a deprecated/archived Roslyn wrapper.

**Suggested fix:** Either remove the entries that aren't really first-class (or split into "tested" vs "advertised-but-no-install"), or add install commands. The `toolchains/` profile system is the right place for install specs.

---

### P1-12 `build_enriched_text` reads from disk per node in the search path
**Category:** Performance
**Location:** `src/server/tools/utils.rs:249-280`

Called from `search.rs:98, 133, 189` — twice per node (once for embedding, once for response text). For a 500-node search with 200 cold embeds, that's 200 disk reads + 500 text rebuilds per query.

**Suggested fix:** Cache the enriched text alongside the embedding in `EmbeddingCache` (`HashMap<String, (Vec<f32>, String)>`), or add `enriched_text: Option<String>` to `GraphNode` populated lazily on first call.

---

### P1-13 Embedding storage format: JSON `String` instead of bincode `Vec<f32>`
**Category:** Data design / performance
**Location:** `src/server/schema.rs` (`GraphNode::embedding: Option<String>`)

JSON-serializing 384 floats per embedding is ~30% larger than bincode and ~5x slower. The graph loader (`load_from_disk` at graph.rs:1318-1356) already uses bincode for everything else; embeddings are the only JSON.

**Suggested fix:** Switch `Option<String>` → `Option<Vec<f32>>` (or `Option<Vec<u8>>` to bincode without serde). One-shot migration gated by `path_format_version` (already exists).

---

### P1-14 Detached `tokio::spawn` futures have no shutdown path
**Category:** Resource management
**Locations:**
- `src/server/ingest/ingestion.rs:253-303` (NLP prewarm spawn)
- `src/server/ingest/jobs.rs:177-196` (sliding-window NLP spawn)
- `src/server/ingest/background.rs:46-52` (presence expiry loop)

`LainServer::shutdown` (server.rs:435-438) only shuts down `lsp_pool`. Tests that build and drop a `LainServer` leave detached futures running.

**Suggested fix:** Constructors return a `JoinHandle` for each detached task; `shutdown` aborts them. Use `tokio::task::AbortHandle` stored on `LainServer`.

---

### P1-15 Tool handler matches for stdio + HTTP duplicated for all 13 presence tools
**Category:** Duplication / OCP
**Locations:**
- `src/server/mcp/handler.rs:533-642` (stdio arms)
- `src/server/mcp/handler.rs:1945-2029` (HTTP arms)

Each presence tool has two parallel arms that differ only in the helper used (`dispatch_presence_tool` vs `jsonrpc_presence_tool`). Adding a 14th presence tool means two arm additions.

**Suggested fix:** Once P0-1 is done, this disappears.

---

### P1-16 `presence_tools::run_*_inner` wrappers are dead weight
**Category:** YAGNI / duplication
**Locations:**
- `src/server/mcp/presence_tools.rs:62-66, 67-84, 92-95, 97-109, 247-254, 256-405, 589-600, 602-616`

Eight tools have an `_inner` split only to support `with_shared_presence(|| run_*_inner(...))`. The `_inner` body is just the public fn with arg parsing deleted — no logic difference.

**Suggested fix:** Either make every presence tool follow the same `run_X(server, Value)` shape and let the runner itself call `with_shared_presence`, or extract a `fn run_with_shared<T>(server, args, fn(&LainServer, T) -> Result<Value, String>)` that collapses the four wrappers to 4-line dispatch sites.

---

### P1-17 Tool chains default_markers lists 14 languages; only 3 are actually supported
**Category:** YAGNI / honesty
**Location:** `src/server/toolchains.rs:274-292` lists `rust, go, python, javascript, typescript, java, csharp, ruby, php, cpp, c, zig, swift, kotlin, scala`; tree-sitter handles 3 (rust/python/js); LSP handles 19; default profiles cover 5.

**Impact:** `detect_toolchains` returns all languages whose marker exists with no check that the system can actually index them. A project with `pom.xml` triggers "java" detection but the indexer can't handle Java.

**Suggested fix:** Either restrict `detect_toolchains` to languages with a working pipeline, or add `supported: bool` to `ToolchainProfile` and skip unsupported ones. Ship actual `toolchains/*.toml` profiles so the directory branch has a real use — currently every caller passes `None`.

---

### P1-18 Empty `toolchains/` directory — `Option<&Path>` API path is dead
**Category:** YAGNI
**Locations:**
- `src/server/toolchains.rs:44-90, 164-202` (`load_toolchain_markers`, `load_toolchain_profiles` — directory branch)
- `src/server/toolchains.rs:316-350` (test for the directory branch only)

All three in-tree callers pass `None`. `toolchains/README.md` is the only thing shipped in the directory. The whole `std::fs::read_dir(dir)` branch is exercised only by the in-tree test that proves it works.

**Suggested fix:** Either ship the profiles the README documents (drop them in `toolchains/`) so the directory branch has a real use, or drop the `Option<&Path>` parameter and remove the directory-loading code. Simplify to `pub fn detect_toolchains(cwd: &Path) -> Vec<String>`.

---

### P1-19 Tool `match` blocks have identical arg-validation ceremony copy-pasted
**Category:** Duplication
**Locations:**
- `src/server/mcp/handler.rs:661-671, 736-745, 747-779, 791-807, 805-820, 836-844, 847-858, 939-948` (stdio)
- `src/server/mcp/handler.rs:2046-2232` (HTTP)

```rust
let id_str = match args_owned.get("id").and_then(|v| v.as_str()) {
    Some(s) => s,
    None => {
        return Ok(tool_text_result(
            "Missing required argument: id".to_string(),
            true, &self.executor.overlay(), static_graph_generation_unix,
        ));
    }
};
```
repeated 14 times (7 stdio + 7 HTTP). `tools/utils.rs:205` already has a `required_str_arg` helper that isn't used from the MCP handlers.

**Suggested fix:** Typed helpers next to `tools/utils::required_str_arg`: `mcp_required_str_arg(args, key, overlay, gen_unix) -> Result<String, CallToolResult>` that returns the missing-arg envelope directly. Each dispatch site becomes `let id = mcp_required_str_arg(&args, "id", ...)?;`.

---

### P1-20 `HandlerStatus` rebuilt at 3 sites; clones 5 `Arc<Mutex>`s
**Category:** SRP / ceremony
**Locations:**
- `src/server/mcp/handler.rs:447-456` (inside `get_server_status` arm)
- `src/server/mcp/handler.rs:1249-1257` (in `serve`)
- `src/server/mcp/handler.rs:1432-1441` (in `run_http`)

`run_http` re-reads `workspaces.read().workspaces.len()` and `federation.list_repos().len()` on every connection. Per-connection rebuild means a slow filesystem stalls every accepted TCP connection.

**Suggested fix:** Compute `repo_count`/`workspaces_count` once at construction and store `HandlerStatus` alongside the listener. `get_server_status` arm reads counts via the executor/federation fields already available.

---

### P1-21 `cli/server.rs::run_server` does 7 things + duplicates attribution-backend picker
**Category:** SRP / DIP
**Locations:**
- `src/cli/server.rs:33-169` (single 137-line function)
- `src/cli/server.rs:107-116` (duplicates `default_attribution_backend()` from `server/ingest/background.rs:22`)

**Suggested fix:** Extract `fn select_attribution(no_op: bool) -> Arc<dyn AttributionBackend>` and `fn build_lain_server(fed, transport, port, workspaces, repos_yaml, attribution, embedding) -> LainServer`.

---

### P1-22 `is_test_container` is private to `scan.rs` but `metrics.rs` re-implements it
**Category:** Modularity / DRY
**Locations:**
- `src/server/ingest/scan.rs:274-277` (private `fn`)
- `src/server/tools/handlers/metrics.rs:156` (`is_test_symbol`, duplicate heuristic)

**Suggested fix:** Promote `is_test_container` to `pub(crate)` and have `metrics.rs` call it. Or make `is_test_symbol` the only place this rule lives and have `scan.rs` write `label = "test"` exactly the way `is_test_symbol` checks.

---

### P1-23 Hand-rolled URL parsing in `server_reachable`
**Category:** Robustness / DIP
**Location:** `src/cli/hooks.rs:516-548`

33 lines of fragile string-slicing (`find("://")`, `split('/')`, `rsplit_once(':')`, bracket-stripping for IPv6) to extract `host:port` from a URL. Doesn't handle paths, queries, or auth correctly (`http://user:pass@host:port/mcp` would slice weirdly).

**Suggested fix:**
```rust
let url = reqwest::Url::parse(health_url)?;
let host = url.host();
let port = url.port_or_known_default().unwrap_or(80);
```

---

### P1-24 LSP startup timeout is shared 5s across all languages
**Category:** Correctness / cross-cutting
**Location:** `src/server/lsp.rs:24, 88-135`

rust-analyzer cold-start routinely exceeds 5s on a cold cache. The mux marks the binary "unavailable" permanently and never clears it; the next indexer pass fails to find any Rust symbols. `get_health` shows `Ok`; the graph has no Rust functions.

**Suggested fix:** Per-language timeout (rust-analyzer gets 30s; clangd 5s; gopls 10s) or retry-on-timeout with background warm-up.

---

## 5. P2 — Nits and cleanup

### P2-1 Dead CLI fields and helpers
- `src/cli/hooks.rs:472, 589, 779` — `_symbol` / `_agent_name` params on `release`, `claim_filesystem`, `unlock` that are accepted and ignored.
- `src/cli/hooks.rs:156, 372, 378-383` — `HookSession::registered_at_unix` field is written on every register but never read.
- `src/cli/mod.rs:177-181`, `src/main.rs:74-83`, `src/cli/ask.rs` — `Ask { config, question }` flags accepted and ignored (comment acknowledges).
- `src/main.rs:127-133` — `None` arm of `Option<Commands>` is unreachable from `Parser::parse`; clap prints help before returning.
- `src/cli/doctor.rs:230-238` — Check 5 sentinel `let _reg = PresenceRegistry::new()` always emits "OK" for a no-op; comment says "sentinel for future refactors."
- `src/server/error.rs:60` — `LainError::NotImplemented` never constructed.
- `src/server/graph.rs:165-167, 542-547` — `insert_node` (alias for `upsert_node`), `upsert_nodes_batch(Vec<GraphNode>)` (dead collides with federation).
- `src/server/mcp/handler.rs:1226-1228` — `LainMcpServer::new_read_only` is a one-line forward to `Self::new(executor)`.

**Suggested fix:** Delete each. (All simple removals.)

---

### P2-2 Naming / convention drift
- `src/cli/hooks.rs:41, 74, 125, 142` — `--path` is `Vec<String>` for `Claim`, `String` for `Release`/`Lock`/`Unlock`.
- `src/cli/server.rs:38` — `workspace_arg: &str` vs every other filesystem arg being `&Path`.
- `src/cli/dispatch.rs` — file's doc comment says "CLI dispatch re-exports" but it actually contains the `HooksAction` dispatcher; misleading name.
- `src/cli/oneshot.rs:75-83` — `trailing_var_arg = true` accepts any number of trailing args but only `args[0]` is read.

**Suggested fix:** Pick conventions and apply consistently.

---

### P2-3 Inconsistent error handling
- `src/cli/query.rs:32-35, 49-51`, `src/cli/ask.rs:8, 13, 23, 35`, `src/cli/workspaces.rs:341-342` — `eprintln!` + `std::process::exit` inside `Result`-returning functions.
- `src/cli/doctor.rs` `run_doctor` — numbered checklist with repeated `if !emit(...) { failures += 1; }` boilerplate (7 checks).
- `src/cli/workspaces.rs:136-142, 326-328, 355` — builds `LainError::Config(...)`, then wraps with `anyhow!("{}", err)` or `.into()`, throwing away the typed variant.
- `src/server/refresh/mod.rs:139-143` — `eprintln!` for env-var parse failure (only `eprintln!` in `src/server/`).

**Suggested fix:** All to `tracing::warn!` + return `Err(anyhow!(...))`. Let `main` decide whether to print and exit (it already does this for `doctor`).

---

### P2-4 Magic numbers / strings
- `src/server/graph.rs:1025, 1062, 1131` — anchor-scoring `8.0`, `100.0`, depth BFS `50` literal.
- `src/server/graph.rs:1124` — `n.name == "main" || n.name == "App"` — entry-point names documented nowhere.
- `src/server/sse.rs:49-58` — eight `&'static str` SSE event names inline.
- `src/server/federation/repo_source.rs:56, 116, 181` — `"local_clone"`, `"shallow_clone"`, `"workspace_dir"` kind strings.
- `src/server/git.rs:443-471` — `RepoIdentity::from_remote` falls through branches on string `contains`.
- `src/server/tools.rs:512-525` — three `const` arrays of tool names bucketing the strategy response.

**Suggested fix:** Named constants at the top of each file. Entry-point names deserve a `const ENTRY_POINT_NAMES: &[&str]` plus a docstring. Kind strings deserve an enum-with-display.

---

### P2-5 Tree-sitter `BUILTIN_CALLS` / `BUILTIN_TYPES` blocklists are unsorted and overlap
**Category:** DRY / data hygiene
**Location:** `src/server/treesitter.rs:27-73`

Linear scan over `BUILTIN_CALLS.contains(&name)` for every call in a 1k-call file. `String` is both a type and called via `String::new()` but lives in different blocklists. `extract_refs_with_locals` exists but `scan.rs:173` calls `extract_refs` (without locals) — so `fn new()` inside a module that defines its own `new()` produces a `Calls` ref instead of a definition.

**Suggested fix:** Single `BUILTIN_NAMES: phf::Set<&str>` (compile-time hash set). Pass `local_definitions` from `extract_definitions` results through to `extract_refs` so the secondary classification fires.

---

### P2-6 `extract_definitions_python` / `extract_definitions_js` duplicate identical helper
**Category:** Duplication
**Location:** `src/server/treesitter.rs:447-507` (Python) and `src/server/ingest/...` 509-580 (JS)

`python_def_name` / `js_function_name` / `js_class_name` are byte-for-byte identical.

**Suggested fix:** Lift `first_identifier_name(node, source)` to a free function.

---

### P2-7 `tokio::sync::Mutex` held across LSP calls in `scan_file_structure`
**Category:** Concurrency
**Location:** `src/server/ingest/scan.rs:97-111`

Default `lsp_pool_size = 1` (constructors.rs:305) means every other file waits for `get_references` + `get_document_symbols_hierarchical` on a single mutex.

**Suggested fix:** Default `lsp_pool_size` to `min(4, num_cpus)` and document the trade-off.

---

### P2-8 Test coverage gaps
- **Tree-sitter fallback path has no integration test.** `tests/federation_integration.rs` covers the federation path end-to-end; no test exercises the LSP-disabled tree-sitter path against a real repo.
- **Ingest partial-scan timeouts are untested.** `ingestion.rs:102-119` has the `partial` flag; no test sets `scan_timeout_secs = 0` and asserts `get_last_commit()` is unchanged.
- **NLP prewarm contract is untested.** No test pins "prewarm anchors before background lazy enrichment" — refactors that change the order silently change first-query latency.
- **Negative paths in `resolve_pattern_edges`.** `resolve.rs:247-345` covers positive and "ambiguous" but not `max_files_per_value` / `cross-file dir pairing` / `seen` set deduplication.

**Suggested fix:** Add 4–5 small tests per gap.

---

### P2-9 `sensors/` lacks a `Sensor` trait; each sensor is a free function with bespoke shape
**Category:** OCP / SRP
**Location:** `src/server/sensors/{graphql,openapi,proto,websocket}_sensor.rs`

`parse_graphql`, `parse_openapi`, `parse_proto`, `extract_websocket_patterns` — each has a different signature, different return type, and is called from `tools/handlers/cross_runtime.rs:18-89` by direct invocation only.

**Suggested fix:** `trait Sensor { fn parse(&self, content: &str, path: &str) -> Vec<SensorMatch>; fn edge_type(&self) -> EdgeType; }`. `cross_runtime_callers` iterates `inventory::iter::<SensorInstance>()` and walks incoming edges for each registered `edge_type`.

---

### P2-10 `Vec<String>` arg validation + arg parsing duplication in `run_*` tools
**Category:** Duplication
**Location:** `src/server/mcp/presence_tools.rs:63, 93, 120, 144, 179, 240, 248, 592, 625, 672, 725` (11 sites) and `src/server/mcp/audit_tools.rs:53, 116` (2 sites)

`serde_json::from_value(args).map_err(|e| e.to_string())?` followed by either `run_*_inner(...)` or an inline body. Same one-liner pattern everywhere.

**Suggested fix:** Tiny `fn parse<T: DeserializeOwned>(v: Value) -> Result<T, String>` helper.

---

### P2-11 `commit_hash.clone()` called repeatedly inside hot loops
**Category:** Performance
**Location:** `src/server/ingest/scan.rs:65, 87, 109, 124-126, 145, 153, 358`; `src/server/ingest/ingestion.rs:78, 551, 82, 555`

Every per-file `scan_file_structure` clones `commit_hash`. For a 5k-file repo, ~5k × N-symbols String clones of a 40-byte hex hash.

**Suggested fix:** Pass `&str` everywhere; clone only at write sites that own.

---

### P2-12 `graph.rs` and `ingestion.rs` are too large for one file
**Category:** SRP
**Locations:**
- `src/server/graph.rs:1-1768` (see P0-7)
- `src/server/ingest/ingestion.rs:1-693` (holds `build_core_memory`, `index_one_repo`, `sync_volatile_overlay`, `process_change`, helpers)

**Suggested fix:** Split per P0-7 and P0-9.

---

### P2-13 Dead `arg_property_schema` hand-coded match per arg name
**Category:** OCP
**Location:** `src/server/mcp/envelope.rs:81-147`

A `match name { "files" => ..., "kind" => ..., ... }` must be edited every time a new typed arg appears. A new arg falls through to `_ => { type: string }` unless someone remembers to add a typed arm — exactly the failure mode that made `claim_files`'s `files` arg unusable until it was added.

**Suggested fix:** Keep typed schemas in `ToolDef` itself (`pub schema: fn(&str) -> serde_json::Map`) or attach via `inventory`.

---

### P2-14 Minor inefficiencies
- `src/server/federation/federation_tools/federation.rs:57-62` — `get_repo_info` is `O(n)` via `list_repos`.
- `src/server/lsp.rs:68-80` — `LANGUAGE_MAP` rebuilt into a `HashMap` on every `LspMultiplexer::new`.
- `src/server/tools.rs:107-111` — `set_diagnostics_port` writes an `AtomicU16` via `&self`, not `&mut self`.
- `src/server/mcp/federation_tools/server_status.rs:118-129` — `request_reload` wraps `bus.request_reload()` (returns `Result<(), String>`) through `LainError::Other` which loses context.
- `src/server/tools/handlers/graphql_sensor.rs:24-150` — two passes over the same content.
- `src/server/tools/handlers/cross_runtime.rs:29-44` — `get_edges_to` called three times with different filters.
- `src/server/ingest/scan.rs:294-336` — `process_symbol_recursive_inner` is `async` but does no async work; `#[async_recursion]` adds state machine for nothing.

---

## 6. Prioritized action plan

**P0 — fix soon (1–2 weeks):**

| # | Item | LoC saved | Effort |
|---|---|---|---|
| P0-1 | `mcp/handler.rs` god file → table-driven dispatch | ~1,500 | High |
| P0-2 | Delete one `resolve_repos_config` | 30 | Trivial |
| P0-3 | Single `find_git_workspace_root` | 60 | Trivial |
| P0-4 | Single `get_or_compute` for embedding cache | 80 | Medium |
| P0-5 | Single `system_time_to_unix_secs` | 30 | Trivial |
| P0-6 | Collapse `LainError` variants | 100 | Medium |
| P0-7 | Split `graph.rs` into 4 files | ~400 | Medium |
| P0-8 | Initialize tracing in `lain mcp` + `silent_drop_count` | 50 | Small |
| P0-9 | Single `IngestPipeline::run` for both ingest paths | ~250 | Medium |

**P1 — next sprint (1–2 weeks each):**

| # | Item | LoC saved | Effort |
|---|---|---|---|
| P1-1 | Single `RepoSource` + `git_refresh` helper | ~200 | Medium |
| P1-2 | Single `LANGS` table + coverage test | 60 | Medium |
| P1-3 | `impl Default for LainMcpServer` | 30 | Trivial |
| P1-4 | Single `McpDeps` | 100 | Small |
| P1-5 | `trait GraphOpHandler` | 40 | Small |
| P1-6 | `write_file_atomic` | 30 | Trivial |
| P1-7 | `cli::mcp_client::post_tool_call` | 100 | Small |
| P1-8 | Single embedder constructor | 30 | Trivial |
| P1-9 | Split `cli/hooks.rs` | 400 | Medium |
| P1-10 | Delete dead `sync_volatile_overlay`, `jobs.rs` | 200 | Trivial |
| P1-11 | LSP coverage honesty | 20 | Small |
| P1-12 | Cache enriched text with embedding | 80 | Small |
| P1-13 | Switch embedding storage to bincode | 20 | Small |
| P1-14 | `JoinHandle`s for detached futures | 30 | Small |
| P1-17 | Restrict `detect_toolchains` or mark `supported: bool` | 30 | Trivial |
| P1-18 | Ship `toolchains/*.toml` or drop the path | 50 | Trivial |
| P1-19 | `mcp_required_str_arg` helper | 50 | Trivial |
| P1-20 | Compute `HandlerStatus` once at startup | 30 | Trivial |
| P1-21 | Split `cli/server.rs::run_server` | 80 | Small |
| P1-22 | Promote `is_test_container` | 20 | Trivial |
| P1-23 | Use `reqwest::Url` for URL parsing | 30 | Trivial |
| P1-24 | Per-language LSP startup timeout | 20 | Small |

**P2 — clean up opportunistically:** All items in section 5 are small (≤30 LoC, ≤1 hour). Batch as a single PR per subsystem.

**Total estimated reduction:** ~3,800 LoC across the codebase (≈8% of source).

---

## 7. What's working well (defend these)

These are the patterns and modules that *should not* be refactored. They're listed here so a future contributor knows what's load-bearing and what's worth preserving.

- **`ToolHandler` trait + `inventory` registry (`src/server/tools/registry.rs`).** The single best abstraction in the codebase. `ToolRegistry::dispatch` is 30 lines; the `for_repo` rebinding (registry.rs:150-167) is the kind of thing that usually grows into a 200-line helper, and the test at registry.rs:308-392 is exhaustive without being brittle. **This is the pattern the rest of the MCP surface should converge on.**

- **`sentinel.rs` (137 lines)** — perfectly scoped, three small functions, three tests. The shared `O_EXCL` primitive pays off the moment a second lock layer shows up; both `state_lock.rs` and `presence_lock.rs` consume it.

- **`build_info.rs` (97 lines)** — `OnceLock<Option<u64>>` with one writer and three readers. The static-state pattern is exactly right for "this is set once, read many."

- **`audit.rs` (~356 lines)** — textbook small focused module. Four functions, zero state, rotation logic is one `if size >= MAX` block. The reset-marker wiring in `presence.rs::load_pair` is the right kind of cross-module coupling.

- **`auth.rs` (295 lines)** — narrow, focused, env-var-driven, with explicit "auth off ⇒ no rate limit by default" documentation. Token bucket in ~25 lines.

- **`revision_log.rs` (191 lines)** — bounded ring buffer with three states (`Ok`, `BeyondCurrent`, `TooOld`).

- **`glob_match.rs` (71 lines)** — thin shim that exists for one reason (audit log path filtering), with a docstring explaining *why* the shim exists rather than just *what* it does.

- **`refresh/mod.rs` (209 lines)** — `RefreshOutcome` state machine is small and focused; `banner_line()` formatter is the right shape for the failure-bug class it exists to fix.

- **`federation/repo_id.rs` (94 lines)** — `RepoId` and `GlobalId` newtype pair at the right level of abstraction: tiny, validated at construction, opaque to callers.

- **`federation_tools/dto.rs` (129 lines)** — all wire types in one file with explicit `#[derive]` and `#[serde(skip_serializing_if = "Option::is_none")]` annotations. Pure data, easy to grep.

- **`ingest::resolve.rs`** — three pure resolve functions, well-documented, unit-tested at the boundary where it matters. The "ambiguous name doesn't fan out" test pins a regression class with a real cause-comment.

- **`toolchains::resolve_in`** — the program-resolution cascade (PATH → env override → profile resolver → glob dir → universal dir) is clean, data-driven, and the in-tree tests pin every ordering invariant including the digit-aware version sort.

- **`cli/doctor.rs`** — long but exceptionally well-documented; every check has an explicit failure-mode paragraph explaining what `FAIL` vs `WARN` vs `OK` means.

- **`cli/hooks.rs::sanitize_agent_name`** — narrow, well-tested, explicit attack-vector coverage (`../../../../etc/passwd`, `ORCA_WORKTREE_ID=…::/home/...`).

- **`cli/signal.rs`** — 180 lines, single responsibility (Unix socket signaling), end-to-end test included.

- **`state.rs::ActiveWorkspace`** — clear file format docs, isolated test module, RAII `XdgGuard`.

- **`main.rs` sync dispatcher's comment block** — explains why `main` is sync and `reqwest::blocking` is the gotcha. Will save the next maintainer hours.

---

## 8. Appendix — file inventory by subsystem

| Subsystem | Files | LoC (est.) |
|---|---|---|
| `src/cli/` | 13 | ~3,800 |
| `src/server/mod.rs`, `error.rs`, `schema.rs`, `graph.rs`, `build_info.rs`, `tuning.rs`, `git.rs`, `glob_match.rs`, `sync_status.rs`, `revision_log.rs`, `events_log.rs`, `sse.rs`, `sentinel.rs` | 13 | ~3,200 |
| `src/server/federation/` | 8 | ~2,800 |
| `src/server/config/` | 2 | ~700 |
| `src/server/mcp/` | 13 | ~6,500 |
| `src/server/tools.rs` + `src/server/tools/` | 16 | ~6,400 |
| `src/server/query/` | 3 | ~2,000 |
| `src/server/overlay.rs` + `overlay_tests.rs` | 2 | ~1,100 |
| `src/server/sensors/` | 4 | ~764 |
| `src/server/ingest/` | 9 | ~3,400 |
| `src/server/reload.rs`, `watcher.rs`, `state_lock.rs`, `presence_lock.rs` | 4 | ~2,200 |
| `src/server/presence.rs`, `attribution.rs`, `audit.rs`, `auth.rs` | 4 | ~3,300 |
| `src/server/refresh/`, `treesitter.rs`, `nlp.rs`, `lsp.rs`, `toolchains.rs` | 5 | ~3,200 |
| `src/lib.rs`, `main.rs`, `state.rs` | 3 | ~500 |
| `tests/` (35 files) | 35 | 8,452 |
| **Total** | **149 src + 35 test** | **54,836** |

Note: subsystem totals overlap (e.g. `mcp/handler.rs` is counted in both `mcp/` and `tools/` totals). The source-level total of 46,384 LoC is authoritative.

---

## 9. Recommended next step

The single highest-leverage refactor is **P0-1** (table-driven MCP dispatch). It removes ~1,500 LoC, eliminates the stdio/HTTP drift class of bugs entirely, and unblocks 4 other P1 items (P1-3, P1-4, P1-15, P1-19, P1-20) that are all symptoms of the same underlying problem. Start there. The `ToolHandler` + `inventory` pattern in `tools/registry.rs` is already proven — extend it.

A reasonable second PR is the **`LANGS` table (P1-2)** — it's small, well-scoped, and catches a real correctness bug (Go is advertised but not supported). Together these two PRs would clean up roughly 1,560 LoC and the most acute correctness gaps.

The P0-2 (`resolve_repos_config` defined twice) is **trivially a bug** and worth fixing in a one-line PR this week regardless of what else moves.
