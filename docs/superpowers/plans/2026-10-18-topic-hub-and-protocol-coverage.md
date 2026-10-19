# Topic Hub + Protocol Coverage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every message/bus topic a single canonical hub node (one node per `broker/topic` with all publishers and subscribers hanging off it), and close the protocol-coverage gaps so no contract kind — including future buses — escapes detection, joining, binding, and visualization.

**Architecture:** Two parts that share one theme (protocol completeness) and land independently. **Part A (Tasks 1–2):** `event_sensor` currently mints one `Topic` node per *site* (`generate_id` includes `path` + `line`, `event_sensor.rs:663-669`), fragmenting a topic across files; the fix is an id without path/line so every site converges on one hub node per `(broker, topic)` per repo, with `Produces`/`Consumes` edges (already site→topic, `event_sensor.rs:714-740`) landing on the hub, plus hub visibility in the graph views. **Part B (Tasks 3–5):** the six `ContractKey` kinds (`Http`, `Topic`, `Rpc`, `Graphql`, `WebSocket`, `Table`, `model.rs:595-635`) all have sensors and joiners today, but `check_binding`/bindings grammar covers only HTTP+GraphQL (`contracts.rs:669`), and the dashboard graph view shows only Functions/Methods/Classes + Calls/Imports — so manual confirmation and visualization silently omit four kinds. We add the grammar, extend the views, and pin the whole matrix with an exhaustive contract test so a future kind cannot escape.

**Tech Stack:** Rust (tree-sitter sensors, petgraph federation, MCP tool layer), TLA+ untouched.

