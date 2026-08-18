# Federation Tool Wiring — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `lain server --config repos.yaml` — the command in the README's TL;DR — answer structural questions correctly. Today roughly 40 of the 59 advertised MCP tools run against an empty throwaway graph in `/tmp` and return successful, empty answers.

**Architecture:** Pure wiring plus one projection fix. Every graph-reading handler already takes `&GraphDatabase` concretely (see `src/server/tools/handlers/registry_impl.rs`), so there are **no handler signature changes**. The work is: make `GraphDatabase` safe to clone, project intra-repo edges into the merged backend, and point `ToolContext` at that backend instead of a temp directory.

**Tech Stack:** Rust 1.75+, petgraph, dashmap, bincode. No new Cargo deps.

**Branch:** `fix/federation-tool-wiring` off `main`.

---

## Reproduction (run this first, and again after each task)

```bash
mkdir -p /tmp/fedcheck && cd /tmp/fedcheck
cat > repos.yaml <<'EOF'
data_dir: ./.lain/federation
repos:
  - id: lain
    source:
      type: workspace_dir
      path: /home/sebastian/lain
EOF
lain server --config ./repos.yaml --transport http --port 9871 &
sleep 20

call() { curl -s -X POST localhost:9871/mcp -H 'Content-Type: application/json' \
  -d "{\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2},\"id\":1}"; }

curl -s localhost:9871/health
call list_repos '{}'
call find_anchors '{}'
call explore_architecture '{}'
call get_blast_radius '{"symbol":"run_doctor"}'
```

**Current output (2026-08-17, main @ `caa0427`):**

```
/health           → federation.total_nodes: 2391, total_edges: 0
                    graph_nodes: 0, graph_edges: 0, tools_count: 35
list_repos        → node_count: 2889, edge_count: 12185      ← the index is real
find_anchors      → "No anchors found in Merged Brain."
explore_architecture → "Found 0 total files in Merged Brain."
get_blast_radius  → "Error: Not found: Node not found for handle: run_doctor"
```

**Target output after this plan:** `find_anchors` returns real symbols, `explore_architecture` lists real files, `get_blast_radius run_doctor` returns callers, and `/health` reports the same node/edge counts as `list_repos`.

---

## Root cause: three defects, not one

Fixing only the obvious one leaves every edge-based tool still empty. All three must land.

### Defect A — the executor is rooted in a throwaway directory

`LainServer::with_federation_with_attribution` and `with_federation_and_workspaces_with_attribution` (`src/server/ingest/mod.rs:313` and `:509`) both build the tool executor against `/tmp/lain-federation-{pid}-{counter}`. The source says so plainly at `ingest/mod.rs:306`:

```rust
// Build a minimal executor … Federation tools never reach the
// executor's underlying services, but `LainMcpServer::with_federation`
// still requires a constructed one.
```

That assumption would be fine if federation mode advertised only federation tools. It advertises all 59.

**This is not only the graph.** Six subsystems are rooted at that temp dir:

| Rooted at temp dir | Consequence |
|---|---|
| `GraphDatabase::new(&mem_path)` | ~40 structural tools see an empty graph |
| `GitSensor::new(&ws)` | co-change, commit history, branch status, file diff all dead |
| `LspPool::new(&ws, 1)` | LSP enrichment rooted in an empty repo |
| `NlpEmbedder` / `CrossEncoder::from_dir(&ws)` | `.lain/models/` in the project is never found → `semantic_search` reports "model not loaded" even when a model is installed |
| `load_tuning_config(&ws)` | `.lain/tuning.toml` (incl. `query_prefix`) never read |
| `ws.to_path_buf()` → `ToolContext.workspace` | `run_build` / `run_tests` / `run_clippy` execute in an empty temp dir |

### Defect B — `project_repo` copies nodes but almost no edges

