# Contract-Surface Soundness + Verification Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the remaining ways LAIN can answer "no impact" when it has not actually checked, and prove the non-HTTP protocols are queryable end-to-end through the public contract tools.

**Architecture:** One soundness fix in `diff.rs` (the `could_match` predicate refuses every non-HTTP protocol, so unresolved topic/RPC/GraphQL consumers never block `NoKnownImpact`), then three verification tasks that turn "a sensor exists" into "a path is queryable", then hygiene. Everything here is reachable through the existing 13 contract tools — no new tools, no new transports.

**Tech Stack:** Rust 2021, `serde_json`, `git2`. Oracle: `cargo test`. Mutation floors via `scripts/mutation-check.py --check-floor`. TLA+ via `tools/tla/tlc` where a state-machine property is involved.

**Spec:** `docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` — invariant **I3 (Verdict soundness)**: `NoKnownImpact(change)` ⇒ every repo in scope is `Analyzed` for every protocol **and no could-match unresolved consumer exists**. Also `docs/formal/CoverageClaim.tla`, which models I3.

## Global Constraints

- **Never claim absent when unanalyzed.** Any path that can answer "no impact" must first establish the scope was analysed. `src/server/federation/contracts/coverage.rs`'s tri-state `LookupResult` is the mechanism; `diff.rs::evaluate`'s coverage gate is the enforcement.
- **`could_match` must be conservative.** Returning `true` costs a `NeedsInvestigation` (a lead). Returning `false` wrongly permits `NoKnownImpact` (a wrong answer). When uncertain, return `true`.
- **T1 ground truth must stay at precision **and** recall 1.0.** `pr13_hermetic_precision_recall_over_t1_fixture` asserts all six metrics; a panic there is a soundness regression, not a test to update.
- **No destructive git.** Never `git reset --hard`, `git checkout --`, `git stash`, `git clean`, `git restore`. **Never launch a detached/background process** — four agents corrupted production code by leaving mutation runners running past their own completion. Run everything in the foreground.
- Every mutation applied by hand is reverted **by editing the line back**. `git diff --stat src/` before every commit.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` clean at every commit. No version bumps (`AGENTS.md`).

## Review Focus

1. **`could_match` returning `false` is the dangerous direction.** Every non-HTTP consumer is currently a silent `false`. Any fix that keeps a `false` arm for a protocol must prove why that protocol's consumer cannot be affected — not just that matching is hard.
2. **"A sensor emits nodes" is not "a path is queryable."** Tasks 3 and 4 must drive the *public contract tools* over a fixture containing the protocol, not call the joiner directly. A green unit test on a hand-built graph proves nothing about `get_contract`/`trace_impact`.
3. **Fixture precision is not production precision.** A four-repo fixture with one gRPC service proves the plumbing, not recall. Every e2e assertion must be phrased as "this specific consumer binds to this specific endpoint", never "at least one thing happened".
4. **Coverage-completeness is the load-bearing gate.** `evaluate` downgrades `NoKnownImpact` → `NeedsInvestigation` when coverage is incomplete. If a new code path bypasses `coverage_complete`, the whole I3 story collapses silently — Task 1 must assert the downgrade still fires for non-HTTP too.
5. **Determinism bugs hide in `find`.** `field_join.rs:228` picks the first matching Schema node. Fixtures that happen to insert in a lucky order will pass. The test must construct the collision explicitly.

---

## File Structure

| File | Responsibility after this plan |
|---|---|
| `src/server/federation/contracts/diff.rs` | `could_match` — the conservative "could this unresolved consumer be affected?" predicate; `evaluate`'s coverage gate |
| `src/server/federation/contracts/field_join.rs` | Schema→field binding; the `find` tie-break |
| `tests/contract_tool_e2e_non_http.rs` | **NEW.** End-to-end `get_contract` / `trace_impact` / `list_unresolved` per non-HTTP protocol |
| `tests/fixtures/contracts-t4/` | **NEW.** Four-repo fixture with one gRPC, one GraphQL, one WebSocket and one SQL service (extend `scripts/contracts-fixture.sh`) |
| `tests/real_federation/*.sh` | Repair the red suite; add verdict assertions |
| `docs/formal/CoverageClaim.tla` | I3 model — extend the `could_match` clause to cover non-HTTP |
| `docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` | Record invariant I8; note the `could_match` clause |

---

## P0 — The soundness hole

### Task 1: `could_match` must cover non-HTTP protocols

**The defect.** `src/server/federation/contracts/diff.rs:2185-2210` returns `false` for `Topic`, `Rpc`, `Graphql` and `Table` — on **both** the consumer key and the endpoint key:

```rust
ConsumerTargetKey::Contract(ContractKey::Topic { .. }) => return false,
ConsumerTargetKey::Contract(ContractKey::Rpc { .. }) => return false,
ConsumerTargetKey::Contract(ContractKey::Graphql { .. }) => return false,
…
ContractKey::Topic { .. } => return false,
ContractKey::Rpc { .. } => return false,
ContractKey::Graphql { .. } => return false,
ContractKey::Table { .. } => return false,
```

Consequence: an unresolved **topic** consumer never "could match" a topic endpoint, so the `unresolved_candidates` rule at `diff.rs:1754` never fires for it, so `evaluate` can conclude `NoKnownImpact` for a change to that topic's producer while a consumer that may read it is still unresolved. That is a direct **I3 violation** and the exact failure the coverage ledger exists to prevent.

Note `WebSocket` *is* handled (`MethodSpec::Any` + route template) and `UrlExpr` falls through to `(Unknown, None)` → `true`. So the code already knows how to be conservative; the four protocol arms just give up.

**Files:**
- Modify: `src/server/federation/contracts/diff.rs:2185-2210` (the two `return false` ladders) and `:2215-2225` (the template check, which also `return false`s for non-HTTP endpoints)
- Test: `src/server/federation/contracts/diff_tests.rs`

**Interfaces:**
- Consumes: `ContractKey` variants from `src/server/federation/contracts/model.rs:545+` (`Topic{broker,name}`, `Rpc{system,service,method}`, `Graphql{op,field}`, `Table`).
- Produces: `could_match(&ConsumerKey, &EndpointId, &ContractSurface) -> bool` is **conservative** for every protocol — `true` when a match cannot be ruled out.

- [ ] **Step 1: write the failing tests** in `diff_tests.rs`. Each asserts a specific unresolved consumer blocks `NoKnownImpact` on its protocol's endpoint:

```rust
// Build one unresolved consumer of each non-HTTP kind and one endpoint
// of the same kind, run `evaluate`, and assert the verdict is
// `NeedsInvestigation` — never `NoKnownImpact`.

#[test]
fn an_unresolved_topic_consumer_blocks_no_known_impact() {
    let (base, head) = surfaces_with_unresolved_consumer(
        ConsumerTargetKey::Contract(ContractKey::Topic {
            broker: "kafka".into(),
            name: "orders.created".into(),
        }),
        (ServiceName("orders".into()), ContractKey::Topic {
            broker: "kafka".into(),
            name: "orders.created".into(),
        }),
    );
    let change = ChangeKind::FieldRemoved {
        endpoint: /* the topic endpoint id */,
        direction: Direction::Payload,
        path: path(&["order_id"]),
    };
    let impact = evaluate(&change, &base, &head, /* coverage */ &full_coverage());
    assert_ne!(
        impact.class,
        Class::NoKnownImpact,
        "an unresolved topic consumer must block NoKnownImpact: {impact:?}"
    );
}