**Spec:** Event topics: `docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §6.7 (the `event_sensor` header quotes it). Binding grammar: `docs/REPOS_YAML.md` + `docs/tool-schema.json` (`check_binding`, `list_unresolved`). The **protocol coverage matrix in this plan** is the audit deliverable ("que no se nos escapen otros protocolos") and binds Tasks 3–5.

## Global Constraints

- Branches cut from `dev`, merged back to `dev`; English commit subjects; no version bumps in feature PRs.
- CHANGELOG entry for every user-visible change. Node-identity changes need a `lain reindex` note in CHANGELOG (precedent: node-type additions fold into the current `FEDERATION_GRAPH_VERSION` with a CHANGELOG note; bump `FEDERATION_GRAPH_VERSION` ONLY if the loader cannot read older graphs — verify, do not assume).
- Tool output schema changes require updating `src/server/mcp/contract_tools/schemas/*.json` AND regenerating `docs/tool-schema.json` (`make schema` / `lain schema`); `tests/contracts_golden.rs` and the CI schema-drift job must stay green.
- `cargo test --workspace` green + `cargo fmt --all -- --check` + CI clippy flags (`-D warnings -A clippy::style -A clippy::complexity -A clippy::perf -A clippy::pedantic -A unused -A dead_code`) before every commit.
- Host quirk: `TMPDIR=/data/agents/tmp-lain-tests` on every cargo call (plain `/tmp` is quota-hostile on the dev box; NEVER a TMPDIR inside a repo — 9 walk-up tests fail).
- Detector patterns stay data, not scattered literals (standing directive): new bus patterns go into the event sensor's pattern list as declarative entries, and the follow-up to move them to config files is named in Task 5, not silently forgotten.

## Protocol coverage audit (2026-10-18, verified against source)

| ContractKey | Sensor(s) | Joiner resolver | `check_binding` / bindings grammar | Dashboard graph view | Verdict |
|---|---|---|---|---|---|
| `Http` | `http_sensor`, `http_client_sensor`, `openapi_sensor` | HTTP ladder (`consumer_http.rs`) | ✅ | ✅ (Calls) | complete |
| `Graphql` | `graphql_consumer_sensor`, `graphql_provider_sensor`, `graphql_resolver_link_sensor` | `resolve_graphql_consumer` (`consumer_protocol.rs:218`) | ✅ (since v0.11 work) | ⚠️ nodes exist; GraphQL edges not in the dashboard filter | Task 2 |
| `Topic` | `event_sensor` (kafkajs, confluent-kafka, confluent_kafka, aiokafka, rdkafka, kafka-go, pika, amqplib, Celery, NestJS `@Cron`) | `resolve_topic_consumer` (`:177`) | ❌ rejected ("only http and graphql… in 0.9") | ❌ | Tasks 1–3 |
| `Rpc` | `grpc_provider_sensor`, `grpc_consumer_sensor`, `grpc_handler_link_sensor`, `proto_sensor` | `resolve_rpc_consumer` (`:287`) | ❌ | ❌ | Tasks 2–3 |
| `WebSocket` | `websocket_sensor` | `resolve_websocket_consumer` (`:428`) | ❌ | ❌ | Tasks 2–3 |
| `Table` | `sql_sensor` | `resolve_table_consumer` (`:523`) | ❌ | ❌ | Tasks 2–3 |

**Bus/system backlog (detection patterns that do NOT exist yet — "más posibles"):** MQTT (paho/mqtt.js), NATS (nats.py/nats.js), Redis pub/sub + Streams (redis-py/ioredis), AWS SQS/SNS (boto3/aws-sdk), Kinesis, EventBridge, Pulsar, ZeroMQ, Google Pub/Sub, job queues (Temporal, Sidekiq, Resque, Dramatiq, Huey), Quartz (Java `@Scheduled`), Inngest, Debezium/CDC output topics. None of these is a `ContractKey` problem — `Topic { broker, name }` is broker-generic; they are pure detection-pattern additions (Task 5 documents the recipe; adding systems is backlog by design).

## Review Focus

1. **Node-identity migration:** rescanning a repo whose old graph has per-site Topic nodes must retract the old ones and leave exactly one hub per `(broker, topic)` (sensor ownership + `replace_sensor_output` semantics — `src/server/sensors/AGENTS.md` warns shared nodes lose edges). Pinned by Task 1's rescan test.
2. **Parallel hub edges:** two sites in one file publishing the same topic must yield ONE hub node and edge provenance per SITE (the `site: SourceSite{path,line}` field exists for exactly this) — decide and pin: one `Produces` edge per site, not deduped. Pinned by Task 1.
3. **Cross-repo hubs:** the same `broker/topic` in two repos is two hub nodes (per-repo identity) linked at the CONTRACT layer; the view must not pretend otherwise. Pinned by Task 2.
4. **Key grammar edge cases:** `topic:<broker>:<name>` where broker or name contain `:` or `/` — parse must be unambiguous (`splitn` discipline) or documented as rejected. Pinned by Task 3.
5. **Coverage cannot silently regress:** a seventh `ContractKey` variant added later must fail the Task 4 matrix test at compile time (exhaustive `match`), not at runtime with a shrug.

## File Structure

- `src/server/sensors/event_sensor.rs` — hub identity (Task 1); pattern list documentation (Task 5).
- `src/server/schema.rs` — `GraphNode::generate_id` (read-only for Task 1; hub uses `line_start: None`).
- `src/server/mcp/federation_tools/federation.rs` — `get_workspace_graph` type/edge filters (Task 2).
- `src/server/federation/contracts/config.rs` (`validate_key`) + `src/server/federation/contracts/key_compat.rs` + `src/server/mcp/contract_tools/contracts.rs` (`run_check_binding`) + `schemas/check_binding.out.json` — bindings grammar (Task 3).
- `src/server/federation/contracts/coverage_matrix.rs` (NEW) + `src/server/federation/contracts/mod.rs` — the exhaustive coverage test (Task 4).
- `docs/protocol-coverage.md` (NEW) + `docs/REPOS_YAML.md` + `docs/query-language.md` + `CHANGELOG.md` — documentation (Tasks 3–5).
- Tests: `tests/event_hub_e2e.rs` (NEW, Task 1–2), `tests/federation_contracts_e2e.rs` (Task 3), `src/server/federation/contracts/coverage_matrix.rs` (Task 4).

---

## Part A — Topic hub

### Task 1: Canonical Topic hub node (one node per broker/topic)

**Files:**
- Modify: `src/server/sensors/event_sensor.rs:662-676` (topic node minting) and `:235-250` (schema-link fan-out)
- Test: `tests/event_hub_e2e.rs` (NEW)

**Interfaces:**
- Consumes: `GraphNode::generate_id(node_type: &NodeType, path: &str, name: &str, line_start: Option<u32>, namespace: &RepoNamespace) -> String` (`src/server/schema.rs:857-867`; id embeds `path` and `line` when `line_start` is `Some`).
- Produces: hub nodes keyed `generate_id(&NodeType::Topic, TOPIC_ANCHOR, &topic_label, None, namespace)` where `TOPIC_ANCHOR = "<topics>"` and `topic_label = format!("{broker}/{topic_name}")` (unchanged label shape, `event_sensor.rs:662`). Later tasks assume one node per `(namespace, topic_label)`.

- [ ] **Step 1: Write the failing tests**

```rust
// tests/event_hub_e2e.rs
#[test]
fn two_sites_one_topic_mint_exactly_one_hub_node() {
    let root = fixture_repo_with(
        "src/producer.ts",
        "producer.send({ topic: 'orders.created' });\nproducer.send({ topic: 'orders.created' });\n",
    );
    let graph = scan_events(&root);
    let hubs: Vec<_> = graph.nodes.iter()
        .filter(|n| n.node_type == NodeType::Topic)
        .collect();
    assert_eq!(hubs.len(), 1, "two sites must converge on one hub, got {hubs:?}");
    assert_eq!(hubs[0].name, "kafka/orders.created");
}

#[test]
fn hub_keeps_one_produces_edge_per_site() {
    let root = fixture_repo_with("src/producer.ts", /* two sends on lines 1 and 2 */);
    let graph = scan_events(&root);
    let produces: Vec<_> = graph.edges.iter()
        .filter(|e| e.edge_type == EdgeType::Produces && e.target_id == hub_id(&graph))
        .collect();
    assert_eq!(produces.len(), 2, "site provenance must survive: one edge per site");
    assert!(produces.iter().all(|e| e.site.is_some()));
}

