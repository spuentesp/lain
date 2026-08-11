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

**But the federation never actually creates such edges in production.** Looking at `src/federation/federated_index.rs::project_repo`, the only cross-repo edges it inserts are `CrossRepoSameSymbol` (via `find_cross_repo_matches`). Per-repo `Calls` edges come from each `RepoIndex`'s `GraphDatabase`, where they point to local reference / import names — never to a global node in another repo. So when an agent asks `get_cross_repo_blast_radius` for a function in repo A that genuinely calls a function in repo B, the result is empty for B's bucket: federation knows the symbols exist in both repos (via `CrossRepoSameSymbol`) but cannot trace the call chain across the boundary.

That's a feature gap, not just a test gap. To make "analyze interconnectedly" real at the call-chain level, the federation must produce cross-repo `Calls` edges at projection time. This spec does both: the engine change that produces the edges, and the test fixtures that prove they exist and the traversal walks them.

The upcoming Workspaces feature (`docs/superpowers/specs/2026-08-11-lain-workspaces-design.md`, forthcoming) sits directly on the federation substrate — a workspace is a named subset of `repos.yaml`'s repos that the federation engine indexes together. Before we build that, we need evidence the substrate actually produces and traverses cross-repo call chains.

### Two fixtures

- **Fixture D:** a deterministic Rust integration test that builds 3 dependent tempdir crates and asserts the federation's semantic contracts end-to-end. Runs on every PR, no network.
- **Fixture A:** an extension to the existing `tests/e2e/federation_e2e.sh` that adds the OpenTelemetry Astronomy Shop (12 polyglot microservices via `WorkspaceDirSource`) and asserts cross-repo tool behavior against real OSS code. Runs on nightly / manual, network-dependent.

Together they turn the federation's correctness claims from "trust the unit tests" into "verified against representative code, including cross-repo `Calls` edges."

---

## Goals

1. **Federation engine change:** `project_repo` produces cross-repo `Calls` edges in the global petgraph by resolving per-repo `Calls` references to global nodes in other repos (via the symbol index). Existing traversal logic (proven cross-repo correct by the unit tests) now has real cross-repo edges to walk.
2. **Per-PR guarantee** that federation's semantic contracts hold: `resolve_symbol` returns the right repo for unique / ambiguous / not-found inputs, cross-repo `Calls` edges exist after indexing, `get_cross_repo_blast_radius` walks them across repo boundaries and buckets by repo, `search_org` finds shared concepts across repos.
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
  └── project_repo(id) — now ALSO resolves per-repo Calls targets via the
                          symbol_to_repos index and inserts cross-repo Calls
                          edges in the global petgraph where unambiguous

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

