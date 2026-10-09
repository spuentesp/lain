# Sensor Ownership and Staleness Gaps — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **Execution method is already fixed: NATIVE / INLINE.** The human has
> directed inline execution repeatedly and explicitly ruled out
> subagents. Implement every task in this session. Do not dispatch.

**Goal:** Close the remaining sensor-ownership and staleness gaps found by the 2026-10-08 review — nodes nothing retracts, ownership nothing enforces, duplicated emission, swallowed write errors, and one silently-unreported rename.

**Architecture:** Every sensor that emits graph nodes must either own them (so `replace_sensor_output` retracts stale ones) or attach them to a node someone else owns. Ownership is derived by `sensor_owner_of` from `(NodeType, ContractFact)` plus a path/name guard. The gaps are all cases where that derivation returns `None` or returns a sensor that never calls `replace_sensor_output`. Fix the derivation, then make each emitter actually call `replace_sensor_output`, then put a mechanical guard in place so the next divergence fails a test instead of a review.

**Tech Stack:** Rust 2021, `inventory`, `petgraph`, `git2`. Oracle: `cargo test`. Mutation verification by hand (apply → run → revert by editing the line back).

**Spec:** `docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` invariant **I8** (scan ownership / no peer retraction), and `src/server/sensors/AGENTS.md` ("Never put a `ContractFact` on a node another sensor owns").

## Global Constraints

- **T1 must hold at precision AND recall 1.0** on all six `PR13_METRICS_JSON` metrics (`diff_precision`, `diff_recall`, `binds_precision`, `binds_recall`, `reads_field_precision`, `reads_field_recall`). A regression there is a soundness regression, not a test to update. Run it as `PR13_METRICS_JSON=1 cargo test --quiet --test federation_contracts_e2e pr13_hermetic -- --nocapture`.
- **Never claim absent when unanalyzed.** A node or edge that silently vanishes lets `NoKnownImpact` stand while real behaviour may have moved — the worst failure in this codebase.
- **One fact, one owner.** A node carrying a `ContractFact` must be owned by exactly one sensor, and that sensor must be the only one that retracts it. `replace_sensor_output(owner, …)` deletes every node with that owner *and its incident edges* (`src/server/graph/mod.rs`).
- **Every emitted edge must have materialized endpoints** — `insert_edges_batch` drops dangling edges silently.
- **No destructive git.** Never `git reset --hard`, `git checkout --`, `git stash`, `git clean`, `git restore`. Revert hand-applied mutations **by editing the line back**. **Never launch a background or detached process.**
- **Never run two `cargo` invocations in parallel.**
- `cargo fmt --check` and `cargo clippy --all-targets` clean at every commit.
- Project lints must stay exit 0: `python3 scripts/check-no-duplicate-sensors.py`, `python3 scripts/check-no-mirror-dtos.py`, `python3 scripts/check-mcp-dispatch-shape.py`. `python3 scripts/mutation-check.py --check-floor` must PASS.
- **Never put backticks in `git commit -m`.** Write the message to a file and use `git commit -F <file>`. Shell command substitution will silently eat them (observed 2026-10-08).

## Review Focus

1. **A node whose owner is a sensor that never calls `replace_sensor_output` is indistinguishable from a node nobody owns** — both are never retracted. Task 1's arms must name owners that *actually replace*, and Task 2's guard must fail if that ever stops being true.
2. **A type-and-path catch-all steals retraction from the sensor that emitted the node.** `grpc_provider_sensor` and `grpc_handler_link_sensor` both emit `NodeType::Module` on `.proto` paths. Task 1's arms must be ordered *after* every fact arm so fact-bearing nodes keep their owners; test both directions.
3. **Retraction is per whole scan, not per file.** `replace_sensor_output` retracts everything the owner made, then inserts what it was handed. If `scan_workspace` hands it only the *last* file's nodes, every earlier file's nodes vanish on every scan. Task 1 must accumulate across files.
4. **A test that only proves "my node survived" passes while staleness is live.** Every retraction test must delete the source and assert the node is gone — and must keep the file present so the orphan sweep is not what removed it.
5. **`cargo clippy --all-targets -- -D warnings` must stay clean**, and a swallowed `Result` (Task 4) is exactly the shape that hides a failing write behind a green build.

---

## File Structure

| File | Responsibility after this plan |
|---|---|
| `src/server/graph/mod.rs` | `sensor_owner_of` — adds fact-less `Module`/`Interface` arms; unchanged otherwise |
| `src/server/sensors/proto_sensor.rs` | `scan_workspace` accumulates nodes/edges and calls `replace_sensor_output(ProtoSensor, …)` |
| `src/server/sensors/graphql_sensor.rs` | same, `GraphqlSensor` |
| `src/server/sensors/util.rs` | `SYNTHETIC_*_PREFIX` constants + `emit_synthetic_site` helper (Task 3) |
| `src/server/sensors/{sql,event,grpc_consumer,graphql_consumer}_sensor.rs` | call the helper; no behaviour change (Task 3) |
| `src/server/sensors/{grpc_consumer,grpc_handler_link,graphql_resolver_link}_sensor.rs` | propagate `replace_sensor_output` errors (Task 4) |
| `src/server/federation/contracts/diff.rs` | path+method rename is reported (Task 5) |
| `tests/sensor_coexistence.rs` | retraction tests (Task 1), ownership contract table (Task 2) |
| `src/server/mcp/contract_tools/services.rs`, `src/server/sensors/field_access_sensor.rs`, `src/server/sensors/payload_schema.rs` | dead-code removal (Task 6) |

