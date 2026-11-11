# Upstream Review Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close every confirmed gap in the upstream review with snapshot-safe, structured, evidence-rich raw APIs and the missing protocol metadata, while leaving already-implemented behavior intact.

**Architecture:** Reuse `SnapshotManager` and its cached federated graphs, add an additive structured-result adapter to the existing tool inventory, and enrich the current query DTOs rather than introducing parallel models. Build sensor additions on the canonical `GraphNode`/`GraphEdge`, `JsonPath`, sensor inventory, and source-site provenance.

**Tech Stack:** Rust 2021, tokio, serde/serde_json, rust-mcp-sdk 1.1, petgraph, tree-sitter, git2, inventory, existing Lain sensors and snapshot cache.

**Spec:** `docs/superpowers/specs/2026-10-31-upstream-review-remediation-design.md`

## Global Constraints

- Preserve existing text content and live behavior; structured output is additive.
- Never satisfy a named snapshot request with live graph, overlay, runtime-store, or working-tree data.
- Do not add dependencies or bump release metadata.
- New persisted enum variants are append-only. The ORM schema change bumps `FEDERATION_GRAPH_VERSION` from 4 to 5 and documents `lain reindex`.
- Use `schema::GraphNode`/`GraphEdge`, the tool/sensor inventory patterns, and shared handler/sensor utilities; do not add central dispatch arms or mirror DTOs.
- Items 7, 10, and 13 require no reimplementation.
- Do not commit or push unless the user explicitly asks.

## Review Focus

- A snapshot call after a live edit must return the old graph and old source lines; pinned by Task 1's live-mutation test.
- Text and structured output must describe the same empty, ambiguous, and populated results; pinned by Task 2's parity tests.
- A composed path must include every hop and no duplicate boundary node, including multi-seed ties; pinned by Task 4's traversal tests.
- Dynamic ORM fields and payload keys must stay unresolved instead of creating guessed columns or paths; pinned by Tasks 6 and 7's negative fixtures.
- Resource attributes must not overwrite span-local identity, and zero-detection emitters must still report ready; pinned by Task 5's tests.

---

### Task 1: Snapshot-bound raw reads

**Files:**
- Modify: `src/server/federation/graph_backend.rs`
- Modify: `src/server/tools/registry.rs`
- Modify: `src/server/tools.rs`
- Modify: `src/server/mcp/handler.rs`
- Modify: `src/server/mcp/contract_tools/evidence.rs`
- Modify: `src/server/tools/handlers/registry_impl.rs`
- Modify: `src/server/tools/handlers/context.rs`
- Modify: `src/server/tools/handlers/semantic.rs`
- Modify: `src/server/tools/handlers/metrics.rs`
- Modify: `src/server/tools/handlers/explain_dispatch.rs`
- Modify: `src/server/tools/handlers/query.rs`
- Test: `tests/raw_snapshots.rs`

**Interfaces:**
- Consumes: `SnapshotManager::from_snapshot_with_wait_ms`, `SnapshotRecord.repos`, and the existing mirror layout.
- Produces: optional `snapshot` on the eight raw read tools; `SnapshotToolContext` carrying record/data-directory identity; a safe snapshot blob reader shared with source-dependent handlers.

- [ ] **Step 1: Write failing historical-isolation tests**

Create a snapshot fixture with one repo/commit, then change both the live graph and source. Exercise all eight tools and assert the old symbol/edges/source remain. Include rejected unknown snapshots and a call-line scan whose live line count differs.

```rust
#[tokio::test]
async fn raw_snapshot_does_not_read_live_graph_or_source() {
    let fixture = RawSnapshotFixture::new().await;
    fixture.prepare_snapshot("snap_test").await;
    fixture.replace_live_symbol_and_source().await;
    let result = fixture.call("get_call_sites", json!({
        "snapshot": "snap_test", "symbol": "old_callee", "repo_id": "orders"
    })).await;
    assert!(!result.is_error);
    assert!(result.text.contains("old_caller"));
    assert!(!result.text.contains("live_only_caller"));
}
```

- [ ] **Step 2: Run the tests and confirm the missing interface**

Run: `cargo test --test raw_snapshots -- --nocapture`
Expected: FAIL because `snapshot` is not routed to raw tools.

