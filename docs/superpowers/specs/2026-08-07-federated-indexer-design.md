# Federated Indexer — Design

**Status:** Draft (brainstorming complete, awaiting user review)
**Date:** 2026-08-07
**Sub-project:** 1 of 7 in the org-wide code intelligence vision
**Depends on:** nothing
**Enables:** sub-projects 2 (Service Identity), 4 (Redundancy), 5 (Multi-tenancy), 6 (UI), 7 (Live PR overlay)

---

## Context and motivation

LAIN is currently a per-workspace MCP server: one process, one repo, one in-memory petgraph persisted to a local `bincode` file. Its structural intelligence — blast radius, call chains, anchors, co-change coupling — stops at the repository boundary. Every "kills knowledge silos" use case (cross-service blast radius, organization-wide redundancy detection, "who else is changing this?") is unreachable from the current model.

The user wants to evolve LAIN into a company-wide code intelligence server: one process that indexes N repos, answers cross-repo queries, and is the substrate for further sub-projects (service identity, IaC ingestion, redundancy analysis, live PR overlay, multi-tenancy, UI).

The full vision decomposes into 7 sub-projects. **This spec is for sub-project 1 only: the Federated Indexer.** It is the foundation; every later sub-project depends on it.

## Goals

1. One LAIN process serves N repos.
2. Every existing per-repo tool (`get_call_chain`, `get_blast_radius`, `find_anchors`, etc.) keeps working unchanged when the server is configured with a single repo.
3. New cross-repo tools answer organization-wide questions: cross-repo blast radius, org-wide symbol search, repo inventory.
4. Cold start of 200 repos / 10M LOC completes in <30 minutes on a 16-core box.
5. Cross-repo queries (depth-5 blast radius) return in <100ms p99.
6. The system tolerates partial failure: one broken repo does not break the server.
7. The architecture is scale-independent: it must work at 10 engineers and not require a rewrite at 1000.

## Non-goals (deferred to later sub-projects)

- **Service identity** (sub-project 2) — no `Service` node type yet.
- **IaC/schema ingestion** (sub-project 3) — `Resource`, `Schema` node types exist in `schema.rs` but have no ingesters in this sub-project.
- **Auth/ACL** (sub-project 5) — server is single-tenant; all clients see all repos. `GraphBackend` trait is designed to leave room for ACL filtering later.
- **External graph DB** — `GraphBackend` trait is defined; only `PetgraphBackend` is implemented. `MemgraphBackend` is the documented escape hatch.
- **UI** (sub-project 6) — MCP tools only.
- **Live PR overlay** (sub-project 7) — file-watcher-only updates; no PR polling. The cross-repo edge pattern in this spec is the foundation for it.
- **Redundancy detection** (sub-project 4) — no clustering, no similarity reports. The symbol-embedding pipeline is preserved for later.

---

## Architecture

Today:

```
LainServer → GraphDatabase (one petgraph) → MCP tools
```

Federated:

```
┌─ RepoSource trait ─────────────────────────────┐
│  • LocalClone      full clone in ./repos/       │
│  • ShallowClone    shallow clone from git remote│
│  • WorkspaceDir    today's single-workspace mode│
└────────┬────────────────────────────────────────┘
         │  clones/fetches
         ▼
┌─ RepoIndex (N) ─────────────────────────────────┐
│  per repo:                                      │
│   • LSP pool                                     │
│   • tree-sitter extractor                       │
│   • per-repo petgraph                            │
│   • per-repo bincode persistence                │
│   • per-repo volatile overlay                   │
│   • file watcher                                │
└────────┬────────────────────────────────────────┘
         │  per-repo graphs
         ▼
┌─ FederatedIndex ────────────────────────────────┐
│  • global IDs:  repo_id:kind:path:name          │
│  • CrossRepoSameSymbol edges                    │
│  • name+signature matching                      │
│  • GraphBackend trait                           │
│     • PetgraphBackend (today, extended)         │
│     • MemgraphBackend (future, not implemented) │
└────────┬────────────────────────────────────────┘
         │
         ▼
┌─ MCP server (extended) ─────────────────────────┐
│  • existing per-repo tools (unchanged)          │
│  • list_repos                                   │
│  • get_repo_info                                │
│  • get_cross_repo_blast_radius                  │
│  • search_org                                   │
│  • get_federation_health                        │
└──────────────────────────────────────────────────┘
```

The two load-bearing abstractions:

- **`RepoSource`** — how the server obtains code.
- **`GraphBackend`** — how the graph is stored.

Both are traits with a default implementation today (`LocalClone` and `PetgraphBackend`) and a documented escape hatch (`ShallowClone` ships now; `MemgraphBackend` ships later).

---

## Components

### 1. `RepoSource` trait and impls

**File:** `src/federation/repo_source.rs` (~150 LOC)

```rust
#[async_trait]
pub trait RepoSource: Send + Sync {
    fn id(&self) -> &RepoId;
    fn local_path(&self) -> &Path;
    async fn fetch(&self) -> Result<(), LainError>;
    fn last_refreshed(&self) -> SystemTime;
    fn is_stale(&self, max_age: Duration) -> bool;
}
```

Impls:

- **`LocalClone`** — `git clone` into `./repos/<id>`, refresh with `git fetch && git reset --hard`. Used when full history is wanted and disk is cheap.
- **`ShallowClone`** — `git clone --depth 1`, refresh with `git fetch --depth 1 && git reset --hard origin/HEAD`. Lower disk; loses co-change history. Co-change degrades gracefully.
- **`WorkspaceDir`** — today's single-workspace mode, kept for backwards compatibility. `fetch()` is a no-op; `local_path()` is the configured workspace.

### 2. `GraphBackend` trait

**File:** `src/federation/graph_backend.rs` (~80 LOC trait, ~120 LOC `PetgraphBackend`)

```rust
pub trait GraphBackend: Send + Sync {
    fn upsert_node(&self, node: GraphNode) -> Result<(), LainError>;
    fn upsert_edge(&self, edge: GraphEdge) -> Result<(), LainError>;
    fn get_node(&self, global_id: &str) -> Result<Option<GraphNode>, LainError>;
    fn traverse(&self, start: &str, edge: EdgeType, depth: Range) -> Result<Vec<GraphNode>, LainError>;
    fn find_path(&self, from: &str, to: &str) -> Result<Vec<GraphNode>, LainError>;
    fn subgraph_around(&self, center: &str, radius: u32) -> Result<Vec<(GraphNode, Vec<GraphEdge>)>, LainError>;
}
```

`PetgraphBackend` wraps today's `GraphDatabase` and adds a `DashMap<String, GlobalId>` index. Implementation is a thin adapter; no algorithmic changes.

`MemgraphBackend` is **not** implemented in this sub-project. The trait is the contract; later sub-projects can add the impl.

### 3. `RepoIndex`

**File:** `src/federation/repo_index.rs` (~300 LOC)

Wraps today's per-repo indexer pipeline for one repo. Owns:

- LSP pool (per-repo; isolates crashes)
- tree-sitter extractor
- per-repo `petgraph::StableGraph` (today's in-memory representation)
- per-repo `bincode` persistence at `<data_dir>/repos/<id>/graph.bin`
- per-repo volatile overlay
- file watcher

Designed to be reloadable: the `RepoIndex` can be dropped and rebuilt from disk without restarting the server. This is what enables `lain reload` (add/remove repos at runtime) in sub-project 5+.

### 4. `FederatedIndex`

**File:** `src/federation/federated_index.rs` (~400 LOC)

Orchestrator. Holds:

- `HashMap<RepoId, Arc<RepoIndex>>` — all per-repo indexes
- `Arc<dyn GraphBackend>` — the global graph (default: `PetgraphBackend`)
- `DashMap<GlobalId, (RepoId, LocalId)>` — bidirectional mapping
- `tokio::sync::RwLock<RepoHealth>` per repo

On every per-repo index update, the `FederatedIndex`:

1. Walks new/changed nodes and edges
2. Generates global IDs by prefixing with `repo_id`
3. Writes them to the global `GraphBackend`
4. Runs cross-repo same-symbol matching (see §5)
5. Adds `CrossRepoSameSymbol` edges to the global graph

### 5. Global ID scheme and cross-repo matching

**Global ID:** `repo_id:node_type:path:name` — extends today's `(NodeType, path, name)` by prefixing `repo_id`. Stable, no collisions, no central registry needed.

**Cross-repo matching:** for each new symbol, look up other repos' symbols with the same `name` and at least 80% signature similarity (cosine on tokenized signature). For each match, add a `CrossRepoSameSymbol` edge with `weight = signature_similarity`.

Matching runs incrementally on every per-repo update. False positives are cheap; we cap at the top-5 candidates per symbol to bound work.

Signature similarity is computed by tokenizing the signature into identifier-like tokens, hashing each to a fixed-dim vector, and computing cosine. This is intentionally simple — the embedding pipeline (sub-project 4) can replace it later for redundancy detection.

### 6. Discovery and loading

**File:** `src/federation/config.rs` (~120 LOC), `src/federation/loader.rs` (~200 LOC)

A `repos.yaml` file lists repos:

```yaml
repos:
  - id: auth-svc
    source:
      type: local_clone
      url: https://github.com/acme/auth-svc
      ref: main
  - id: billing-svc
    source:
      type: shallow_clone
      url: https://github.com/acme/billing-svc
      ref: main
      refresh_interval: 5m
  - id: legacy-monolith
    source:
      type: workspace_dir
      path: /srv/legacy
data_dir: /var/lib/lain
max_concurrent_indexers: 8
ready_threshold: 0.8
```

CLI:

```bash
lain server --config /etc/lain/repos.yaml --transport http --port 9999
```

Loader behavior:

- Parse config, build `RepoSource` per entry
- Load existing `federation_manifest.bin` if present; reconcile with config
- For each repo, call `RepoSource::fetch()` and spawn a `RepoIndex` worker
- Workers run in parallel, bounded by `max_concurrent_indexers`
- Server reaches `Ready` state when `ready_threshold` fraction of repos are `Ready` (rest can be `Indexing` or `Degraded`)

### 7. MCP server extensions

**File:** `src/mcp/federation_tools.rs` (~250 LOC)

New tools:

- `list_repos() -> Vec<RepoInfo>` — id, path, health, last_refreshed, node_count
- `get_repo_info(id: String) -> RepoInfo` — single repo detail
- `get_cross_repo_blast_radius(symbol: String, depth: Range) -> BlastRadius` — org-wide
- `search_org(query: String, limit: usize) -> Vec<SymbolMatch>` — org-wide symbol search
- `get_federation_health() -> FederationHealth` — counts per health state, total nodes/edges, memory estimate

Existing per-repo tools (`get_blast_radius`, `get_call_chain`, etc.) are unchanged when the server is configured with one repo. When the server has multiple repos, those tools take an optional `repo_id` argument: if omitted, the server resolves the symbol to a unique repo (single match = use that repo, zero matches = `NotFound`, multiple matches = return the list of candidate repos and ask the caller to disambiguate). New `get_cross_repo_*` tools never require `repo_id`.

### 8. Persistence

**File:** `src/federation/manifest.rs` (~80 LOC)

- Per-repo `bincode` (unchanged) at `<data_dir>/repos/<id>/graph.bin`
- Top-level `<data_dir>/federation_manifest.bin` — list of repos, last-sync time, content hash per repo
- `lain reload` re-reads the manifest and reconciles with running state

Cold restart loads the manifest first, then re-attaches each repo's bincode, then continues file watching. The reload is O(N) on bincode reads, ~1s per repo for typical sizes.

---

## Data flow

### Cold start of a 200-repo / 10M-LOC server

1. Operator runs `lain server --config repos.yaml`
2. Server reads `federation_manifest.bin` (if present) to learn which repos it knew about
3. For each repo, calls `RepoSource::fetch()` — clones or refreshes as configured
4. Spawns `RepoIndex` per repo, runs in parallel bounded by `max_concurrent_indexers` (default 8)
5. Each `RepoIndex` runs today's pipeline: tree-sitter extract → LSP hydrate → git co-change
6. As each `RepoIndex` finishes initial index, its nodes/edges are added to the global `GraphBackend` with global IDs
7. Cross-repo matching runs incrementally on each new symbol
8. Server becomes `Ready` when `ready_threshold` (default 80%) of repos are `Ready`
9. File watchers continue updating per-repo indexes; volatile overlays remain per-repo but are projected into the global graph

### Per query: `get_cross_repo_blast_radius(symbol, depth)`

1. Resolve `symbol` to a `GlobalId` via the index
2. BFS in the global `PetgraphBackend` up to `depth`
3. Group results by repo
4. Return `BlastRadius { by_repo: HashMap<RepoId, Vec<Node>>, total_count, truncated }`

Latency: dominated by BFS, which is O(V + E) within depth. For depth=5 and a 50M-edge graph, target is <100ms p99.

### File edit during operation

1. Per-repo `notify` watcher fires
2. `RepoIndex` re-extracts the changed file via tree-sitter (LSP on-demand)
3. Volatile overlay updates per-repo graph
4. Diff is pushed to `FederatedIndex`, which updates the global graph
5. Cross-repo matching re-runs for changed symbols

---

## Error handling

| Failure | Behavior | Rationale |
|---|---|---|
| `RepoSource::fetch()` fails (network, auth) | Log warning, mark repo `Unavailable`, continue | One bad repo must not break the server |
| Per-repo `RepoIndex` indexer fails (LSP crash, OOM) | Restart with exponential backoff (max 3). Then mark `Degraded` | Indexers fail; restart is normal |
| Cross-repo matching finds no candidates | No-op; add symbol to global graph alone | Most symbols are repo-local |
| Cross-repo matching finds many candidates | Take top-5 by signature similarity | Bound work; FP cost is low |
| `PetgraphBackend` OOM | Return `LainError::ResourceExhausted`. Operator adds RAM or removes repos | No in-memory fix; surface honestly |
| Query on unknown symbol | Return `NotFound` with list of closest same-name symbols across repos | Today's behavior, extended |
| Concurrent per-repo updates during a query | Per-repo `RwLock` (already exists). Federated queries take read locks in deterministic order (sort by `repo_id`) | Standard lock-ordering |
| `repos.yaml` references missing repo | Server starts; marks repo `Missing`, logs error. `lain reload` after config fix | Config errors must not block startup |

New `RepoHealth` enum in `schema.rs`:

```rust
pub enum RepoHealth {
    Ready,
    Indexing,
    Degraded,
    Unavailable,
    Missing,
}
```

`get_federation_health` exposes counts per state, total nodes/edges, memory estimate.

---

## Testing

### Unit tests (`src/federation/*_tests.rs`, ~600 LOC)

- `RepoSource` impls against in-memory test fixtures (no real clones)
- `GraphBackend` contract tests run against `PetgraphBackend`
- Global ID generation: stable across runs, no collisions across N synthetic repos
- Cross-repo matching: same name+sig → edge; different sig → no edge; same sig different name → no edge
- Concurrent updates do not deadlock (stress test with N=20 threads)

### Integration tests (`tests/federation_integration.rs`, ~400 LOC)

- Spin up `FederatedIndex` with 5 synthetic repos (tiny Rust crates)
- Index, run cross-repo queries, assert
- Add a 6th repo at runtime, assert it appears
- Stop a repo, assert it degrades to `Unavailable` and others still serve
- Cold restart from bincode manifest, assert all repos reload

### Performance test (`tests/federation_benchmark.rs`, ~300 LOC)

Two fixtures to validate the goals:

- **Small fixture:** 10 synthetic repos, ~50K LOC each (~500K LOC total)
  - Cold start: < 2 min on 16-core box
  - Cross-repo blast radius (depth 5): < 100ms p99
  - Memory: < 4 GB
- **Large fixture:** 200 synthetic repos, ~50K LOC each (~10M LOC total) — directly validates Goals #4 and #5
  - Cold start: < 30 min on 16-core box
  - Cross-repo blast radius (depth 5): < 100ms p99
  - Memory: < 32 GB
  - This fixture is gated behind a `--ignored` flag by default and run in nightly CI; the small fixture runs on every PR

### E2E test (`tests/e2e/federation_e2e.sh`)

- Start `lain server` against 3 small public repos
- Connect via MCP HTTP, call `list_repos` and `get_cross_repo_blast_radius`
- Assert responses are correct

### Backwards compatibility

- All today's tests pass byte-identical when `lain --workspace ./myrepo` is used (WorkspaceDir source)
- CI runs both single-workspace and federated test matrices

---

## Open questions / risks

1. **Signature similarity threshold** — 80% is a guess. Real-world tuning will be needed; documented as a config knob.
2. **Cross-repo edge explosion** — top-5 cap is conservative. May need to revisit for repos with very common names (`new`, `init`).
3. **Co-change history under `ShallowClone`** — `--depth 1` removes it. Either `LocalClone` for repos where co-change matters, or accept the degradation. Documented, not solved here.
4. **`MemgraphBackend` deferral** — the trait contract is the deliverable; actual swap-in is a separate sub-project.

---

## Effort estimate

- One senior Rust engineer
- ~8–10 weeks
- ~1,700 LOC of new code + ~1,200 LOC of tests
- No external dependencies beyond what's already in `Cargo.toml` (petgraph, dashmap, tokio, git2, notify)
- Blocked on nothing — can start immediately
