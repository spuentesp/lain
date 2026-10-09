# Command-Center Contract Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the 8 fixable correctness defects found in the architecture-command-center integration review so a separate application can trust LAIN's contract tools over HTTP/MCP.

**Architecture:** All fixes land in the contract-federation read path (`src/server/federation/contracts/` + `src/server/mcp/contract_tools/`) and the sensor layer that mints `SymbolKey.repo`. No new transports, no new tools, no schema version bumps — every change makes an existing response field *correct* rather than adding a field. Each task is one defect, one regression test, one commit.

**Tech Stack:** Rust 2021, `serde_json`, `git2`, `inventory`, `#[cfg(test)]` unit tests + `tests/` integration tests. `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.

**Spec:** `docs/superpowers/plans/2026-10-06-command-center-contract-fixes.md` (this plan is the spec — it was produced from a live defect review of `43f9544b7bc77cbbfee4b96fde9f00bf76309132` + uncommitted working tree, with observed API payloads quoted inline).

## Global Constraints

- Branch: `fix/dev-command-center-contract`. PRs target **`dev`** — never `main` (`AGENTS.md`).
- Do **not** bump versions in these PRs (`AGENTS.md`).
- The working tree already holds ~2300 lines of uncommitted sensor work that predate this plan. **Only `git add` files this plan owns.** Task 3 necessarily edits `src/server/sensors/codeowners_sensor.rs`, which is also in that set — its commit will include those pre-existing edits; call that out in the commit body.
- `cargo fmt --check` must pass before each commit (currently RED: 55 diffs).
- `cargo clippy --all-targets -- -D warnings` must stay clean (currently clean).
- Every response field this plan touches must stay named exactly as it is today. These are correctness fixes, not API redesigns.
- Never report "no known impact" without the accompanying `scope` block. Any new code that can answer "absent" must answer "not analyzed" when coverage is missing (`src/server/federation/contracts/coverage.rs` tri-state lookups).

## Review Focus

1. **A fix that makes a field *appear* where it was previously omitted can silently change an LLM's conclusion.** `owners: []` (explicit empty) vs. absent is the difference between "no owner declared" and "owners not loaded" — Task 3 must make the empty case explicit and a test must pin both.
2. **Cross-instance / cross-transport determinism.** A value that is right in one process and wrong in another is worse than a constant wrong value. Tasks 2 and 3 need tests that run the *same* query through two independent server instances and through both transports.
3. **Fixing a render site without fixing the source leaves the next consumer broken.** Task 1 fixes `handlers[].repo` at the render site *and* removes the sensor-name fallback literals so no sensor name can ever be a `RepoId` again; a test asserts no emitted `SymbolKey.repo` is a sensor name.
4. **`ChangedWithoutSchema` must not start firing on endpoints that have a schema** (Task 6 widens detection — the regression risk is over-firing, which would create false "needs investigation" noise). Every new emission needs a negative test.
5. **Fixture precision is not production precision.** The T1 fixture is 4 repos with 1 external host. A test that passes there proves nothing about gRPC/GraphQL/WS/SQL end-to-end — Tasks 7 and 8 must not claim that coverage.

---

## File Structure

| File | Responsibility after this plan |
|---|---|
| `src/server/mcp/contract_tools/services.rs` | `get_service` consumer rows; owns the endpoint attribution and `owners` enrichment |
| `src/server/mcp/contract_tools/contracts.rs` | `list_contracts` / `get_contract`; owns evidence refs and `kind` filter validation |
| `src/server/mcp/contract_tools/analysis.rs` | `diff_contracts` / `trace_impact` / `get_coverage` wire rendering; owns `handlers[]` |
| `src/server/mcp/contract_tools/evidence.rs` | `resolve_evidence` / `read_source`; owns snippet windows |
| `src/server/federation/contracts/diff.rs` | Pure diff + classify; owns `ChangedWithoutSchema` emission |
| `src/server/sensors/codeowners_sensor.rs` | Global CODEOWNERS index; owns the lookup key |
| `src/server/sensors/{http,http_client,field_access,websocket,grpc_handler_link,graphql_resolver_link}_sensor.rs` | Sensor entry points; stop minting sensor names as `RepoId` |
| `src/server/mcp/contract_tools/schemas/list_contracts.in.json` | Advertised `kind` enum |
| `tests/command_center_contract_fixes.rs` | NEW — one integration test per defect, run against the T1 fixture over HTTP **and** stdio |

---

### Task 1: Stop minting sensor names as `RepoId`; fix `handlers[].repo`

**Files:**
- Modify: `src/server/mcp/contract_tools/analysis.rs:1105-1122`
- Modify: `src/server/sensors/http_sensor.rs:678-679`
- Modify: `src/server/sensors/http_client_sensor.rs:91-92`
- Modify: `src/server/sensors/field_access_sensor.rs:151-152`
- Modify: `src/server/sensors/websocket_sensor.rs:204-210`
- Modify: `src/server/sensors/grpc_handler_link_sensor.rs:78-79`
- Modify: `src/server/sensors/graphql_resolver_link_sensor.rs:85-86`
- Test: `src/server/mcp/contract_tools/analysis.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Consumes: `Provider { node_id: GlobalId, handler: Option<SymbolKey> }` from `src/server/federation/contracts/index.rs:83`.
- Produces: `handlers[].repo` is the **provider's repo id** (e.g. `orders`), never a sensor name. Later tasks assume this.