#[test]
fn an_unresolved_rpc_consumer_blocks_no_known_impact() { /* same shape, ContractKey::Rpc */ }

#[test]
fn an_unresolved_graphql_consumer_blocks_no_known_impact() { /* ContractKey::Graphql */ }

#[test]
fn an_unresolved_table_consumer_blocks_no_known_impact() { /* ContractKey::Table */ }

#[test]
fn could_match_is_conservative_when_the_consumer_key_is_unknown() {
    // A `UrlExpr` consumer against a Topic endpoint: we cannot rule it
    // out, so it must could-match.
    assert!(could_match(&url_expr_key, &topic_endpoint_id, &surface));
}

#[test]
fn a_non_http_endpoint_still_allows_no_known_impact_when_nothing_is_unresolved() {
    // The fix must not over-report: a clean scope with no unresolved
    // consumers keeps `NoKnownImpact`.
    let impact = evaluate(&change, &clean_base, &clean_head, &full_coverage());
    assert_eq!(impact.class, Class::NoKnownImpact);
}
```

Adapt `surfaces_with_unresolved_consumer` / `full_coverage` to whatever helpers `diff_tests.rs` already has — read `topic_payload_schema_removal_reports_breaking_change` (`diff_tests.rs` ~`:2895`) and the `could_match` tests at `:1358-1601` for the exact construction. Do not invent helpers that already exist.

- [ ] **Step 2: run and confirm they FAIL for the right reason** (verdict is `NoKnownImpact`, not a compile error).

Run: `cargo test --quiet --lib diff_tests 2>&1 | tail -30`
Expected: the four `blocks_no_known_impact` tests FAIL with `assertion left == right failed: NoKnownImpact`.

- [ ] **Step 3: implement conservatively.** Replace the `return false` arms with key matching, and where matching is uncertain return `true`:

```rust
    // Method + template check (HTTP / WebSocket only).
    let (consumer_method, consumer_template) = match &consumer_key.target {
        ConsumerTargetKey::Contract(ContractKey::Http { method, template }) => {
            (method.clone(), Some(template.clone()))
        }
        ConsumerTargetKey::Contract(ContractKey::WebSocket { route }) => (
            MethodSpec::Known(HttpMethod::Any),
            Some(route.clone()),
        ),
        // Non-HTTP protocols have no method/template axis. They are
        // matched on their own key below; do NOT `return false` here,
        // which is what let `NoKnownImpact` through while an
        // unresolved topic/RPC/GraphQL/Table consumer existed.
        ConsumerTargetKey::Contract(
            ContractKey::Topic { .. }
            | ContractKey::Rpc { .. }
            | ContractKey::Graphql { .. }
            | ContractKey::Table { .. },
        ) => {
            return non_http_could_match(&consumer_key.target, &endpoint.1);
        }
        ConsumerTargetKey::UrlExpr(_) => (MethodSpec::Unknown, None),
    };