#[test]
fn rescan_retracts_old_per_site_topic_nodes() {
    // scan once with the OLD id scheme seeded into the graph (simulate by
    // minting a second Topic node with a line-anchored id), rescan, and
    // assert exactly one hub remains and no edges dangle.
}
```

(Adapt fixture helpers to the event sensor's existing unit-test style in `event_sensor.rs` `mod tests`; the assertions are the contract.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `TMPDIR=/data/agents/tmp-lain-tests cargo test --test event_hub_e2e`
Expected: FAIL — `hubs.len()` is 2 for two sites (per-site ids today).

- [ ] **Step 3: Implement the hub id**

In `event_sensor.rs` at the minting site (`:662-676`):

```rust
let topic_label = format!("{}/{}", site.broker, topic_name);
// Hub identity: one node per (broker, topic) per repo — no path, no line.
const TOPIC_ANCHOR: &str = "<topics>";
let topic_id = GraphNode::generate_id(
    &NodeType::Topic,
    TOPIC_ANCHOR,
    &topic_label,
    None,
    namespace,
);
```

Keep the `emitted_topic_ids` set (it now dedupes per repo scan); when the hub already exists, do NOT re-push the node, only push the site's `Produces`/`Consumes` edge (the edge keeps `site: Some(SourceSite{ path, line })` — that is the per-site provenance). The schema-link fan-out at `:235-250` simplifies to the single hub id (delete the "one edge per emitted node with this name" loop — one hub, one `PayloadSchema` edge).

- [ ] **Step 4: Run tests to verify they pass**

Run: `TMPDIR=/data/agents/tmp-lain-tests cargo test --test event_hub_e2e` → PASS (3/3), then the event sensor's own tests: `cargo test --lib event_sensor` → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/server/sensors/event_sensor.rs tests/event_hub_e2e.rs
git commit -m "feat: topic nodes are canonical hubs (one per broker/topic per repo)"
```