`FederatedIndex::project_repo` (`src/server/federation/federated_index.rs:90`) re-keys and upserts every **node** into the merged backend, then adds edges — but only `EdgeType::CrossRepoSameSymbol`, derived from similarity against *other* repos' nodes.

The intra-repo edges — `Calls`, `Imports`, `Contains`, `Uses`, `Implements`, `CoChangedWith`, and the cross-runtime types — are **never projected**. With a single repo, `other_nodes` is empty, so the merged backend gets zero edges. That is exactly the `total_edges: 0` in `/health`.

Consequence: fixing Defect A alone makes `explore_architecture` work (nodes only) while `get_blast_radius`, `get_call_chain`, `find_anchors`, `trace_dependency`, `get_context_depth`, and `get_coupling_radar` all still return nothing, because every one of them traverses edges.

> The node-count gap (2889 per-repo vs 2391 merged) is expected, not a bug: `upsert_node` dedupes on the global id, so nodes sharing `(kind, path, name)` collapse.

### Defect C — `GraphDatabase::clone()` deep-copies the index maps

```rust
#[derive(Clone)]
pub struct GraphDatabase {
    graph: Arc<RwLock<StableGraph<GraphNode, GraphEdge>>>,   // shared
    index_map: DashMap<String, NodeIndex>,                    // DEEP COPIED
    path_index: DashMap<String, Vec<NodeIndex>>,              // DEEP COPIED
    last_commit: Arc<RwLock<Option<String>>>,                 // shared
    …
}
```

`ToolContext` is `Clone` and holds `graph: GraphDatabase` by value. Once Defect A is fixed, the executor holds a *clone* of the backend's database: it shares the petgraph but has its own frozen `index_map` and `path_index`. Any node written after that clone (hot reload, re-index, watcher, `project_repo`) lands in the shared graph but not in the executor's index — so `get_node`, `get_node_by_id`, `find_node_by_name`, and `find_node_by_path` miss nodes that are demonstrably present.

This must land **before** Defect A, or the fix will appear to work at startup and silently rot on the first reload.

### Landmine — per-write full-graph serialization

`PetgraphBackend::upsert_node` and `upsert_edge` (`src/server/federation/graph_backend.rs:88` and `:105`) each call `save_to_disk_sync()`, which clones the entire graph and bincode-serializes it (`src/server/graph.rs:683`).

Projecting 2,889 nodes already costs 2,889 full serializations. Naively projecting 12,185 edges adds 12,185 more — of a graph that is 2.1 MB on disk. Task 2 **must** use a batch path, or the fix will make startup unusable.

---

## Global Constraints

- **No new Cargo deps.**
- **No handler signature changes.** Every graph handler already takes `&GraphDatabase`; the seam is `ToolContext.graph`.
- **No silent empties.** Any tool that genuinely cannot work in federation mode must say so explicitly (Task 6). A confident empty answer is worse than an error — an agent reads "No anchors found" as a fact about the codebase.
- **Startup must not regress measurably.** Baseline the reproduction's `sleep 20` and keep indexing under it.
- **All existing tests must pass:** 493 lib + 20 presence + persistence/presence/attribution e2e.

---

## File Structure

```
src/server/
├── graph.rs                              (Task 1: Arc the index maps)
├── federation/
│   ├── graph_backend.rs                  (Task 2: batch upsert; Task 4: as_graph_db)
│   └── federated_index.rs                (Task 2: project intra-repo edges)
├── ingest/
│   └── mod.rs                            (Task 3: project root; Task 4: wire graph)
├── mcp/
│   └── handler.rs                        (Task 6: gate tools/list; Task 7: health body)
└── tools/
    └── registry.rs                       (Task 6: federation-mode capability flag)

tests/
├── federation_tool_wiring.rs             (new — Task 0, the pinning test)
├── graph_clone_shared_index.rs           (new — Task 1)
└── federation_projection.rs              (new — Task 2)

docs/
├── FEDERATION.md                         (Task 8: document what works in which mode)
└── TECHNICAL.md                          (Task 8: merged-brain section)
```