The production change is small and targeted: a new pass inside `project_repo` that takes per-repo `Calls` edges whose target is an unresolved name reference, looks the name up in `symbol_to_repos`, and — when the lookup is unambiguous and the target repo is different from the source repo — replaces the local target id with the global id. Ambiguous names and not-found names leave the original intra-repo edge in place (we don't fabricate cross-repo calls out of fuzzy matches).

---

## Production code change — `src/federation/federated_index.rs::project_repo`

### Current behavior

`project_repo(id)` does (today):

1. Re-keys per-repo nodes from `(NodeType, path, name)` to global ids (`{id}:{NodeType}:{path}:{name}`) and upserts them into the `GraphBackend`.
2. Re-keys and upserts per-repo edges (`Calls`, `Contains`, `Defines`, `Imports`, etc.) — including per-repo `Calls` edges whose target is a local reference name (a node that exists in this repo's `GraphDatabase` because the import is recorded as a placeholder, not because the function is defined here).
3. Iterates over every other repo's nodes and runs `find_cross_repo_matches` against the projected repo's signatures, adding `CrossRepoSameSymbol` edges for matches above threshold.

The per-repo `Calls` edges stay intra-repo even when the underlying call is to a function in another repo — the edge's target is the local reference placeholder, not the global node.

### New behavior (this spec)

After step 2 and before step 3, insert a new step 2.5:

```rust
// NEW: resolve per-repo Calls edges whose target is a reference/placeholder
// against the symbol_to_repos index (which already reflects every OTHER repo
// because they were all added via add_repo before project_repo runs).
for edge in repo_index.calls_edges() {
    if edge.source_id_starts_with(&repo_id)
        && !edge.target_id_starts_with(&repo_id)
        && let Some(global_target) = symbol_to_repos
            .get(&edge.target_name())
            .filter(|repos| repos.len() == 1)
    {
        // Unambiguous external owner — rewrite the edge to point at the
        // global id of the function in the other repo.
        backend.upsert_edge(GraphEdge::new(
            EdgeType::Calls,
            global_source_id,
            global_target_id,
        ))?;
    }
    // else: ambiguous (skip), not-found (leave intra-repo), or already global (skip)
}
```

The key correctness property: **we only fabricate a cross-repo `Calls` edge when the resolver is unambiguous** (single owner in `symbol_to_repos`). If a name is owned by ≥2 repos, we leave the intra-repo edge alone rather than guessing which owner the call meant. This is conservative on purpose — a wrong cross-repo edge would produce false positives in `get_cross_repo_blast_radius` results, which is worse than no cross-repo info.

### Algorithm invariants

- **No new edges created when the target name resolves to a node in the same repo.** Intra-repo calls stay intra-repo.
- **No new edges created when the target name is ambiguous** (`symbol_to_repos.get(name)` returns ≥2 entries). Logged at debug level.
- **No new edges created when the target name is not in `symbol_to_repos`** at all. Logged at debug level.
- **Edges are written via `upsert_edge` with the existing `Calls` edge type.** No new edge types introduced.
- **Existing intra-repo `Calls` edges are not removed.** Both edges can coexist: the local reference placeholder remains as a `Calls` target (intra-repo), and a new `Calls` edge from the same source to the resolved global node is added (cross-repo). For traversal purposes the global one wins because it's what `find_path` and `get_cross_repo_blast_radius` see first (depending on graph order; the implementation plan must pin this down).

### Why this works

The existing `GraphBackend::traverse(..., EdgeType::Calls, ...)` already walks `Calls` edges regardless of source/target repo (proven by `src/mcp/federation_tools.rs`'s unit tests, which manually insert cross-repo `Calls` edges and assert the traversal buckets correctly). The missing piece was the edges themselves. This spec adds them at the right point in the projection pipeline so the existing traversal logic has real cross-repo edges to walk.

### Failure modes

- **`project_repo` crashes mid-resolution**: per-repo state is already persisted (bincode under `data_dir/<id>/`); the global backend's `save_to_disk_sync` (called per `upsert_edge`) means a federation crash loses at most the in-flight batch. Re-running projection is idempotent because per-repo nodes are upserted with deterministic global ids.
- **`symbol_to_repos` is stale at projection time**: it's rebuilt on every `add_repo` before `project_repo` runs, so when `project_repo(id)` runs, all OTHER repos are already in the index. The repo being projected (`id`) is added during projection itself — we resolve against OTHER repos' symbols, not our own.

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
- `src/federation/federated_index.rs`: new pass in `project_repo` (described above).
- Existing federation tests must still pass.
- New test fixtures (PR 2) are NOT in this PR — we land the engine change with the existing test surface so the diff is reviewable in isolation.

**PR 2 — Test fixtures (lands immediately after PR 1):**
- `tests/federation_cross_repo_e2e.rs` (new file, gated behind `--features test-utils` like the existing benchmark file).
- `tests/e2e/federation_e2e.sh` extensions + docs updates (`docs/FEDERATION.md` "Smoke test" and "Performance" sections, README one-liner).

If the OTel demo clone is unreliable in the nightly environment, the script can fall back to a pre-cloned fixture in CI cache (future work, not in this spec).

---

## Definition of done

1. `src/federation/federated_index.rs::project_repo` produces cross-repo `Calls` edges for unambiguous per-repo `Calls` references to functions in other repos.
2. `tests/federation_cross_repo_e2e.rs` exists; builds with `--features test-utils`; all 8 tests pass on a clean clone.
3. `tests/e2e/federation_e2e.sh` extended; existing 3 assertions still pass; new OTel assertions pass against a freshly cloned OTel demo at upstream HEAD.
4. `cargo test --test federation_cross_repo_e2e` passes in CI on a clean PR branch.
5. `tests/e2e/federation_e2e.sh` (extended) passes in the nightly workflow.
6. `docs/FEDERATION.md` smoke-test and performance sections updated with pointers to both fixtures.
7. The OTel service subdirectory list in the script's `repos.yaml` matches what's actually in `opentelemetry-demo/src/` at HEAD — if upstream adds/removes a service, the script handles it gracefully (skips missing dirs, asserts against a tolerant threshold).
8. The existing `tests/federation_integration.rs` and `tests/federation_benchmark.rs` still pass after PR 1 lands (no regression).

## Open questions (for the implementation plan, not blockers)

- Should the per-PR D test share a `OnceCell` for setup, or each test own its tempdir? Profile first, decide second.
- Should `GetProduct` be the A fixture's blast-radius seed, or is there a more stable symbol across OTel demo versions? If `GetProduct` is renamed upstream within 6 months, the assertion needs updating — pick a candidate that has been stable the longest (e.g., `PlaceOrder` on `checkoutservice`).
- Should the A fixture also assert `get_repo_info` returns a `health` field that's one of the documented values? Add to the implementation plan if cheap.

---

## Status

Brainstorming complete. Sections 1–3 approved. Spec expanded to include the federation engine change after user pushback on the cross-repo `Calls` semantic. Awaiting user review of the rewritten spec before invoking writing-plans.