### Task 2: Hub visibility in the graph views

**Files:**
- Modify: `src/server/mcp/federation_tools/federation.rs` (`get_workspace_graph` type/edge filter — today: Function/Method/Class + Calls/Imports)
- Test: `tests/event_hub_e2e.rs` (extend), `src/server/mcp/federation_tools/` verification tests if present

**Interfaces:**
- Consumes: hub nodes/edges from Task 1.
- Produces: `get_workspace_graph` includes `NodeType::Topic` and `EdgeType::Produces|Consumes` (and `PayloadSchema` when a schema is linked); response shape unchanged (`{nodes, edges, truncated}`, `cross_repo` marking as today).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn workspace_graph_shows_topic_hub_with_its_legs() {
    // repo with one producer site and one consumer site on 'orders.created'
    let out = call_get_workspace_graph(&fixture, None);
    let topics: Vec<_> = out.nodes.iter().filter(|n| n["type"] == "Topic").collect();
    assert_eq!(topics.len(), 1);
    let legs: Vec<_> = out.edges.iter()
        .filter(|e| e["type"] == "Produces" || e["type"] == "Consumes")
        .collect();
    assert_eq!(legs.len(), 2, "hub must show publisher and subscriber legs");
}

#[test]
fn cross_repo_same_topic_stays_two_hubs() {
    // repo A publishes 'kafka/orders'; repo B subscribes to 'kafka/orders'.
    // Per-repo identity: two Topic nodes (one per repo), NOT merged in the
    // view — the contract layer is what links them cross-repo.
    let out = call_get_workspace_graph(&federated_fixture, None);
    let topics: Vec<_> = out.nodes.iter().filter(|n| n["type"] == "Topic").collect();
    assert_eq!(topics.len(), 2);
    assert!(out.edges.iter().any(|e| e["type"] == "Consumes" && e["cross_repo"] == false),
        "each hub keeps its own legs inside its repo");
}
```

- [ ] **Step 2: Run it** → Expected: FAIL (Topic nodes filtered out today).

- [ ] **Step 3: Extend the filters** in `get_workspace_graph` to admit `Topic` nodes and `Produces`/`Consumes`/`PayloadSchema` edges alongside the existing set. Keep the response DTO unchanged.

- [ ] **Step 4: Run** → PASS; also `cargo test --test event_hub_e2e` stays green.

- [ ] **Step 5: Commit**

```bash
git add src/server/mcp/federation_tools/federation.rs tests/event_hub_e2e.rs
git commit -m "feat: dashboard graph shows topic hubs and their produces/consumes legs"
```

## Part B — No protocol escapes

### Task 3: Bindings + `check_binding` for Topic/Rpc/WebSocket/Table keys

**Files:**
- Modify: `src/server/federation/contracts/config.rs` (`validate_key`, ~`:587-630` — today: `graphql:` branch + HTTP space grammar)
- Modify: `src/server/mcp/contract_tools/contracts.rs` (`run_check_binding` match on `&key`, ~`:615-668` — today: Http arm, Graphql arm, `_ => error "only http and graphql endpoints are supported in 0.9"`)
- Modify: `src/server/mcp/contract_tools/schemas/check_binding.out.json` (+ `make schema`)
- Modify: `docs/REPOS_YAML.md`, `CHANGELOG.md`
- Test: `tests/federation_contracts_e2e.rs` (pr15 module)

**Interfaces:**
- Consumes: `ContractKey::FromStr` already parses `topic:…` (`model.rs:757-821`; `parse_contract_key` passes `topic:` through, `joiner/mod.rs:785-793`) and `keys_compatible` (`key_compat.rs`) which today falls back to equality for non-HTTP/GraphQL.
- Produces: `validate_key` accepts `topic:<broker>:<name>`, `rpc:<system>:<service>:<method>`, `ws:<route>`, `table:<name>`; `check_binding` validates them with the same key-compatibility predicate; new reasons `key_kind_mismatch` is NOT added — cross-kind stays `op_mismatch`/`field_mismatch` for GraphQL and gains `method_mismatch`/`template_mismatch` analogues ONLY if they fit; otherwise document the reason mapping in the tool description. Exact rule: for these four kinds, well-formed pair = same variant + `keys_compatible` equality semantics (broker+name / system+service+method / route / name).

- [ ] **Step 1: Write the failing tests** (pr15 in `tests/federation_contracts_e2e.rs`)

```rust
#[tokio::test]
async fn pr15_check_binding_accepts_topic_keys() {
    // fixture: topic provider 'kafka/orders' in repo A, consumer site in repo B
    let out = check_binding_handle(&ctx, json!({
        "snapshot": "live",
        "consumer": "<consumer global id>",
        "endpoint": {"service": "orders-svc", "key": "topic:kafka:orders"}
    })).await;
    assert_eq!(out["data"]["valid"], json!(true));
    assert!(out["data"]["bindings_entry"].as_str().unwrap().contains("topic:kafka:orders"));
}