---

## Task 0: Pin the bug with a failing test

**Files:** Create `tests/federation_tool_wiring.rs`

**Goal:** A test that fails today for the right reason, and is the definition of done for the whole plan.

- [ ] **Step 1: Build a fixture federation over a small real repo**

Use the existing `workspace_dir` source against a temp git repo containing three Rust files with a known call chain (`main` → `handler` → `helper`). Reuse the fixture helpers in `tests/federation_integration.rs` if they fit; otherwise write the smallest thing that indexes.

- [ ] **Step 2: Assert through the MCP tool surface, not the internals**

Drive `tools/call` via the same dispatch path the server uses, so the test covers the wiring rather than the library:

```rust
assert!(!find_anchors(&ctx, json!({})).contains("No anchors found"));
assert!(explore_architecture(&ctx, json!({})).contains("main.rs"));
let br = get_blast_radius(&ctx, json!({"symbol": "helper"}));
assert!(br.contains("handler"), "blast radius must reach the caller: {br}");
```

- [ ] **Step 3: Confirm it fails for the stated reason**

```bash
cargo test --test federation_tool_wiring -- --nocapture
```

Expected today: all three assertions fail, with "No anchors found in Merged Brain" and "Node not found for handle: helper". **If it fails for a different reason, stop and re-diagnose before continuing.**

**Verification:** Test compiles, runs, and fails with the messages above.

---

## Task 1: Make `GraphDatabase` safe to clone

**Files:** Modify `src/server/graph.rs`; create `tests/graph_clone_shared_index.rs`

**Goal:** A cloned `GraphDatabase` observes writes made through any other clone. Prerequisite for Task 4.

- [ ] **Step 1: Wrap the two index maps in `Arc`**

```rust
#[derive(Clone)]
pub struct GraphDatabase {
    graph: Arc<RwLock<StableGraph<GraphNode, GraphEdge>>>,
    index_map: Arc<DashMap<String, NodeIndex>>,
    path_index: Arc<DashMap<String, Vec<NodeIndex>>>,
    last_commit: Arc<RwLock<Option<String>>>,
    persistence_path: PathBuf,
    read_only: bool,
}
```

`DashMap` already gives interior mutability, so every existing `self.index_map.insert(…)` / `.get(…)` call site compiles unchanged through the `Arc` deref. Fix `GraphDatabase::new` and `open_read_only` to construct `Arc::new(DashMap::new())`.

- [ ] **Step 2: Confirm the persistence round-trip still holds**

`save_to_disk_sync` serializes `index_map` but not `path_index`; `load_from_disk` (`graph.rs:699`) already rebuilds `path_index` from the loaded graph, and clears-then-refills both maps in place. Those `.clear()` / `.insert()` calls work unchanged through an `Arc<DashMap>`, so this step is a confirmation, not a change — but confirm it, because it is the one place both maps are rewritten wholesale.

- [ ] **Step 3: Write the regression test**

```rust
let a = GraphDatabase::new(&tmp.path().join("g.bin")).unwrap();
let b = a.clone();
a.upsert_node(GraphNode::new(NodeType::Function, "f".into(), "x.rs".into())).unwrap();
assert!(b.find_node_by_name("f").is_some(), "clone must see writes through the original");
assert!(b.get_node_by_id(&id).unwrap().is_some());
assert_eq!(b.node_count(), 1);
```

**Verification:**
```bash
cargo test --test graph_clone_shared_index
cargo test --workspace --features test-utils     # nothing else regresses
```

---

## Task 2: Project intra-repo edges into the merged backend

**Files:** Modify `src/server/federation/graph_backend.rs`, `src/server/federation/federated_index.rs`; create `tests/federation_projection.rs`

**Goal:** The merged backend carries every intra-repo edge, re-keyed to global ids — and projection stays O(nodes + edges) in disk writes, not O(n) full serializations.

