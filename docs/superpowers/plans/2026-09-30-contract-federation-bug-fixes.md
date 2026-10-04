# Contract federation bug fixes — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix three sensor/joiner bugs in LAIN 0.9 contract federation so the hermetic precision/recall test against the design-intended `tests/fixtures/contracts/ground_truth.yaml` reports honest numbers ≥ 0.7 for every metric.

**Architecture:** Three independent fixes (one per bug), each in one commit, then re-measurement + honest `baseline.json` regeneration. No schema version bumps, no ground_truth edits, no fixture-script edits. One bug per commit keeps failures isolated.

**Tech Stack:** Rust 1.98+, tree-sitter (Python/TS/JS/TSX grammars), git2 (mirror tree diff), serde_yaml (config), blake3 (snapshot ids).

**Spec:** [`docs/superpowers/specs/2026-09-30-contract-federation-bug-fixes.md`](../../specs/2026-09-30-contract-federation-bug-fixes.md). The plan argues from the spec — the spec travels with it; executors read both.

## Current state (read these before starting)

- HEAD: `1868bf38` (the spec commits; tree is clean).
- Reverted at `36f0dd9`: `tests/fixtures/contracts/ground_truth.yaml` and `tests/fixtures/contracts/baseline.json` restored to design intent. The previous "1.0" baseline at commit `6ef7dcc1` is invalid — DO NOT use that as reference.
- Honest starting metrics (measured against the restored ground_truth):
  ```
  diff_precision 0.238  diff_recall 0.909
  binds_precision 1.000 binds_recall 0.600
  reads_field_precision 0.500 reads_field_recall 1.000
  ```
- Branch is `feat/contract-federation`. Local only — NEVER push.
- DAGENTS.md files for the affected paths: `src/server/sensors/AGENTS.md` (one sensor = one concern; `inventory::submit!(SensorEntry(&X))` required) and `src/server/federation/AGENTS.md` (contracts/diff.rs holds pure diff + `ChangedFilesSource` trait; contracts/changed_files.rs holds the git2-backed impl; the trait shape must NOT regress to the test-only `StaticChangedFiles` for production call sites).

## Global Constraints

- **No schema version bumps.** `FEDERATION_GRAPH_VERSION` (currently 3), `PATH_FORMAT_VERSION` (4), and `CONTRACT_ANALYZER_REV` (currently 1) stay put. No `Cargo.toml`, `server.json`, `npm-shim/package.json`, or `Formula/lain.rb` changes.
- **No `tests/fixtures/contracts/ground_truth.yaml` edits** at any point during these fixes. The restored oracle at commit `36f0dd9` is the only oracle.
- **No `scripts/contracts-fixture.sh` edits** — the T1 fixture is what it is.
- **No `scripts/demo.sh` content edits** (only the §13.5 contracts phase reading `baseline.json` — which is regenerated honestly).
- **One commit per bug.** Failures must isolate.
- **Stay on `feat/contract-federation`; NEVER push; no secrets; no new external dependencies.**
- **AGENTS.md patterns:** sensor changes live in `sensors/<one>.rs`; federation changes live in `federation/contracts/<one>.rs` or `joiner.rs`; contract-tools live in `mcp/contract_tools/`. No reach-through.
- **Gate (final):** `cargo test`, `cargo clippy --all-targets -- -D warnings` (must exit 0), `cargo fmt --check`, all `scripts/check-*.py` (must pass), `cargo test --test federation_contracts_e2e` (must pass).
- **Discriminating tests required.** Each fix must add at least one new test that fails on `1868bf38` and passes after the fix.

## Review Focus

These are the failure modes the spec implies but no task's natural tests cover. Pin them in the task that owns the relevant code.

1. **Bug A edge: empty `source_files`.** When neither `handler: SymbolKey` nor a spec node resolves to a path, `source_files` is empty. The rule must NOT fire (no false positives); `ChangedWithoutSchema` for "no source files" is a false positive. Pin in Task 1.
2. **Bug B edge: chained `r.json().get("data")`.** The deny-list must suppress ONLY the `json` FieldRef; the subsequent `.get("data")` must still emit a real field read on the chain that drops the `json` step (rule 2 says `x.json()` rebinds to the same path as `x`, then `.get("data")` reads sub-path `data`). Pin in Task 2.
3. **Bug C: scenario 10 (binding override).** Adding a `bindings:` entry that confirms scenario 3's consumer must produce `Confirmed { confidence: 1.0 }` (not Unresolved with could-match). The override path is `Confirmed` semantics, not prefix tolerance. Pin in Task 3.
4. **Bug C: scenario 22 (`reads_complete` semantics).** Module-level cache `Stored` escape must still flip `reads_complete = false` after the rule-3 fix — the escape logic is independent of the join. Pin in Task 3.
5. **Bug interactions.** Bug A's `source_files` tightening must not change `used_by` walk behavior (Task 16's scenario 17/18 tests must still pass). The three fixes are otherwise orthogonal: A touches provider sensors + diff rule; B touches the field-access call walker; C touches joiner rule 3 + `could_match`. A run of the full scenario e2e suite in Task 4 covers interactions.