```

and add the shared helper (in `diff.rs`, next to `could_match`):

```rust
/// §9.7 for the non-HTTP protocols. Conservative by construction:
/// we return `true` unless a match can be positively ruled out.
/// Returning `true` costs a `NeedsInvestigation`; returning `false`
/// wrongly permits `NoKnownImpact` (I3).
fn non_http_could_match(target: &ConsumerTargetKey, endpoint_key: &ContractKey) -> bool {
    use ContractKey::*;
    let ConsumerTargetKey::Contract(tc) = target else {
        // A URL-shaped consumer against a non-HTTP endpoint cannot be
        // ruled out from the URL alone.
        return true;
    };
    match (tc, endpoint_key) {
        (Topic { broker: cb, name: cn }, Topic { broker: eb, name: en }) => {
            // Same topic name; brokers are interchangeable when either
            // side defaulted. Unknown in either direction -> true.
            cn == en && (cb == eb || cb.is_empty() || eb.is_empty())
        }
        (Rpc { service: cs, method: cm, .. }, Rpc { service: es, method: em, .. }) => {
            // Method must agree; a package-qualified service may differ
            // by suffix, so only a hard mismatch rules it out.
            cm == em && (cs == es || cs.ends_with(es.as_str()) || es.ends_with(cs.as_str()))
        }
        (Graphql { field: cf, .. }, Graphql { field: ef, .. }) => cf == ef,
        (Table { .. }, Table { .. }) => {
            // Table identity is not carried on the key; cannot rule out.
            true
        }
        // Different protocol families can be ruled out.
        _ => false,
    }
}
```

Then delete the four `ContractKey::Topic/Rpc/Graphql/Table => return false` arms from the **endpoint** ladder (`:2204-2210`) — they are unreachable once the consumer arm returns early — and make the template check skip non-HTTP endpoints instead of `return false`:

```rust
    if let Some(template) = consumer_template.as_deref() {
        match &endpoint.1 {
            ContractKey::Http { template: et, .. } => {
                if !template_matches(template, et) {
                    return false;
                }
            }
            // WebSocket routes were already matched above; other
            // families never reach here.
            _ => {}
        }
    }