- [ ] **Step 1: Add batch methods to the `GraphBackend` trait**

```rust
fn upsert_nodes_batch(&self, nodes: Vec<GraphNode>) -> Result<(), LainError>;
fn upsert_edges_batch(&self, edges: Vec<GraphEdge>) -> Result<(), LainError>;
/// Flush pending state to disk. Batch methods do NOT save; callers
/// invoke this once at the end of a projection pass.
fn flush(&self) -> Result<(), LainError>;
```

Implement on `PetgraphBackend` over `GraphDatabase::upsert_nodes_batch` / `insert_edges_batch`, with **no** `save_to_disk_sync` inside — `flush()` calls it exactly once. Keep the existing single-item `upsert_node` / `upsert_edge` behavior unchanged so no other caller is affected.

Two details on the underlying methods, so nobody has to rediscover them:

- The signatures differ — `upsert_nodes_batch(Vec<GraphNode>)` takes by value, `insert_edges_batch(&[GraphEdge])` by slice.
- `GraphDatabase::upsert_nodes_batch` (`graph.rs:136`) is a plain loop over `upsert_node`, *not* a single-lock batch. That is still fine here, because the per-write `save_to_disk_sync` lives in `PetgraphBackend`, not in `GraphDatabase` — so routing through it already removes the serialization storm. Optimizing it to one write lock is optional; if you skip it, say so in the task report rather than leaving it implied.

- [ ] **Step 2: Re-key and project intra-repo edges in `project_repo`**

After the node loop in `federated_index.rs:90`, build a local map from per-repo node id → global id, then translate every edge:

```rust
let local_to_global: HashMap<String, String> = nodes.iter()
    .map(|n| (n.id.clone(),
              GlobalId::new(id, n.node_type.clone(), &n.path, &n.name).as_str().to_string()))
    .collect();

let mut projected = Vec::with_capacity(repo.edges().len());
let mut dropped = 0usize;
for e in repo.edges() {
    match (local_to_global.get(&e.source_id), local_to_global.get(&e.target_id)) {
        (Some(s), Some(t)) => projected.push(GraphEdge {
            edge_type: e.edge_type.clone(),
            source_id: s.clone(),
            target_id: t.clone(),
            weight: e.weight,
        }),
        // An endpoint that did not project is a dangling edge, not a
        // silent data loss — count it and log once at the end.
        _ => dropped += 1,
    }
}
self.backend.upsert_edges_batch(projected)?;
if dropped > 0 {
    warn!(repo = %id, dropped, "project_repo: edges with unprojected endpoints");
}
```

`federated_index.rs` does not currently import `tracing` — add `use tracing::warn;`.

Note the node-id collapse from Task 0's analysis: several local nodes can map to one global id, so edge counts after projection may be *lower* than `list_repos` reports. That is correct dedup, not loss — assert the relationship rather than exact equality.

- [ ] **Step 3: Convert the node loop and the cross-repo loop to batch, then flush once**

Replace the per-node `upsert_node` and per-match `upsert_edge` with accumulate-then-batch, ending with a single `self.backend.flush()?` before `rebuild_symbol_index()`.

- [ ] **Step 4: Test projection**

```rust
// Two-file fixture with a known Calls edge.
fed.project_repo(&rid).await.unwrap();
let backend = fed.backend();
assert!(backend.edge_count() > 0, "intra-repo edges must project");
let out = backend.traverse(&global_id_of("helper"), EdgeType::Calls, 0..2).unwrap();
assert!(out.iter().any(|n| n.name == "handler"));
```

Add a timing guard so the landmine cannot come back:

```rust
let t = Instant::now();
fed.project_repo(&rid).await.unwrap();
assert!(t.elapsed() < Duration::from_secs(10), "projection regressed to per-write saves");
```

**Verification:**
```bash
cargo test --test federation_projection
# then the reproduction: /health must now show total_edges > 0
```