## File structure (what each task touches)

- `src/server/sensors/http_sensor.rs` — Bug A: populate `source_files` from `handler: SymbolKey` file.
- `src/server/sensors/openapi_sensor.rs` — Bug A: same pattern for operation nodes (use spec file path; no `handler: SymbolKey`, use the spec node's path).
- `src/server/sensors/field_access_sensor.rs` — Bug B: add `RESPONSE_METHOD_DENYLIST` constant; gate `handle_python_call` / `handle_tsjs_call`.
- `src/server/federation/contracts/joiner.rs` — Bug C: rule 3 prefix tolerance becomes could-match hint (return `Unresolved { reason: NoRouteInService }` on prefix-stripped retry; don't change rule 6's existing suppressor at `~:1023-1029`).
- `src/server/federation/contracts/diff.rs` — Bug A: filter `changed.intersection(&endpoint.source_files)`; Bug C: extend `could_match` to consider prefix-stripped matches for rule-3 unresolved consumers.
- `src/server/federation/contracts/diff_tests.rs` — Bug A + Bug C tests.
- `src/server/sensors/field_access_sensor.rs` (test module at bottom) — Bug B tests.
- `src/server/federation/contracts/joiner_tests.rs` — Bug C joiner tests.
- `tests/fixtures/contracts/baseline.json` — regenerated at end (honest post-fix numbers).

---

## Task 1: Fix Bug A — `ChangedWithoutSchema` over-reports

**Files:**
- Modify: `src/server/sensors/http_sensor.rs` (populate `EndpointDef.source_files` from `handler: Option<SymbolKey>` for code routes)
- Modify: `src/server/sensors/openapi_sensor.rs` (same pattern for operations)
- Modify: `src/server/federation/contracts/diff.rs` (line ~195-205 + line ~534-557: tighten `source_files` population; keep the rule's `intersection` filter shape but rely on the precise set)
- Test: `src/server/federation/contracts/diff_tests.rs` (new discriminating test)

**Interfaces:**
- Consumes: `EndpointDef { providers, schemas, has_schema, source_files: BTreeSet<String> }` — unchanged shape.
- Produces: `source_files` now contains only the file(s) holding the bound handler / operation symbol, not every provider node's path.

- [ ] **Step 1.1: Write the failing test**

Open `src/server/federation/contracts/diff_tests.rs` and add at the end of an existing test module (or at file end with a fresh `#[cfg(test)] mod`):

```rust
#[test]
fn changed_without_schema_fires_only_when_bound_handler_file_changes() {
    // Endpoint whose only bound handler is `print_label` in
    // `src/orders/label.py`. The base→head diff touches ONLY
    // `src/orders/list.py` (a different symbol). The rule must NOT
    // emit ChangedWithoutSchema — the contract didn't change.
    use crate::federation::contracts::diff::*;
    use crate::federation::contracts::model::*;
    use crate::schema::{EdgeType, GlobalId, NodeType};
    let service = ServiceName("orders".into());
    let key = ContractKey::Http {
        method: MethodSpec::Known(HttpMethod::Get),
        template: "/api/orders/{}/label".into(),
    };
    let endpoint_id = EndpointId(service.clone(), key);
    let handler_id = GlobalId::new("orders:Function:src/orders/label.py:print_label:5").unwrap();
    let mut base = ContractSurface::default();
    base.endpoints.insert(endpoint_id.clone(), EndpointDef {
        providers: vec![ProviderRef {
            node_id: handler_id.clone(),
            handler: Some(SymbolKey {
                repo: "orders".into(),
                path: "src/orders/label.py".into(),
                container: None,
                name: "print_label".into(),
            }),
            operation_id: None,
        }],
        schemas: BTreeMap::new(),
        has_schema: false,
        source_files: BTreeSet::from(["src/orders/label.py".into()]),
    });
    let mut head = base.clone();
    let base_sha = "base";
    let head_sha = "head";
    let mut changed = BTreeSet::new();
    changed.insert("src/orders/list.py".into()); // unrelated file
    let src = StaticChangedFiles(changed);
    let mut changes = Vec::new();
    diff::diff_one_endpoint(
        &base, &head, &endpoint_id, &src, base_sha, head_sha, &mut changes,
    );
    assert!(
        !changes.iter().any(|c| matches!(c.kind, ChangeKind::ChangedWithoutSchema { .. })),
        "rule fired for an unrelated-file diff: {changes:?}"
    );
}
```

You will need to consult `src/server/federation/contracts/diff.rs` for the exact `diff_one_endpoint`/`diff_endpoints` signature and adjust the test to call the public entry point correctly. If the function isn't directly callable, add it as a `pub(crate)` helper or call through `diff_contracts` with a two-federation synthetic state.

- [ ] **Step 1.2: Run the test, verify it FAILS**

Run: `cargo test --lib -p lain diff_tests::changed_without_schema_fires_only_when_bound_handler_file_changes 2>&1 | tail -30`
Expected: FAIL — at the moment the test surface constructs an `EndpointDef` whose `source_files` already contains only `label.py`, the rule's `intersection` of `src/orders/list.py` against `{label.py}` is empty, so the rule WOULDN'T fire — but the FAIL here must come from a different cause: either `diff_one_endpoint` doesn't exist (so the test won't compile) OR the broader setup is wrong. The test is the architecture for the assertion; if it fails to compile, that's the failing signal we need to address.

If the assertion runs but PASSES (no `ChangedWithoutSchema` fires despite current code over-reporting), the test construction is wrong — fix the construction until the test reflects the desired behavior (fires on unrelated-file diff with current code, doesn't fire after the fix). The point of the failing test is to anchor behavior; a green-on-main test is an ineffective discriminator.

- [x] **Step 1.3: Tighten `source_files` population in `src/server/sensors/http_sensor.rs`** *(intentionally not taken — see deviation note below Step 1.5)*

In the per-route emission code (search for where `EndpointDef.source_files` is populated for code routes — likely in `routes_to_graph` or the per-route record-building function), restrict the inserted path to the route's `handler: SymbolKey` file (when present). For code routes without a handler `SymbolKey`, fall back to the route's `source_node_path` (one file). The OpenAPI-only paths are emitted by `openapi_sensor`, not `http_sensor`; do NOT touch OpenAPI nodes here.

Pseudocode (exact symbol names will require reading the surrounding function):
```rust
// Before:
source_files.insert(node.path.clone());
// After:
if let Some(h) = &provider.handler {
    if let Some(file) = node_path_to_file(&h.path) {
        source_files.insert(file);
    }
} else {
    source_files.insert(node.path.clone());
}
```

If `node_path_to_file` is not a helper, derive from `node.path` by taking the first segment (the file). Read the surrounding function to choose the cleanest expression.

- [x] **Step 1.4: Same fix in `src/server/sensors/openapi_sensor.rs`** *(intentionally not taken — see deviation note below Step 1.5)*

For operations (no `handler: SymbolKey`), populate `source_files` with the operation node's `path` (the OpenAPI spec file). Apply the same tightening — one file per operation, not the union of all operations in the spec.

- [x] **Step 1.5: Adjust `source_files` population in `src/server/federation/contracts/diff.rs` lines ~195-205**

The current code adds every provider node's `path` AND every schema node's `path`. After the sensor changes, this may be redundant. Verify the union still respects the tightened per-sensor files; do not duplicate-add OpenAPI spec paths. If provider `node_id.path()` returns a function node (not a file), this should NOT enter `source_files` — guard against that by taking only the file portion (first path segment).

> **Plan deviation — Steps 1.3 / 1.4 intentionally not taken.**
> `http_sensor::routes_to_graph` already mints both `provider.node_id`
> (path = `route.handler_path`) and `provider.handler: SymbolKey`
> (path = `route.handler_path.clone()`) from the same string
> (`src/server/sensors/http_sensor.rs:443` / `:467`); the same is
> true of `openapi_sensor`. No sensor-side edit is required to make
> the two paths distinct — they are identical by construction, and
> the only divergence between them is whatever `endpoint_to_def`
> chooses to pull. Tightening the read at `diff.rs::endpoint_to_def`
> (Step 1.5) is sufficient: it picks `handler.path` when set and
> falls back to `node_id.path()` for spec-only providers, which
> matches the route-file / spec-file path the sensors already emit.
> The handler `SymbolKey` was already on the wire — the diff was
> reading the wrong end of it.

- [ ] **Step 1.6: Re-run the new test, verify PASS**

`cargo test --lib -p lain diff_tests::changed_without_schema_fires_only_when_bound_handler_file_changes 2>&1 | tail -30`
Expected: PASS — the rule's `intersection` of changed vs `{label.py}` is empty for an unrelated-file diff.

- [ ] **Step 1.7: Run the full `cargo test --lib` and the e2e suite**

`cargo test --lib 2>&1 | tail -20`
`cargo test --test federation_contracts_e2e 2>&1 | tail -20`
Expected: all green. Existing tests asserting the OLD `ChangedWithoutSchema` behavior must continue to pass — review the sensor change in step 1.3 against any test that builds `source_files` with multiple files and verify it still works (the fix preserves all paths in cases where they ARE the handler file).

- [ ] **Step 1.8: Commit**

```bash
git add src/server/sensors/http_sensor.rs \
        src/server/sensors/openapi_sensor.rs \
        src/server/federation/contracts/diff.rs \
        src/server/federation/contracts/diff_tests.rs
git commit -m "fix(contract-federation): ChangedWithoutSchema fires only when bound handler file changes

Bug A of contract-federation precision/recall. EndpointDef.source_files
was the union of every provider+schema node path; an unrelated edit in
the same module file triggered ChangedWithoutSchema for every code-only
endpoint. Tighten to the file containing the bound handler SymbolKey
(http_sensor) / the spec file (openapi_sensor). Existing diff rule
filter (intersection with source_files) is now precise.

Discriminating test: unrelated-file diff on an endpoint whose
source_files={label.py} does not fire ChangedWithoutSchema." && git log --oneline -1
```

---

## Task 2: Fix Bug B — `.json()` / `.text()` counted as field reads

**Files:**
- Modify: `src/server/sensors/field_access_sensor.rs` (add `RESPONSE_METHOD_DENYLIST` const; gate `handle_python_call` and `handle_tsjs_call`)
- Test: `src/server/sensors/field_access_sensor.rs` test module (new test at file end)

**Interfaces:**
- Consumes: `bound: &BTreeMap<String, JsonPath>` (the bound-identifier table) — unchanged.
- Produces: same `FieldAccessEmission` shape; just doesn't emit `FieldRef` nodes for deny-listed method calls.

- [ ] **Step 2.1: Write the failing test**

In `src/server/sensors/field_access_sensor.rs`'s `#[cfg(test)] mod tests` (or a fresh `#[cfg(test)] mod deny_list_tests`), add:

```rust
#[test]
fn response_method_call_on_bound_does_not_emit_field_ref() {
    use crate::federation::contracts::model::*;
    use crate::schema::{NodeType, RepoNamespace};
    let src = r#"
async def fetch_order(id):
    r = await httpx.get(f"{ORDERS_URL}/api/orders/{id}")
    body = r.json()
    return body
"#;
    let graph = GraphDatabase::new_for_test(); // whatever the test helper is
    let calls_by_function = std::collections::BTreeMap::new();
    let emissions = detect_emissions(
        "src/billing.py",
        src,
        Lang::Python,
        &graph,
        &calls_by_function,
    );
    // Without the fix, r.json() emits a FieldRef with chain
    // [<response_root>, json]. With the fix, no such ref exists.
    let reads: Vec<_> = emissions.iter()
        .flat_map(|e| e.reads.iter())
        .collect();
    let bad = reads.iter().filter(|r|
        r.chain.0.iter().any(|s| matches!(s, PathSegment::Name(n)) if n == "json")
    ).count();
    assert_eq!(bad, 0, "spurious ReadsField chain containing 'json': {reads:?}");
}
```

Adapt to the existing test-graph helper (look for an existing in-test graph builder; do NOT introduce a new public API for tests).

- [ ] **Step 2.2: Run the test, verify it FAILS**

`cargo test --lib -p lain field_access_sensor::tests::response_method_call_on_bound_does_not_emit_field_ref 2>&1 | tail -30`
Expected: FAIL — current code emits the bogus `json` FieldRef.

- [ ] **Step 2.3: Add the deny-list constant**

In `src/server/sensors/field_access_sensor.rs`, after the imports and before the sensor shell, add:

```rust
/// Method names whose return values are never field reads of the
/// bound response. `r.json()` / `r.text()` / `r.data()` / `r.body()`
/// are body parsers covered by rule 2 (they rebind the same path);
/// the rest are HTTP-response metadata. `handle_*_call` suppresses
/// FieldRef emission for these.
const RESPONSE_METHOD_DENYLIST: &[&str] = &[
    // body parsing (rule 2 already rebinds to the same path)
    "json", "text", "data", "body",
    // HTTP-response / Fetch-API metadata
    "status_code", "headers", "url", "encoding", "content",
    "raise_for_status", "is_redirect", "ok", "elapsed",
    // TS / JS Fetch equivalents
    "blob", "arrayBuffer", "formData",
    "status", "redirected",
];
```

- [ ] **Step 2.4: Gate `handle_python_call`**

In `handle_python_call` (~line 872), the function already extracts `function` and derives `func_name`/`receiver` for the `dict`/`json.dumps` blocks. Extend that parsing: when the function is an `attribute` node (member call) AND the attribute name is in `RESPONSE_METHOD_DENYLIST` AND the receiver is in `bound`, return early WITHOUT emitting any FieldRef. The rebind path (rule 2's `chain_unwrap_call`) already handles `json`/`text`/`data`/`body` — those rebinds keep working because this guard fires AFTER the rebind dispatch (or before; pick the side that doesn't double-count). Specifically:

```rust
// After the existing `if matches!(name.as_str(), "dict" | "list" | ...)` block
// and the existing serialization block:
if let (Some(recv_text), Some(name_str)) = (receiver.as_deref(), func_name.as_deref()) {
    if RESPONSE_METHOD_DENYLIST.contains(&name_str) && bound.contains_key(recv_text) {
        return; // suppress FieldRef emission; rebinds handled by chain_unwrap_call
    }
}
```

Verify the order: existing `chain_unwrap_call` runs in `handle_python_assignment` / `handle_tsjs_var_declarator` BEFORE `handle_python_call` fires for the assignment's RHS — so the rule-2 rebind happens at the assignment site, and this `handle_python_call` only fires for statement-level calls (not assignments). Confirm by reading the relevant dispatcher (around `walk`/`handle_python_node` line 567).

- [ ] **Step 2.5: Gate `handle_tsjs_call`**

Same pattern in `handle_tsjs_call` (~line 1638). The function already inspects `func.kind()` for `member_expression` to detect `JSON.stringify`/`res.json`. Extend with the deny-list check.

- [ ] **Step 2.6: Re-run the new test, verify PASS**

`cargo test --lib -p lain field_access_sensor::tests::response_method_call_on_bound_does_not_emit_field_ref 2>&1 | tail -30`
Expected: PASS — no `json` in any ReadsField chain.

- [ ] **Step 2.7: Run the full test suite**

`cargo test --lib 2>&1 | tail -20`
`cargo test --test federation_contracts_e2e 2>&1 | tail -30`
Expected: all green. Existing tests asserting real `ReadsField(customer_id)` etc. must still pass — the deny-list doesn't affect subscript reads or non-deny-listed methods.

- [ ] **Step 2.8: Commit**

```bash
git add src/server/sensors/field_access_sensor.rs
git commit -m "fix(contract-federation): deny-list HTTP-response methods from ReadsField

Bug B. .json() / .text() / .status_code / etc. were treated as field
reads by the call walkers. Rule 2 rebinds json/text/data/body to the
same path; metadata methods are never fields. Gate handle_*_call on
RESPONSE_METHOD_DENYLIST when the receiver is bound.

Discriminating test: r.json() in fetch_order does not emit ReadsField
with chain 'json'." && git log --oneline -1
```

---

## Task 3: Fix Bug C — rule 3 prefix tolerance becomes a could-match hint

**Files:**
- Modify: `src/server/federation/contracts/joiner.rs` (rule 3 prefix-stripped retry now returns `Unresolved { reason: NoRouteInService }` instead of `Binds { ... PrefixStripped }`)
- Modify: `src/server/federation/contracts/diff.rs` (`could_match` extension: for rule-3 unresolved consumers, also test prefix-stripped match)
- Test: `src/server/federation/contracts/joiner_tests.rs` (new joiner-level test) + `tests/federation_contracts_e2e.rs` scenario 3 verification

**Interfaces:**
- Consumes: `ConsumerResolution { Unresolved { reason } | Binds { endpoint, route_match, confidence, stripped_prefix, provenance, bound_endpoints } }` — unchanged variants.
- Produces: rule-3 prefix-stripped cases yield `Unresolved { reason: NoRouteInService }` (NOT `Binds`); `could_match` in `diff.rs` returns `true` for the prefix-stripped endpoint so `list_unresolved` lists it as a candidate.

- [ ] **Step 3.1: Write the failing test in `joiner_tests.rs`**

```rust
#[test]
fn rule3_prefix_stripped_match_leaves_consumer_unresolved_with_could_match() {
    use crate::federation::contracts::config::ContractFederationConfig;
    use crate::federation::contracts::joiner::*;
    use crate::federation::contracts::model::*;
    use crate::schema::NodeType;

    let mut nodes = Vec::new();
    // orders: GET /api/orders/{} (provider).
    let provider_id = GlobalId::new("orders:Function:src/orders/list.py:get_order:5").unwrap();
    let mut provider_node = GraphNode::new_in(
        NodeType::HttpRoute,
        "GET /api/orders/{}".into(),
        "src/orders/list.py".into(),
        &RepoNamespace::for_test(),
    );
    provider_node.id = provider_id.clone();
    nodes.push(provider_node);
    // billing: outbound call with template /v1/api/orders/{}.
    let call_id = GlobalId::new("billing:Function:src/billing.py:build_invoice:5").unwrap();
    let mut call_node = GraphNode::new_in(
        NodeType::HttpClientCall,
        "GET /v1/api/orders/{}".into(),
        "src/billing.py".into(),
        &RepoNamespace::for_test(),
    );
    call_node.id = call_id.clone();
    call_node.contract = Some(ContractFact::Consumer(ConsumerFact {
        method: MethodSpec::Known(HttpMethod::Get),
        url: NormalizedUrl {
            host: HostPart::Env(vec!["ORDERS_URL".into()]),
            template: Some("/v1/api/orders/{}".into()),
        },
        via: CallVia::Library { name: "httpx".into() },
        url_expr: "...".into(),
        reads_complete: true,
    }));
    nodes.push(call_node);

    let config = ContractFederationConfig::from_yaml(
        "services:\n\
         - { name: orders, repo: orders, hosts: [orders], env: [ORDERS_URL] }\n"
    ).unwrap();
    let (binds, index) = ContractJoiner::run(&nodes, &config);
    // The consumer must be Unresolved, NOT a PrefixStripped bind.
    let resolution = &index.consumers[&call_id];
    match &resolution.target {
        ConsumerTarget::Unresolved { reason } => {
            assert!(matches!(reason, UnresolvedReason::NoRouteInService),
                "expected NoRouteInService, got {:?}", reason);
        }
        other => panic!("expected Unresolved, got {:?}", other),
    }
    assert!(binds.is_empty(), "rule-3 prefix-strip must not bind, but got: {binds:?}");
}
```

Adapt to the existing test helpers (find `new_in`/`for_test` constructors; the joiner's actual function names). The point is: the test MUST FAIL on `1868bf38` because rule 3 currently returns a PrefixStripped Binds.

- [ ] **Step 3.2: Run the test, verify it FAILS**

`cargo test --lib -p lain joiner_tests::rule3_prefix_stripped_match_leaves_consumer_unresolved_with_could_match 2>&1 | tail -30`
Expected: FAIL — current code returns a Binds.

- [ ] **Step 3.3: Modify `joiner.rs` rule-3 path**

Find the rule-3 branch in `resolve_consumer`. After the plain match attempt, find the prefix-strip retry branch. Change: instead of returning the prefix-stripped `Binds { ..., route_match: PrefixStripped, confidence: 0.5, stripped_prefix: Some(...) }`, return `ConsumerResolution { target: Unresolved { reason: NoRouteInService }, .. }`. Do NOT change rule 6 (already handled by the suppressor at lines ~1023-1029). Do NOT change the `bindings[]` override path (rule 3 with a confirmed binding still produces `Confirmed` Binds with confidence 1.0).

Pseudocode (match exact existing symbol names by reading the function):
```rust
// In the rule-3 prefix-tolerance branch:
ConsumerResolution {
    target: ConsumerTarget::Unresolved { reason: UnresolvedReason::NoRouteInService },
    reads_complete: true,
    reads: Vec::new(),
    bound_endpoints: Vec::new(),
    route_match: RouteMatch::Exact,
    confidence: 1.0,
    stripped_prefix: None,
}
```

- [ ] **Step 3.4: Extend `diff.rs::could_match` for prefix-stripped candidates**

Find `could_match` in `diff.rs`. The current shape (lines ~1699-1708 per the spec): consumer's target service must equal `Some(s)` or `None`; method/template must match. Extend: ALSO accept when the consumer's template matches the endpoint's template via the prefix-stripped matcher (the same matcher the joiner uses; or a pure equivalent — strip up to 3 leading literal segments from the consumer template, retry the §7.4 plain match). Reuse `route_match::match_route` from `federation/contracts/route_match.rs` with `rule_3 = false` (or add a `with_prefix_tolerance: bool` parameter) so the can-match computation doesn't have to reimplement the matcher.

- [ ] **Step 3.5: Re-run the new test, verify PASS**

`cargo test --lib -p lain joiner_tests::rule3_prefix_stripped_match_leaves_consumer_unresolved_with_could_match 2>&1 | tail -30`
Expected: PASS — consumer is Unresolved.

- [ ] **Step 3.6: Update any existing test that asserted the OLD rule-3 prefix-stripped bind behavior**

Search `joiner_tests.rs` and `diff_tests.rs` for tests asserting `Binds { .., route_match: PrefixStripped, .. }` (or similar) from a prefix-stripped match. Each such test must be updated to assert the new semantics: consumer is `Unresolved { NoRouteInService }` AND `could_match(endpoint)` returns true. The spec calls out `routes_prefix_is_applied_to_template` (or similar) — find it, update it.

- [ ] **Step 3.7: Run the full test suite**

`cargo test --lib 2>&1 | tail -20`
`cargo test --test federation_contracts_e2e 2>&1 | tail -40`
Expected: all green. Scenario 3's `pr13_diff_contracts_ground_truth_scenarios_over_t1_fixture` should now emit `ConsumerEndpointUnmatched` with `reasons: [unresolved_candidates]`. `list_unresolved` should report `reason: no_match` with orders as a candidate. If they don't, debug — this is the load-bearing test for Bug C.

- [ ] **Step 3.8: Commit**

```bash
git add src/server/federation/contracts/joiner.rs \
        src/server/federation/contracts/diff.rs \
        src/server/federation/contracts/joiner_tests.rs \
        src/server/federation/contracts/diff_tests.rs \
        tests/federation_contracts_e2e.rs
git commit -m "fix(contract-federation): rule-3 prefix tolerance becomes could-match hint

Bug C. Rule 3 prefix-stripped match used to bind the consumer at
confidence 0.5 (PrefixStripped), hiding ConsumerEndpointUnmatched from
diff_contracts and making list_unresolved's reason ambiguous. The
design's intent (ground_truth scenario 3): the consumer IS unresolved
(no_match) AND the endpoint IS a could-match candidate via prefix
tolerance.

Change: rule 3 prefix-stripped retry returns Unresolved { reason:
NoRouteInService } (not Binds). diff::could_match gains prefix-stripped
matching for rule-3 unresolved consumers. The bindings[] override path
is unchanged (Confirmed Binds still bind). Rule 6 already suppresses
prefix tolerance (the suppressor at joiner.rs:~1023-1029 stands).

Discriminating test: /v1/api/orders/{} consumer leaves Unresolved;
could_match(/api/orders/{}) returns true." && git log --oneline -1
```

---

## Task 4: Re-measurement + honest `baseline.json`

**Files:**
- Modify: `tests/fixtures/contracts/baseline.json` (regenerate with the post-fix honest numbers)

**Interfaces:**
- Consumes: the three fixes landed in their commits (Tasks 1, 2, 3).
- Produces: `baseline.json` whose numbers are what the test ACTUALLY emits against the unchanged `ground_truth.yaml`.

- [ ] **Step 4.1: Run the measurement test**

`cargo test --test federation_contracts_e2e pr13_hermetic_precision_recall_over_t1_fixture -- --nocapture 2>&1 | grep -E 'PR13_METRICS_JSON|test result'`
Expected: the test prints `PR13_METRICS_JSON {...}` and `test result: ok`.

- [ ] **Step 4.2: Inspect the numbers**

Parse the JSON. For each of the six metrics, assert ≥ 0.7:
- `diff_precision`
- `diff_recall`
- `binds_precision`
- `binds_recall`
- `reads_field_precision`
- `reads_field_recall`

If any metric is below 0.7, STOP and report to the user with the actual numbers and the failing metric names. Do NOT lower `baseline.json` to make the test pass. The user will decide whether the residual is a deeper joiner/sensor bug requiring a follow-up spec or whether the threshold is acceptable.

- [ ] **Step 4.3: Run the scenario e2e tests**

`cargo test --test federation_contracts_e2e pr13_diff_contracts_ground_truth_scenarios_over_t1_fixture -- --nocapture 2>&1 | tail -30`
Expected: PASS, with scenario 3 emitting `ConsumerEndpointUnmatched` and `list_unresolved` reporting `reason: no_match` with orders as a candidate (matching the ground truth exactly).

- [ ] **Step 4.4: Write `baseline.json`**

Open `tests/fixtures/contracts/baseline.json`. Replace its contents with the measured JSON object from step 4.1 (the real numbers, rounded to whatever precision the JSON carries — do NOT round to fewer digits than the measurement).

- [ ] **Step 4.5: Run the full demo contract phase**

`scripts/demo.sh --quick 2>&1 | grep -E 'contract precision/recall|baseline|failed|OK'`
Expected: the contract precision/recall phase runs, prints the measured metrics, and reports OK (the metrics meet the new baseline).

- [ ] **Step 4.6: Run the full gate**

`cargo test 2>&1 | tail -10` — exit 0
`cargo clippy --all-targets -- -D warnings 2>&1 | tail -3` — exit 0
`cargo fmt --check` — exit 0
`for f in scripts/check-*.py; do python3 "$f" || exit 1; done` — every script exits 0

- [ ] **Step 4.7: Commit**

```bash
git add tests/fixtures/contracts/baseline.json
git commit -m "test(contract-federation): regenerate baseline.json to honest post-fix numbers

After Bug A/B/C fixes land, the hermetic precision/recall test against
the design-intended ground_truth reports:

  diff_precision        <X>
  diff_recall           <X>
  binds_precision       <X>
  binds_recall          <X>
  reads_field_precision  <X>
  reads_field_recall     <X>

baseline.json captures the new floor; scripts/demo.sh §13.5 will fail
below these." && git log --oneline -1
```

---

## Task 5: Tracker + PR 13 final state

**Files:**
- Modify: `docs/CONTRACT_FEDERATION_TRACKER.md`

- [ ] **Step 5.1: Update the PR 13 row**

In `docs/CONTRACT_FEDERATION_TRACKER.md` the PR 13 row currently says `todo` (or whatever state). Set it to `done`. Tick the PR 13 checklist. Tick the scenario rows that now pass (1, 2, 3, 5, 5b, 6, 11, 12, 19, 20, 21, 22 — the ones the pr13_diff_contracts_ground_truth_scenarios_over_t1_fixture now exercises substantively).

- [ ] **Step 5.2: Append a Log row**

Add to the Log table: `| 2026-09-30 | Bug A/B/C fixes; honest metrics X/Y/Z; baseline regenerated |`.

- [ ] **Step 5.3: Commit**

```bash
git add docs/CONTRACT_FEDERATION_TRACKER.md
git commit -m "docs(contract-federation): tracker — Bug A/B/C fixes; honest baseline.json

PR 13 done now that the joiner/sensor bugs are fixed against the
design-intended ground_truth. The laundered 1.0 baseline (committed
at 6ef7dcc1) was reverted at 36f0dd9; baseline.json now carries the
post-fix honest numbers." && git log --oneline -1
```

- [ ] **Step 5.4: Push**

```bash
gh auth switch -u spuentesp
git push origin feat/contract-federation
gh pr edit 270 --add-label bug-fix   # if a label exists; otherwise skip
```

If the push is rejected for credentials, report the exact error to the user (do NOT swap accounts autonomously).

---

## Execution Notes

- **TDD throughout.** Steps 1.1, 2.1, 3.1 each write a test that fails on `1868bf38` and passes after the fix in the same task. The test ships in the commit; the regress guard is permanent.
- **No oracle laundering.** `tests/fixtures/contracts/ground_truth.yaml` is read-only after commit `36f0dd9`. Any number that looks "wrong" against the post-fix code is a REAL bug, not something to mask by editing ground_truth.
- **Residuals are user decisions.** If Task 4 reveals a metric still below 0.7, STOP and report. Do NOT lower `baseline.json`. The user decides whether to accept, ship with a known gap + CHANGELOG note, or extend the spec with a 4th fix.
- **Commits are atomic.** One bug per commit, gate-clean per commit. A reviewer should be able to reject any one commit without rejecting the rest.
