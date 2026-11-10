# Topology Detection Improvements — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the four blind coupling classes the acceptance program proved invisible to the graph: Avro/JSON-Schema payloads, non-Kafka buses (AMQP), generated-code gRPC consumers, and author-less co-change noise.

**Architecture:** Each task extends the existing detection pipeline at a verified hook point — `event_sensor` (payload schemas + AMQP patterns), `anchor_miner` (generated-name anchors), `GitSensor`/`anchor_miner` (author-aware co-change). Nothing changes the on-disk graph format; evidence grades follow the existing provenance conventions (typed sensors mint `Static{Regex}` like today's proto sensor; the anchor miner stays heuristic).

**Tech Stack:** Rust (workspace `lain`), tree-sitter sensors + regex idiom walker (`src/server/sensors/`), federated contract joiner (`src/server/federation/contracts/`), libgit2 via the in-process `GitSensor`. No new dependencies.

**Spec:** `docs/acceptance/FINDINGS-2026-10-11.md` (§Open items, §Product gaps) — this plan closes the documented blind classes; the acceptance fixtures in `tests/fixtures/enterprise_mesh|enterprise_scale/` are the validation oracle.

## Global Constraints

- A node carrying a `ContractFact` must be owned by exactly one sensor (`sensor_owner_of` / `replace_sensor_output`, `src/server/graph/mod.rs:141-305`); new emitters either extend their owner's scan or register via `register_sensor!` / `inventory::submit!(SensorEntry(&XSensor))`.
- `insert_edges_batch` silently drops dangling edges (`src/server/graph/mod.rs:1156`) — always materialize endpoint nodes first.
- No `FEDERATION_GRAPH_VERSION` bump: these changes mint existing node/edge types. If a task genuinely needs a format change, STOP and report instead of bumping.
- No new crates. `cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings` must stay clean (repo style allows `-A clippy::style|complexity|perf|pedantic` — match CI).
- Evidence discipline: anchor-miner edges stay `Heuristic` with a named detector and never upgrade claims to `verified`; typed sensors follow the existing `Static{Regex}` convention of `event_sensor`/`grpc_provider_sensor`.
- Tests live beside the code (`*_tests.rs` / `mod tests`) or in `tests/sensors/` / `tests/federation_contracts_e2e.rs`; follow the existing test names' idiom.

## Review Focus

1. **Avro file without a `topic` key** (or a topic no one publishes) — the schema nodes must still emit and remain joinable via `SchemaDecl`, but NO `PayloadSchema` edge may dangle to a nonexistent Topic. *(Task 1 tests.)*
2. **Broker attribution** — AMQP idioms must label nodes `amqp/<queue>`, never `kafka/<queue>`; a pika-looking line in a Kafka file must not flip the broker. *(Task 2 tests.)*
3. **Generated-name collisions** — `TicketsClient` (support-portal's own proto) must NOT anchor to `orders.Orders`; grouping is by service-name equality against a REAL proto service key, never free-text. *(Task 3 tests.)*
4. **Sidecar-mode git** — history without author (sidecar transport) must keep co-change working exactly as today; author only *refines* matching when both sides have one. *(Task 4 tests.)*
5. **Cross-task interference** — Task 1's schema `Field` nodes must not feed the anchor miner's literal scan with field names (no `customer_id` anchors, ever), and a typed AMQP `Consumes` edge must suppress the corresponding heuristic anchor mint via the existing static-wins rule. *(Tests in Tasks 1 and 2.)*

---

### Task 1: Avro/JSON-Schema payload edges (Topic → Schema)

**Files:**
- Modify: `src/server/sensors/event_sensor.rs` (add `scan_payload_schemas` + emit; call from `scan_workspace_event` at `:83-115`)
- Modify: `src/server/schema.rs:411-417` (flip `EdgeType::PayloadSchema` to indexed)
- Modify: `src/server/graph/mod.rs:193-217` (owner routing for `*.schema.json` if needed)
- Test: `src/server/sensors/event_sensor.rs` `mod tests` + `src/server/schema.rs` `indexed_flags_match_reality`

**Interfaces:**
- Consumes: `payload_schema::{parse_avro_schema, parse_json_schema, ParsedPayloadSchema, ParsedField}` (`src/server/sensors/payload_schema.rs:19-31,63,181`), `event_sensor::emit_sites` conventions (`:471-626`), `ContractFact::Schema { direction: Direction::Payload }` + `Field` + `HasField` as built by `grpc_provider_sensor.rs:180-250`.
- Produces: `Schema` nodes (owned by `SensorOwner::EventSensor`) with `Field` children, `EdgeType::PayloadSchema` edges `Topic → Schema`, and — via the existing `field_join.rs:141,455-458` — payload fields flowing into `FieldRef → Field` joins. Later tasks rely on nothing new from this task.

- [ ] **Step 1: Write the failing test (Avro with topic)**

In `event_sensor.rs` `mod tests`, add:

```rust
#[test]
fn avro_schema_with_topic_emits_payload_schema_edge() {
    let dir = tempdir_with(&[("schemas/order.avsc", r#"{
        "type": "record", "name": "Order", "topic": "order.events",
        "fields": [{"name": "customer_id", "type": "string"}]
    }"#), ("src/producer.py", "producer.send('order.events', msg)\n")]);
    let g = scan_workspace(dir.path());
    let schema = g.nodes().find(|n| n.node_type == NodeType::Schema && n.name.contains("Order")).expect("schema node");
    let topic = g.nodes().find(|n| n.node_type == NodeType::Topic && n.name.ends_with("order.events")).expect("topic node");
    assert!(g.edges_between(topic.id, schema.id).iter().any(|e| e.edge_type == EdgeType::PayloadSchema),
        "Topic -> PayloadSchema edge missing");
    let field = g.nodes().find(|n| n.node_type == NodeType::Field && n.name == "customer_id").expect("field node");
    assert!(g.edges_between(schema.id, field.id).iter().any(|e| e.edge_type == EdgeType::HasField));
}
```

(Use the existing temp-fixture helpers in this test module — `kafkajs_producer_emits_produces_edge` at `:652` shows the idiom; mirror its setup function names exactly.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib event_sensor::tests::avro_schema_with_topic_emits_payload_schema_edge`
Expected: FAIL — "schema node" not found (no emitter exists).

- [ ] **Step 3: Write the failing test (Avro without topic — Review Focus 1)**

```rust
#[test]
fn avro_schema_without_topic_emits_schema_but_no_payload_edge() {
    let dir = tempdir_with(&[("schemas/audit.avsc", r#"{
        "type": "record", "name": "Audit",
        "fields": [{"name": "actor", "type": "string"}]
    }"#)]);
    let g = scan_workspace(dir.path());
    assert!(g.nodes().any(|n| n.node_type == NodeType::Schema && n.name.contains("Audit")));
    assert!(!g.edges().any(|e| e.edge_type == EdgeType::PayloadSchema),
        "no topic -> nothing to bind; a dangling PayloadSchema edge would lie");
}
```

Run: `cargo test --lib event_sensor::tests::avro_schema_without_topic` — expected FAIL.

- [ ] **Step 4: Implement `scan_payload_schemas`**

In `event_sensor.rs`, after `scan_workspace_event`'s site emission (`:83-115`), walk the workspace for `*.avsc` and `*.schema.json`; for each, call `parse_avro_schema` / `parse_json_schema`; then emit exactly the node set `grpc_provider_sensor.rs:180-250` emits for proto messages (reuse its node-building code if extractable — otherwise mirror it):

```rust
fn scan_payload_schemas(graph: &mut Graph, root: &Path) -> Result<usize, LainError> {
    let mut minted = 0;
    for (path, content) in walk_files(root, |p| {
        p.extension() == Some("avsc".as_ref()) || p.file_name().map_or(false, |n| n.to_string_lossy().ends_with(".schema.json"))
    }) {
        let parsed = if path.extension() == Some("avsc".as_ref()) {
            parse_avro_schema(&content)
        } else {
            parse_json_schema(&content)
        };
        let Some(ps) = parsed else { continue };
        let schema_id = upsert_schema_node(graph, &ps.name, &path, SensorOwner::EventSensor)?;
        for f in &ps.fields {
            let field_id = upsert_field_node(graph, &f.path, &f.meta, &path)?;
            insert_edge(graph, EdgeType::HasField, schema_id, field_id, Static::regex())?;
        }
        if let Some(topic_name) = &ps.topic {
            if let Some(topic_id) = find_topic_node(graph, &format!("{}/{}", DEFAULT_BROKER, topic_name)) {
                insert_edge(graph, EdgeType::PayloadSchema, topic_id, schema_id, Static::regex())?;
            }
        }
        minted += 1;
    }
    Ok(minted)
}
```

(Exact helper names may differ — the requirement is: `Schema` + `Field` + `HasField` match `grpc_provider_sensor`'s output shape, ownership is `SensorOwner::EventSensor`, and the `PayloadSchema` edge only exists when a `Topic` node with name `{DEFAULT_BROKER}/{topic}` was already emitted.)

- [ ] **Step 5: JSON-Schema ownership (only if Step 4's `.schema.json` files are not already routed to `EventSensor`)**

In `src/server/graph/mod.rs:193-217` (`derive_owner`), extend the routing so `*.schema.json` files' `Schema`/`Field` nodes route to `SensorOwner::EventSensor` (plain `.json` stays `OpenApiSensor`). Add a unit test next to `derive_owner`'s existing ones asserting both routings.

- [ ] **Step 6: Flip `is_indexed` and make the flag test pass**

`src/server/schema.rs:411-417`: change `EdgeType::PayloadSchema | ... => false` to keep `PayloadSchema` out of the `false` arm (i.e. indexed). The existing `indexed_flags_match_reality` test will now demand a producer — Task 1 Steps 3-4 are that producer. Run `cargo test --lib schema::` — expected PASS (this is the plan's own honesty gate).

- [ ] **Step 7: Run the full task suites and commit**

Run: `cargo test --lib event_sensor schema graph::` then `cargo test --test sensors` then `cargo fmt --check`.
Expected: all PASS.

- [ ] **Step 8: Pin the cross-task rule — schema fields never become anchors (Review Focus 5)**

The miner's literal scan will now see `.avsc` content (quoted `customer_id` inside schema files). In `anchor_miner_tests.rs`:

```rust
#[test]
fn avro_schema_field_names_do_not_become_cross_repo_anchors() {
    let obs = occurrences(&[
        ("orders-svc", "schemas/order.avsc", r#"{"name": "Order", "fields": [{"name": "customer_id"}]}"#),
        ("billing-svc", "src/main.py", "row[\"customer_id\"]\n"),
    ]);
    let edges = mint(obs, commits(vec![]));
    assert!(!edges.iter().any(|e| e.detector.starts_with("anchor:")),
        "field names are not structural anchors, inside or outside schema files: {edges:?}");
}
```

Run: `cargo test --lib federation::anchor_miner_tests::avro_schema_field_names` — expected PASS (guards the existing exclusion against Task 1's new file kind).

```bash
git add src/server/sensors/event_sensor.rs src/server/schema.rs src/server/graph/mod.rs src/server/federation/anchor_miner_tests.rs
git commit -m "feat(sensors): Avro/JSON-Schema payload edges — Topic -> Schema (closes a documented gap)

payload_schema.rs parsers had no production callers and EdgeType::
PayloadSchema was known-but-unindexed (schema.rs:411). event_sensor now
walks .avsc/*.schema.json, emits Schema/Field/HasField owned by
EventSensor, and mints Topic -> PayloadSchema when the schema names a
topic that exists. Payload fields then flow into the existing
FieldRef -> Field join via field_join.rs:141. is_indexed flipped — the
flag test is the honesty gate."
```

---

### Task 2: AMQP bus sensor (pika + amqplib)

**Files:**
- Modify: `src/server/sensors/patterns/frameworks.yaml` (python + tsjs entries)
- Modify: `src/server/sensors/event_sensor.rs` (`detect_sites` `:221-281` — broker derivation)
- Test: `src/server/sensors/event_sensor.rs` `mod tests`, `tests/sensors/patterns_protocol_idioms.rs`

**Interfaces:**
- Consumes: `Patterns::topic_producer_patterns(lang)` / `topic_consumer_patterns(lang)` (`src/server/sensors/patterns/mod.rs:622,637`), `util::walk_idioms` (`util.rs:766`), `schedule_value`'s framework→value mapping idiom (`event_sensor.rs:344-366`).
- Produces: `Topic` nodes named `amqp/<queue>` with `Produces`/`Consumes` `Static{Regex}` edges and `TopicConsumerFact { broker: "amqp", .. }` — the same shapes Kafka produces with `kafka/`. Task 3 and the anchor miner rely on nothing new; the static-wins rule in `anchor_miner.rs:339-347` then suppresses the heuristic twin automatically.

- [ ] **Step 1: Write the failing test (pika consumer)**

In `event_sensor.rs` `mod tests`, beside `confluent_kafka_subscribe_emits_consumes_edge` (`:744`):

```rust
#[test]
fn pika_basic_consume_emits_amqp_consumes_edge() {
    let dir = tempdir_with(&[("src/ledger.py", r#"
import pika
ch = conn.channel()
ch.queue_bind(queue="order-events", exchange="x")
ch.basic_consume(queue="order-events", on_message_callback=cb)
"#)]);
    let g = scan_workspace(dir.path());
    let topic = g.nodes().find(|n| n.node_type == NodeType::Topic && n.name == "amqp/order-events")
        .expect("amqp topic node (NOT kafka/order-events)");
    assert!(g.edges().any(|e| e.edge_type == EdgeType::Consumes && e.target_id == topic.id));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib event_sensor::tests::pika_basic_consume_emits_amqp_consumes_edge`
Expected: FAIL — no `amqp/order-events` node (today this pattern mints nothing typed; the anchor miner's heuristic is not a sensor edge).

- [ ] **Step 3: Write the failing tests (broker + collision — Review Focus 2)**

```rust
#[test]
fn broker_derives_from_framework_not_filename() {
    // A kafka-shaped file with a pika call must produce an amqp label,
    // and a pika-shaped file with a kafka call must produce a kafka label.
    let dir = tempdir_with(&[
        ("src/a.py", "ch.basic_publish(exchange='x', routing_key='q1', body=b)\n"),
        ("src/b.py", "producer.send('t1', msg)\n"),  // aiokafka idiom
    ]);
    let g = scan_workspace(dir.path());
    assert!(g.nodes().any(|n| n.name == "amqp/q1"), "pika publish -> amqp/q1");
    assert!(g.nodes().any(|n| n.name == "kafka/t1"), "kafka send -> kafka/t1");
}
```

Run: `cargo test --lib event_sensor::tests::broker_derives_from_framework` — expected FAIL.

- [ ] **Step 4: Add pattern entries (data-only)**

In `frameworks.yaml` under `python:`:

```yaml
    - id: pika-publish
      kind: topic_producer
      path_regex: '\.basic_publish\s*\([^)]*?routing_key\s*=\s*(?:["'']([^"'']+)["'']|([A-Za-z_]\w*))'
    - id: pika-consume
      kind: topic_consumer
      path_regex: '\.basic_consume\s*\([^)]*?queue\s*=\s*(?:["'']([^"'']+)["'']|([A-Za-z_]\w*))'
    - id: pika-queue-declare
      kind: topic_producer
      path_regex: '\.queue_declare\s*\([^)]*?queue\s*=\s*(?:["'']([^"'']+)["'']|([A-Za-z_]\w*))'
```

Under `tsjs:`:

```yaml
    - id: amqplib-publish
      kind: topic_producer
      path_regex: '\.publish\s*\([^,]+,\s*(?:["'']([^"'']+)["'']|([A-Za-z_]\w*))'
    - id: amqplib-consume
      kind: topic_consumer
      path_regex: '\.consume\s*\([^,]+,\s*(?:["'']([^"'']+)["'']|([A-Za-z_]\w*))'
```

Add a test in `tests/sensors/patterns_protocol_idioms.rs` (idiom of `a_new_idiom_is_a_data_change`, `:356`) asserting the five new entries are bundled and yield literals via `walk_idioms`.

- [ ] **Step 5: Derive the broker from `framework_id`**

In `event_sensor.rs::detect_sites` (`:221-281`), mirror `schedule_value` (`:344-366`): a new `broker_for(m: &IdiomMatch) -> &'static str` returning `"amqp"` when `m.framework_id` starts with `pika`/`amqplib`, else `DEFAULT_BROKER`. Use it at the two `broker: DEFAULT_BROKER.into()` sites (`:249`, `:260`).

```rust
fn broker_for(m: &IdiomMatch) -> &'static str {
    if m.framework_id.starts_with("pika") || m.framework_id.starts_with("amqplib") {
        "amqp"
    } else {
        DEFAULT_BROKER
    }
}
```

- [ ] **Step 6: Pin the static-wins interaction (Review Focus 5)**

A typed AMQP `Consumes` edge must suppress the anchor miner's heuristic twin. In `anchor_miner_tests.rs`:

```rust
#[test]
fn typed_amqp_consumes_edge_suppresses_queue_anchor_mint() {
    // Same pair as queue_anchor_shared_across_two_repos_mints_heuristic_edges,
    // but a Static edge already joins the two symbols.
    let obs = occurrences(&[
        ("orders-svc", "src/legacy_amqp.rs", "ch.queue_declare(queue=\"order-events\")\n"),
        ("ledger-svc", "src/ledger.py", "ch.basic_consume(queue=\"order-events\", on_message_callback=cb)\n"),
    ]);
    let edges = mint(obs, commits(vec![]), Some(static_edge("orders-svc:src/legacy_amqp.rs", "ledger-svc:src/ledger.py")));
    assert!(!edges.iter().any(|e| e.detector.starts_with("anchor:queue")),
        "static evidence wins; the heuristic twin must not duplicate it");
}
```

(Match the existing helper in `static_edge_between_pair_suppresses_anchor_mint` (`:373`) for how a static edge is injected.)

Run: `cargo test --lib federation::anchor_miner_tests::typed_amqp` — expected PASS against the existing suppression rule.

- [ ] **Step 7: Run the task suites and commit**

Run: `cargo test --lib event_sensor` , `cargo test --test sensors` , `cargo test --test federation_contracts_e2e` , `cargo fmt --check`.
Expected: PASS (the joiner needs no change — `default_broker_for` (`endpoints.rs:224-232`) parses the `amqp/` prefix into `ContractKey::Topic { broker: "amqp", .. }`).

```bash
git add src/server/sensors/patterns/frameworks.yaml src/server/sensors/event_sensor.rs tests/sensors/patterns_protocol_idioms.rs
git commit -m "feat(sensors): AMQP bus detection — pika/amqplib mint amqp/<queue> topics

Previously only Kafka idioms minted typed Produces/Consumes; an AMQP
consumer showed zero sensor facts (the enterprise fixture's ledger-svc
blind spot). Patterns are data-only; the one code change derives the
broker from the framework id (pika/amqplib -> 'amqp') instead of the
hardcoded 'kafka', so the joiner's topic bind works unchanged."
```

---

### Task 3: Generated-code gRPC anchors in the anchor miner

**Files:**
- Modify: `src/server/federation/anchor_miner.rs` (`extract_from_line` `:867-915`, grouping in `mint_cross_repo_observations` `:361-367`)
- Test: `src/server/federation/anchor_miner_tests.rs`

**Interfaces:**
- Consumes: `AnchorKind::Proto` (`anchor_miner.rs:160-166`), `proto_ident_key` (`:1042-1076`), the proto-side keys already minted (`pkg.Service` from `service Foo` at `:823-834`, `pkg.Service/Method` from `rpc` at `:836-847`), the suffix-compare idiom from `consumer_protocol.rs:381-408`.
- Produces: new anchor occurrences of kind `Proto` whose key is a **service segment** (e.g. `Orders`), matched at group time against proto keys whose service segment equals them case-insensitively. Edge shape unchanged (`Calls`, `Heuristic`, `anchor:proto:<key>`, 0.6).

- [ ] **Step 1: Write the failing test (generated stub links to proto service)**

In `anchor_miner_tests.rs`, beside `proto_identifiers_extract` (`:663`):

```rust
#[test]
fn generated_stub_names_anchor_to_proto_service() {
    // orders-svc declares service Orders in protos/orders.proto;
    // logistics-svc references only generated code — no proto literals.
    let obs = occurrences(&[
        ("orders-svc", "protos/orders.proto", "service Orders {\n  rpc GetOrder(GetOrderRequest) returns (Order);\n}"),
        ("logistics-svc", "src/handler.go", "c := pb.NewOrdersClient(conn)\nresp, err := c.GetOrder(ctx, req)\nvar s pb.OrdersClient\n"),
    ]);
    let edges = mint(obs, commits(vec![]));
    assert!(edges.iter().any(|e| e.detector.starts_with("anchor:proto")
        && e.source.contains("orders-svc") && e.target.contains("logistics-svc")),
        "OrdersClient/NewOrdersClient must anchor to the Orders service: {edges:?}");
}
```

(Match the module's existing helper names — `occurrences`/`mint` equivalents live in the test file's fixture helpers; copy the exact names from `queue_anchor_shared_across_two_repos_mints_heuristic_edges` at `:102`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib federation::anchor_miner_tests::generated_stub_names_anchor_to_proto_service`
Expected: FAIL — `pb.NewOrdersClient`/`OrdersClient` anchor nothing today (`pb.FooClient` keys as `pb.FooClient`, never equal to `pb.Foo`).

- [ ] **Step 3: Write the failing collision test (Review Focus 3)**

```rust
#[test]
fn generated_stub_never_crosses_unrelated_proto_services() {
    // support-portal's own TicketsClient must NOT link to orders.Orders.
    let obs = occurrences(&[
        ("orders-svc", "protos/orders.proto", "service Orders {\n  rpc GetOrder(..) returns (Order);\n}"),
        ("support-portal", "src/show.ts", "const c = new TicketsClient(channel);\nc.showTicket(id);\n"),
    ]);
    let edges = mint(obs, commits(vec![]));
    assert!(!edges.iter().any(|e| e.detector.starts_with("anchor:proto")),
        "TicketsClient has no Orders service segment: {edges:?}");
}
```

Run: expected FAIL (or PASS vacuously — if it passes before the feature exists, it still pins the post-feature behavior).

- [ ] **Step 4: Implement the generated-name extractor + suffix grouping**

In `extract_from_line` (`:867-915`), add an identifier pass (beyond quoted literals): identifiers matching the generated-client shapes produce `AnchorKind::Proto` occurrences keyed by the **service segment only**:

```rust
fn generated_service_key(ident: &str) -> Option<String> {
    // NewOrdersClient / OrdersClient / OrdersStub / OrdersBlockingStub /
    // ordersClient (Go var) / FooGrpc.FooBlockingStub (receiver type)
    let base = ident.rsplit('.').next().unwrap_or(ident);
    let base = base.strip_prefix("New").unwrap_or(base);
    for suffix in ["BlockingStub", "Client", "Stub"] {
        if let Some(s) = base.strip_suffix(suffix) {
            if s.len() >= 3 && s.chars().next().map_or(false, |c| c.is_uppercase()) {
                return Some(s.to_string());
            }
        }
    }
    None
}
```

In the grouping step of `mint_cross_repo_observations` (`:361-367`), where keys currently match by exact `(kind, key)` equality, extend `AnchorKind::Proto` matching: a group forms when the service segments are equal ASCII-case-insensitively — a generated key `Orders` joins a proto key `orders.Orders` (compare the last dot-segment of the proto key, and the leading segment of `pkg.Svc/Method` keys). Detector label keeps the proto key: `anchor:proto:orders.Orders`.

- [ ] **Step 5: Run the task suites and commit**

Run: `cargo test --lib federation::anchor_miner` (18 existing + 2 new), `cargo test --lib claims` , `cargo fmt --check`.
Expected: all PASS — including `anchor_chain_classifies_as_needs_investigation` (`:428`) untouched (same heuristic shape).

```bash
git add src/server/federation/anchor_miner.rs src/server/federation/anchor_miner_tests.rs
git commit -m "feat(federation): generated-code anchors — stub type names join proto services

pb.NewOrdersClient / OrdersClient / OrdersStub never anchored: proto
keys are pkg.Service but consumer source carries only generated type
names, so gRPC consumers without proto literals stayed unlinkable.
The miner now extracts service segments from generated-client
identifiers and groups them case-insensitively against real proto
service keys — still heuristic 0.6, and only against a declared
service, so TicketsClient can never reach orders.Orders."
```

---

### Task 4: Author-aware co-change

**Files:**
- Modify: `src/server/git.rs` (`CommitInfo` `:435-440`; fill author in `GitSensor::get_commit_history` `:217-262`)
- Modify: `src/server/sidecar.rs` (`proto_to_commitinfo` `:711` — author stays empty)
- Modify: `src/server/federation/anchor_miner.rs` (`CommitRecord` `:217-223`; `collect_commit_records` `:781-804`; match in `process_cochange_cluster` `:584-644`)
- Test: `src/server/federation/anchor_miner_tests.rs`, `tests/sidecar_parity.rs`

**Interfaces:**
- Consumes: `CommitInfo` (`git.rs:435-440`), `AnyGitSensor::get_commit_history` (`git.rs:791-796`), wire `CommitInfo` in `src/sidecar_proto.rs:143-148` (SEPARATE struct — the wire layout does NOT change).
- Produces: `CommitRecord.author: String` (empty when the source cannot provide it — sidecar mode). Co-change rule: `normalize_message` equality AND `|Δt| <= 120s` AND (`a.author == b.author` when both non-empty; otherwise today's behavior).

- [ ] **Step 1: Write the failing test (same subject, different authors — no edge)**

In `anchor_miner_tests.rs`, beside `cross_repo_same_message_commits_mint_cochange_at_point_three` (`:238`):

```rust
#[test]
fn cochange_requires_same_author_when_both_known() {
    let recs = vec![
        CommitRecord { repo: "a".into(), message: "bump dep".into(), time: 1000,
                       files: vec!["Cargo.toml".into()], author: "bot".into() },
        CommitRecord { repo: "b".into(), message: "bump dep".into(), time: 1005,
                       files: vec!["Cargo.toml".into()], author: "human".into() },
    ];
    let edges = mint(occurrences(vec![]), recs);
    assert!(!edges.iter().any(|e| e.detector == "cochange"),
        "two authors bumping the same message in the same minute is coincidence, not coupling");
}

#[test]
fn cochange_falls_back_when_author_unknown() {
    let recs = vec![
        CommitRecord { repo: "a".into(), message: "bump dep".into(), time: 1000,
                       files: vec!["Cargo.toml".into()], author: String::new() },
        CommitRecord { repo: "b".into(), message: "bump dep".into(), time: 1005,
                       files: vec!["Cargo.toml".into()], author: String::new() },
    ];
    let edges = mint(occurrences(vec![]), recs);
    assert!(edges.iter().any(|e| e.detector == "cochange"),
        "sidecar-mode history has no author; behavior must not regress");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib federation::anchor_miner_tests::cochange`
Expected: FAIL (compile error first — `CommitRecord` has no `author`; add the field with `String::default()` semantics and re-run to see the behavioral failure).

- [ ] **Step 3: Implement**

1. `git.rs:435-440`: add `pub author: String` to `CommitInfo`.
2. `git.rs:254-259` (in-process revwalk): `author: commit.author().name().unwrap_or_default().to_string()`.
3. `src/server/sidecar.rs:711` (`proto_to_commitinfo`): `author: String::new()` (wire struct untouched — `sidecar_proto.rs:143-148` stays as is; NO protocol bump).
4. `anchor_miner.rs:217-223`: add `pub author: String` to `CommitRecord`; `collect_commit_records` (`:789-795`) maps it through.
5. `process_cochange_cluster` (`:584-644`): inside the pair loop (next to the `|a.time - b.time| > 120` skip at `:603`):

```rust
if !a.author.is_empty() && !b.author.is_empty() && a.author != b.author {
    continue;
}
```

- [ ] **Step 4: Run the task suites and commit**

Run: `cargo test --lib federation::anchor_miner git` , `cargo test --test sidecar_parity` , `cargo test --test sidecar_robustness` , `cargo fmt --check`.
Expected: all PASS (parity tests prove the wire protocol is unchanged).

```bash
git add src/server/git.rs src/server/sidecar.rs src/server/federation/anchor_miner.rs src/server/federation/anchor_miner_tests.rs
git commit -m "feat(federation): author-aware co-change — bot noise stops minting edges

CommitRecord gains author (in-process GitSensor fills it from the
commit; the sidecar wire protocol is a separate struct and stays
version 2, so sidecar-mode keeps working with author '').
Co-change now requires author equality when both sides know one —
'two bots bumping the same message in the same minute' is no longer
coupling. Author-less history behaves exactly as before."
```

---

## Final validation (after all four tasks)

- [ ] **Smoke the acceptance fixtures** (the program's own oracle):

```bash
cargo build --release --bin lain --bin lain-git-sidecar
python3 tests/fixtures/enterprise_scale/make_task.py build /tmp/td-val && python3 tests/fixtures/enterprise_scale/make_task.py apply-edit /tmp/td-val
cd /tmp/td-val && ../path/to/target/release/lain reindex
```

Expected, from `ground_truth.json._smoke_notes`: `ledger-svc` now shows a TYPED `amqp/order-events` consume edge (`Static{Regex}` — the anchor heuristic is suppressed by the static-wins rule); `logistics-svc` anchors to `orders.Orders` even in source with only generated names; schema payloads (if the fixture carries an `.avsc`) appear as `Schema` nodes joined to their topic. Paste all outputs in the closing report.

- [ ] **Full suites**: `cargo test --lib` (2522+), `cargo test --test sensors`, `--test federation_contracts_e2e`, `--test sidecar_parity`, `cargo fmt --check`, clippy as in Global Constraints.

- [ ] **Update the fixture smoke notes** in `tests/fixtures/enterprise_{mesh,scale}/ground_truth.json` `_smoke_notes`: the gaps they name (confluent_kafka edge missing, rpc_stub_unknown, payload schemas unindexed) are now closed — rewrite the notes to state the new behavior, keeping the historical record in `docs/acceptance/FINDINGS-2026-10-11.md`.