---

## Task 3: Root the executor in the project directory, not `/tmp`

**Files:** Modify `src/server/ingest/mod.rs` (both federation constructors)

**Goal:** Tuning, embedding models, and the execution `cwd` resolve against the real project — the directory that owns `repos.yaml`.

- [ ] **Step 1: Derive a project root**

`repos_yaml: Option<PathBuf>` is already a parameter of both constructors and is currently unused for this. Use it:

```rust
let project_root = repos_yaml.as_ref()
    .and_then(|p| p.parent())
    .map(Path::to_path_buf)
    .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir()));
```

- [ ] **Step 2: Point the path-rooted subsystems at it**

Replace `&ws` with `&project_root` for `load_tuning_config`, `CrossEncoder::from_dir`, the embedder's model lookup, and `ToolContext.workspace`. This alone fixes `semantic_search` reporting "model not loaded" when `.lain/models/` exists in the project, and makes `.lain/tuning.toml` (including `query_prefix`) take effect.

- [ ] **Step 3: Scope `GitSensor` honestly**

Git is inherently per-repo and federation has N. For this phase:

- **N == 1:** construct `GitSensor` against that repo's path — co-change, commit history, branch status, and file diff all start working.
- **N > 1:** keep a `GitSensor` over the staging dir, and let Task 6 gate the git tools out rather than have them answer about an empty repo.

Leave `LspPool` on the staging dir for now — LSP enrichment is a separate pipeline and is out of scope. Note it in the task report.

- [ ] **Step 4: Keep the staging dir only where structurally required**

If nothing still needs it after the above, delete the `git2::Repository::init` and the `STAGING_COUNTER` dance entirely. If `GitSensor` for N > 1 still needs it, keep it and add a comment saying exactly that — the current comment claims the executor is never reached, which is what caused this bug.

**Verification:**
```bash
# with a model installed under <project>/.lain/models/
call semantic_search '{"query":"claim a file for editing"}'   # must not say "model not loaded"
call get_health '{}'                                           # Workspace: must be the project dir
```

---

## Task 4: Wire `ToolContext.graph` to the federation backend

**Files:** Modify `src/server/federation/graph_backend.rs`, `src/server/ingest/mod.rs`

**Goal:** The ~40 structural tools read the merged federation graph. This is the fix; Tasks 1–3 make it correct.

- [ ] **Step 1: Expose the backing database on the trait**

`FederatedIndex.backend` is `Arc<dyn GraphBackend>`, so the accessor must be on the trait:

```rust
/// The backend's underlying `GraphDatabase`, when it has one.
/// `PetgraphBackend` returns `Some`; alternative backends that are not
/// petgraph-backed return `None` and the caller degrades per Task 6.
fn as_graph_db(&self) -> Option<GraphDatabase> { None }
```

Implement on `PetgraphBackend` as `Some(self.db.clone())` — safe to clone as of Task 1 — and remove the `#[cfg(any(test, feature = "test-utils"))]` gate on `PetgraphBackend::db()` (`graph_backend.rs:81`), or leave that helper test-only and use the trait method in production.

- [ ] **Step 2: Use it in both federation constructors**

```rust
let graph = match federation.backend().as_graph_db() {
    Some(db) => db,
    None => {
        warn!("federation backend exposes no graph database; \
               structural tools will be unavailable (see Task 6 gating)");
        GraphDatabase::new(&mem_path)?
    }
};
```

Do this in **both** `with_federation_with_attribution` (`ingest/mod.rs:313`) and `with_federation_and_workspaces_with_attribution` (`:509`) — `src/cli/server.rs:124` and `:133` pick between them based on whether `workspaces.yaml` exists, so missing one leaves half the users broken.

- [ ] **Step 3: Confirm ordering against the indexing pipeline**

