# Federation Test Gap Fix — Design

**Status:** Draft (brainstorming complete, awaiting user review)
**Date:** 2026-08-11
**Sub-project:** standalone, prerequisite for the Lain Workspaces feature
**Depends on:** nothing in mainline; enables confidence in the federation substrate the Workspaces feature will build on
**Enables:** `2026-08-11-lain-workspaces-design.md` (next spec), and any future feature that asserts on cross-repo reasoning

---

## Context and motivation

Lain's federation mode (`lain server --config repos.yaml`) answers org-wide structural questions across N repos. The engineering behind it (`FederatedIndex`, `GraphBackend`, `find_cross_repo_matches`, `resolve_symbol`, `get_cross_repo_blast_radius`, `search_org`) is non-trivial and correct only if the contracts hold. Today the test coverage has two gaps that block landing higher-level features (notably Workspaces) on solid ground:

1. **No real-code proof of cross-repo reasoning.** `tests/federation_benchmark.rs::small_fixture_blast_radius_under_100ms_p99` exercises the latency budget on a synthetic 50K-node chain. It proves the `GraphBackend::traverse` hot path, not the semantic contracts (`resolve_symbol` returns the right repo, `AmbiguousSymbol` surfaces when it should, `search_org` finds shared concepts, the `by_repo` bucketing is correct, and — most importantly — **cross-repo `Calls` edges actually exist** in the projected graph).

2. **No polyglot real-OSS e2e.** `tests/e2e/federation_e2e.sh` runs against three famous independent Rust crates (rayon, ripgrep, serde). Those projects don't call each other, so the federation is just three unrelated subgraphs. The e2e asserts `list_repos`, `get_federation_health`, and `search_org("serialize") ≥ 1 hit`. It does not call `get_cross_repo_blast_radius`, `get_repo_info`, or any cross-repo reasoning tool. It does not exercise polyglot indexing (12+ languages).

### The cross-repo `Calls` gap

`src/mcp/federation_tools.rs` already has a unit test (`cross_repo_blast_radius_walks_calls_across_repo_boundaries` and variants) that proves `GraphBackend::traverse` **correctly walks `Calls` edges whose source and target live in different repos** when such edges exist in the global petgraph. The traversal logic is right.

**But the federation never actually creates such edges in production — and the gap is wider than that.** Reading `src/federation/federated_index.rs::project_repo` (lines 84–119) carefully: it only (1) upserts per-repo **nodes** (re-keyed to global ids) and (2) adds `CrossRepoSameSymbol` edges from `find_cross_repo_matches`. It does **not** call `RepoIndex::edges()` (`src/federation/repo_index.rs:106`) at all. **No per-repo `Calls`, `Contains`, `Defines`, or `Imports` edges are projected into the global backend.** Existing federation unit tests that pass (the unit test above, and `federation_index_for_test` in `src/federation/mod.rs`) all manually insert `Calls` edges via `backend.upsert_edge(...)` or `insert_edges_batch(...)`, bypassing `project_repo`.

So in production today, `get_cross_repo_blast_radius` returns empty for every seed — not because the seed has no cross-repo callers, but because the global backend has zero `Calls` edges of any kind. Both intra-repo and cross-repo call-chain reasoning are unavailable.

That's a feature gap, not just a test gap. To make "analyze interconnectedly" real at the call-chain level, the federation must (1) actually project per-repo edges into the global backend, and (2) resolve cross-repo `Calls` references to global nodes in other repos. This spec does both: two engine passes, plus test fixtures that prove they work end-to-end.

The upcoming Workspaces feature (`docs/superpowers/specs/2026-08-11-lain-workspaces-design.md`, forthcoming) sits directly on the federation substrate — a workspace is a named subset of `repos.yaml`'s repos that the federation engine indexes together. Before we build that, we need evidence the substrate actually produces and traverses cross-repo call chains.

### Two fixtures

- **Fixture D:** a deterministic Rust integration test that builds 3 dependent tempdir crates and asserts the federation's semantic contracts end-to-end. Runs on every PR, no network.
- **Fixture A:** an extension to the existing `tests/e2e/federation_e2e.sh` that adds the OpenTelemetry Astronomy Shop (12 polyglot microservices via `WorkspaceDirSource`) and asserts cross-repo tool behavior against real OSS code. Runs on nightly / manual, network-dependent.