**Background (why this is not an edge case):** `RepoId::new` rejects any value containing `/` (`src/server/federation/repo_id.rs:9`). Every real call passes `root.to_string_lossy()` — a filesystem path — so `RepoId::new(...)` **always fails** and the `unwrap_or_else` fallback always runs. That is why the observed payload was `{"file":"src/orders/label.rs","repo":"http-sensor","symbol":"get_order_label"}`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` at the bottom of `src/server/mcp/contract_tools/analysis.rs`:

```rust
#[test]
fn handler_repo_is_the_provider_repo_not_a_sensor_name() {
    use crate::federation::contracts::index::{ContractIndex, ConsumerResolution, Endpoint, EndpointId};
    use crate::federation::contracts::model::{
        ContractKey, HttpMethod, MethodSpec, ServiceName, SymbolKey,
    };
    use crate::federation::contracts::diff::ChangeKind;
    use crate::federation::repo_id::GlobalId;

    let mut idx = ContractIndex::default();
    let provider_id = GlobalId::parse("orders:HttpRoute:src/orders/label.rs:GET /api/orders/%3Aid/label:8")
        .expect("valid global id");
    let ep_id = EndpointId(
        ServiceName("orders".into()),
        ContractKey::Http {
            method: HttpMethod::Get,
            template: "/api/orders/{}/label".into(),
        },
    );
    let mut ep = Endpoint::new(ep_id.clone());
    ep.providers.push(crate::federation::contracts::index::Provider {
        node_id: provider_id,
        handler: Some(SymbolKey {
            repo: crate::federation::repo_id::RepoId::new("http-sensor").unwrap(),
            path: "src/orders/label.rs".into(),
            container: None,
            name: "get_order_label".into(),
        }),
        operation_id: None,
    });
    idx.endpoints.insert(ep_id.clone(), ep);

    let change = ChangeKind::ChangedWithoutSchema { endpoint: ep_id };
    let value = impact_to_value(
        &change,
        &ServiceName("orders".into()),
        &idx,
        /* compat */ "NeedsReview",
    );

    let handlers = value["handlers"].as_array().expect("handlers present");
    assert_eq!(handlers[0]["repo"], "orders");
    assert_ne!(handlers[0]["repo"], "http-sensor");
}
```

> If `impact_to_value`'s real signature differs, read it at `src/server/mcp/contract_tools/analysis.rs:1060` and match it exactly — the assertion (`handlers[0]["repo"] == "orders"`) is the contract, not the call shape. If `Endpoint::new` / `Provider` field names differ, match `src/server/federation/contracts/index.rs:62-93` exactly.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib handler_repo_is_the_provider_repo_not_a_sensor_name -- --nocapture`
Expected: FAIL — `assertion left == right: "http-sensor" != "orders"`

- [ ] **Step 3: Fix the render site**

In `src/server/mcp/contract_tools/analysis.rs`, replace the `handlers` mapping so the repo comes from the provider node, not the (bogus) `SymbolKey.repo`:

```rust
            let handlers: Vec<Value> = ep
                .providers
                .iter()
                .filter_map(|p| {
                    p.handler.as_ref().map(|h| {
                        json!({
                            "repo": p.node_id.repo_id(),
                            "file": h.path,
                            "symbol": h.name,
                        })
                    })
                })
                .collect();
```

The only changed token is `"repo": h.repo.as_str()` → `"repo": p.node_id.repo_id()`.

- [ ] **Step 4: Remove the sensor-name fallbacks**

In each of the six sensor files, replace the `unwrap_or_else` fallback that mints a sensor-named `RepoId`. Use the shared helper added here so all six agree. Add to `src/server/sensors/util.rs`:

```rust
/// Fallback `RepoId` for sensors whose caller could not supply one.
/// Deliberately a single neutral token: a `RepoId` must never be a
/// sensor name, because `SymbolKey.repo` is rendered as an evidence
/// field and an external client resolves it as a repository.
pub fn fallback_repo_id() -> crate::federation::repo_id::RepoId {
    crate::federation::repo_id::RepoId::new("unknown").expect("valid repo id")
}
```

Then in each sensor, change exactly the fallback arm:

```rust
// http_sensor.rs:678-679
let repo_id = RepoId::new(root.to_string_lossy().as_ref())
    .unwrap_or_else(|_| crate::sensors::util::fallback_repo_id());
```

```rust
// http_client_sensor.rs:91-92
let repo_id = RepoId::new(root.to_string_lossy().as_ref())
    .unwrap_or_else(|_| crate::sensors::util::fallback_repo_id());
```

```rust
// field_access_sensor.rs:151-152  — same shape
```

```rust
// grpc_handler_link_sensor.rs:78-79
let repo_id = RepoId::new(root.to_string_lossy().as_ref())
    .unwrap_or_else(|_| crate::sensors::util::fallback_repo_id());
```

```rust
// graphql_resolver_link_sensor.rs:85-86 — same shape
```

```rust
// websocket_sensor.rs:204-210
let repo_id = crate::federation::repo_id::RepoId::new(
    root.to_string_lossy().as_ref(),
)
.unwrap_or_else(|_| crate::sensors::util::fallback_repo_id());
```

