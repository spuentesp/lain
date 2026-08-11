# Federation Test Gap Fix — Design

**Status:** Draft (brainstorming complete, awaiting user review)
**Date:** 2026-08-11
**Sub-project:** standalone, prerequisite for the Lain Workspaces feature
**Depends on:** nothing in mainline; enables confidence in the federation substrate the Workspaces feature will build on
**Enables:** `2026-08-11-lain-workspaces-design.md` (next spec), and any future feature that asserts on cross-repo reasoning

---

## Context and motivation

Lain's federation mode (`lain server --config repos.yaml`) answers org-wide structural questions across N repos. The engineering behind it (`FederatedIndex`, `GraphBackend`, `find_cross_repo_matches`, `resolve_symbol`, `get_cross_repo_blast_radius`, `search_org`) is non-trivial and correct only if the contracts hold. Today the test coverage has two gaps that block landing higher-level features (notably Workspaces) on solid ground:

1. **No real-code proof of cross-repo reasoning.** `tests/federation_benchmark.rs::small_fixture_blast_radius_under_100ms_p99` exercises the latency budget on a synthetic 50K-node chain. It proves the `GraphBackend::traverse` hot path, not the semantic contracts (`resolve_symbol` returns the right repo, `AmbiguousSymbol` surfaces when it should, `search_org` finds shared concepts, the `by_repo` bucketing is correct). The chain is constructed in-memory and is intra-repo; the "cross-repo" in the tool name is just "the seed can come from any repo."

2. **No polyglot real-OSS e2e.** `tests/e2e/federation_e2e.sh` runs against three famous independent Rust crates (rayon, ripgrep, serde). Those projects don't call each other, so the federation is just three unrelated subgraphs. The e2e asserts `list_repos`, `get_federation_health`, and `search_org("serialize") ≥ 1 hit`. It does not call `get_cross_repo_blast_radius`, `get_repo_info`, or any cross-repo reasoning tool. It does not exercise polyglot indexing (12+ languages).

The upcoming Workspaces feature (`docs/superpowers/specs/2026-08-11-lain-workspaces-design.md`, forthcoming) will sit directly on the federation substrate — a workspace is a named subset of `repos.yaml`'s repos that the federation engine indexes together. Before we build that, we need evidence the substrate works against realistic code. This spec closes the gap with two fixtures:

- **Fixture D:** a deterministic Rust integration test that builds 3 dependent tempdir crates and asserts the federation's semantic contracts end-to-end. Runs on every PR, no network.
- **Fixture A:** an extension to the existing `tests/e2e/federation_e2e.sh` that adds the OpenTelemetry Astronomy Shop (12 polyglot microservices via `WorkspaceDirSource`) and asserts cross-repo tool behavior against real OSS code. Runs on nightly / manual, network-dependent.

Together they turn the federation's correctness claims from "trust the unit tests" into "verified against representative code."

---

## Goals

1. **Per-PR guarantee** that federation's semantic contracts hold: `resolve_symbol` returns the right repo for unique / ambiguous / not-found inputs, `get_cross_repo_blast_radius` walks outgoing `Calls` correctly and buckets by repo, `search_org` finds shared concepts across repos.
2. **Nightly guarantee** that federation works against real polyglot OSS code: ≥9 OTel services indexed (out of the 12 service subdirs at upstream HEAD) across 6+ languages, `search_org` finds shared domain concepts (`Product`, `Money`) in ≥2 repos, `get_repo_info` returns valid shape for a known OTel service, `get_cross_repo_blast_radius` returns valid shape against a documented gRPC method.
3. **No regression** to the existing per-PR test matrix or the existing e2e behavior — the existing 3-repo e2e assertions stay green and become the "famous independent projects still index" baseline.
4. **CI-budget-bounded:** D adds <30s to per-PR. A extends the existing nightly e2e (no new CI workflow).

## Non-goals