```

Check `ContractKey`'s field names against `model.rs:545+` before writing — if `Topic` carries `broker`/`name` and `Rpc` carries `system`/`service`/`method` as the plan assumes, the match arms compile as written.

- [ ] **Step 4: run and verify GREEN**, including the negative test.

Run: `cargo test --quiet --lib diff_tests && cargo test --quiet --lib 'federation::contracts' 2>&1 | tail -6`
Expected: all PASS. The `no_known_impact_when_nothing_is_unresolved` test must still pass — that is the over-reporting guard.

- [ ] **Step 5: confirm the coverage gate still downgrades.** The `evaluate` gate at `diff.rs:1731-1786` downgrades `NoKnownImpact` → `NeedsInvestigation` when any in-scope repo's ledger is incomplete. Add a case asserting that still fires for a **topic** change with an incomplete ledger (today it is only tested for HTTP):

```rust
#[test]
fn incomplete_coverage_still_downgrades_a_topic_change() {
    let impact = evaluate(&topic_change, &base, &head, &incomplete_coverage());
    assert_eq!(impact.class, Class::NeedsInvestigation);
}
```

- [ ] **Step 6: extend the TLA+ model.** `docs/formal/CoverageClaim.tla` models I3. Its `CouldMatch` clause is the same predicate — extend it so the state space includes a non-HTTP consumer and confirm TLC still finds no counterexample (or, if it finds one, that is the expected outcome and must be recorded like `ScanRetract` was).

Run: `./tools/tla/tlc docs/formal/CoverageClaim.tla`
Expected: `Model checking completed. No error has been found.` — or a recorded counterexample with the code line it confirms.

- [ ] **Step 7: commit.**

```bash
git add src/server/federation/contracts/diff.rs src/server/federation/contracts/diff_tests.rs docs/formal/
git commit -m "fix(contracts): could_match covers non-HTTP protocols (I3)"
```

---

## P1 — Turn "a sensor exists" into "a path is queryable"

### Task 2: A four-repo fixture with one service per non-HTTP protocol

Nothing today drives the **public contract tools** over a fixture containing gRPC, GraphQL, WebSocket or SQL. The sensors emit nodes and the joiner binds them, and unit tests pass — but "sensor exists" is not "complete provider-to-consumer path is queryable".

**Files:**
- Modify: `scripts/contracts-fixture.sh` (add protocol services to the T1 fixture, or add a sibling `scripts/contracts-fixture-t4.sh`)
- Create: `tests/contract_tool_e2e_non_http.rs`
- Test: same

**Interfaces:**
- Consumes: `tests/support/contracts_snapshot_harness.rs` (`build_fixture`, `manager`, `contract_config`, `prepare_ready`, `snapshot_ctx`) and the `*_handle` test hooks in `src/server/mcp/contract_tools/`.
- Produces: a fixture whose repos each expose exactly one non-HTTP service, and e2e assertions on `get_contract` / `trace_impact` / `list_unresolved` output.

- [ ] **Step 1: extend the fixture.** Add to `scripts/contracts-fixture.sh` (or a t4 sibling) four services alongside the existing five:

  | repo | protocol | provider | consumer |
  |---|---|---|---|
  | `orders` | gRPC | `proto/orders.proto` with `rpc GetOrder` | — |
  | `billing` | GraphQL | — | `gql` query selecting `orders { id customer_id }` |
  | `reports` | WebSocket | `app.ws("/feed", …)` | — |
  | `platform` | SQL | — | `cursor.execute("SELECT id FROM shipments")` |

  Keep it deterministic like the existing fixture — fixed `GIT_AUTHOR_DATE`, `base` + scenario tags.

- [ ] **Step 2: write the failing e2e tests.** Each asserts a *specific* path through a *public tool*, not a count:

```rust
#[tokio::test]
async fn grpc_provider_and_consumer_are_queryable_through_get_contract() {
    let ctx = harness::snapshot_ctx(&mgr, &status);
    let base = harness::prepare_ready(&mgr, all_at("base"), None, config.clone()).await;

    let out = get_contract_handle(&ctx, json!({"snapshot": base, "key": "rpc:orders.GetOrder"})).await.unwrap();
    let data = &out.structured["data"];
    let item = &data["items"][0];

    assert_eq!(item["endpoint"]["service"], json!("orders"));
    // The specific consumer, not "some consumer".
    let consumers = item["consumers"].as_array().expect("consumers");
    assert!(
        consumers.iter().any(|c| c["site"]["id"].as_str().unwrap_or("").contains("billing")),
        "billing's stub call must be listed as a consumer: {consumers:?}"
    );
    // …and its schema fields must be joinable.
    assert!(item["schemas"].as_array().map(|s| !s.is_empty()).unwrap_or(false));
}