---

## Task 1: Fact-less `Module`/`Interface` nodes are owned and retracted

**Background.** `proto_sensor` emits `NodeType::Module` and `graphql_sensor` emits `NodeType::Interface`, both with **no** `ContractFact`, both via `upsert_node`. `sensor_owner_of` returns `None` for them, and neither sensor calls `replace_sensor_output`. Consequence: rename a service in a `.proto`, rescan, and the old `Module` node stays forever. Same for a GraphQL operation. This is the accumulation-direction twin of the `.proto` `Schema`/`Field` staleness fixed in `429f6b43`.

**Safety check already done (do not re-derive):** `grpc_provider_sensor` and `graphql_provider_sensor` also emit `NodeType::Module`, but theirs **carry** `RpcProvider` / `GraphqlProvider` facts and are claimed by the earlier fact arms. `grpc_handler_link_sensor` / `graphql_resolver_link_sensor` emit `Module` with `RpcHandler` / `GraphqlHandler` facts — also claimed earlier. So a catch-all placed **after** every fact arm catches only the fact-less nodes. Do not move these arms above the fact arms.

**No `FEDERATION_GRAPH_VERSION` bump is required.** `sensor_owner_of` is a derivation over nodes that are already on disk; this task changes which sensor retracts them, not the on-disk shape. The federation-schema-bumps rule in `AGENTS.md` fires only when the serialized graph format changes. Do not bump, do not add a `lain reindex` recovery note, do not add a CHANGELOG entry for a schema bump. (A plain CHANGELOG "Fixed" line for the staleness bug is fine at release time but is the release PR's job, not this one.)

**Files:**
- Modify: `src/server/graph/mod.rs` (`sensor_owner_of`, the arm list just before the node-type catch-alls at the `_ => None`)
- Modify: `src/server/sensors/proto_sensor.rs` (`enrich_with_proto`, `scan_workspace`)
- Modify: `src/server/sensors/graphql_sensor.rs` (`enrich_with_graphql`, `scan_workspace`)
- Test: `tests/sensor_coexistence.rs`

**Interfaces:**
- Consumes: `SensorOwner::{ProtoSensor, GraphqlSensor}` (already declared in `src/server/graph/mod.rs`), `GraphDatabase::replace_sensor_output`, `util::walk_workspace`.
- Produces: `enrich_with_proto(graph, proto_path, root, namespace, nodes: &mut Vec<GraphNode>, edges: &mut Vec<GraphEdge>) -> Result<usize, LainError>` and `enrich_with_graphql(...)` with the same appended parameters. Task 2 reads `sensor_owner_of`; nothing else depends on these.

- [ ] **Step 1: Write the failing tests** in `tests/sensor_coexistence.rs`, next to `deleting_a_graphql_type_retracts_its_schema`:

```rust
/// The same staleness for fact-less `Module` nodes: `proto_sensor`
/// upserted them and nothing retracted them, so renaming a service left
/// the old node in the graph forever.
#[test]
fn deleting_a_proto_service_retracts_its_module() {
    let ws = workspace("proto_module_stale");
    write(
        &ws,
        "proto/orders.proto",
        r#"
syntax = "proto3";

package orders;

service Orders {
  rpc GetOrder (Order) returns (Order);
}
"#,
    );
    let graph = scan(&ws);

    let before = nodes_of(&graph, NodeType::Module).len();
    assert!(before >= 1, "expected at least one Module node, got {before}");

    // Rename the service; keep the file so this is the sensor's own
    // retraction, not the orphan sweep.
    write(
        &ws,
        "proto/orders.proto",
        r#"
syntax = "proto3";

package orders;

service Billing {
  rpc GetInvoice (Invoice) returns (Invoice);
}
"#,
    );
    run_all(&graph, &ws, &RepoNamespace::for_test(), "svc");

    let stale = nodes_of(&graph, NodeType::Module)
        .into_iter()
        .filter(|n| n.name.contains("Orders"))
        .count();
    assert_eq!(
        stale, 0,
        "a service renamed away in a .proto must have its Module retracted on rescan"
    );
}

/// The twin for fact-less `Interface` nodes from `graphql_sensor`.
#[test]
fn deleting_a_graphql_operation_retracts_its_interface() {
    let ws = workspace("graphql_iface_stale");
    write(
        &ws,
        "schema.graphql",
        r#"
type Query {
  orders: [ID!]!
}
"#,
    );
    let graph = scan(&ws);

    let before = nodes_of(&graph, NodeType::Interface).len();
    assert!(before >= 1, "expected at least one Interface node, got {before}");

    write(
        &ws,
        "schema.graphql",
        r#"
type Query {
  invoices: [ID!]!
}
"#,
    );
    run_all(&graph, &ws, &RepoNamespace::for_test(), "svc");

    let stale = nodes_of(&graph, NodeType::Interface)
        .into_iter()
        .filter(|n| n.name.contains("orders"))
        .count();
    assert_eq!(
        stale, 0,
        "an operation removed from a .graphql must have its Interface retracted on rescan"
    );
}
```

- [ ] **Step 2: Run them and confirm both fail**

Run: `cargo test --quiet --test sensor_coexistence deleting_a_proto_service deleting_a_graphql_operation`
Expected: both FAIL. `deleting_a_proto_service_retracts_its_module` reports `stale: 1` (the old `Orders` module survives). `deleting_a_graphql_operation_retracts_its_interface` reports `stale: 1`.

Note: `cargo test` accepts only one filter; pass both names after `--`:
`cargo test --quiet --test sensor_coexistence -- deleting_a_proto_service deleting_a_graphql_operation`

- [ ] **Step 3: Give fact-less `Module`/`Interface` nodes an owner.** In `src/server/graph/mod.rs`, insert **immediately before** the node-type catch-alls (`(NodeType::HttpClientCall, _)` …) and therefore after every `ContractFact` arm:

```rust
        // Legacy scanners emit bare `Module` / `Interface` nodes with no
        // contract fact (`proto_sensor`, `graphql_sensor`). Placed after
        // every fact arm on purpose: `grpc_provider_sensor` and
        // `graphql_provider_sensor` also emit `Module`, but theirs carry
        // `RpcProvider` / `GraphqlProvider` and are claimed above — a
        // catch-all placed earlier would take retraction away from the
        // sensor that emitted the node.
        (NodeType::Module, None) if node.path.ends_with(".proto") => {
            Some(SensorOwner::ProtoSensor)
        }
        (NodeType::Interface | NodeType::Module, None)
            if node.path.ends_with(".graphql") || node.path.ends_with(".gql") =>
        {
            Some(SensorOwner::GraphqlSensor)
        }
```

Also update the `ProtoSensor` / `GraphqlSensor` variant docs in the same file: they currently say "nothing passes this owner to `replace_sensor_output`" — after this task they do. Change both to say the legacy scanner owns its bare nodes and retracts them per scan.

- [ ] **Step 4: Make the scanners collect and replace.** In `src/server/sensors/proto_sensor.rs`, change `enrich_with_proto` to accumulate instead of writing, and `scan_workspace` to replace once per scan.

```rust
pub fn enrich_with_proto(
    graph: &GraphDatabase,
    proto_path: &Path,
    root: &Path,
    namespace: &crate::schema::RepoNamespace,
    nodes: &mut Vec<GraphNode>,
    edges: &mut Vec<GraphEdge>,
) -> Result<usize, LainError> {
    // …existing read + parse unchanged…
    let mut count = 0;
    for svc in &services {
        let service_key = format!("{}.{}", svc.package, svc.service_name);
        let service_id = GraphNode::generate_id(
            &NodeType::Module,
            &svc.proto_path,
            &service_key,
            None,
            namespace,
        );

        let mut service_node = GraphNode::new(
            NodeType::Module,
            format!("{}.{}", svc.service_name, svc.method_name),
            svc.proto_path.clone(),
        );
        service_node.id = service_id.clone();
        service_node.signature = Some(format!("{} -> {}", svc.input_type, svc.output_type));
        nodes.push(service_node);

        if let Some(handler) =
            crate::server::sensors::util::find_handler_in_graph(graph, &svc.method_name)
        {
            edges.push(GraphEdge::new(EdgeType::Implements, handler.id.clone(), service_id));
            count += 1;
        }
    }

    Ok(count)
}
```

and in `scan_workspace` of the same file:

```rust
pub fn scan_workspace(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &crate::schema::RepoNamespace,
) -> Result<usize, LainError> {
    let mut count = 0;
    let mut all_nodes: Vec<GraphNode> = Vec::new();
    let mut all_edges: Vec<GraphEdge> = Vec::new();

    for entry in crate::server::sensors::util::walk_workspace(root) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("proto") {
            match enrich_with_proto(graph, path, root, namespace, &mut all_nodes, &mut all_edges) {
                Ok(n) => count += n,
                Err(e) => tracing::warn!("Failed to parse {:?}: {}", path, e),
            }
        }
    }

    // One replace for the whole scan, not per file: `replace_sensor_output`
    // retracts every node this owner made and inserts only what it is
    // handed, so calling it per file would delete the earlier files' nodes.
    graph.replace_sensor_output(SensorOwner::ProtoSensor, &all_nodes, &all_edges)?;
    Ok(count)
}
```

`graphql_sensor.rs` gets the identical treatment with `NodeType::Interface`, `SensorOwner::GraphqlSensor`, and its existing `ext == "graphql" || ext == "gql"` filter.

- [ ] **Step 5: Run the tests and make sure they pass**

Run: `cargo test --quiet --test sensor_coexistence`
Expected: PASS (22 tests). Then the wider battery:
`cargo test --quiet --lib sensors` → PASS
`cargo test --quiet --lib 'federation::contracts'` → PASS
`cargo test --quiet --test federation_contracts_e2e` → PASS (42)

- [ ] **Step 6: Mutation-verify the arms are load-bearing**

Hand-apply: change `(NodeType::Module, None) if node.path.ends_with(".proto")` to `(NodeType::Module, None) if false`. Run the two new tests; both must FAIL. Revert by editing the line back.

- [ ] **Step 7: Commit**

```bash
git add src/server/graph/mod.rs src/server/sensors/proto_sensor.rs src/server/sensors/graphql_sensor.rs tests/sensor_coexistence.rs
git commit -F /tmp/msg.txt
```
Message (write to `/tmp/msg.txt`; **no backticks**):

```
fix(graph): fact-less Module/Interface nodes are owned and retracted

proto_sensor and graphql_sensor emitted bare Module / Interface nodes
through upsert_node with no contract fact. sensor_owner_of returned None
for them and neither sensor called replace_sensor_output, so nothing ever
retracted them: rename a service in a .proto and the old Module stayed
forever. Same accumulation-direction staleness that 429f6b43 fixed for
.proto Schema/Field.

The arms land after every ContractFact arm on purpose. grpc_provider
and graphql_provider also emit Module nodes, but theirs carry
RpcProvider / GraphqlProvider and are claimed above — a catch-all placed
earlier would take retraction away from the sensor that emitted the node,
which is the clobber this range keeps fixing.

The scanners now accumulate across the whole scan and call
replace_sensor_output once. Per-file replacement would delete the earlier
files' nodes, since replace retracts everything the owner made and
inserts only what it is handed.

Tests (RED-first): deleting_a_proto_service_retracts_its_module,
deleting_a_graphql_operation_retracts_its_interface. Both keep the file
present so the orphan sweep is not what removed the node.
```

---

## Task 2: A mechanical ownership guard

**Background.** The proto and graphql staleness bugs both survived because the *derived* owner and the *replacing* sensor had silently diverged. Nothing checked they agreed. A test that states the contract for every sensor will fail instead of being discovered in review.

**Files:**
- Test: `tests/sensor_coexistence.rs`

**Interfaces:**
- Consumes: `sensor_owner_of`, `SensorOwner`.
- Produces: none. This is a guard.

- [ ] **Step 1: Write the failing test** at the end of `tests/sensor_coexistence.rs`:

```rust
// ─── Ownership contract ─────────────────────────────────────────────
//
// `sensor_owner_of` and the sensor that calls `replace_sensor_output`
// must agree. When they diverge the node is simply never retracted —
// which is exactly how the proto and graphql schema/module staleness
// bugs shipped. This table is the contract; a new sensor must add a row.

#[test]
fn every_owned_node_shape_is_retracted_by_the_sensor_that_makes_it() {
    use lain::server::federation::contracts::model::{
        ContractFact, GraphqlProviderFact, HttpMethod, ProviderFact, ProviderOrigin, RpcProviderFact,
        RpcSystem, Table, TableConsumerFact, TopicConsumerFact, TopicConsumerKind,
    };
    use lain::server::sensors::util::{SQL_READ_PREFIX, TOPIC_READ_PREFIX};

    // (description, node, expected owner)
    let mut rows: Vec<(&str, lain::schema::GraphNode, Option<SensorOwner>)> = Vec::new();

    let mut mk = |ty: NodeType, name: &str, path: &str| {
        lain::schema::GraphNode::new(ty, name.to_string(), path.to_string())
    };

    // HTTP routes: origin decides.
    let mut n = mk(NodeType::HttpRoute, "GET /a", "src/a.rs");
    n.contract = Some(ContractFact::Provider(ProviderFact {
        method: HttpMethod::Get,
        template: "/a".into(),
        handler: None,
        operation_id: None,
        origin: ProviderOrigin::Code,
    }));
    rows.push(("http route (code)", n, Some(SensorOwner::HttpSensor)));

    // gRPC / GraphQL provider nodes carry facts and are owned by their
    // emitter even on .proto / .graphql paths.
    let mut n = mk(NodeType::Module, "Orders.GetOrder", "proto/a.proto");
    n.contract = Some(ContractFact::RpcProvider(RpcProviderFact {
        system: RpcSystem::Grpc,
        service: "orders.Orders".into(),
        method: "GetOrder".into(),
        request_type: "Order".into(),
        response_type: "Order".into(),
        handler: None,
    }));
    rows.push(("grpc provider module", n, Some(SensorOwner::GrpcProviderSensor)));

    let mut n = mk(NodeType::Schema, "Order", "proto/a.proto");
    n.contract = Some(ContractFact::Schema {
        direction: lain::federation::contracts::model::Direction::Response,
    });
    rows.push(("proto schema", n, Some(SensorOwner::GrpcProviderSensor)));

    // Legacy fact-less nodes.
    rows.push((
        "proto service module",
        mk(NodeType::Module, "orders.Orders.GetOrder", "proto/a.proto"),
        Some(SensorOwner::ProtoSensor),
    ));
    rows.push((
        "graphql operation interface",
        mk(NodeType::Interface, "query: orders", "schema.graphql"),
        Some(SensorOwner::GraphqlSensor),
    ));

    // Synthetic per-site consumer nodes are owned by name prefix.
    let mut n = mk(NodeType::Function, &format!("{SQL_READ_PREFIX}src/a.py:3"), "src/a.py");
    n.contract = Some(ContractFact::TableConsumer(TableConsumerFact {
        tables: vec!["t".into()],
    }));
    rows.push(("sql-read site", n, Some(SensorOwner::SqlSensor)));

    let mut n = mk(NodeType::Function, &format!("{TOPIC_READ_PREFIX}src/a.py:3"), "src/a.py");
    n.contract = Some(ContractFact::TopicConsumer(TopicConsumerFact {
        broker: "kafka".into(),
        name: "t".into(),
        kind: TopicConsumerKind::Subscription,
    }));
    rows.push(("topic-read site", n, Some(SensorOwner::EventSensor)));

    let mut n = mk(NodeType::Table, "shipments", "src/a.py");
    n.contract = Some(ContractFact::Table(Table {
        service: String::new(),
        name: "shipments".into(),
    }));
    rows.push(("sql table", n, Some(SensorOwner::SqlSensor)));

    // A plain symbol is nobody's — this is the migration arm's whole job.
    rows.push((
        "plain symbol",
        mk(NodeType::Function, "job", "src/a.py"),
        None,
    ));

    for (what, node, want) in &rows {
        assert_eq!(
            sensor_owner_of(node),
            *want,
            "ownership drift for {what}: sensor_owner_of disagrees with the \
             sensor that calls replace_sensor_output"
        );
    }
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test --quiet --test sensor_coexistence every_owned_node_shape`
Expected: FAIL on `"proto service module"` and `"graphql operation interface"` (returns `None`, expected `Some(...)`) **if Task 1 has not landed**. If Task 1 has landed, it PASSES — in that case still run it before and after Step 3 to prove it bites.

To prove it bites at all, hand-apply a mutant now: in `src/server/graph/mod.rs` change the `(NodeType::Module, None)` arm's return to `None`, run, expect FAIL, then revert by editing the line back.

- [ ] **Step 3: Assert the table covers every owner variant.** Append to the same test, so a new `SensorOwner` variant cannot be added without a row:

```rust
    let covered: std::collections::BTreeSet<String> = rows
        .iter()
        .filter_map(|(_, _, o)| o.map(|s| format!("{s:?}")))
        .collect();
    for variant in [
        "HttpSensor",
        "OpenApiSensor",
        "ProtoSensor",
        "GraphqlSensor",
        "GrpcProviderSensor",
        "GrpcConsumerSensor",
        "GrpcHandlerLinkSensor",
        "GraphqlProviderSensor",
        "GraphqlConsumerSensor",
        "GraphqlResolverLinkSensor",
        "WebSocketSensor",
        "FieldAccessSensor",
        "EventSensor",
        "SqlSensor",
        "EntryPointSensor",
        "HttpClientSensor",
    ] {
        assert!(
            covered.contains(variant),
            "SensorOwner::{variant} has no row in the ownership contract \
             table — add one so its retraction is pinned"
        );
    }
```

Add whatever rows are needed to satisfy this. Each row is a node shape that sensor emits. The variants that currently have **no** row and will need one:

- `OpenApiSensor` — an `HttpRoute` with `ProviderOrigin::OpenApi`, and a `Schema`/`Field` on a non-`.proto`/`.graphql`/`.avsc` path (e.g. `openapi.json`).
- `HttpClientSensor` — an `HttpClientCall`.
- `FieldAccessSensor` — a `FieldRef`.
- `EntryPointSensor` — this one is special: `sensor_owner_of` returns `None` for everything it writes (`GraphNode.entry` is a field, not a node), and `replace_sensor_output(EntryPointSensor, …)` is a deliberate wipe-and-reapply of the `entry` field. Do **not** add a node row; add a comment saying so, and add `EntryPointSensor` to a small `no_node_owner` list that the coverage assertion skips. That skip must be justified in the comment, not silent.
- `WebSocketSensor` — a node carrying a `WebSocketProvider`/`WebSocketConsumer`/`WebSocketHandler` fact.

If a variant genuinely owns nothing, say so in a comment and do **not** add it to the list — instead record the reason in the plan ledger.

- [ ] **Step 4: Run and commit**

Run: `cargo test --quiet --test sensor_coexistence` → PASS
Run: `cargo test --quiet --lib graph` → PASS

```bash
git add tests/sensor_coexistence.rs
git commit -F /tmp/msg.txt
```

```
test(graph): pin the ownership contract between sensor_owner_of and
replace_sensor_output

The proto and graphql staleness bugs both shipped because the derived
owner and the replacing sensor had silently diverged and nothing checked
they agreed. This table states the contract for every owner variant, and
asserts coverage so a new SensorOwner cannot be added without a row.

The fact-bearing rows are the ones that matter most: grpc_provider and
graphql_provider emit Module nodes on .proto / .graphql paths and must
keep GrpcProviderSensor / GraphqlProviderSensor, not be caught by the
fact-less catch-alls added in the previous commit.
```

---

## Task 3: One synthetic-site emitter, all the prefixes named

**Background.** The "synthetic per-site node" shape — `format!("{}{path}:{line}")` as the name, `generate_id` for the id, `line_start = Some(line)`, `line_end = None`, one `ContractFact` — is written out five times: `sql_sensor.rs`, `event_sensor.rs`, `grpc_consumer_sensor.rs`, `graphql_consumer_sensor.rs` (twice). Only `sql-read:` and `topic-read:` have named constants; `rpc-call:` and `graphql-call:` are raw string literals, so a rename cannot be found and the `sensor_owner_of` guards cannot be checked against them.

`line_end = None` is load-bearing, not cosmetic: `util::enclosing_symbol` requires both bounds, so a synthetic node with `line_end` set can win its `min_by` and steal a peer sensor's edge anchor. That exact bug was fixed in `cae035ac`. The helper must own that rule.

**Files:**
- Modify: `src/server/sensors/util.rs`
- Modify: `src/server/sensors/{sql_sensor,event_sensor,grpc_consumer_sensor,graphql_consumer_sensor}.rs`
- Test: `tests/sensor_coexistence.rs`

**Interfaces:**
- Consumes: `GraphNode::generate_id`, `RepoNamespace`.
- Produces:
  - `pub const SQL_READ_PREFIX: &str = "sql-read:";` (exists)
  - `pub const TOPIC_READ_PREFIX: &str = "topic-read:";` (exists)
  - `pub const RPC_CALL_PREFIX: &str = "rpc-call:";`
  - `pub const GRAPHQL_CALL_PREFIX: &str = "graphql-call:";`
  - `pub fn synthetic_site_node(id_name: String, path: &str, line: u32, namespace: &RepoNamespace, fact: ContractFact) -> GraphNode`
    — returns the node with `name == id_name`, `id` from `generate_id(Function, path, id_name, Some(line), namespace)`, `line_start = Some(line)`, `line_end = None`, `contract = Some(fact)`.

- [ ] **Step 1: Write the failing test** in `tests/sensor_coexistence.rs`:

```rust
/// `line_end` must stay `None` on every synthetic site node.
/// `util::enclosing_symbol` requires both bounds, so a synthetic node
/// with `line_end` set wins its `min_by` (range 0 beats the real
/// function) and steals a peer sensor's edge anchor — the bug fixed in
/// cae035ac for graphql-call:/rpc-call: specifically.
#[test]
fn synthetic_site_nodes_never_set_line_end() {
    use lain::server::sensors::util::{
        synthetic_site_node, SQL_READ_PREFIX, TOPIC_READ_PREFIX,
    };
    use lain::server::federation::contracts::model::{
        ContractFact, TableConsumerFact,
    };
    let ns = RepoNamespace::for_test();
    let node = synthetic_site_node(
        format!("{SQL_READ_PREFIX}src/a.py:3"),
        "src/a.py",
        3,
        &ns,
        ContractFact::TableConsumer(TableConsumerFact {
            tables: vec!["t".into()],
        }),
    );
    assert_eq!(node.line_start, Some(3));
    assert_eq!(node.line_end, None, "line_end must stay None");
    assert!(node.name.starts_with(SQL_READ_PREFIX));
    let _ = TOPIC_READ_PREFIX;
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test --quiet --test sensor_coexistence synthetic_site_nodes_never`
Expected: FAIL — `synthetic_site_node` is not defined (compile error is an acceptable RED here).

- [ ] **Step 3: Add the constants and the helper** to `src/server/sensors/util.rs`, beside `SQL_READ_PREFIX`:

```rust
/// Prefix for a gRPC stub call site's synthetic node. See
/// [`SQL_READ_PREFIX`] for the shape rule.
pub const RPC_CALL_PREFIX: &str = "rpc-call:";
/// Prefix for a GraphQL operation call site's synthetic node.
/// See [`SQL_READ_PREFIX`].
pub const GRAPHQL_CALL_PREFIX: &str = "graphql-call:";

/// Build the synthetic per-site node every consumer sensor emits: one
/// `Function`-typed node named `<prefix><path>:<line>` carrying a single
/// `ContractFact`.
///
/// `line_end` is deliberately `None` and must stay that way:
/// `enclosing_symbol` requires both bounds, so a synthetic node with
/// `line_end` set wins its `min_by` (a zero-width range beats the real
/// enclosing function) and steals every peer sensor's edge anchor.
pub fn synthetic_site_node(
    id_name: String,
    path: &str,
    line: u32,
    namespace: &crate::schema::RepoNamespace,
    fact: crate::federation::contracts::model::ContractFact,
) -> crate::schema::GraphNode {
    let mut node = crate::schema::GraphNode::new(
        crate::schema::NodeType::Function,
        id_name.clone(),
        path.to_string(),
    );
    node.id = crate::schema::GraphNode::generate_id(
        &crate::schema::NodeType::Function,
        path,
        &id_name,
        Some(line),
        namespace,
    );
    node.line_start = Some(line);
    node.line_end = None;
    node.contract = Some(fact);
    node
}
```

- [ ] **Step 4: Replace the five call sites.** Each becomes one call. For example `sql_sensor.rs`'s `sql-read:` block becomes:

```rust
        let id_name = format!(
            "{}{graph_path_str}:{}",
            crate::server::sensors::util::SQL_READ_PREFIX,
            site.line
        );
        nodes.push(crate::server::sensors::util::synthetic_site_node(
            id_name,
            graph_path_str,
            site.line,
            namespace,
            ContractFact::TableConsumer(TableConsumerFact {
                tables: fact_tables,
            }),
        ));
```

Do the same in `event_sensor.rs` (`TOPIC_READ_PREFIX`, `TopicConsumer`), `grpc_consumer_sensor.rs` (`RPC_CALL_PREFIX`, `RpcConsumer`) and both copies in `graphql_consumer_sensor.rs` (`GRAPHQL_CALL_PREFIX`, `GraphqlConsumer`). Do **not** change the node id derivation, the name, or which sites emit — this task is a pure refactor.

- [ ] **Step 5: Run the tests**

Run: `cargo test --quiet --test sensor_coexistence` → PASS
Run: `cargo test --quiet --lib sensors` → PASS
Run: `cargo test --quiet --lib 'federation::contracts'` → PASS
Run: T1 → all six 1.0. **This is the gate that catches a changed id derivation.**

- [ ] **Step 6: Commit**

```bash
git add src/server/sensors/util.rs src/server/sensors/sql_sensor.rs src/server/sensors/event_sensor.rs src/server/sensors/grpc_consumer_sensor.rs src/server/sensors/graphql_consumer_sensor.rs tests/sensor_coexistence.rs
git commit -F /tmp/msg.txt
```

```
refactor(sensors): one synthetic-site emitter, all prefixes named

The synthetic per-site node shape was written out five times and only two
of the four prefixes were named constants, so rpc-call: and graphql-call:
were raw literals — a rename could not be found and the sensor_owner_of
name guards could not be checked against them.

synthetic_site_node owns the shape, including the load-bearing
line_end = None rule (enclosing_symbol requires both bounds, so a
synthetic node with line_end set wins its min_by and steals a peer
sensor's edge anchor — cae035ac). Pure refactor: same ids, same names,
same emission sites.

Verified T1 all six metrics 1.0, which is the gate that would catch a
changed id derivation.
```

---

## Task 4: Stop swallowing `replace_sensor_output` errors

**Background.** Three sensors write `let _ = graph.replace_sensor_output(…)` — `grpc_consumer_sensor.rs`, `grpc_handler_link_sensor.rs`, `graphql_resolver_link_sensor.rs`. A failed write (read-only graph, poisoned lock) is discarded and `scan_workspace` reports success. That is the shape that hides a failing persist behind a green build.

**Files:**
- Modify: `src/server/sensors/grpc_consumer_sensor.rs`, `src/server/sensors/grpc_handler_link_sensor.rs`, `src/server/sensors/graphql_resolver_link_sensor.rs`
- Test: `src/server/sensors/grpc_consumer_sensor.rs` (unit test in-file)

**Interfaces:**
- Consumes: `GraphDatabase::replace_sensor_output -> Result<usize, LainError>`, `LainError`.
- Produces: no signature change — these are `-> Result<usize, LainError>` already.

- [ ] **Step 1: Write the failing test** in the `grpc_consumer_sensor.rs` test module:

```rust
/// A failing `replace_sensor_output` must not be reported as a
/// successful scan. `let _ = …` used to discard the error, so a
/// read-only or poisoned graph looked like a clean run.
#[test]
fn a_write_failure_is_not_reported_as_success() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("client.go"),
        "func f() { stub.GetOrder(orders_pb2.Order{}) }\n",
    )
    .unwrap();
    // `empty_read_only` sets the flag that makes `check_writable`
    // return `LainError::Other("graph is read-only")` on every write.
    let graph = GraphDatabase::empty_read_only();
    let out = scan_workspace_grpc_consumer(&graph, dir.path(), &RepoNamespace::for_test());
    assert!(
        out.is_err(),
        "a write failure must surface as an error, not a silent success: {out:?}"
    );
}
```

Note `scan_workspace_grpc_consumer` already returns `Ok(0)` early when
`graph.is_read_only()` — so if that guard fires first the assertion is
vacuous. Check which happens: if the early return wins, assert instead
that the *guard* is what returned, and prove the `?` propagation
separately with a unit test on a graph whose lock is poisoned (or drop
the early return, which is the honest fix — a read-only graph is not a
successful scan either).

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test --quiet --lib grpc_consumer -- a_write_failure`
Expected: FAIL — `out` is `Ok(_)` because the error was discarded.

- [ ] **Step 3: Propagate.** In each of the three files replace `let _ = graph.replace_sensor_output(…)` with `graph.replace_sensor_output(…)?;`.

- [ ] **Step 4: Run and commit**

Run: `cargo test --quiet --lib sensors` → PASS
Run: `cargo test --quiet --lib 'federation::contracts'` → PASS
Run: T1 → all six 1.0

```bash
git add src/server/sensors/grpc_consumer_sensor.rs src/server/sensors/grpc_handler_link_sensor.rs src/server/sensors/graphql_resolver_link_sensor.rs
git commit -F /tmp/msg.txt
```

```
fix(sensors): a failed replace_sensor_output is not a successful scan

Three sensors wrote `let _ = graph.replace_sensor_output(...)`, so a
write failure (read-only graph, poisoned lock) was discarded and the
scan reported success. sql / http / event already propagate with `?`.
```

---

## Task 5: A rename of both path and method is reported

**Background.** `diff_contracts` pushes `PathChanged` only when `path_changed && !method_changed`, and `MethodChanged` only when `method_changed && !path_changed`. A pairing where **both** differ pushes neither, so the rename is invisible. `from` / `to` are full `ContractKey`s, so `PathChanged` already carries the method change.

**Files:**
- Modify: `src/server/federation/contracts/diff.rs`
- Test: `src/server/federation/contracts/diff_tests.rs`

**Interfaces:**
- Consumes: `ChangeKind::PathChanged { from: ContractKey, to: ContractKey }`.
- Produces: none — same enum, same fields.

- [ ] **Step 1: Write the failing test** in `diff_tests.rs`, beside `handler_change_alongside_a_path_rename_is_not_double_reported`:

```rust
/// A pairing that moved in BOTH path and method reported nothing at
/// all — `PathChanged` required the method to be unchanged and
/// `MethodChanged` required the path to be unchanged. `from` / `to` are
/// full `ContractKey`s, so `PathChanged` already carries both halves.
#[test]
fn a_path_and_method_rename_is_reported() {
    let handler = SymbolKey {
        repo: repo("orders"),
        path: "src/main.rs".into(),
        container: None,
        name: "get_order".into(),
    };
    let mut response_fields = BTreeMap::new();
    response_fields.insert(path(&["customer_id"]), field(TypeDesc::String, true, false));
    let mut schemas = BTreeMap::new();
    schemas.insert(Direction::Response, response_fields);

    let base_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let head_endpoint = endpoint_id("orders", HttpMethod::Post, "/api/order/{}");
    let provider = ProviderRef {
        node_id: id("orders", "HttpRoute", "src/main.rs", "get_order", 12),
        handler: Some(handler.clone()),
        operation_id: None,
    };
    let mut base_endpoints = BTreeMap::new();
    base_endpoints.insert(
        base_endpoint.clone(),
        EndpointDef {
            providers: vec![provider.clone()],
            schemas: schemas.clone(),
            has_schema: true,
            source_files: BTreeSet::new(),
        },
    );
    let mut head_endpoints = BTreeMap::new();
    head_endpoints.insert(
        head_endpoint.clone(),
        EndpointDef {
            providers: vec![provider],
            schemas,
            has_schema: true,
            source_files: BTreeSet::new(),
        },
    );
    let base = ContractSurface {
        endpoints: base_endpoints,
        consumers: BTreeMap::new(),
    };
    let head = ContractSurface {
        endpoints: head_endpoints,
        consumers: BTreeMap::new(),
    };
    let src = StaticChangedFiles(BTreeSet::new());

    let changes = diff_contracts(&base, &head, &src);
    assert!(
        changes
            .iter()
            .any(|c| matches!(c.kind, ChangeKind::PathChanged { .. })),
        "a rename that moved both path and method must not go unreported: {changes:?}"
    );
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test --quiet --lib -- a_path_and_method_rename_is_reported`
Expected: FAIL — `changes` is empty.

- [ ] **Step 3: Report it.** In `diff_contracts`, replace the `if path_changed && !method_changed { … } else if method_changed && !path_changed { … }` with:

```rust
        // A pairing can move in both path and method at once. `from` /
        // `to` are full `ContractKey`s, so `PathChanged` already carries
        // the method half — report the move rather than dropping it.
        if path_changed {
            changes.push(Change {
                service: head_id.0.clone(),
                kind: ChangeKind::PathChanged {
                    from: base_id.1.clone(),
                    to: head_id.1.clone(),
                },
            });
        } else if method_changed {
            changes.push(Change {
                service: head_id.0.clone(),
                kind: ChangeKind::MethodChanged {
                    from: base_id.1.clone(),
                    to: head_id.1.clone(),
                },
            });
        }
```

- [ ] **Step 4: Run and commit**

Run: `cargo test --quiet --lib 'federation::contracts'` → PASS
Run: `cargo test --quiet --test federation_contracts_e2e` → PASS (42)
Run: T1 → all six 1.0. **Watch `diff_precision`** — if a T1 scenario pairs endpoints that differ in both keys and ground truth expected no change, this will surface it. Report the scenario via `PR13_OVERREPORT` rather than weakening the rule.

```bash
git add src/server/federation/contracts/diff.rs src/server/federation/contracts/diff_tests.rs
git commit -F /tmp/msg.txt
```

```
fix(contracts): a rename that moves both path and method is reported

PathChanged required the method to be unchanged and MethodChanged
required the path to be unchanged, so a pairing that moved in both
pushed neither and the rename went unreported. from / to are full
ContractKeys, so PathChanged already carries the method half.
```

---

## Task 6: Dead-code sweep

**Background.** Three pieces of code are unreachable or write-only, and one of them (`_entry_kind_display`) duplicates a live function in another file. Dead code with plausible names is how a reader ends up trusting a code path that cannot run.

**Files:**
- Modify: `src/server/mcp/contract_tools/services.rs` (`_entry_kind_display`)
- Modify: `src/server/sensors/field_access_sensor.rs` (`FieldRead::reader_id`)
- Modify: `src/server/sensors/payload_schema.rs` (`parse_payload_file` and the `ContractFact::Schema` arm it would feed)

**Interfaces:**
- Consumes: none.
- Produces: none — this task removes only.

- [ ] **Step 1: Confirm each is dead before deleting.**

```
grep -rn "_entry_kind_display" src/            # only its own definition
grep -rn "\.reader_id\|reader_id:" src/        # only writes + the field
grep -rn "parse_payload_file" src/             # only its own definition
```
If any has a live caller, stop and report it instead of deleting.

- [ ] **Step 2: Delete `_entry_kind_display`** from `services.rs`. It is an underscore-prefixed duplicate of `kind_wire` in `src/server/mcp/contract_tools/used_by.rs`.

- [ ] **Step 3: Delete `FieldRead::reader_id`** and every `reader_id: "self".to_string()` initialiser. It is written and never read.

- [ ] **Step 4: Decide `parse_payload_file`.** If it truly has no caller, the `.avsc` branch of `sensor_owner_of`'s `Schema`/`Field` arms has no live emitter. Do **not** delete the arm — a future emitter is plausible and the arm is harmless. Delete only `parse_payload_file` if it is unreferenced, and leave a one-line comment on the `.avsc` arm saying it has no emitter today so nobody mistakes it for live coverage.

- [ ] **Step 5: Run and commit**

Run: `cargo test --quiet --lib` → PASS
Run: `cargo clippy --all-targets` → 0 warnings

```bash
git add -A
git commit -F /tmp/msg.txt
```

```
chore: remove dead code a review turned up

_entry_kind_display duplicated kind_wire in used_by.rs and was
underscore-prefixed into silence; FieldRead::reader_id was written at
fifteen sites and read at none; parse_payload_file has no caller, so the
.avsc Schema/Field arm currently has no live emitter — noted on the arm
rather than deleted.
```

---

## Out of scope

- **Synthetic-node emission in the handler/provider sensors** (`rpc-handler:`, `graphql-handler:`, `ws:client:`, `ws:server:`). They share the prefix convention but not the exact shape — some set `line_end`. Task 3 covers only the four consumer-side sites whose shape is identical. Extending it is a follow-up.
- **A `Node`-type-wide `Module` catch-all.** Four sensors emit `Module`; the arms in Task 1 are deliberately narrow (fact-less only). Widening needs per-emitter id evidence.
- **Org-wide safety claims.** `scope.configured_only` stays `true` and `caveats.unconfigured_scope` must remain rendered on every response. No code change is "a fix" for that; it is the contract.
- **Version bumps and releases.** `AGENTS.md`: release PRs only.