- [ ] **Step 3: Implement snapshot context and safe blob reads**

Expose `PetgraphBackend::db()` as `pub(crate)`. Add a cloneable snapshot context to `ToolContext`, defaulting to `None`; snapshot contexts use an empty overlay. Promote the contract evidence blob-reading logic to a shared safe helper rather than duplicating it:

```rust
#[derive(Clone)]
pub struct SnapshotToolContext {
    pub id: String,
    pub record: SnapshotRecord,
    pub data_dir: PathBuf,
}

pub fn snapshot_blob_text(
    ctx: &SnapshotToolContext,
    repo: &str,
    path: &str,
) -> Result<String, LainError>;
```

In MCP dispatch, resolve `snapshot` before ordinary repo scoping, acquire and hold the guard through `await`, bind the snapshot graph/context, and call the same inventory handlers. Do not append indexing or live-freshness notices. Reject unsupported server mode and non-ready records explicitly.

- [ ] **Step 4: Route all source and runtime reads through the bound context**

`get_call_sites`, `get_context`, and `explain_symbol` read pinned blobs by repo/path. `explain_dispatch` uses an empty runtime store for snapshots. `query_graph` executes against the bound graph without the live overlay. Live calls keep their existing code paths.

- [ ] **Step 5: Verify snapshot behavior**

Run: `cargo test --test raw_snapshots -- --nocapture`
Expected: PASS for all eight tools, source isolation, unknown snapshot, excluded repo, and residency cleanup.

Run: `cargo test --test snapshots_e2e --test contract_tool_e2e_non_http`
Expected: PASS; existing contract snapshots are unchanged.

---

### Task 2: Structured raw-tool output and schemas

**Files:**
- Modify: `src/server/tools/registry.rs`
- Modify: `src/server/tools/definitions.rs`
- Modify: `src/server/tools.rs`
- Modify: `src/server/mcp/handler.rs`
- Modify: `src/server/tools/handlers/registry_impl.rs`
- Modify: `src/server/tools/handlers/semantic.rs`
- Modify: `src/server/tools/handlers/context.rs`
- Modify: `src/server/tools/handlers/cross_runtime.rs`
- Modify: `src/server/tools/handlers/metrics.rs`
- Modify: `src/server/tools/handlers/explain_dispatch.rs`
- Modify: `src/server/tools/handlers/query.rs`
- Create: `src/server/tools/schemas/{find_symbol,get_context,get_call_sites,get_cross_runtime_callers,explain_symbol,assess_change,explain_dispatch,query_graph}.out.json`
- Test: `tests/raw_structured_output.rs`
- Modify: `tests/schema_dump_smoke.rs`
- Modify: `docs/tool-schema.json`

**Interfaces:**
- Consumes: existing inventory `ToolHandler` and domain report builders.
- Produces: `ToolOutput { text: String, structured: Option<Value> }`, `ToolRegistry::dispatch_with_output`, and per-tool `output_schema()`.

- [ ] **Step 1: Write failing MCP and schema parity tests**

```rust
#[tokio::test]
async fn find_symbol_has_typed_and_legacy_representations() {
    let result = fixture.call("find_symbol", json!({"name": "shared"})).await;
    assert!(result.content_text.contains("shared"));
    let structured = result.structured_content.expect("structured result");
    assert_eq!(structured["count"], 2);
    assert_eq!(structured["symbols"].as_array().unwrap().len(), 2);
    assert!(fixture.tool("find_symbol").output_schema.is_some());
}
```

Add populated, empty, and ambiguous fixtures for each of the eight tools and validate their structured values against the advertised schemas using the existing dev-only `jsonschema` crate.

- [ ] **Step 2: Run the tests and confirm the missing output**

Run: `cargo test --test raw_structured_output -- --nocapture`
Expected: FAIL because raw tools have `structured_content == None` and `output_schema == None`.

- [ ] **Step 3: Add the additive result adapter**

```rust
pub struct ToolOutput {
    pub text: String,
    pub structured: Option<serde_json::Value>,
}

#[async_trait]
pub trait ToolHandler {
    // existing metadata and call remain
    fn output_schema(&self) -> Option<&'static str> { None }
    async fn call_with_output(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, LainError> {
        let text = self.call(ctx, args).await?;
        let structured = serde_json::from_str(&text).ok();
        Ok(ToolOutput { text, structured })
    }
}
```