#[tokio::test]
async fn pr15_check_binding_rejects_cross_kind_topic_vs_http() {
    // topic consumer + http endpoint key → valid:false, no panic
}

#[tokio::test]
async fn pr15_validate_key_accepts_the_four_new_grammars() {
    // config load with bindings keys: topic:kafka:orders, rpc:grpc:orders.v1:Get,
    // ws:/realtime, table:public.orders → Ok; and a malformed topic:a:b:c fails.
}
```

- [ ] **Step 2: Run** → Expected: FAIL ("only http and graphql endpoints are supported in 0.9").

- [ ] **Step 3: Implement** — extend `validate_key` with a `topic:`/`rpc:`/`ws:`/`table:` branch (parse via `ContractKey::from_str`, reject empty names), replace the `_ =>` error arm in `run_check_binding` with the four variants routed through `keys_compatible`, and keep the HTTP/GraphQL arms byte-identical. Update `check_binding.out.json` ONLY if the reason enum changes (prefer reusing existing reasons: cross-kind → `method_mismatch`, same-kind mismatch → `template_mismatch`, matching the mapping already used for HTTP). Regenerate `docs/tool-schema.json`.

- [ ] **Step 4: Run** → `cargo test --test federation_contracts_e2e pr15` PASS + `cargo test --test contracts_golden` PASS + schema dump diff clean.

- [ ] **Step 5: Commit**

```bash
git add -A src/server/federation/contracts src/server/mcp/contract_tools docs/tool-schema.json docs/REPOS_YAML.md CHANGELOG.md tests/federation_contracts_e2e.rs
git commit -m "feat: bindings and check_binding accept topic/rpc/ws/table keys"
```

**Decision carried from Task 1's review (hub `path`/service attribution):**
the hub record's `path` is a real site file — the MIN `site.path` among
its `Produces`/`Consumes` legs (deterministic; `prune_orphans` is
path-keyed so the record cannot use the id anchor). `assign_services`
maps services by `node.path` prefix, so the hub's service attribution is
that file's service. **T3 must NOT assume hub service == publisher
service** — in a path-scoped monorepo a same-repo cross-service
publisher/consumer pair can lose its bind to `resolve_by_key`'s
own-service skip (the hub is one endpoint entry). Same-repo cross-service
pub/con may need multi-service endpoint entries; decide in T3.

### Task 4: Protocol coverage matrix — the escape-proof test

**Files:**
- Create: `src/server/federation/contracts/coverage_matrix.rs` (+ `mod` line in `contracts/mod.rs`)
- Test: same file (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `ContractKey` variants (`model.rs:595-635`), the resolver dispatch (`protocol_dispatch.rs`), `keys_compatible`, the workspace-graph filter set (Task 2).
- Produces: `pub fn coverage_matrix() -> Vec<ProtocolCoverage>` with `ProtocolCoverage { kind: &'static str, sensor: &'static str, resolver: &'static str, binding_supported: bool, view_edges: &'static [&'static str] }` — one row per kind, and a test that fails to COMPILE when a variant is added without a row.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn every_contract_kind_has_sensor_resolver_and_declared_binding_support() {
    // Exhaustive match: adding a ContractKey variant must break compilation
    // here, forcing the author to declare its coverage row.
    let rows = coverage_matrix();
    let _ = |k: &ContractKey| match k {
        ContractKey::Http { .. } => (),
        ContractKey::Topic { .. } => (),
        ContractKey::Rpc { .. } => (),
        ContractKey::Graphql { .. } => (),
        ContractKey::WebSocket { .. } => (),
        ContractKey::Table { .. } => (),
    };
    assert_eq!(rows.len(), 6);
    for row in &rows {
        assert!(!row.sensor.is_empty() && !row.resolver.is_empty(), "{row:?}");
    }
    // Binding support must match reality (the Task 3 truth):
    let supported: Vec<&str> = rows.iter().filter(|r| r.binding_supported).map(|r| r.kind).collect();
    assert_eq!(supported, vec!["http", "graphql", "topic", "rpc", "websocket", "table"]);
}
```

- [ ] **Step 2: Run** → FAIL (module missing).

- [ ] **Step 3: Implement** the matrix (static rows referencing the real sensor/resolver names from the audit table above) and the exhaustiveness guard.

- [ ] **Step 4: Run** → PASS. Also add the same table as `docs/protocol-coverage.md` (generated by hand from the rows, one section per kind + the bus backlog from this plan).

- [ ] **Step 5: Commit**

```bash
git add src/server/federation/contracts/coverage_matrix.rs src/server/federation/contracts/mod.rs docs/protocol-coverage.md
git commit -m "test: protocol coverage matrix is exhaustive over ContractKey"
```

### Task 5: Event-pattern backlog + extension recipe

**Files:**
- Modify: `src/server/sensors/event_sensor.rs` (header doc), `docs/protocol-coverage.md`, `CHANGELOG.md`

**Interfaces:**
- Consumes: the audit backlog (MQTT, NATS, Redis, SQS/SNS, Kinesis, EventBridge, Pulsar, ZeroMQ, Pub/Sub, Temporal, Sidekiq, Resque, Dramatiq, Huey, Quartz, Inngest, Debezium).
- Produces: a documented "how to add a bus" recipe + the backlog table; no new detectors in this task (YAGNI — the recipe is the deliverable).

- [ ] **Step 1: Write the doc** — in `docs/protocol-coverage.md`, a "Backlog" section listing each missing system with its client libraries and the exact call shapes to look for (one line each), and a "Recipe" section: (a) add pattern entries to `event_sensor`'s declarative pattern list (sites map to `SiteKind::Produces|Consumes`), (b) names from literals or same-file consts — dynamic names land in `coverage.unnormalized`, (c) the hub model needs nothing new: any broker is `Topic { broker, name }`, (d) tests follow `tests/event_hub_e2e.rs`. Note the standing follow-up: extract patterns to a config file (the `<repo>/.lain/patterns` direction) so adding a bus is data, not code.
- [ ] **Step 2: Sanity** — the doc's example shapes match the existing pattern entries in `event_sensor.rs` (read two and quote them).
- [ ] **Step 3: Commit**

```bash
git add docs/protocol-coverage.md src/server/sensors/event_sensor.rs CHANGELOG.md
git commit -m "docs: event-bus backlog and the recipe to add a bus without new design"
```