#[tokio::test]
async fn graphql_selected_fields_are_joinable_through_trace_impact() {
    let base = harness::prepare_ready(&mgr, all_at("base"), None, config.clone()).await;
    let out = trace_impact_handle(&ctx, json!({
        "snapshot": base,
        "from": {"endpoint": {"service": "orders", "key": "graphql:Query.orders"}},
        "depth": 3
    })).await.unwrap();
    let paths = out.structured["data"]["paths"].as_array().expect("paths");
    // The specific field the query selects, not "some hop".
    assert!(
        paths.iter().flat_map(|p| p["hops"].as_array().unwrap())
             .any(|h| h["node"].as_str().unwrap_or("").contains("customer_id")),
        "the selected field must appear on an impact path: {paths:?}"
    );
}

#[tokio::test]
async fn websocket_provider_reaches_its_consumer_in_trace_impact() {
    let base = harness::prepare_ready(&mgr, all_at("base"), None, config.clone()).await;
    let out = trace_impact_handle(&ctx, json!({
        "snapshot": base,
        "from": {"endpoint": {"service": "reports", "key": "websocket:/feed"}},
        "depth": 3
    })).await.unwrap();
    let paths = out.structured["data"]["paths"].as_array().expect("paths");
    assert!(
        paths.iter().flat_map(|p| p["hops"].as_array().unwrap())
             .any(|h| (h["edge"].as_str().unwrap_or("") == "Binds")
                   && h["node"].as_str().unwrap_or("").contains("billing")),
        "the WS consumer's bind must be on the path: {paths:?}"
    );
}

#[tokio::test]
async fn sql_table_reads_are_listed_through_get_contract() {
    let base = harness::prepare_ready(&mgr, all_at("base"), None, config.clone()).await;
    let out = get_contract_handle(&ctx, json!({"snapshot": base, "key": "table:shipments"}))
        .await.unwrap();
    let item = &out.structured["data"]["items"][0];
    assert_eq!(item["endpoint"]["service"], json!("platform"));
    let consumers = item["consumers"].as_array().expect("consumers");
    assert!(
        consumers.iter().any(|c| c["site"]["id"].as_str().unwrap_or("").contains("platform")),
        "the SQL reader must be listed: {consumers:?}"
    );
}