Keep `ToolRegistry::dispatch` as a text wrapper. Use `dispatch_with_output` from `ToolExecutor` and MCP dispatch. Indexing notices modify only `text`.

- [ ] **Step 4: Build typed domain reports before rendering**

Refactor the seven Markdown-shaped handlers to return a serializable report plus a text renderer. Preserve `explain_dispatch`'s existing report. Explicit ambiguity metadata belongs in `find_symbol` and `get_call_sites`; availability belongs in cross-runtime results. `query_graph` uses its existing JSON value, including occupancy.

Add `output_schema: Option<Value>` to `ToolDefinition`, default it to `None` for special tools, and wire it identically into stdio, HTTP, and schema dump output.

- [ ] **Step 5: Verify both representations and regenerate the schema artifact**

Run: `cargo test --test raw_structured_output --test schema_dump_smoke`
Expected: PASS.

Run: `make schema`
Expected: command succeeds and the generated artifact contains the eight output schemas.

---

### Task 3: Raw node and edge evidence

**Files:**
- Modify: `src/server/query/spec.rs`
- Modify: `src/server/query/executor.rs`
- Modify: `src/server/query/executor_tests.rs`
- Modify: `src/server/tools/schemas/query_graph.out.json`
- Modify: `docs/query-language.md`

**Interfaces:**
- Consumes: canonical `GraphNode` and `GraphEdge` fields.
- Produces: enriched `GraphNodeRef` and `GraphEdgeRef` in nodes, edges, paths, and groups.

- [ ] **Step 1: Write failing serialization tests**

Build one node with path/lines/commit/repo and static, heuristic, confirmed, and runtime edges. Assert references retain all available evidence and legacy `None` provenance stays `None`.

```rust
assert_eq!(json["nodes"][0]["path"], "src/orders.rs");
assert_eq!(json["edges"][0]["provenance"]["Heuristic"]["detector"], "resolver");
assert_eq!(json["edges"][0]["confidence"], 0.9);
assert_eq!(json["edges"][0]["site"]["line"], 42);
```

- [ ] **Step 2: Run the focused test**

Run: `cargo test query::executor_tests --lib -- --nocapture`
Expected: FAIL because the reduced refs omit evidence.

- [ ] **Step 3: Enrich the canonical query refs**

Add optional fields with `skip_serializing_if = "Option::is_none"`:

```rust
pub struct GraphNodeRef {
    // existing id/type/name/label
    pub path: Option<String>,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    pub commit: Option<String>,
    pub repo_id: Option<String>,
}
```

Use one `From<&GraphNode>` and one `From<&GraphEdge>` conversion at every construction site. Confidence comes from explicit heuristic provenance/weight only; do not assign synthetic confidence to legacy edges.

- [ ] **Step 4: Verify all query paths**

Run: `cargo test query:: --lib`
Expected: PASS, including grouped results and paths.

Run: `cargo test --test raw_structured_output`
Expected: PASS after updating the query schema for the new optional properties.

---

### Task 4: Composed paths and cursor pagination

**Files:**
- Modify: `src/server/query/spec.rs`
- Modify: `src/server/query/executor.rs`
- Modify: `src/server/query/executor_tests.rs`
- Modify: `src/server/query/spec_verification.rs`
- Modify: `src/server/tools/schemas/query_graph.out.json`
- Modify: `docs/query-language.md`

**Interfaces:**
- Consumes: the prior step's path map and deterministic candidate ordering.
- Produces: `QuerySpec { cursor: Option<String>, limit: Option<usize>, .. }`, composed `GraphPath`, and `QueryMeta.next_cursor`.

- [ ] **Step 1: Write failing composition and pagination tests**

Cover two chained connects, two seeds reaching one node, filtering after traversal, malformed cursor, cross-query cursor, and pages whose sizes sum to the unpaginated result.

```rust
let spec = QuerySpec::new(vec![find("seed"), connect("Calls"), connect("Uses")]);
let result = execute(spec)?;
assert_eq!(result.paths[0].nodes.first().unwrap().id, "seed");
assert_eq!(result.paths[0].edges.len(), 2);
assert_eq!(result.paths[0].nodes.len(), 3);
```