The executor is built at construction time; repos are indexed and projected afterwards (`loader.rs:48`, `:126`). This is only safe because Task 1 made the clone share the index maps. Add a comment at the assignment saying so, naming Task 1 — this is the exact invariant a future refactor will break.

- [ ] **Step 4: Run the Task 0 test**

```bash
cargo test --test federation_tool_wiring -- --nocapture
```

All three assertions must now pass.

**Verification:** Full reproduction script. `find_anchors`, `explore_architecture`, and `get_blast_radius run_doctor` all return real data.

---

## Task 5: Resolve symbols across repos without silent first-wins

**Files:** Modify `src/server/tools/handlers/` symbol-resolution helpers (via `src/server/tools/utils.rs`)

**Goal:** `get_blast_radius {"symbol": "main"}` across a 12-repo federation must not silently pick one repo's `main`.

- [ ] **Step 1: Find the resolution path**

Handlers resolve a user-supplied name through `find_node_by_name` / `get_node_at_location`, which return the first match. With global ids in one merged graph, `main`, `new`, and `handler` collide across repos.

- [ ] **Step 2: Accept a qualified form and report ambiguity**

- Accept a fully-qualified global id (`lain:Function:/path:run_doctor`) verbatim.
- For a bare name with exactly one match, resolve it — today's behavior, now provably unambiguous.
- For a bare name with several matches, return an error listing the candidates, mirroring the existing `LainError::AmbiguousSymbol` used by `FederatedIndex::resolve_symbol`.

The error is the feature: an agent that gets three candidates picks one, where an agent that gets the wrong `main` reasons from it.

- [ ] **Step 3: Test with a two-repo fixture** where both repos define `main`.

**Verification:** `cargo test --test federation_tool_wiring` plus a new ambiguity case.

---

## Task 6: Stop advertising tools that cannot work

**Files:** Modify `src/server/tools/registry.rs`, `src/server/mcp/handler.rs`

**Goal:** No tool returns a confident empty answer. Either it works, or it is not offered, or it errors with a reason.

- [ ] **Step 1: Add a capability flag to the registry**

Mark each handler with what it needs — `needs_graph`, `needs_git`, `needs_workspace_exec`, `needs_embedder`. Derive availability at startup from the constructed context.

- [ ] **Step 2: Filter `tools/list` by capability**

In federation mode with N > 1, drop the git-dependent tools (`get_commit_history`, `get_branch_status`, `get_file_diff`, `get_coupling_radar`, `run_enrichment`, `sync_state`) until they take a `repo_id`. Always drop `debug_sleep` from the default listing — it is a job-infrastructure test fixture and is currently shipping to every client.

- [ ] **Step 3: Make any surviving unavailable tool error explicitly**

```
Error: get_coupling_radar requires a single-repo workspace; this server is
in federation mode with 12 repos. Pass repo_id, or run a per-repo server.
```

Never `Ok("")`, never "No results found" when the truth is "not wired".

- [ ] **Step 4: Test** that `tools/list` in federation mode omits the gated tools and that calling one directly returns `isError: true` with the reason.

**Verification:**
```bash
call tools/list | jq '.result.tools | length'   # < 59 in federation mode
call debug_sleep '{}'                            # not listed
```

---

## Task 7: Fix the health body and guard the counts

**Files:** Modify `src/server/mcp/handler.rs` (`build_health_body`)

**Goal:** `/health` stops contradicting `list_repos`, and the contradiction cannot come back.

- [ ] **Step 1: Report the real numbers.** After Task 4, `graph_nodes` / `graph_edges` are the merged backend's counts and `federation.total_edges` is non-zero. Verify they agree with `list_repos` (modulo the documented global-id dedup).

- [ ] **Step 2: Fix `tools_count`.** It reports `35`; `tools/list` returns `59`. It is *not* a stale constant — `build_health_body` (`handler.rs:1395`) already derives it from `ToolRegistry::definitions().len()`. The gap is that `tools/list` (`handler.rs:1663`) builds `ToolRegistry::definitions()` **and then** `.extend(special_tool_definitions())`, which is where the federation, presence, and workspace tools live. Health counts one half of the union.