#[tokio::test]
async fn each_protocol_reports_where_the_graph_cannot_establish_a_data_origin() {
    // The workbench must be able to show "we do not know" — assert
    // `list_unresolved` and `get_coverage.schemaless_endpoints` name
    // the protocols that have no schema, rather than staying silent.
}
```

Run: `cargo test --test contract_tool_e2e_non_http 2>&1 | tail -30`
Expected: FAIL. For each protocol, determine *which* layer is missing (sensor not emitting, joiner not binding, tool not rendering) and say which in your report — that is the actual finding.

- [ ] **Step 3: fix the weakest layer only.** If the sensor emits and the joiner binds but the tool drops it, fix the tool. If the joiner never binds, that is a Task-1-class soundness bug — report it rather than papering over it in the test.

- [ ] **Step 4: verify GREEN + T1 unchanged.**

Run: `cargo test --test contract_tool_e2e_non_http && cargo test --quiet --test federation_contracts_e2e 2>&1 | tail -6`
Expected: all PASS, `PR13_METRICS_JSON` all six at 1.0.

- [ ] **Step 5: commit.**

```bash
git add scripts/ tests/contract_tool_e2e_non_http.rs
git commit -m "test: gRPC/GraphQL/WebSocket/SQL are queryable through the contract tools"
```

---

### Task 3: `ChangedWithoutSchema` must not fall through to `NoKnownImpact`

`diff.rs`'s final `else` (`:1703-1724`) means a schemaless endpoint with **no bound consumers and no could-match candidates** reaches `NoKnownImpact` under complete coverage. Untested, and wrong: a handler changed on an endpoint we cannot reason about is never "no known impact".

**Files:**
- Modify: `src/server/federation/contracts/diff.rs:1703-1724`
- Test: `src/server/federation/contracts/diff_tests.rs`

- [ ] **Step 1: write the failing test.**

```rust
#[test]
fn a_schemaless_endpoint_with_no_consumers_is_never_no_known_impact() {
    // Handler file changed, endpoint has no schema, no bound consumer,
    // no unresolved candidates, coverage complete.
    let changes = diff_contracts(&base, &head, &changed_handlers_only());
    let cw = changes.iter().find(|c| matches!(c.kind, ChangeKind::ChangedWithoutSchema { .. }))
        .expect("ChangedWithoutSchema must fire");
    let impact = evaluate(&cw.kind, &base, &head, &full_coverage());
    assert_ne!(
        impact.class,
        Class::NoKnownImpact,
        "a changed handler on a schemaless endpoint with no consumers is \
         an unanalysed behaviour change, not 'no impact'"
    );
}
```

- [ ] **Step 2: confirm it FAILS** (`assertion left == right failed: NoKnownImpact`).

- [ ] **Step 3: implement.** In `evaluate`'s final `else`, map `ChangedWithoutSchema` to `NeedsInvestigation` / `Reason::NoSchema` regardless of consumer count — the same verdict it already gets when a consumer *is* bound. Do not touch the `Compatible` arm.

- [ ] **Step 4: verify GREEN** — `cargo test --quiet --lib diff_tests` and the T1 metrics still at 1.0.

- [ ] **Step 5: commit.**

```bash
git add src/server/federation/contracts/diff.rs src/server/federation/contracts/diff_tests.rs
git commit -m "fix(contracts): a schemaless handler change is never NoKnownImpact"
```

---

## P2 — Determinism and hygiene

### Task 4: `field_join` schema lookup must be deterministic

`field_join.rs:228` step 4b uses `nodes.iter().find(...)` to pick the Schema node matching `(path, repo)`. With several Schema nodes in one file (multiple types per SDL), the result depends on insertion order — so output can differ across runs.

**Files:**
- Modify: `src/server/federation/contracts/field_join.rs:228` region
- Test: `src/server/federation/contracts/field_join.rs` (`#[cfg(test)]` only)