- [ ] **Step 2: Run the tests**

Run: `cargo test query::executor_tests --lib -- --nocapture`
Expected: FAIL on second-step path loss and absent cursor fields.

- [ ] **Step 3: Compose segments at each connect**

Pass the previous path map into `execute_connect`. After selecting the new segment, clone the prefix, remove its duplicate boundary node, and append the segment:

```rust
fn compose(prefix: &GraphPath, segment: &GraphPath) -> GraphPath {
    let mut nodes = prefix.nodes.clone();
    nodes.pop();
    nodes.extend(segment.nodes.iter().skip(1).cloned());
    let mut edges = prefix.edges.clone();
    edges.extend(segment.edges.iter().cloned());
    GraphPath { nodes, edges }
}
```

A first connect uses a one-node prefix. `find` clears paths; later candidate selection continues to compare only the new segment.

- [ ] **Step 4: Add deterministic cursor paging**

Sort final candidates deterministically before top-level paging. Encode `{fingerprint, after}` as hex using the existing `hex` and `blake3` dependencies. Fingerprint canonical serialization with `cursor` removed. Return `meta.next_cursor` only when more entries exist. Existing `limit` operations retain their current pipeline behavior; the new top-level limit runs last.

- [ ] **Step 5: Verify traversal and documentation**

Run: `cargo test query:: --lib`
Expected: PASS, including no duplicate boundary nodes and exact page union.

Run: `cargo test --test raw_structured_output --test schema_dump_smoke`
Expected: PASS after the schema update.

---

### Task 5: OTLP resource propagation and authoritative capabilities

**Files:**
- Modify: `src/server/runtime_trace/otlp.rs`
- Modify: `src/server/runtime_trace/mod.rs`
- Modify: `src/server/runtime_trace/spans.rs`
- Modify: `src/server/tools.rs`
- Modify: `src/server/tools/registry.rs`
- Modify: `src/server/mcp/handler.rs`
- Test: `tests/capability_discovery.rs`

**Interfaces:**
- Consumes: OTLP resource/scope attributes, registered sensors, repo readiness, and the real `SnapshotManager` handle.
- Produces: resource-aware `SpanRecord`, per-relation/emitter readiness, and snapshot capability metadata.

- [ ] **Step 1: Write failing OTLP and capability tests**

OTLP fixtures must prove resource `service.name` is inherited, a span-local value wins, scope name/version survive, and separate resources do not leak. Capability fixtures assert every relation is listed, ready zero-detection sensors are not marked unavailable, and snapshot support reflects manager presence.

- [ ] **Step 2: Run the focused tests**

Run: `cargo test runtime_trace::otlp --lib -- --nocapture`
Run: `cargo test --test capability_discovery -- --nocapture`
Expected: FAIL on discarded resource/scope attributes and absent capability fields.

- [ ] **Step 3: Merge OTLP attributes with explicit precedence**

Translate resource attributes first, then span attributes:

```rust
let mut attributes = resource_attributes(&resource.resource);
attributes.extend(span_attributes(span)); // span-local values win
attributes.insert("otel.scope.name".into(), AttributeValue::Str(scope.name.clone()));
attributes.insert("otel.scope.version".into(), AttributeValue::Str(scope.version.clone()));
```

Update stale comments that call OTLP a future adapter or say resource data is discarded.

- [ ] **Step 4: Publish relation, emitter, and snapshot facts**

Enumerate `EdgeType::all()` and registered `SensorEntry`s, sorted by name. Map repo lifecycle to `ready`, `warming_up`, or `unavailable_error` without consulting detection counts. Wire optional snapshot manager availability into the same `get_capabilities` builder used by stdio and HTTP.

- [ ] **Step 5: Verify readiness and runtime regressions**

Run: `cargo test runtime_trace:: --lib`
Run: `cargo test --test capability_discovery --test federation_readiness`
Expected: PASS.

---

### Task 6: ORM model/table/column relations and migrations