Extract the assembly into one function that both call, so the two can never diverge again, and add a test asserting `health.tools_count == tools/list().len()`. Note this interacts with Task 6: once `tools/list` is capability-filtered, the shared function must apply the same filter, and the test then pins the filtered count.

- [ ] **Step 3: Add the git SHA.** `build.rs` already emits `LAIN_GIT_SHA` and `lain_git_sha()` already exists for `lain doctor`. Put it in the health body so `lain doctor` can compare the running server's build against the local binary — the mismatch axis wishlist #6 was actually about.

**Verification:**
```bash
curl -s localhost:9871/health | jq '{graph_nodes, graph_edges, tools_count, commit}'
cargo test --test version_consistency
```

---

## Task 8: Documentation and end-to-end proof

**Files:** Modify `docs/FEDERATION.md`, `docs/TECHNICAL.md`, `README.md`; create `tests/e2e/federation-tools.sh`

- [ ] **Step 1: Document the mode matrix** — a table in `FEDERATION.md` of which tool groups work in single-repo vs federation-with-N-repos, and what each degrades to. This is the doc whose absence let the bug live.

- [ ] **Step 2: Correct the README.** It says lain exposes "exactly five subcommands"; the binary has seven (`hooks`, `doctor`).

- [ ] **Step 3: Fix the phantom command.** `semantic_search`'s unavailable path tells the caller to run `lain install-embeddings`, which does not exist — it returns `error: unrecognized subcommand`. Point it at the documented `install.sh --download-model` flow or the `LAIN_EMBEDDING_MODEL` env var.

- [ ] **Step 4: Script the reproduction** as `tests/e2e/federation-tools.sh` — the block at the top of this plan, with assertions — and wire it into CI alongside `cargo test`. CI currently runs only `cargo test`: no `clippy`, no `fmt --check`, and no e2e.

**Verification:** `bash tests/e2e/federation-tools.sh` exits 0.

---

## What this plan does *not* fix

Stated so scope creep is a decision, not an accident:

- **LSP enrichment in federation mode** stays rooted at the staging dir (Task 3, Step 3). Separate pipeline, separate plan.
- **Git tools across N > 1 repos** are gated off rather than made repo-aware. Making them take `repo_id` is the follow-up.
- **`semantic_search` still needs a model on disk.** Task 3 makes an installed model discoverable; it does not install one.
- **Node-id collapse** during projection (2889 → 2391) is left as-is. It is correct dedup under `(repo, kind, path, name)`, but if two distinct symbols in one repo share all four, they merge. Worth a follow-up audit.

## Risks

| Risk | Mitigation |
|---|---|
| Projection slows startup on large federations | Task 2's batch path plus the timing assertion; re-run the nightly `federation-nightly.yml` large-fixture benchmark before merge |
| `Arc`-ing the index maps changes behavior somewhere that relied on copy-on-clone | Task 1 lands alone with the full suite green before Task 4 |
| Response shape changes — node ids become global ids (`lain:Function:/path:name`) | Intended and an improvement (repo-qualified), but call it out in `FEDERATION.md`; check the Command Center's graph tab still renders |
| Symbol ambiguity turns previously "working" calls into errors | That is the point of Task 5 — but check the e2e scripts and Command Center for bare-name calls that now need qualifying |

## Definition of done

1. `tests/federation_tool_wiring.rs` passes.
2. The reproduction script shows real data from `find_anchors`, `explore_architecture`, and `get_blast_radius`.
3. `/health` node/edge counts agree with `list_repos`, and `tools_count` matches `tools/list`.
4. No tool in `tools/list` returns an empty success for a question it cannot answer.
5. `cargo test --workspace --features test-utils` green; nightly federation benchmark not regressed.