Together they turn the federation's correctness claims from "trust the unit tests" into "verified against representative code, including cross-repo `Calls` edges."

---

## Goals

1. **Federation engine change (two passes):**
   - **Pass A:** `project_repo` projects every per-repo edge (`Calls`, `Contains`, `Defines`, `Imports`, etc.) into the global petgraph by re-keying source and target ids to global format. After this pass, intra-repo `Calls` traversal works in production for the first time.
   - **Pass B:** After Pass A, `project_repo` walks the projected `Calls` edges whose target is a reference name (not a defined function in the source repo), looks each name up in `symbol_to_repos`, and inserts a cross-repo `Calls` edge when the lookup is unambiguous. After this pass, cross-repo `Calls` traversal works.
2. **Per-PR guarantee** that federation's semantic contracts hold: `resolve_symbol` returns the right repo for unique / ambiguous / not-found inputs, both intra-repo and cross-repo `Calls` edges exist after indexing, `get_cross_repo_blast_radius` walks them and buckets by repo, `search_org` finds shared concepts across repos.
3. **Nightly guarantee** that federation works against real polyglot OSS code: ≥9 OTel services indexed (out of the 12 service subdirs at upstream HEAD) across 6+ languages, `search_org` finds shared domain concepts (`Product`, `Money`) in ≥2 repos, `get_repo_info` returns valid shape for a known OTel service, `get_cross_repo_blast_radius` returns valid shape against a documented gRPC method.
4. **No regression** to the existing per-PR test matrix or the existing e2e behavior — the existing 3-repo e2e assertions stay green and become the "famous independent projects still index" baseline.
5. **CI-budget-bounded:** D adds <30s to per-PR. A extends the existing nightly e2e (no new CI workflow).

## Non-goals

- **Service identity, multi-tenancy, redundancy detection, UI, live PR overlay.** All explicitly deferred sub-projects of the federation vision (see `docs/superpowers/specs/2026-08-07-federated-indexer-design.md`). Untouched.
- **Replacing or rewriting existing federation tests.** `tests/federation_integration.rs` and `tests/federation_benchmark.rs` stay. This spec adds new artifacts.
- **Verifying per-language LSP correctness for all 12 OTel services.** The nightly A fixture uses `ready >= 8` as the threshold, tolerating up to 4 services being degraded because their language server isn't on the CI image. Stricter per-language validation is out of scope.
- **Replacing the signature-similarity heuristic with embeddings.** The `find_cross_repo_matches` heuristic stays; the deferred "Redundancy" sub-project replaces it later.
- **The Workspaces feature itself.** That's the next spec.

---

## Architecture

A small federation-engine change plus two new test artifacts.

```
Production code (CHANGED):
  src/federation/federated_index.rs
  └── project_repo(id) — gains TWO passes:
        Pass A: re-key every per-repo edge (Calls/Contains/Defines/Imports/...)
                to global ids and upsert into the global backend
        Pass B: for each projected Calls edge whose target resolves (via
                symbol_to_repos) to a single repo different from the source,
                add a cross-repo Calls edge to the global node in that repo

Per-PR (NEW):
  tests/federation_cross_repo_e2e.rs
  ├── write_three_dependent_crates(root)         → helper, builds 3 tempdir crates
  ├── cross_repo_resolver_unique_owner           → resolve_symbol returns Ok
  ├── cross_repo_resolver_ambiguous              → resolve_symbol returns AmbiguousSymbol
  ├── cross_repo_search_org_finds_shared_concepts → search_org hits ≥2 repos
  ├── cross_repo_calls_edge_resolves_to_global_node → cross-repo Calls edge exists
  ├── cross_repo_blast_radius_within_owning_repo → intra-repo outgoing Calls traversal
  ├── cross_repo_blast_radius_walks_into_other_repos → traversal crosses repo boundary
  ├── cross_repo_blast_radius_ambiguous_for_tool → tool surfaces AmbiguousSymbol
  └── cross_repo_blast_radius_not_found          → tool surfaces NotFound

Nightly / manual:
  tests/e2e/federation_e2e.sh (EXTENDED, not rewritten)
  ├── existing 3 famous repos (rayon, ripgrep, serde)  ← unchanged assertions
  ├── NEW: git clone https://github.com/open-telemetry/opentelemetry-demo.git
  ├── NEW: 12 service subdirs registered as workspace_dir entries in repos.yaml
  ├── NEW: total_repos ≥ 12 assertion
  ├── NEW: get_federation_health.ready ≥ 8 wait + assertion
  ├── NEW: search_org("Product") distinct_repos ≥ 2
  ├── NEW: search_org("Money") distinct_repos ≥ 2
  ├── NEW: get_repo_info("otel-productcatalogservice") returns valid shape
  └── NEW: get_cross_repo_blast_radius("GetProduct", "1..3") returns valid shape
```