**Files:**
- Modify: `src/server/schema.rs`
- Modify: `src/server/federation/contracts/model.rs`
- Modify: `src/server/federation/graph_backend.rs`
- Modify: `src/server/graph/mod.rs`
- Modify: `src/server/sensors/mod.rs`
- Modify: `src/server/sensors/django_orm_sensor.rs`
- Create: `src/server/sensors/orm_model_sensor.rs`
- Create: `tests/orm_models.rs`
- Modify: `src/server/query/schema.rs`
- Modify: `CHANGELOG.md`

**Interfaces:**
- Consumes: existing model/table nodes, `SourceSite`, and sensor ownership/retraction.
- Produces: `Column`, `MapsTo`, `HasColumn`, `ReadsColumn`, `WritesColumn`, ORM contract metadata, and definition extractors for all six named ORMs.

- [ ] **Step 1: Write failing schema and extractor tests**

Use explicit and default table names, renamed columns, and one migration per framework. Include dynamic table/column expressions that must produce no guessed relation.

```rust
#[test]
fn prisma_model_table_column_and_migration_are_linked() {
    let analysis = analyze_prisma(&fixture);
    assert!(analysis.edge("Product", "MapsTo", "products").is_some());
    assert!(analysis.edge("products", "HasColumn", "display_name").is_some());
    assert_eq!(analysis.column("display_name").migration_line, Some(7));
}
```

Add equivalent Django, SQLAlchemy, Drizzle, TypeORM, and Sequelize cases, plus graph retraction proving a rescan does not delete SQL/Django peer output.

- [ ] **Step 2: Run the tests**

Run: `cargo test --test orm_models -- --nocapture`
Expected: FAIL because column/model relations and the extractors do not exist.

- [ ] **Step 3: Extend the canonical schema and version**

Append `Column` to `NodeType`; append the four edge variants to `EdgeType`; append ORM fact variants to `ContractFact`. Add descriptions, producers, source/target types, and schema tests. Set `FEDERATION_GRAPH_VERSION` to 5. Record the bump and `lain reindex` recovery in `CHANGELOG.md`.

- [ ] **Step 4: Implement definition and migration extraction**

Create one sensor with per-framework parsers returning a common `OrmAnalysis { models, tables, columns, relations, unresolved }`. Keep detection literals in `patterns/frameworks.yaml` and support repo overrides. Extend Django separately so its existing access sites and ownership remain unchanged. Migration `HasColumn` edges carry the migration site when known, otherwise the model site. Dynamic expressions add unresolved records and never mint names.

- [ ] **Step 5: Verify all ORM and graph-schema suites**

Run: `cargo test --test orm_models --test django_orm_trace --test schema_dump_smoke`
Run: `cargo test schema:: --lib`
Run: `cargo test federation::graph_backend_tests --lib`
Expected: PASS.

---

### Task 7: Operation-scoped topic and GraphQL field lineage

**Files:**
- Modify: `src/server/federation/contracts/model.rs`
- Modify: `src/server/sensors/field_access_sensor.rs`
- Modify: `src/server/sensors/graphql_consumer_sensor.rs`
- Modify: `src/server/sensors/event_sensor.rs`
- Modify: `src/server/graph/mod.rs`
- Test: `tests/payload_field_lineage.rs`

**Interfaces:**
- Consumes: topic consumer facts, GraphQL operation facts, existing `JsonPath` and local binding rules.
- Produces: topic/GraphQL field-read origins, operation-scoped seeds, and `FieldRef`/`ReadsField`/`ReadsFrom` links.

- [ ] **Step 1: Write failing operation-isolation tests**

Create two operations using the same variable name with different payload paths. Assert reads join only to their own operation. Cover decode aliases, destructuring, pass/return, dynamic keys, and escapes.

```rust
assert_eq!(read.chain.to_string(), "customer.id");
assert_eq!(read.origin, FieldReadOrigin::TopicConsumer);
assert_eq!(read_from.target_id, orders_consumer_id);
assert_ne!(read_from.target_id, billing_consumer_id);
```

- [ ] **Step 2: Run the tests**

Run: `cargo test --test payload_field_lineage -- --nocapture`
Expected: FAIL because only HTTP and static GraphQL selection seeds exist.

- [ ] **Step 3: Add operation identity and seed conversion**