- **Cross-repo `Calls` traversal.** Federation's `GraphBackend::traverse` walks only `Calls` edges (intra-repo). Cross-repo links are `CrossRepoSameSymbol` edges, which `traverse` does not walk. So no fixture in this spec asserts that a `get_cross_repo_blast_radius` call returns nodes bucketed to multiple repos. That semantic doesn't exist in federation today. If we want it later, that's a federation-engine change, not a test change.
- **Service identity, multi-tenancy, redundancy detection, UI, live PR overlay.** All explicitly deferred sub-projects of the federation vision (see `docs/superpowers/specs/2026-08-07-federated-indexer-design.md`). Untouched.
- **Replacing or rewriting existing federation tests.** `tests/federation_integration.rs` and `tests/federation_benchmark.rs` stay. This spec adds new artifacts.
- **Verifying per-language LSP correctness for all 12 OTel services.** The nightly A fixture uses `ready >= 8` as the threshold, tolerating up to 4 services being degraded because their language server isn't on the CI image. Stricter per-language validation is out of scope.
- **The Workspaces feature itself.** That's the next spec.

---

## Architecture

Two new artifacts land; nothing in production code changes.

```
Per-PR:
  tests/federation_cross_repo_e2e.rs       (NEW)
  ├── write_three_dependent_crates(root)  →  helper, builds the 3 tempdir crates
  ├── cross_repo_resolver_unique_owner    →  resolve_symbol returns Ok
  ├── cross_repo_resolver_ambiguous       →  resolve_symbol returns AmbiguousSymbol
  ├── cross_repo_search_org_finds_shared_concepts →  search_org hits ≥2 repos
  ├── cross_repo_blast_radius_within_owning_repo  →  outgoing Calls traversal correct
  ├── cross_repo_blast_radius_ambiguous_for_tool  →  tool surfaces AmbiguousSymbol
  └── cross_repo_blast_radius_not_found   →  tool surfaces NotFound

Nightly / manual:
  tests/e2e/federation_e2e.sh              (EXTENDED, not rewritten)
  ├── existing 3 famous repos (rayon, ripgrep, serde)  ← unchanged assertions
  ├── NEW: git clone https://github.com/open-telemetry/opentelemetry-demo.git
  ├── NEW: 12 service subdirs registered as workspace_dir entries in repos.yaml
  ├── NEW: list_repos ≥ 15 assertion
  ├── NEW: get_federation_health.ready ≥ 8 wait + assertion
  ├── NEW: search_org("Product") distinct_repos ≥ 2
  ├── NEW: search_org("Money") distinct_repos ≥ 2
  ├── NEW: get_repo_info("otel-productcatalogservice") returns valid shape
  └── NEW: get_cross_repo_blast_radius("GetProduct", "1..3") returns valid shape
```

No production code (src/, src/federation/, src/mcp/) is modified. Both fixtures are pure additions to the test surface.

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

**Tests** — six `#[tokio::test]` functions, each builds the fixture (or shares one via `OnceCell` if profiling shows setup dominates), loads it via `load_federation` (`tests/federation_integration.rs` already shows the pattern with 5 tempdir repos), and asserts on `FederatedIndex` methods + tool return shapes:

| Test | Federation contract proven |
|---|---|
| `cross_repo_resolver_unique_owner` | `resolve_symbol("hash")` → `Ok(RepoId("shared"))`. Sole owner case. |
| `cross_repo_resolver_ambiguous` | `resolve_symbol("verify_token")` → `AmbiguousSymbol(["shared", "db-client"])`. Multiple owners case. |
| `cross_repo_search_org_finds_shared_concepts` | `search_org("verify", 10)` returns ≥2 hits with ≥2 distinct `repo_id`s. Cross-repo indexing + substring search work. |
| `cross_repo_blast_radius_within_owning_repo` | `get_cross_repo_blast_radius("hash", "1..3")` returns `{by_repo: {"shared": [<inner_hash_node>]}, total_count: 1, truncated: false}`. Outgoing `Calls` traversal + bucketing correct. |
| `cross_repo_blast_radius_ambiguous_for_tool` | `get_cross_repo_blast_radius("verify_token", "1..3")` returns the JSON `{error: "ambiguous_symbol", candidates: [...], message: "..."}` payload. Tool surface honors `AmbiguousSymbol`. |
| `cross_repo_blast_radius_not_found` | `get_cross_repo_blast_radius("does_not_exist", "1..3")` returns `NotFound: symbol does_not_exist not found in any repo`. `NotFound` surfaces correctly. |