The production change has two passes, both inside `project_repo`. Pass A copies per-repo edges (re-keyed) into the global backend — this is the prerequisite Pass B needs. Pass B walks the projected `Calls` edges, takes those whose target is a reference name (not a defined function in the source repo), looks each name up in `symbol_to_repos`, and — when the lookup is unambiguous and the target repo is different from the source repo — adds a cross-repo `Calls` edge to the global node in that repo. Ambiguous names and not-found names leave the original intra-repo edge in place (we don't fabricate cross-repo calls out of fuzzy matches).

---

## Production code change — `src/federation/federated_index.rs::project_repo`

### Current behavior

`project_repo(id)` does (today), reading `src/federation/federated_index.rs:84–119`:

1. Re-keys per-repo nodes from `(NodeType, path, name)` to global ids (`{id}:{NodeType}:{path}:{name}`) and upserts them into the `GraphBackend`.
2. Iterates over every other repo's nodes and runs `find_cross_repo_matches` against the projected repo's signatures, adding `CrossRepoSameSymbol` edges for matches above threshold.
3. Rebuilds the `symbol_to_repos` index.

**Notably absent:** `RepoIndex::edges()` (`src/federation/repo_index.rs:106`) is never called. No per-repo edges (`Calls`, `Contains`, `Defines`, `Imports`, etc.) are projected into the global backend. The global backend accumulates only `CrossRepoSameSymbol` edges. As a result, in production today, `get_cross_repo_blast_radius` returns empty for every seed — the global backend has zero `Calls` edges of any kind, and the traversal has nothing to walk.

### New behavior (this spec)

After step 1 and before step 2, insert **Pass A** (project per-repo edges). After Pass A and before the existing step 2, insert **Pass B** (resolve cross-repo `Calls` edges).

#### Pass A — Project per-repo edges into the global backend

```rust
// NEW (Pass A): Re-key every per-repo edge to global ids and upsert into
// the global backend. Source and target ids are rewritten from local
// per-repo ids (whatever RepoIndex::edges() yields) to global ids.
for edge in repo_index.edges() {
    let global_source = GlobalId::new(id, ...).as_str().to_string();
    let global_target = GlobalId::new(id, ...).as_str().to_string();
    let mut rewritten = edge.clone();
    rewritten.source_id = global_source;
    rewritten.target_id = global_target;
    self.backend.upsert_edge(rewritten)?;
}
```

The exact id re-keying depends on what `RepoIndex::edges()` returns. If edges carry `(NodeType, path, name)` for source and target (in addition to the local id), re-keying is direct. If they carry only local ids, the implementation plan must derive `NodeType`/`path`/`name` from the local ids (via a lookup against the per-repo `nodes()` set).

After Pass A, intra-repo `Calls` traversal works in production for the first time. `get_cross_repo_blast_radius("hash", "1..3")` now returns the inner_hash node.

#### Pass B — Cross-repo `Calls` resolution

```rust
// NEW (Pass B): For each projected Calls edge whose target is a reference
// name (a node that exists in this repo as an imported name but is defined
// in another repo), look up the target's name in symbol_to_repos. If the
// lookup is unambiguous and the target repo is different from the source
// repo, insert a cross-repo Calls edge from the global source to the
// global target in the other repo.
for (global_source, ref_name) in self.repo_index(id).external_calls() {
    if let Some(repos) = self.symbol_to_repos.get(ref_name) {
        if repos.len() != 1 { continue; }            // ambiguous: skip
        let target_repo = &repos[0];
        if target_repo == id { continue; }            // already global: skip
        let global_target = self.global_id(target_repo, ...).as_str().to_string();
        self.backend.upsert_edge(GraphEdge::new(
            EdgeType::Calls,
            global_source,
            global_target,
        ))?;
    }
}
```

`external_calls()` is a new accessor on `RepoIndex` that returns `(global_source_id, target_name)` for every `Calls` edge where the target is not a function defined in this repo. The implementation walks `repo_index.edges()`, checks each `Calls` edge's target against `repo_index.nodes()` to determine "defined locally" vs "imported reference", and emits the tuple only for imports.

After Pass B, cross-repo `Calls` traversal works. `get_cross_repo_blast_radius("auth", "1..3")` returns nodes bucketed into `shared`.

### Algorithm invariants

- **Pass A always projects every per-repo edge** (no filtering). The global backend accumulates the full per-repo edge set, re-keyed.
- **Pass B never creates an edge when the target name is ambiguous** (`symbol_to_repos.get(name)` returns ≥2 entries). Logged at debug level.
- **Pass B never creates an edge when the target name is not in `symbol_to_repos`.** Logged at debug level.
- **Pass B never creates an edge when the target repo is the same as the source repo.** (Defensive; shouldn't happen if `external_calls` filters correctly.)
- **Edges are written via `upsert_edge` with the existing `Calls` edge type.** No new edge types introduced.
- **Pass A's projected intra-repo edges coexist with Pass B's cross-repo edges.** A single `auth` node may have one `Calls` edge to a local reference (Pass A) and another `Calls` edge to the global node in `shared` (Pass B). For traversal purposes both edges are valid; the implementation plan must decide traversal ordering (the spec recommends: traverse all outgoing `Calls` edges from a node, regardless of source).

### Why this works

The existing `GraphBackend::traverse(..., EdgeType::Calls, ...)` already walks `Calls` edges regardless of source/target repo (proven by `src/mcp/federation_tools.rs`'s unit tests, which manually insert both intra- and cross-repo `Calls` edges and assert the traversal buckets correctly). The missing piece was the edges themselves — Pass A adds intra-repo edges, Pass B adds cross-repo edges. After both, the existing traversal logic has real edges to walk.

### Failure modes

- **`project_repo` crashes mid-projection**: per-repo state is already persisted (bincode under `data_dir/<id>/`); the global backend's `save_to_disk_sync` (called per `upsert_edge` and `upsert_node`) means a federation crash loses at most the in-flight batch. Re-running projection is idempotent because per-repo nodes are upserted with deterministic global ids, and `upsert_edge` deduplicates by `(source_id, target_id, edge_type)`.
- **`symbol_to_repos` is stale at Pass B time**: it's rebuilt on every `add_repo` before `project_repo` runs, so when `project_repo(id)` runs, all OTHER repos are already in the index. The repo being projected (`id`) is added during projection itself — we resolve against OTHER repos' symbols, not our own.
- **`RepoIndex::edges()` returns stale data**: each `RepoIndex::index()` rebuilds its `GraphDatabase` from disk; the bincode file under `data_dir/<id>/graph.bin` is the source of truth. If a project_repo runs while a repo's bincode is being rewritten by an in-flight indexing, we'd see partial edges. Same hazard exists today for per-repo nodes; we don't add new exposure.

---

## Components

### D fixture — `tests/federation_cross_repo_e2e.rs`

**Fixture builder** (`write_three_dependent_crates`, follows the existing pattern in `tests/federation_integration.rs::write_tiny_rust_crate` and `init_bare_git_repo`):

```
{tmp}/
├── shared/
│   ├── Cargo.toml              [package] name = "shared"
│   └── src/lib.rs              pub fn verify_token(&str) -> bool
│                               pub fn hash(&str) -> u64
│                                   { inner_hash(s) }   // non-leaf so blast-radius test has results
│                               pub fn inner_hash(&str) -> u64 { ... }
├── db-client/
│   ├── Cargo.toml              [package] name = "db-client"
│   │                           [dependencies] shared = { path = "../shared" }
│   └── src/lib.rs              use shared::verify_token;
│                               pub fn connect() -> bool { shared::verify_token("...") }
│                               pub fn verify_token(&str) -> bool { false }   // duplicate symbol
└── auth-svc/
    ├── Cargo.toml              [package] name = "auth-svc"
    │                           [dependencies] shared = { path = "../shared" }
    └── src/lib.rs              use shared::hash;
                                pub fn auth(s: &str) -> bool { shared::hash(s) > 0 }
```

The duplicate `verify_token` in `db-client` exists solely to exercise `AmbiguousSymbol` resolution. `hash` calls `inner_hash` so the blast-radius assertion has a non-empty traversal.

**Tests** — eight `#[tokio::test]` functions, each builds the fixture (or shares one via `OnceCell` if profiling shows setup dominates), loads it via `load_federation` (`tests/federation_integration.rs` already shows the pattern with 5 tempdir repos), and asserts on `FederatedIndex` methods + tool return shapes:

| Test | Federation contract proven |
|---|---|
| `cross_repo_resolver_unique_owner` | `resolve_symbol("hash")` → `Ok(RepoId("shared"))`. Sole owner case. |
| `cross_repo_resolver_ambiguous` | `resolve_symbol("verify_token")` → `AmbiguousSymbol(["shared", "db-client"])`. Multiple owners case. |
| `cross_repo_search_org_finds_shared_concepts` | `search_org("verify", 10)` returns ≥2 hits with ≥2 distinct `repo_id`s. Cross-repo indexing + substring search work. |
| `cross_repo_calls_edge_resolves_to_global_node` | `find_path("auth-svc:Function:src/lib.rs:auth", "shared:Function:src/lib.rs:hash")` returns a non-empty path. **Proves the federation engine change actually produced a cross-repo `Calls` edge.** Without this edge, `auth` cannot reach `shared::hash`. |
| `cross_repo_blast_radius_within_owning_repo` | `get_cross_repo_blast_radius("hash", "1..3")` returns `{by_repo: {"shared": [<inner_hash_node>]}, total_count: 1, truncated: false}`. Intra-repo outgoing `Calls` traversal correct (regression check for the original behavior). |
| `cross_repo_blast_radius_walks_into_other_repos` | `get_cross_repo_blast_radius("auth", "1..3")` returns `{by_repo: {"shared": [<hash_node>, <inner_hash_node>]}, total_count: 2, truncated: false}`. **The headline cross-repo call-chain test:** the seed is in `auth-svc`, the result buckets nodes into `shared`, proving the traversal walked the new cross-repo `Calls` edge and continued traversing `Calls` from the other repo. |
| `cross_repo_blast_radius_ambiguous_for_tool` | `get_cross_repo_blast_radius("verify_token", "1..3")` returns the JSON `{error: "ambiguous_symbol", candidates: [...], message: "..."}` payload. Tool surface honors `AmbiguousSymbol`. |
| `cross_repo_blast_radius_not_found` | `get_cross_repo_blast_radius("does_not_exist", "1..3")` returns `NotFound: symbol does_not_exist not found in any repo`. `NotFound` surfaces correctly. |

That's 8 tests. The two new ones (`cross_repo_calls_edge_resolves_to_global_node`, `cross_repo_blast_radius_walks_into_other_repos`) are the load-bearing tests for the federation engine change. The other 6 are the existing federation contract coverage.

**Setup reuse:** if profiling shows the 3-crate tempdir build + git init dominates the test runtime, wrap `write_three_dependent_crates` behind a `tokio::sync::OnceCell` shared across the 8 tests. Otherwise, each test builds its own tempdir (matches the existing pattern in `federation_integration.rs`).

### A fixture — extensions to `tests/e2e/federation_e2e.sh`

**Existing script stays.** The 3 famous-repo assertions (`list_repos == 3`, `health.total_repos == 3`, `search_org("serialize") ≥ 1`) are unchanged.

**Additions** between existing line 43 (the heredoc closing the `repos.yaml`) and line 45 (the "Starting lain server" line):

```bash
# Clone OpenTelemetry Demo (Astronomy Shop) — 12 polyglot microservices
git clone --depth 1 https://github.com/open-telemetry/opentelemetry-demo.git \
    "${WORKDIR}/opentelemetry-demo" \
    || { echo "ERROR: failed to clone opentelemetry-demo"; exit 1; }

# Append the 12 service subdirs as WorkspaceDirSource entries
OTEL_DIR="${WORKDIR}/opentelemetry-demo/src"
for svc in adservice cartservice checkoutservice currencyservice \
           emailservice frontend loadgenerator paymentservice \
           productcatalogservice recommendationservice shippingservice \
           accountingservice; do
    if [[ -d "${OTEL_DIR}/${svc}" ]]; then
        cat >> "${WORKDIR}/repos.yaml" <<EOF
  - id: otel-${svc}
    source:
      type: workspace_dir
      path: ${OTEL_DIR}/${svc}
EOF
    else
        echo "WARN: otel-${svc} not present in opentelemetry-demo/src — skipping" >&2
    fi
done
```

The `[[ -d ... ]]` guard means upstream adding/removing a service doesn't break the script — it just skips. This handles spec criterion 6 of the definition-of-done.

**New assertion block** after the existing 3 assertions:

```bash
# Total repos: 3 famous + ≥9 OTel = ≥12 (tolerates up to 3 upstream service renames/removals)
total_repos="$(printf '%s' "${health_text}" | python3 -c \
  'import json,sys; print(json.load(sys.stdin)["total_repos"])')"
if [[ "${total_repos}" -lt 12 ]]; then
    echo "ERROR: total_repos=${total_repos}, expected >= 12 (3 famous + >=9 otel)" >&2
    echo "    Payload: ${health_text}" >&2
    exit 1
fi
echo "    get_federation_health.total_repos = ${total_repos}"

# Wait until at least 8 OTel repos are ready (tolerate up to 4 degraded)
for i in $(seq 1 150); do
    health_text="$(call_tool "get_federation_health" '{}' | mcp_text)"
    ready_count="$(printf '%s' "${health_text}" | python3 -c \
      'import json,sys; print(json.load(sys.stdin).get("ready", 0))')"
    [[ "${ready_count}" -ge 8 ]] && break
    sleep 2
done
if [[ "${ready_count}" -lt 8 ]]; then
    echo "ERROR: only ${ready_count} repos ready after 300s, expected >= 8" >&2
    exit 1
fi
echo "    ready_count = ${ready_count}"

# search_org finds shared domain concepts in ≥2 distinct repos
for query in Product Money; do
    hits="$(call_tool "search_org" "{\"query\":\"${query}\",\"limit\":20}" | mcp_text)"
    distinct="$(printf '%s' "${hits}" | python3 -c \
      'import json,sys; d=json.load(sys.stdin); print(len({h["repo_id"] for h in d}))')"
    if [[ "${distinct}" -lt 2 ]]; then
        echo "ERROR: search_org('${query}') only hit ${distinct} repos, expected >= 2" >&2
        echo "    Payload: ${hits}" >&2
        exit 1
    fi
    echo "    search_org('${query}'): ${distinct} distinct repos"
done

# get_repo_info returns valid shape for a known OTel service
otel_info="$(call_tool "get_repo_info" '{"id":"otel-productcatalogservice"}' | mcp_text)"
otel_id="$(printf '%s' "${otel_info}" | python3 -c \
  'import json,sys; print(json.load(sys.stdin).get("id",""))')"
if [[ "${otel_id}" != "otel-productcatalogservice" ]]; then
    echo "ERROR: get_repo_info('otel-productcatalogservice') returned id='${otel_id}'" >&2
    echo "    Payload: ${otel_info}" >&2
    exit 1
fi
echo "    get_repo_info('otel-productcatalogservice'): ok"

# get_cross_repo_blast_radius returns valid shape
blast="$(call_tool "get_cross_repo_blast_radius" \
  '{"symbol":"GetProduct","depth":"1..3"}' | mcp_text)"
has_by_repo="$(printf '%s' "${blast}" | python3 -c \
  'import json,sys; print("by_repo" in json.load(sys.stdin))')"
if [[ "${has_by_repo}" != "True" ]]; then
    echo "ERROR: get_cross_repo_blast_radius('GetProduct','1..3') missing 'by_repo' key" >&2
    echo "    Payload: ${blast}" >&2
    exit 1
fi
echo "    get_cross_repo_blast_radius('GetProduct','1..3'): ok"

echo "==> E2E PASSED"
```

`GetProduct` is a documented gRPC method on `productcatalogservice` (Go) per the OTel demo's `protos/demo.proto`. If a future OTel demo refactor renames it, the test fails clearly with the actual response payload visible — the assertion deliberately echoes the payload so debugging is one log read away.

---

## Data flow

### D fixture

```
1. tempdir::tempdir() → root path
2. write_three_dependent_crates(root) → 3 dirs, each with Cargo.toml + src/lib.rs
3. init_bare_git_repo(each_dir)         → git2::Repository::init (from federation_integration.rs pattern)
4. write YAML → {root}/repos.yaml       → 3 workspace_dir entries pointing at the 3 dirs
5. load_federation(yaml_path)           → FederatedIndex built (existing federation code path; the new project_repo pass runs here)
6. fed.resolve_symbol(name) / search_org / find_path / get_cross_repo_blast_radius → assert
7. tempdir dropped at end of test (each test owns its tempdir)
```

### A fixture

```
1. WORKDIR = mktemp -d
2. Write initial repos.yaml with 3 shallow_clone entries (existing)
3. NEW: git clone --depth 1 opentelemetry-demo
4. NEW: append 12 workspace_dir entries to repos.yaml
5. lain server --config WORKDIR/repos.yaml --transport http --port $PORT &
6. Poll /mcp get_federation_health until 200 (existing, 120s timeout)
7. Existing 3 famous-repo assertions pass (list_repos=3, health.total_repos=3, search_org("serialize") ≥1)
8. NEW: assert total_repos >= 12 + poll get_federation_health.ready >= 8 (5 min timeout)
9. NEW: 4 tool assertions (search_org "Product", search_org "Money", get_repo_info, get_cross_repo_blast_radius)
10. kill lain server; rm -rf WORKDIR
```

---

## Error handling

### D fixture

- **Setup failure** (git init, fs write): test panics with the underlying IO error. Matches the existing `federation_integration.rs` style.
- **Indexing failure** (`load_federation` returns Err): test asserts the `Ok` arm; an `Err` triggers a `panic!("load_federation failed: {e}")`.
- **Wrong symbol picked by resolver**: the assertion message echoes the actual resolver output (`AmbiguousSymbol(candidates: [...])` etc.) so the test failure is one log read away from diagnosis.
- **LSP not on PATH**: tree-sitter-only mode (per existing pattern); tests still pass because the `FederatedIndex` indexes files and modules regardless of LSP hydration.

### A fixture

- **git clone failure**: script exits 1 at the clone step with stderr from `git` echoed.
- **OTel service subdir missing**: warning printed, service skipped from `repos.yaml`. The `total_repos >= 12` assertion (3 famous + ≥9 OTel) tolerates up to 3 upstream service renames/removals before failing. The assertion message echoes the actual count and payload so the operator can decide whether to lower the threshold further or investigate upstream.
- **LSP missing for an OTel service**: that service degrades to `degraded`/`unavailable` health; `ready >= 8` tolerates this.
- **Symbol not picked up** (e.g., `GetProduct` renamed upstream): assertion prints the actual MCP response payload (`echo "    Payload: ${blast}" >&2`). Operator changes the test symbol.

---

## Testing

This spec is itself a test artifact. Meta-validation:

- **D runs on every PR.** A green per-PR run is the contract.
- **A runs on nightly.** A green nightly run is the cross-OSS validation.
- **No tests test the tests.** We trust that the assertions fail loudly when the substrate breaks (each assertion echoes the actual MCP/tool output).

**Out of scope for this spec:**
- Mutation testing of the new assertions (e.g., `cargo-mutants`). The assertions are short and read what the federation machinery actually returns; if the machinery regresses, the assertion message is clear.
- Property-based / fuzz testing of the resolver. The 3-crate fixture deterministically exercises unique-owner / ambiguous / not-found cases; that's sufficient for the per-PR contract.

---

## Backward compatibility

- **Production code change is additive only.** `project_repo` gains a new pass; its existing behavior (node projection, intra-repo edge projection, `CrossRepoSameSymbol` matching) is unchanged. Per-repo `GraphDatabase` files are not migrated — they're left as-is. The next `add_repo` call rebuilds them.
- **Existing federation tools return the same shapes.** `get_cross_repo_blast_radius` now may include nodes from multiple repos in `by_repo` where it previously didn't (when a seed has cross-repo `Calls` edges). Clients that only read the bucket counts see richer data; clients that read individual node ids see ids they couldn't have seen before (with global id format `{repo_id}:{NodeType}:{path}:{name}`). Both are forward-compatible — old clients just see smaller buckets.
- **No `repos.yaml` schema changes.** The A fixture uses the existing `workspace_dir` source kind.
- **No new CI workflow.** The Rust integration test slots into the existing `cargo test` matrix; the e2e extensions ride the existing nightly + manual triggers.
- **The existing `tests/federation_e2e.sh` assertions still pass.** The OTel additions are additive; the 3 famous-repo assertions are unchanged.
- **Existing per-PR tests still pass.** The engine change adds edges but doesn't remove any. `tests/federation_integration.rs` and `tests/federation_benchmark.rs` are unaffected by the projection change (the benchmark uses `federation_index_for_test` which inserts directly into the backend, bypassing `project_repo`).

---

## Migration / rollout

Two PRs, in order:

**PR 1 — Engine change (must land first):**
- `src/federation/federated_index.rs`: Pass A (project per-repo edges) and Pass B (cross-repo `Calls` resolution) added to `project_repo` (described above).
- A new accessor on `RepoIndex` (`external_calls()` returning `(global_source_id, target_name)` tuples) if needed.
- Existing federation tests must still pass. `tests/federation_integration.rs` and `tests/federation_benchmark.rs` cover this.
- New test fixtures (PR 2) are NOT in this PR — we land the engine change with the existing test surface so the diff is reviewable in isolation.

**PR 2 — Test fixtures (lands immediately after PR 1):**
- `tests/federation_cross_repo_e2e.rs` (new file, gated behind `--features test-utils` like the existing benchmark file).
- `tests/e2e/federation_e2e.sh` extensions + docs updates (`docs/FEDERATION.md` "Smoke test" and "Performance" sections, README one-liner).

If the OTel demo clone is unreliable in the nightly environment, the script can fall back to a pre-cloned fixture in CI cache (future work, not in this spec).

---

## Definition of done

1. `src/federation/federated_index.rs::project_repo` projects every per-repo edge into the global backend (Pass A).
2. `src/federation/federated_index.rs::project_repo` produces cross-repo `Calls` edges for unambiguous per-repo `Calls` references to functions in other repos (Pass B).
3. `tests/federation_cross_repo_e2e.rs` exists; builds with `--features test-utils`; all 8 tests pass on a clean clone.
4. `tests/e2e/federation_e2e.sh` extended; existing 3 assertions still pass; new OTel assertions pass against a freshly cloned OTel demo at upstream HEAD.
5. `cargo test --test federation_cross_repo_e2e` passes in CI on a clean PR branch.
6. `tests/e2e/federation_e2e.sh` (extended) passes in the nightly workflow.
7. `docs/FEDERATION.md` smoke-test and performance sections updated with pointers to both fixtures.
8. The OTel service subdirectory list in the script's `repos.yaml` matches what's actually in `opentelemetry-demo/src/` at HEAD — if upstream adds/removes a service, the script handles it gracefully (skips missing dirs, asserts against a tolerant threshold).
9. The existing `tests/federation_integration.rs` and `tests/federation_benchmark.rs` still pass after PR 1 lands (no regression).

## Open questions (for the implementation plan, not blockers)

- Should the per-PR D test share a `OnceCell` for setup, or each test own its tempdir? Profile first, decide second.
- Should `GetProduct` be the A fixture's blast-radius seed, or is there a more stable symbol across OTel demo versions? If `GetProduct` is renamed upstream within 6 months, the assertion needs updating — pick a candidate that has been stable the longest (e.g., `PlaceOrder` on `checkoutservice`).
- Should the A fixture also assert `get_repo_info` returns a `health` field that's one of the documented values? Add to the implementation plan if cheap.

---

## Status

Brainstorming complete. Spec expanded twice after user pushback: first to include the federation engine change (Pass B for cross-repo `Calls` resolution), then to acknowledge that `project_repo` doesn't even copy per-repo edges today (Pass A added). The full engine change is now two passes inside `project_repo`: A) project per-repo edges, B) resolve cross-repo `Calls` edges. Test fixtures and migration plan updated. Awaiting user review of the rewritten spec before invoking writing-plans.