Append `TopicConsumer` to `FieldReadOrigin` and add optional operation identity to field-read metadata with serde defaults. Convert topic and GraphQL consumer facts into the same internal `CallSite` seed shape used by HTTP, including payload root and owning function. Keep GraphQL static selection facts intact.

- [ ] **Step 4: Reuse local dataflow and emit honest incompleteness**

Run the existing binding/read/escape walker for the new origins. Dynamic keys emit no exact path and mark `reads_complete = false`. `ReadsFrom` targets the operation's consumer node; synthetic nodes remain single-owner and have no `line_end`.

- [ ] **Step 5: Verify field, event, and GraphQL suites**

Run: `cargo test --test payload_field_lineage --test event_hub_e2e --test graphql_resolution --test used_by_real_sensors`
Expected: PASS.

---

### Task 8: Frontend value dispositions

**Files:**
- Modify: `src/server/schema.rs`
- Modify: `src/server/graph/mod.rs`
- Modify: `src/server/sensors/mod.rs`
- Create: `src/server/sensors/frontend_value_sensor.rs`
- Test: `tests/frontend_value_sinks.rs`
- Modify: `src/server/query/schema.rs`

**Interfaces:**
- Consumes: field refs, generated GraphQL consumer links, and enclosing TS/TSX symbols.
- Produces: append-only `Renders`, `Stores`, `Forwards`, and `Discards` edges with source-site and detector provenance.

- [ ] **Step 1: Write failing disposition tests**

Use one TSX fixture with JSX interpolation, localStorage/state assignment, function/props forwarding, and an unused selected value. Assert exactly the four expected relations and preserve the generated document→hook→component chain.

- [ ] **Step 2: Run the tests**

Run: `cargo test --test frontend_value_sinks -- --nocapture`
Expected: FAIL because disposition relations do not exist.

- [ ] **Step 3: Add schema and sensor**

Append the four edge variants and their schema metadata. Implement an inventory-registered phase-2 TS/TSX sensor using shared parsing/walking utilities. `Renders` recognizes JSX interpolation; `Stores` recognizes storage/state writes; `Forwards` recognizes call/props flow; `Discards` applies only to a selected field with no observed use. Emit confidence/source sites and do not classify ambiguous dynamic expressions.

- [ ] **Step 4: Verify sinks and GraphQL regressions**

Run: `cargo test --test frontend_value_sinks --test graphql_resolution`
Run: `cargo test schema:: --lib`
Expected: PASS.

---

### Task 9: Cross-phase verification and cleanup

**Files:**
- Modify only files needed to fix failures caused by this work.
- Verify: `docs/tool-schema.json`, `docs/query-language.md`, `CHANGELOG.md`.

**Interfaces:**
- Consumes: all prior public interfaces.
- Produces: a clean, verified working tree containing only intentional changes.

- [ ] **Step 1: Run formatting and static checks**

Run: `cargo fmt --all -- --check`
Run: `cargo clippy --all-targets -- -D warnings`
Run: `python3 scripts/check-no-mirror-dtos.py`
Run: `python3 scripts/check-mcp-dispatch-shape.py`
Run: `python3 scripts/check-no-duplicate-sensors.py`
Expected: all PASS.

- [ ] **Step 2: Run the focused integration battery**

Run: `cargo test --test raw_snapshots --test raw_structured_output --test capability_discovery --test orm_models --test payload_field_lineage --test frontend_value_sinks`
Expected: PASS.

- [ ] **Step 3: Run the full project verification**

Run: `cargo test --workspace`
Run: `make schema && git diff --exit-code docs/tool-schema.json`
Expected: PASS. If a pre-existing unrelated failure appears, record its exact test and evidence rather than changing unrelated code.

- [ ] **Step 4: Inspect final changes and artifacts**

Run: `git status --short`
Run: `git diff --check`
Expected: no whitespace errors and no generated logs, temporary graphs, test databases, or build artifacts beyond the normal ignored build directory. Remove only artifacts created by this work.

- [ ] **Step 5: Report verdicts and evidence**

Report each disputed item with its existing implementation, each fixed item with its files/tests, and any unverified limitation explicitly. Do not claim completion while any covering test is red or any requested confirmed gap remains unimplemented.