**Setup reuse:** if profiling shows the 3-crate tempdir build + git init dominates the test runtime, wrap `write_three_dependent_crates` behind a `tokio::sync::OnceCell` shared across the 6 tests. Otherwise, each test builds its own tempdir (matches the existing pattern in `federation_integration.rs`).

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
5. load_federation(yaml_path)           → FederatedIndex built (existing federation code path)
6. fed.resolve_symbol(name) / search_org / get_cross_repo_blast_radius → assert
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
7. Existing 3 assertions pass
8. NEW: poll get_federation_health.ready >= 8 (5 min timeout)
9. NEW: 4 more tool assertions
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

- **No production code changes.** Federation's API surface (`FederatedIndex` methods, MCP tool return shapes) is exercised as-is by the new fixtures.
- **No `repos.yaml` schema changes.** The A fixture uses the existing `workspace_dir` source kind.
- **No new CI workflow.** The Rust integration test slots into the existing `cargo test` matrix; the e2e extensions ride the existing nightly + manual triggers.
- **The existing `tests/federation_e2e.sh` assertions still pass.** The OTel additions are additive; the 3 famous-repo assertions are unchanged.

---

## Migration / rollout

Single PR. Land both artifacts in one commit (or one PR with two commits for reviewability):
1. Commit 1: `tests/federation_cross_repo_e2e.rs` (new file, gated behind `--features test-utils` like the existing benchmark file).
2. Commit 2: `tests/e2e/federation_e2e.sh` extensions + docs updates (`docs/FEDERATION.md` "Smoke test" and "Performance" sections, README one-liner).

If the OTel demo clone is unreliable in the nightly environment, the script can fall back to a pre-cloned fixture in CI cache (future work, not in this spec).

---

## Definition of done

1. `tests/federation_cross_repo_e2e.rs` exists; builds with `--features test-utils`; all 6 tests pass on a clean clone.
2. `tests/e2e/federation_e2e.sh` extended; existing 3 assertions still pass; new OTel assertions pass against a freshly cloned OTel demo at upstream HEAD.
3. `cargo test --test federation_cross_repo_e2e` passes in CI on a clean PR branch.
4. `tests/e2e/federation_e2e.sh` (extended) passes in the nightly workflow.
5. `docs/FEDERATION.md` smoke-test and performance sections updated with pointers to both fixtures.
6. The OTel service subdirectory list in the script's `repos.yaml` matches what's actually in `opentelemetry-demo/src/` at HEAD — if upstream adds/removes a service, the script handles it gracefully (skips missing dirs, asserts against a tolerant threshold).

## Open questions (for the implementation plan, not blockers)

- Should the per-PR D test share a `OnceCell` for setup, or each test own its tempdir? Profile first, decide second.
- Should `GetProduct` be the A fixture's blast-radius seed, or is there a more stable symbol across OTel demo versions? If `GetProduct` is renamed upstream within 6 months, the assertion needs updating — pick a candidate that has been stable the longest (e.g., `PlaceOrder` on `checkoutservice`).
- Should the A fixture also assert `get_repo_info` returns a `health` field that's one of the documented values? Add to the implementation plan if cheap.

---

## Status

Brainstorming complete. Sections 1–3 approved. Awaiting user review of this written spec before invoking writing-plans.