- [ ] **Step 1: write the failing test** that constructs the collision explicitly.

```rust
#[test]
fn schema_lookup_is_order_independent_when_two_types_share_a_file() {
    // Two Schema nodes, same path, same repo, different names.
    let a = schema_node("orders", "schema.graphql", "Order", 1);
    let b = schema_node("orders", "schema.graphql", "Customer", 20);
    // Insert b first — `find` would return b; the correct answer is
    // the one whose name matches the declared schema, independent of
    // insertion order.
    let mut nodes = vec![b.clone(), a.clone()];
    let picked = pick_schema_node(&mut nodes, "orders", "schema.graphql", "Order");
    assert_eq!(picked.name, "Order");

    let mut nodes = vec![a.clone(), b.clone()];
    let picked = pick_schema_node(&mut nodes, "orders", "schema.graphql", "Order");
    assert_eq!(picked.name, "Order");
}
```

Extract the lookup into a named `pick_schema_node` if it is currently inline — that is what makes it testable at all.

- [ ] **Step 2: confirm it FAILS** on the reversed order.

- [ ] **Step 3: implement the tie-break** — prefer exact name match, then lowest `line_start`, then lexicographically smallest `GlobalId`. Deterministic, not just "sorted".

- [ ] **Step 4: verify GREEN** — `cargo test --quiet --lib field_join` (23 tests), `--test federation_contracts_e2e` (T1 1.0).

- [ ] **Step 5: commit.**

```bash
git add src/server/federation/contracts/field_join.rs
git commit -m "fix(contracts): field_join schema lookup is order-independent"
```

---

### Task 5: Repair `tests/real_federation/*` and assert verdicts

The real-repo suite is red for two mechanical reasons and asserts the wrong thing.

**Files:**
- Modify: `tests/real_federation/ground_truth.sh`, `soundness.sh`, `tools_smoke.sh`
- Modify: `scripts/demo-federation-fixture.sh`

- [ ] **Step 1: fix the fixture mismatch.** `ground_truth.sh:65` and `soundness.sh:81` pass `serde` to `prepare_snapshot`, but `scripts/demo-federation-fixture.sh:25-28` clones only `bytes` + `tokio`. Either add serde to the builder or drop it from the scripts and `tests/fixtures/contracts/ground_truth_real.json` — pick one and make the two agree.

- [ ] **Step 2: fix the stale tool arguments.** `tools_smoke.sh` has 4 shapes that no longer match the tool contracts: `trace_impact` `{"from":["bytes::…"]}` should be `{"from":{"endpoint":{…}}}`; `check_binding` `"GET /x"` should be the canonical `"http:GET /x"` key. Read `src/server/mcp/contract_tools/schemas/*.in.json` for the current shapes.

- [ ] **Step 3: add a verdict assertion.** The suite currently asserts joiner precision (0 false `Binds`) and tool health — never "this change breaks X". Add one diff assertion per protocol available in the real fixture.

- [ ] **Step 4: run.**

Run: `tests/real_federation/ground_truth.sh <fixture> 19876 && tests/real_federation/soundness.sh <fixture> 19878 && tests/real_federation/tools_smoke.sh <fixture> 19877`
Expected: all three exit 0.

- [ ] **Step 5: commit.**

```bash
git add tests/real_federation/ scripts/demo-federation-fixture.sh tests/fixtures/
git commit -m "test: real_federation suite green, and asserting verdicts not just precision"
```

---

### Task 6: Hygiene

Four small items, each independently rejectable.

- [ ] **6a. `check-clean-build.sh` diagnostic.** It reports "a tracked file is referencing something not in the commit" when the real cause can be a C toolchain failure in a cold cargo cache (observed: `libgit2-sys`/`pcre2` compile error). Distinguish them:

```bash
if ! (cd "$tmp" && cargo check --all-targets --quiet); then
    if (cd "$tmp" && cargo check --all-targets --quiet 2>&1 | grep -q "E0583\|E0432\|E0433\|E0425"); then
        echo "FAIL: $tree_ish references source that is not in the commit."
    else
        echo "FAIL: $tree_ish could not be checked (toolchain/environment, not necessarily missing source)."
    fi
    exit 1
fi
```

Test it by pointing the script at a deliberately broken tree-ish (see the technique in the script's own history: `git commit-tree` over a tree missing `payload_schema.rs`) and at a clean one.

- [ ] **6b. Record I8 in the design spec.** `ScanRetract.tla` defines invariant **I8 (scan ownership / no peer retraction)** but the invariant list in `docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` has only I1–I7. Append I8 with a pointer to `docs/formal/ScanRetract.tla` and the `graph/mod.rs::replace_sensor_output` it targets. Doc-only.

- [ ] **6c. Run `--self-test`.** The parallel-vs-serial equality assertion was written and never run. Two full measurements (~80 min).

Run: `python3 scripts/mutation-check.py --self-test`
Expected: `self-test: PASS — <N> mutants, identical verdicts serial vs parallel`

- [ ] **6d. Raise the floors.** Once the in-flight re-measurement lands, update `scripts/mutation-baseline.json`'s `floor.per_target`:
  - `"src/server/graph/mod.rs"`: 45 → **60** (the plan value) if it measures ≥60 (expected 37/37 = 100%); delete the `floor_notes` entry explaining the reduction.
  - `"src/server/ingest/resolve.rs"`: **add at 75** (measured 77.8%, 8 survivors — 4 of them the equivalent `max_edges` budget mutants already in `triage`).
  - `"src/server/sensors/http_sensor.rs"`: **add at 100** (measured 12/12 after `b0a0894a`).

  Every floor change must land as a visible diff in that file with the measured number in the commit message. If any target measures *below* its proposed floor, leave the floor where it is and say so — a silent reduction is the failure mode this gate exists to prevent.

- [ ] **Commit 6 as one.**

```bash
git add scripts/ docs/
git commit -m "chore: clean-build diagnostic, I8 in the spec, mutation self-test, floors"
```

---

## Final review — not optional

This plan ends with a **fresh-context review of the whole branch**, by a reviewer that did not write it. The last branch review was a *self*-review (the reviewer subagent hit a rate limit), which is the weakest link in the work so far. Use `superpowers:requesting-code-review`'s `code-reviewer.md` on `docs/superpowers/plans/` and the branch diff, with this plan's Review Focus verbatim. Grade findings by effect on a user, not by whether the spec names the input.

---

## Out of scope — separate plans

- **Defect K: ownership / entry points / env bindings write no graph nodes.** CODEOWNERS, `entry_point_sensor` and `env_sensor` feed side tables only, so a graph UI cannot show "who owns this". This needs a `FEDERATION_GRAPH_VERSION` bump (`src/server/federation/graph_backend.rs:24`) plus a `lain reindex` recovery path and a CHANGELOG entry — a **breaking on-disk change with an operator recovery procedure**, which is a different risk class from everything above and deserves its own plan and its own review.
- **Task 8 Tier 2/3 of the mutation-credibility plan** — `.scm` extraction is blocked (no `tree-sitter-protobuf`/`-graphql`/`-sql` in `Cargo.toml`); the shared emitter is half-done.
- **The 40 classified `gap` mutation survivors** — tracked per `path:line` in `scripts/mutation-baseline.json`; closing them is test work with no soundness impact.
- **Org-wide safety claims.** `scope.configured_only` stays `true` and `caveats.unconfigured_scope` must remain rendered on every response. No code change is "a fix" for that; it is the contract.