(Adjust the `use` path to whatever `util.rs` is already imported as in each file — they already import from `sensors::util`; do not invent a second helper module.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib handler_repo_is_the_provider_repo_not_a_sensor_name && cargo test --quiet --test federation_contracts_e2e && cargo test --quiet --test contract_mcp_parity`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add src/server/mcp/contract_tools/analysis.rs src/server/sensors/util.rs \
        src/server/sensors/http_sensor.rs src/server/sensors/http_client_sensor.rs \
        src/server/sensors/field_access_sensor.rs src/server/sensors/websocket_sensor.rs \
        src/server/sensors/grpc_handler_link_sensor.rs src/server/sensors/graphql_resolver_link_sensor.rs
git commit -m "fix(contracts): handlers[].repo is the provider repo, not a sensor name

RepoId::new rejects paths containing '/', so the unwrap_or_else fallback
in six sensors always ran and leaked the sensor name into SymbolKey.repo,
which diff_contracts renders as handlers[].repo. An external client cannot
resolve 'http-sensor' as a repository. Render the provider's repo id
instead and collapse the six fallbacks onto one neutral token."
```

---

### Task 2: Attribute each consumer to the endpoint it actually calls

**Files:**
- Modify: `src/server/mcp/contract_tools/services.rs:494-520`
- Test: `tests/command_center_contract_fixes.rs` (NEW)

**Interfaces:**
- Consumes: `ConsumerResolution { call_id, service, target, bound_endpoints: Vec<EndpointId>, reads_complete }` from `src/server/federation/contracts/index.rs:100-105`.
- Produces: `get_service.data.consumers[].uses[].endpoint = {"service": <the provider service>, "key": <the bound ContractKey>}` — one entry per bound endpoint.

**Background (observed bug):** `build_consumer_rows` picks the endpoint with
```rust
idx.endpoints.values().find(|e| provider_endpoints.contains(&e.id.1.to_string()))
```
`provider_endpoints` is a set of the *queried* service's `ContractKey` strings, so this returns the **first entry in the `BTreeMap` whose key string happens to match** — for the T1 fixture that is billing's `topic:kafka/orders.created`, which is then reported as the endpoint for all 7 consumers including `reports` calling `GET /invoices/{}`. The fix is to read `resolution.bound_endpoints`, which already holds the truth.

- [ ] **Step 1: Write the failing test**

Create `tests/command_center_contract_fixes.rs`:

```rust
//! Regression tests for the command-center contract fixes.
//! Every test drives the T1 fixture (scripts/contracts-fixture.sh)
//! through a real server and asserts on the wire payload.

mod common;

use serde_json::Value;

/// `get_service` must attribute each consumer to the endpoint it
/// actually calls, not to the alphabetically-first endpoint whose
/// ContractKey collides with the provider's key set.
#[test]
fn get_service_attributes_each_consumer_to_its_real_endpoint() {
    let ctx = common::contracts_t1_federation(); // see Step 2 for the helper
    let base = ctx.snapshot_all_at_tag("base");

    let service = ctx.call("get_service", serde_json::json!({
        "snapshot": base, "service": "orders", "limit": 50
    }));

    let consumers = service["consumers"].as_array().expect("consumers array");
    let mut seen: Vec<(String, String)> = Vec::new();
    for c in consumers {
        for u in c["uses"].as_array().unwrap() {
            let site = u["site"]["id"].as_str().unwrap_or_default().to_string();
            let key = u["endpoint"]["key"].as_str().unwrap_or_default().to_string();
            let svc = u["endpoint"]["service"].as_str().unwrap_or_default().to_string();
            seen.push((site, format!("{svc}/{key}")));
        }
    }

    // The consumer that calls GET /api/orders/{} must be attributed to it.
    let (_, endpoint) = seen
        .iter()
        .find(|(site, _)| site.contains("GET /api/orders/{}"))
        .expect("a consumer site for GET /api/orders/{}");
    assert_eq!(endpoint, "orders/http:GET /api/orders/{}");

    // ...and no row may claim billing's topic endpoint as its target
    // unless the site really is the topic.
    for (site, endpoint) in &seen {
        if endpoint == "billing/topic:kafka/orders.created" {
            assert!(
                site.contains("kafka/orders.created"),
                "site {site} attributed to the topic endpoint"
            );
        }
    }
}
```

- [ ] **Step 2: Add the T1 helper**

In `tests/common/mod.rs` (or a new `tests/common/contracts_t1.rs` re-exported from it), add a helper that builds the fixture once and exposes `call(tool, args) -> Value` over HTTP **and** `call_stdio(tool, args) -> Value`. It must:

1. Run `scripts/contracts-fixture.sh <tmpdir>` via `std::process::Command`.
2. Start `target/debug/lain server --config <tmpdir>/repos.yaml --transport http --port <ephemeral>`.
3. Poll `GET /health` until `federation.repos[].health == "ready"` (≤ 120 s).
4. POST JSON-RPC `initialize`, `notifications/initialized`, `tools/call load_package {"package":"contracts"}`.
5. Expose `snapshot_all_at_tag(tag)` → `prepare_snapshot` with all four repos at `tag`, returning `data.snapshot`.
6. Expose `call` (HTTP) and `call_stdio` (spawn `--transport stdio`, correlate responses by JSON-RPC `id` — **responses arrive out of order**).

Match the existing style in `tests/support/contracts_snapshot_harness.rs:29-116`, which already builds this fixture for `tests/federation_contracts_e2e.rs`. Prefer extending that harness over writing a second one.

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --test command_center_contract_fixes get_service_attributes_each_consumer_to_its_real_endpoint -- --nocapture`
Expected: FAIL — `assertion left == right: "billing/topic:kafka/orders.created" != "orders/http:GET /api/orders/{}"`

- [ ] **Step 4: Implement**

In `src/server/mcp/contract_tools/services.rs`, inside `build_consumer_rows`, replace the endpoint lookup and iterate **all** bound endpoints rather than one arbitrary one:

```rust
    for (call_id, resolution) in &idx.consumers {
        let Some(ConsumerTarget::Binds {
            provenance,
            route_match,
            ..
        }) = &resolution.target
        else {
            continue;
        };
        // The endpoints THIS call is bound to — not the first entry
        // in the table whose key string collides with the provider's
        // key set (which picked the alphabetically-first service).
        let bound: Vec<&Endpoint> = resolution
            .bound_endpoints
            .iter()
            .filter_map(|eid| idx.endpoints.get(eid))
            .collect();
        if bound.is_empty() {
            continue;
        }
        for endpoint in bound {
            let consumer_service = resolution.service.0.clone();
            // ... unchanged body that builds `use_value`, but with
            //     "endpoint": {"service": endpoint.id.0.0.as_str(),
            //                  "key": &endpoint.id.1.to_string()}
            // ...
        }
    }
```

Concretely: keep the existing `use_value` construction exactly as it is (it already reads `endpoint.id.0` / `endpoint.id.1`), move it inside `for endpoint in bound`, and delete the `let Some(endpoint) = idx.endpoints.values().find(...)` block and the now-unused `provider_endpoints` set. Preserve the `by_consumer` / `caller_key|line_key` dedup so one site still yields one row.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --test command_center_contract_fixes && cargo test --quiet --test federation_contracts_e2e && cargo test --quiet --test contract_mcp_parity`
Expected: all PASS. `contract_mcp_parity`'s `get_service` case must still be byte-identical across transports.

- [ ] **Step 6: Commit**

```bash
git add src/server/mcp/contract_tools/services.rs tests/command_center_contract_fixes.rs tests/common/
git commit -m "fix(contracts): get_service attributes consumers to their bound endpoints

build_consumer_rows resolved the endpoint with a .find() over the whole
Endpoint table matching on ContractKey string, so every consumer of a
service was attributed to the alphabetically-first endpoint whose key
collided (billing/topic:kafka/orders.created for all seven rows of
get_service orders). Read resolution.bound_endpoints instead."
```

---

### Task 3: Make `owners` deterministic across instances and transports

**Files:**
- Modify: `src/server/sensors/codeowners_sensor.rs:268-297`
- Modify: `src/server/mcp/contract_tools/services.rs:770-785`
- Modify: `src/server/sensors/mod.rs` (add `scan_for_repo`)
- Test: `tests/command_center_contract_fixes.rs`

**Interfaces:**
- Consumes: `Sensor::scan(&self, graph, root, namespace)` (`src/server/sensors/mod.rs:205-215`); `run_all` / `run_all_with_reports` (`mod.rs:313-343`); caller sites in `src/server/ingestion.rs:699` (single workspace) and `:1840` (federation per-repo).
- Produces: `Sensor::scan_for_repo(&self, graph, root, namespace, repo_id: &str)` — default delegates to `scan`. `get_service.data.consumers[].uses[].used_by[].owners` is **always present** (possibly `[]`).

**Background (observed bug):** `scan_workspace_codeowners` keys its global index by `root.file_name()`. On the first ingest `root` is `/tmp/t1/billing` → key `billing` (matches the `GlobalId` repo). On a later ingest `root` is `<data_dir>/worktrees/billing/<sha>` → key `<sha>`, which never matches `codeowners_for("billing", …)`. Observed: one HTTP instance returned `owners` on 4 entries, a second HTTP instance and every stdio run returned the field absent entirely. Additionally `enrich_used_by_with_owners` omits the field when the lookup is empty (`if !owners.is_empty()`), so a client cannot tell "no owner" from "owners not loaded".

- [ ] **Step 1: Write the failing test**

Append to `tests/command_center_contract_fixes.rs`:

```rust
/// Owners must be identical across two independent server instances
/// and across both transports, and must be present (possibly empty).
#[test]
fn owners_are_deterministic_across_instances_and_transports() {
    let a = common::contracts_t1_federation();
    let base_a = a.snapshot_all_at_tag("base");
    let b = common::contracts_t1_federation();
    let base_b = b.snapshot_all_at_tag("base");

    let http_a = a.call("get_service", serde_json::json!({"snapshot": base_a, "service": "orders", "limit": 50}));
    let http_b = b.call("get_service", serde_json::json!({"snapshot": base_b, "service": "orders", "limit": 50}));
    let stdio_a = a.call_stdio("get_service", serde_json::json!({"snapshot": base_a, "service": "orders", "limit": 50}));

    let owners = |v: &Value| -> Vec<Value> {
        let mut out = Vec::new();
        fn walk(v: &Value, out: &mut Vec<Value>) {
            match v {
                Value::Object(m) => {
                    for (k, val) in m {
                        if k == "owners" {
                            out.push(val.clone());
                        } else {
                            walk(val, out);
                        }
                    }
                }
                Value::Array(items) => items.iter().for_each(|i| walk(i, out)),
                _ => {}
            }
        }
        walk(v, &mut out);
        out
    };

    let oa = owners(&http_a);
    let ob = owners(&http_b);
    let os = owners(&stdio_a);

    assert!(!oa.is_empty(), "owners key must always be present");
    assert_eq!(oa, ob, "two server instances disagree on owners");
    assert_eq!(oa, os, "HTTP and stdio disagree on owners");
    assert!(
        oa.iter().any(|v| v.as_array().map(|a| !a.is_empty()).unwrap_or(false)),
        "at least one used_by entry should carry a real owner (billing has CODEOWNERS)"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test command_center_contract_fixes owners_are_deterministic_across_instances_and_transports -- --nocapture`
Expected: FAIL — `owners key must always be present` (or `two server instances disagree`).

- [ ] **Step 3: Give the sensor a real repo id**

In `src/server/sensors/mod.rs`, extend the `Sensor` trait with a defaulted method and thread the repo id through `run_all_with_reports` / `run_all`:

```rust
    /// Like [`Self::scan`], but with the repository id the caller
     /// already knows. Sensors whose output is keyed by repo (the
     /// CODEOWNERS index) override this; the default ignores
     /// `repo_id` and delegates.
    fn scan_for_repo(
        &self,
        graph: &GraphDatabase,
        root: &Path,
        namespace: &RepoNamespace,
        _repo_id: &str,
    ) -> Result<usize, LainError> {
        self.scan(graph, root, namespace)
    }
```

`run_all_with_reports` gains a `repo_id: &str` parameter and calls `scan_for_repo`; update the two call sites (`src/server/ingestion.rs:699`, `:1840`) to pass the repo id they already hold.

In `src/server/sensors/codeowners_sensor.rs`, replace the key derivation:

```rust
pub fn scan_workspace_codeowners_for_repo(
    _graph: &GraphDatabase,
    root: &Path,
    _namespace: &RepoNamespace,
    repo: &str,
) -> Result<usize, LainError> {
    let mut rules: Vec<Rule> = Vec::new();
    for cand in CANDIDATE_PATHS {
        let p = root.join(cand);
        if let Ok(content) = std::fs::read_to_string(&p) {
            rules.extend(parse_codeowners(&content));
        }
    }
    let mut map = match global().lock() {
        Ok(m) => m,
        Err(p) => p.into_inner(),
    };
    if rules.is_empty() {
        map.remove(repo);
    } else {
        map.insert(repo.to_string(), RepoRules { rules });
    }
    Ok(0)
}

impl crate::server::sensors::Sensor for CodeownersSensor {
    fn name(&self) -> &'static str { "codeowners" }
    fn count_field(&self) -> SensorCountField { SensorCountField::EntryPoints }
    fn phase(&self) -> u8 { 1 }
    fn scan(
        &self,
        graph: &GraphDatabase,
        root: &Path,
        namespace: &RepoNamespace,
    ) -> Result<usize, LainError> {
        // Fallback for callers that cannot name the repo: key by the
        // directory name so tests keep working.
        let repo = root.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
        scan_workspace_codeowners_for_repo(graph, root, namespace, &repo)
    }
    fn scan_for_repo(
        &self,
        graph: &GraphDatabase,
        root: &Path,
        namespace: &RepoNamespace,
        repo_id: &str,
    ) -> Result<usize, LainError> {
        scan_workspace_codeowners_for_repo(graph, root, namespace, repo_id)
    }
}
```

(If `register_sensor!` already generates the `Sensor` impl, keep using it for `scan` and add only the `scan_for_repo` override via a manual `impl` extension or by extending the macro — match whatever the macro supports rather than duplicating the impl.)

- [ ] **Step 4: Always emit the `owners` key**

In `src/server/mcp/contract_tools/services.rs`, change `enrich_used_by_with_owners` so absence is never ambiguous:

```rust
fn enrich_used_by_with_owners(entries: &mut [Value]) {
    for entry in entries.iter_mut() {
        let Some(ref_obj) = entry.get("ref").and_then(|v| v.as_object()) else {
            continue;
        };
        let Some(id) = ref_obj.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let repo = id.split(':').next().unwrap_or("");
        let path = ref_obj.get("path").and_then(|v| v.as_str()).unwrap_or("");
        // Always present: `[]` means "CODEOWNERS declares no owner for
        // this path". Omission would mean "owners could not be loaded",
        // which an external client must not silently treat as "none".
        entry["owners"] = json!(codeowners_for(repo, path));
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --test command_center_contract_fixes owners_are_deterministic_across_instances_and_transports -- --nocapture && cargo test --quiet --lib codeowners`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add src/server/sensors/mod.rs src/server/sensors/codeowners_sensor.rs \
        src/server/mcp/contract_tools/services.rs tests/command_center_contract_fixes.rs src/server/ingestion.rs
git commit -m "fix(contracts): owners are deterministic across instances and transports

scan_workspace_codeowners keyed its global index by root.file_name(),
which is the worktree SHA on a re-indexed federation, so codeowners_for()
never matched and get_service silently omitted 'owners' on some
instances. Key the index by the repo id the caller already knows, and
always emit the key (empty array = 'no owner declared').

NOTE: src/server/sensors/codeowners_sensor.rs also carries pre-existing
uncommitted sensor work from this branch; those edits ride along."
```

---

### Task 4: Validate `list_contracts(kind=…)` instead of silently returning empty

**Files:**
- Modify: `src/server/mcp/contract_tools/contracts.rs:185-212`
- Modify: `src/server/mcp/contract_tools/schemas/list_contracts.in.json`
- Test: `tests/command_center_contract_fixes.rs`

**Interfaces:**
- Consumes: `ContractKey::kind()` (`src/server/federation/contracts/model.rs:545+`).
- Produces: `list_contracts` rejects an unknown `kind` with `error.code == "invalid_argument"`; the advertised enum lists every kind the code accepts (`http, topic, rpc, graphql, websocket, table`).

**Background (observed bug):** `kind: "bogus"` returned `isError: false, items: []`. A client that asks "is there a GraphQL contract here?" and gets `[]` will report "no known impact" — the exact conflation the product must never make. Meanwhile `kind: "rpc"|"graphql"|"websocket"|"table"` are accepted by the code but **not advertised** in the input schema's enum (`["http","topic"]`).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn list_contracts_rejects_unknown_kind_instead_of_returning_empty() {
    let ctx = common::contracts_t1_federation();
    let base = ctx.snapshot_all_at_tag("base");

    let res = ctx.call_raw("list_contracts", serde_json::json!({
        "snapshot": base, "kind": "bogus", "limit": 50
    }));
    assert_eq!(res["isError"], true, "unknown kind must be an error, not an empty page");
    assert_eq!(res["error"]["code"], "invalid_argument");

    // Every advertised kind must be accepted (0 items is fine when the
    // fixture has no such contract — that is data, not a filter failure).
    for kind in ["http", "topic", "rpc", "graphql", "websocket", "table"] {
        let ok = ctx.call_raw("list_contracts", serde_json::json!({
            "snapshot": base, "kind": kind, "limit": 50
        }));
        assert_eq!(ok["isError"], false, "kind {kind} must be accepted");
    }
}
```

(`ctx.call_raw` returns the full result object including `isError`, unlike `call` which unwraps `structuredContent.data`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test command_center_contract_fixes list_contracts_rejects_unknown_kind_instead_of_returning_empty -- --nocapture`
Expected: FAIL — `unknown kind must be an error` (observed `isError: false`).

- [ ] **Step 3: Implement**

In `src/server/mcp/contract_tools/contracts.rs`, validate against the same set `ContractKey::kind()` can produce:

```rust
    const VALID_KINDS: &[&str] = &["http", "topic", "rpc", "graphql", "websocket", "table"];
    if let Some(ref k) = kind_filter {
        if !VALID_KINDS.contains(&k.as_str()) {
            return Err(error_outcome(
                "invalid_argument",
                format!("unknown kind {k:?}"),
                Some(json!({"arg": "kind", "reason": "unknown", "allowed": VALID_KINDS})),
                snapshot_label,
                started,
            ));
        }
    }
```

(Match the existing error helper used by `run_list_contracts` for `range_too_large` / `malformed cursor` — see `src/server/mcp/contract_tools/paging.rs:96` and `contract_tools/view.rs:124` for the established shape.)

In `src/server/mcp/contract_tools/schemas/list_contracts.in.json`, replace the `kind` enum with the full list:

```json
"kind": {
  "type": "string",
  "description": "Filter by contract kind.",
  "enum": ["http", "topic", "rpc", "graphql", "websocket", "table"]
}
```

Then regenerate the dump so `schema-drift` CI stays green:

```bash
make schema && git diff --stat docs/tool-schema.json
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test command_center_contract_fixes && cargo test --quiet --test contracts_golden && cargo test --quiet --test schema_dump_smoke`
Expected: all PASS

- [ ] **Step 5: Commit**

```bash
git add src/server/mcp/contract_tools/contracts.rs src/server/mcp/contract_tools/schemas/list_contracts.in.json docs/tool-schema.json tests/command_center_contract_fixes.rs
git commit -m "fix(contracts): list_contracts rejects an unknown kind instead of returning empty

kind:'bogus' returned items:[] with isError:false, which an external
client reads as 'no such contract here' — the 'absent' vs 'not analyzed'
conflation the coverage ledger exists to prevent. The advertised enum
also listed only http/topic while the code accepts six kinds."
```

---

### Task 5: Complete evidence references (`ref.commit` and line-0 snippets)

**Files:**
- Modify: `src/server/mcp/contract_tools/contracts.rs:854-886`
- Modify: `src/server/mcp/contract_tools/evidence.rs:795-815`
- Test: `tests/command_center_contract_fixes.rs`

**Interfaces:**
- Consumes: `ContractView::commits()` (used at `evidence.rs:255`), `GlobalId::{repo_id, path, line_start}`.
- Produces: every evidence ref carries a non-empty `commit`; `resolve_evidence` returns a snippet for OpenAPI-derived nodes whose `line_start == 0` (first `context_lines + 1` lines of the file).

**Background (observed):** `evidence_ref` hardcodes `"commit": ""` and `"text": ""`, so `list_contracts`/`get_contract` refs are not self-contained — the client must join `view.git_commits`. And `snapshot_blob_window` returns `None` when `cited == 0`, so `orders:HttpRoute:openapi.yaml:GET /api/orders/me:0` resolved to `exists: true, snippet: null`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn evidence_refs_carry_a_commit() {
    let ctx = common::contracts_t1_federation();
    let base = ctx.snapshot_all_at_tag("base");
    let contracts = ctx.call("list_contracts", serde_json::json!({"snapshot": base, "limit": 100}));
    let items = contracts["items"].as_array().unwrap();
    for item in items {
        for p in item["providers"].as_array().unwrap() {
            assert!(!p["commit"].as_str().unwrap_or("").is_empty(),
                "provider ref {} has an empty commit", p["id"]);
        }
    }
}

#[test]
fn openapi_line_zero_refs_resolve_to_a_snippet() {
    let ctx = common::contracts_t1_federation();
    let base = ctx.snapshot_all_at_tag("base");
    let res = ctx.call("resolve_evidence", serde_json::json!({
        "snapshot": base,
        "refs": ["orders:HttpRoute:openapi.yaml:GET /api/orders/me:0"],
        "context_lines": 3
    }));
    let item = &res["items"][0];
    assert_eq!(item["exists"], true);
    assert!(
        item["snippet"].as_str().map(|s| !s.is_empty()).unwrap_or(false),
        "line-0 ref must still yield a snippet, got {:?}", item["snippet"]
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test command_center_contract_fixes evidence_refs_carry_a_commit -- --nocapture`
Expected: FAIL — `provider ref … has an empty commit`

- [ ] **Step 3: Implement**

Thread the commit into the three ref builders. Change each signature to take `commit: &str` and fill it:

```rust
fn evidence_ref(id: &GlobalId, path: &str, line: u32, commit: &str) -> Value {
    let text = if commit.is_empty() || path.is_empty() {
        String::new()
    } else {
        format!("{}@{}:{}:{}", id.repo_id(), commit, path, line)
    };
    json!({
        "id": id.as_str(),
        "repo": id.repo_id(),
        "commit": commit,
        "path": path,
        "line": line,
        "text": text,
    })
}
```

Apply the same to `evidence_ref_from_call` and `caller_evidence`. At every call site, pass the commit from the view: `view.commits().get(id.repo_id()).map(String::as_str).unwrap_or("")` — the same lookup `evidence.rs:255` already performs. The `text` field becomes the `repo@sha:path:line` form already documented in `resolve_evidence.in.json`, so a client can round-trip it straight back into `resolve_evidence`.

For the snippet, relax the `line == 0` early-return in `snapshot_blob_window`:

```rust
fn snapshot_blob_window(
    fed: &crate::federation::contracts::snapshots::SnapshotFederation,
    repo: &str,
    commit: &str,
    path: &str,
    line: u32,
    context_lines: u32,
) -> Option<String> {
    let text = snapshot_blob_at(fed, repo, commit, path)?;
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return None;
    }
    // `line_start == 0` marks a node with no line anchor (OpenAPI
    // operations, schema nodes). Return the head of the file so the
    // reference still resolves to real source instead of `null`.
    let cited = if line == 0 {
        1
    } else {
        line as usize
    };
    if cited > lines.len() {
        return None;
    }
    let start = cited.saturating_sub(context_lines as usize + 1);
    let end = (cited + context_lines as usize + 1).min(lines.len());
    Some(lines[start..end].join("\n"))
}
```

Apply the same `line == 0 → 1` clamp in `snippet_from_live`'s callee `read_file_lines`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test command_center_contract_fixes && cargo test --quiet --test contracts_golden`
Expected: all PASS

- [ ] **Step 5: Commit**

```bash
git add src/server/mcp/contract_tools/contracts.rs src/server/mcp/contract_tools/evidence.rs tests/command_center_contract_fixes.rs
git commit -m "fix(contracts): evidence refs carry a commit and line-0 refs resolve to source

list_contracts/get_contract emitted refs with commit:'' and text:'',
forcing clients to join view.git_commits by hand. Fill both (text becomes
the repo@sha:path:line form resolve_evidence already accepts).
snapshot_blob_window returned None for line_start==0, so every
OpenAPI-derived reference resolved to exists:true, snippet:null."
```

---

### Task 6: Detect handler changes on schema-bearing endpoints

**Files:**
- Modify: `src/server/federation/contracts/diff.rs:574-637`
- Test: `src/server/federation/contracts/diff_tests.rs`

**Interfaces:**
- Consumes: `EndpointDef { has_schema, source_files, providers, schemas }`, `ChangeKind::ChangedWithoutSchema`, `ChangedFilesSource`.
- Produces: a new `ChangeKind::HandlerChanged { endpoint }` emitted when a schema-bearing endpoint's handler file changed; classified `Compat::NeedsReview` → `Class::NeedsInvestigation`, `reason: needs_review`. **No new `ChangeKind` is emitted for schema-less endpoints** (they keep `ChangedWithoutSchema`).

**Background (observed blind spot):** `diff.rs:584-637` fires only when `!base_def.has_schema && !head_def.has_schema`. A behavior change behind an endpoint that *does* have a schema produces **no** `ChangeKind` at all — the field diff is empty because the schema is byte-identical, so the change vanishes. This is the single largest hole in the PR-intelligence use case.

- [ ] **Step 1: Write the failing tests**

In `src/server/federation/contracts/diff_tests.rs`, following the style of the existing `ChangedWithoutSchema` cases at `:2504-2737`:

```rust
#[test]
fn handler_change_on_a_schema_bearing_endpoint_is_reported() {
    let base = surface_with_schema_bearing_endpoint("GET /a", "handler.rs");
    let head  = surface_with_schema_bearing_endpoint("GET /a", "handler.rs"); // identical schema
    let changed = StaticChangedFiles::new("svc", ["handler.rs"].map(String::from).into());

    let changes = diff_contracts_with_files(&base, &head, &changed);
    assert!(
        changes.iter().any(|c| matches!(c.kind, ChangeKind::HandlerChanged { .. })),
        "schema-bearing endpoint with a changed handler file must not vanish: {changes:?}"
    );
    // The schema is unchanged, so no field-level change may appear.
    assert!(changes.iter().all(|c| !matches!(
        c.kind,
        ChangeKind::FieldAdded { .. } | ChangeKind::FieldRemoved { .. }
    )));
}

#[test]
fn schema_bearing_endpoint_unchanged_handler_emits_nothing() {
    let base = surface_with_schema_bearing_endpoint("GET /a", "handler.rs");
    let head  = surface_with_schema_bearing_endpoint("GET /a", "handler.rs");
    let changed = StaticChangedFiles::new("svc", [].map(String::from).into());

    let changes = diff_contracts_with_files(&base, &head, &changed);
    assert!(changes.is_empty(), "no file changed -> no change: {changes:?}");
}

#[test]
fn schema_less_endpoint_still_reports_changed_without_schema() {
    // Regression: widening detection must not swallow the old rule.
    let base = surface_with_schemaless_endpoint("GET /b", "handler.rs");
    let head  = surface_with_schemaless_endpoint("GET /b", "handler.rs");
    let changed = StaticChangedFiles::new("svc", ["handler.rs"].map(String::from).into());

    let changes = diff_contracts_with_files(&base, &head, &changed);
    assert!(changes.iter().any(|c| matches!(c.kind, ChangeKind::ChangedWithoutSchema { .. })));
    assert!(changes.iter().all(|c| !matches!(c.kind, ChangeKind::HandlerChanged { .. })));
}
```

Reuse the existing fixture builders in that file (grep for `fn surface_with_` / `StaticChangedFiles::new` at `:2504`); if no builder exists, add one that constructs `ContractSurface` directly, mirroring `diff_tests.rs:1009`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib handler_change_on_a_schema_bearing_endpoint_is_reported -- --nocapture`
Expected: FAIL — `schema-bearing endpoint with a changed handler file must not vanish`

- [ ] **Step 3: Implement**

Add the variant to `ChangeKind` (`diff.rs:301-394`) next to `ChangedWithoutSchema`:

```rust
    /// A handler file behind an endpoint that HAS a schema changed.
    /// The schema is byte-identical so no field-level change exists,
    /// but the behaviour behind the endpoint may have moved. §9.2's
    /// `ChangedWithoutSchema` is the schema-less sibling; this is the
    /// schema-bearing one, and it is the reason a pure behaviour
    /// change is never invisible to `diff_contracts`.
    HandlerChanged { endpoint: EndpointId },
```

In the paired-endpoint loop (`diff.rs:584-637`), split the branch:

```rust
        let changed_here = /* the existing per-repo changed-file test */;
        if changed_here {
            if !base_def.has_schema && !head_def.has_schema {
                out.push(Change { kind: ChangeKind::ChangedWithoutSchema { endpoint: head_id.clone() }, .. });
            } else if base_def.has_schema && head_def.has_schema {
                out.push(Change { kind: ChangeKind::HandlerChanged { endpoint: head_id.clone() }, .. });
            }
            // A schema appeared or disappeared: the field-level diff
            // below already reports the schema change, so no extra
            // handler change is needed.
        }
```

Map it in `classify` (`diff.rs:1275-1370`) to `Compat::NeedsReview`, and in `evaluate` to `Class::NeedsInvestigation` with `Reason::NeedsReview` — **not** `NoKnownImpact`. Render `"kind": "HandlerChanged"` in `analysis.rs`'s `kind_label` and add `handlers[]` for it exactly as `ChangedWithoutSchema` does.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib federation::contracts && cargo test --quiet --test federation_contracts_e2e`
Expected: all PASS. The e2e ground-truth scenarios must be unchanged (the T1 fixture tags touch schema-less or schema-diff endpoints only).

- [ ] **Step 5: Commit**

```bash
git add src/server/federation/contracts/diff.rs src/server/federation/contracts/diff_tests.rs src/server/mcp/contract_tools/analysis.rs
git commit -m "feat(contracts): report handler changes behind schema-bearing endpoints

ChangedWithoutSchema only fired when BOTH sides had no schema, so a
behaviour change on an endpoint with a byte-identical schema produced no
ChangeKind at all and diff_contracts went silent. Add HandlerChanged for
that case (NeedsReview -> NeedsInvestigation), leaving the schema-less
rule untouched."
```

---

### Task 7: Give impact paths real provenance on non-`Binds` hops

**Files:**
- Modify: the hop builder in `src/server/federation/contracts/graph_backend.rs` (`traverse_impact`, ~`:631-668`)
- Test: `tests/command_center_contract_fixes.rs`

**Interfaces:**
- Consumes: `GraphEdge.provenance: Option<EdgeProvenance>` and `GraphEdge.site: Option<SourceSite>` (`src/server/schema.rs:1026-1058`).
- Produces: `paths[].hops[].provenance` is the edge's own `EdgeProvenance` (never `{"kind":"unknown","confidence":0.0}` for a sensor-emitted edge); `paths[].min_confidence` is the true minimum, so `trace_impact(min_confidence=…)` filters meaningfully.

**Background (observed):** every `SendsHttp` / `Calls` / `HasField` / `ResponseSchema` hop rendered `{"confidence":0.0,"kind":"unknown"}` even though the same edges carry `Static{source:TreeSitter}` in `get_contract`'s `binding` field. Consequently `min_confidence` was `0.0` on every multi-hop path and the documented `min_confidence` filter could only ever drop everything.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn impact_path_hops_carry_real_provenance() {
    let ctx = common::contracts_t1_federation();
    let base = ctx.snapshot_all_at_tag("base");
    let res = ctx.call("trace_impact", serde_json::json!({
        "snapshot": base,
        "from": {"endpoint": {"service": "orders", "key": "http:GET /api/orders/{}"}},
        "depth": 3
    }));
    let paths = res["paths"].as_array().unwrap();
    assert!(!paths.is_empty());
    for p in paths {
        for h in p["hops"].as_array().unwrap() {
            let prov = &h["provenance"];
            assert_ne!(
                prov["kind"], "unknown",
                "hop {} has unknown provenance", h["node"]
            );
        }
        assert!(
            (p["min_confidence"].as_f64().unwrap_or(0.0)) > 0.0,
            "min_confidence must reflect real edge confidence"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test command_center_contract_fixes impact_path_hops_carry_real_provenance -- --nocapture`
Expected: FAIL — `hop … has unknown provenance`

- [ ] **Step 3: Implement**

In the hop-serialization function (grep `fn hop_to_value` / `"provenance"` in `graph_backend.rs` and `analysis.rs:1940`), map `EdgeProvenance` through the same `provenance_to_json` already used by `services.rs:787`:

```rust
fn hop_provenance(edge: &crate::schema::GraphEdge) -> Value {
    match edge.provenance.as_ref() {
        Some(p) => crate::mcp::contract_tools::services::provenance_to_json(p),
        // Sensor-emitted edges without an explicit provenance are
        // Static from the sensor that created them; do not claim
        // "unknown" (which reads as "we do not know if this edge is
        // real") when what we mean is "no extra signal beyond the
        // static parse".
        None => json!({"kind": "static", "confidence": 1.0, "source": "Sensor"}),
    }
}
```

Make `provenance_to_json` `pub(crate)` in `services.rs` (it is currently private at `:787`). Keep `min_confidence` computed as the minimum of the real per-hop values.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test command_center_contract_fixes impact_path_hops_carry_real_provenance && cargo test --quiet --test federation_contracts_e2e`
Expected: all PASS

- [ ] **Step 5: Commit**

```bash
git add src/server/federation/contracts/graph_backend.rs src/server/mcp/contract_tools/services.rs src/server/mcp/contract_tools/analysis.rs tests/command_center_contract_fixes.rs
git commit -m "fix(contracts): impact-path hops carry real edge provenance

SendsHttp/Calls/HasField/ResponseSchema hops rendered
{kind:unknown, confidence:0.0} even though get_contract reports the same
edges as static, so min_confidence was 0.0 on every multi-hop path and
the documented min_confidence filter could only drop everything."
```

---

### Task 8: rustfmt + cross-transport parity harness

**Files:**
- Modify: all files this plan touched (formatting only)
- Test: `tests/command_center_contract_fixes.rs`

**Interfaces:**
- Consumes: everything above.
- Produces: `cargo fmt --check` exits 0; a parity test that runs every contract tool over HTTP and stdio and asserts byte-identical `structuredContent` on a fixture that **has CODEOWNERS**.

**Background:** `cargo fmt --check` is currently RED with 55 diffs across the pre-existing uncommitted work and this plan's edits. `tests/contract_mcp_parity.rs` passes 8/8 but uses a mini-fixture without CODEOWNERS, which is exactly why it missed the `owners` divergence.

- [ ] **Step 1: Write the failing parity test**

```rust
#[test]
fn every_contract_tool_is_byte_identical_across_http_and_stdio() {
    let ctx = common::contracts_t1_federation();
    let base = ctx.snapshot_all_at_tag("base");
    let s21 = ctx.snapshot_with("orders", "s21-code-only-handler");

    let calls: Vec<(&str, Value)> = vec![
        ("list_services",   serde_json::json!({"snapshot": base, "limit": 50})),
        ("list_contracts",  serde_json::json!({"snapshot": base, "limit": 100})),
        ("get_contract",    serde_json::json!({"snapshot": base, "key": "http:GET /api/orders/{}"})),
        ("get_service",     serde_json::json!({"snapshot": base, "service": "orders", "limit": 50})),
        ("get_coverage",    serde_json::json!({"snapshot": base})),
        ("list_unresolved", serde_json::json!({"snapshot": base, "limit": 50})),
        ("trace_impact",    serde_json::json!({"snapshot": base, "from": {"endpoint": {"service": "orders", "key": "http:GET /api/orders/{}"}}, "depth": 3})),
        ("diff_contracts",  serde_json::json!({"base": base, "head": s21})),
        ("resolve_evidence",serde_json::json!({"snapshot": base, "refs": ["orders:HttpRoute:src/main.rs:GET /api/orders/%3Aid:17"], "context_lines": 2})),
    ];

    for (tool, args) in calls {
        let h = ctx.call(tool, args.clone());
        let s = ctx.call_stdio(tool, args.clone());
        assert_eq!(
            serde_json::to_string(&h).unwrap(),
            serde_json::to_string(&s).unwrap(),
            "{tool} differs between HTTP and stdio"
        );
    }
}
```

Compare `structuredContent.data` with volatile keys (`meta`, `elapsed_ms`, `analyzer_version`) stripped — match the normalisation `tests/contract_mcp_parity.rs:216-348` already uses.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test command_center_contract_fixes every_contract_tool_is_byte_identical_across_http_and_stdio -- --nocapture`
Expected: FAIL — `get_service differs between HTTP and stdio`

- [ ] **Step 3: Fix whatever it catches**

Do **not** weaken the assertion. If a tool still diverges, file the cause as a new defect and fix it in this task before proceeding.

- [ ] **Step 4: Format and verify**

```bash
cargo fmt
cargo fmt --check && cargo clippy --all-targets -- -D warnings
cargo test --quiet --test command_center_contract_fixes
cargo test --quiet --test federation_contracts_e2e --test contract_mcp_parity --test contracts_golden --test contracts_soundness --test coverage_ledger --test snapshots_e2e
make schema && git diff --quiet docs/tool-schema.json || echo "tool-schema.json drifted"
```

Expected: `fmt --check` rc=0, clippy clean, all listed tests PASS, no schema drift.

- [ ] **Step 5: Commit**

```bash
git add -u
git commit -m "chore: rustfmt + HTTP/stdio parity harness over a CODEOWNERS fixture

contract_mcp_parity passes 8/8 on a mini-fixture with no CODEOWNERS,
which is precisely why it missed the get_service 'owners' divergence
(4 entries over one HTTP instance, absent over stdio and over a second
HTTP instance). Parity now runs every contract tool against the T1
fixture, which does have CODEOWNERS."
```

---

## Out of scope (do not implement here)

- **Defect J** (`Resource` / `Imports` / `DeployedTo` declared but unpopulated) — already correctly reported via `is_indexed=false` and pinned by `src/server/query/schema.rs:413`. No fix needed.
- **Defect K** (CODEOWNERS / entry points / env bindings write no graph nodes) — a model change that would need a `FEDERATION_GRAPH_VERSION` bump (`src/server/federation/graph_backend.rs:24`) and a recovery path via `lain reindex`. Separate plan.
- **`tests/real_federation/*`** is red for unrelated reasons (scripts pass `serde` to `prepare_snapshot` but the builder clones only `bytes`+`tokio`; `tools_smoke.sh` has stale argument shapes). Separate plan.
- Org-wide safety claims. `scope.configured_only` stays `true` and `caveats.unconfigured_scope` must remain rendered on every response.

## Review Focus → owning task

| Risk | Task | Pinning test |
|---|---|---|
| `owners: []` vs absent ambiguity | 3 | `owners_are_deterministic_across_instances_and_transports` |
| Cross-instance / cross-transport divergence | 3, 8 | same + `every_contract_tool_is_byte_identical_across_http_and_stdio` |
| Render-site fix without source fix | 1 | `handler_repo_is_the_provider_repo_not_a_sensor_name` |
| `HandlerChanged` over-firing | 6 | `schema_bearing_endpoint_unchanged_handler_emits_nothing`, `schema_less_endpoint_still_reports_changed_without_schema` |
| Assuming fixture precision is production precision | 7, 8 | tests assert only HTTP/topic behaviour; the gRPC/GraphQL/WS/SQL end-to-end gap is explicitly listed as unverified |

---

## Implementation notes (post-execution)

Three tasks needed a different fix than the plan specified. Recorded so
the reviewer checks the code, not the plan.

**Task 3 — owners.** The plan blamed `root.file_name()` keying alone.
That was necessary but not sufficient: the snapshot indexer
(`contracts/snapshots/jobs.rs::run_job_index`) passed
`source_repo: None` even though `repo_id: &str` was in scope, so the
sensor had no repo to key by on the path `get_service` actually uses.
Fixed both. `enrich_used_by_with_owners` now always emits `owners`
(empty array = "no owner declared") instead of omitting the key.

**Task 6 — HandlerChanged.** The plan's rule (schema-bearing on both
sides + a source file changed) over-fired badly:
`diff_precision` fell 1.0 → 0.53 on the T1 ground truth. Two precision
guards were required and are load-bearing:

1. Fire only when the schema diff reported *nothing* for this endpoint.
   A schema change already explains the diff (0.53 → 0.86).
2. Fire only when the changed handler file is claimed by exactly one
   endpoint. T1 scenario 6 renames a route string in `src/main.rs` with
   no handler body change, and the naive rule emitted three spurious
   leads (0.86 → 0.91).

With both, `diff_precision` and `diff_recall` are 1.0 again.

Known limit, deliberately not hidden: a real behaviour change in a
*shared* handler file is still not reported. File granularity cannot
attribute it, and reporting it per-endpoint is exactly the noise guard 2
removes. Closing that needs line- or hunk-level attribution.

**Task 7 — provenance.** The plan said to map `EdgeProvenance` through
`provenance_to_json`. That was wrong: the data was absent
(`GraphEdge::new` left `provenance: None`), not mis-mapped. Fixing the
label alone would have contradicted `traverse_impact`, which
deliberately maps `None` → 0.0 ("confidence must be evidence-backed").
Fixed at the source instead: `GraphEdge::new` now defaults to
`Static{TreeSitter}`, consistent with its sibling `new_heuristic` whose
doc already says to use the confidence-bearing builders from sensors.
Legacy edges deserialized without provenance still read as `None` and
are still 0.0 in both the label and the traversal.
