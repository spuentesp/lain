# Remaining Work — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the correctness and quality gaps left after the command-center contract fixes and the protocol-sensor batch, so LAIN's read-only contract surface is safe to build an external architecture command center against.

**Architecture:** No new transports or tools. The work is (a) making the sensor→graph→joiner pipeline *sound* — every node a sensor mints survives the next scan, and every consumer reaches a terminal join state, (b) making the hand-written protocol parsers honest about what they could not parse, and (c) moving library-idiom tables out of Rust and into the existing data-driven registry.

**Tech Stack:** Rust 2021, `inventory`, tree-sitter, `git2`, `serde_json`. Gates: `cargo test --workspace`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `scripts/check-*.py`, `scripts/check-mod-resolution.sh`, `make schema` (no drift).

**Spec:** This plan. It was produced from three parallel pre-commit reviews of the uncommitted protocol-sensor batch plus the earlier command-center integration review (see `docs/superpowers/plans/2026-10-06-command-center-contract-fixes.md` for the completed work).

## Global Constraints

- PRs target **`dev`** — never `main` (`AGENTS.md`). Do not bump versions in feature PRs.
- `describe_schema` must not advertise a node/edge type unless a registered sensor actually emits it. `src/server/query/schema.rs::the_known_fictions_stay_marked_unavailable` is the enforcement point.
- **Never claim absent when unanalyzed.** Anything a sensor cannot parse must surface through `ScanReport::{error, unresolved}` so `RepoCoverage::is_complete` goes false and `NoKnownImpact` is downgraded.
- A sensor's output must survive a rescan: `replace_sensor_output(owner, …)` must be called with an owner that matches *only* that sensor's own nodes.
- Every new protocol needs **negative** tests (no match, ambiguous, unparseable), not just positive ones.
- Fixture precision is not production precision. A green 4-repo fixture proves nothing about gRPC/GraphQL/WS/SQL end-to-end.

## Review Focus

1. **Node ownership is invisible until it's wrong.** A node claimed by the wrong `SensorOwner` is silently retracted by an unrelated sensor's rescan — the graph looks fine in a single-sensor test and is empty in production. Every ownership change needs a `run_all`-level test over a workspace where two sensors touch the same node shape.
2. **`is_indexed` is a promise to clients.** A `true` for an unpopulated type makes `describe_schema` lie, and the command center will draw an edge nobody can produce. Prefer `false` + a test over `true` + a TODO.
3. **A config section that validates and does nothing is worse than no section.** `repos.yaml` sections must either be consumed or rejected. Silent no-ops erode the operator's ability to reason about coverage.
4. **Parsers over user files must be total.** Empty, truncated, deeply nested, and proto2/commented input are normal, not adversarial. Silence on those is a soundness bug, not a nicety.
5. **Ambiguity must refuse, not guess.** A resolver that binds on "any candidate" invents cross-repo edges. Every dispatch branch needs an exact-match-or-`Unresolved` terminal state.

---

## File Structure

| File | Responsibility after this plan |
|---|---|
| `src/server/graph/mod.rs` | `sensor_owner_of` — single source of truth for node ownership |
| `src/server/sensors/payload_schema.rs` | Payload schema parsers (Avro / JSON-Schema / protobuf) — must return errors, not silently drop |
| `src/server/sensors/{grpc,graphql}_*_sensor.rs`, `sql_sensor.rs`, `event_sensor.rs` | Protocol detectors — report unparseable input via `scan_with_report` |
| `src/server/sensors/patterns/frameworks.yaml` | Data-driven registry — gains library-idiom entries |
| `src/server/federation/contracts/joiner/consumer_protocol.rs` | Per-protocol consumer resolution (host evidence + ambiguity refusal) |
| `src/server/federation/contracts/field_join.rs` | Schema→endpoint keying — must reuse `build_endpoints`' key derivation |
| `src/server/federation/contracts/config.rs` | `repos.yaml` validation — every section consumed or rejected |
| `tests/sensor_coexistence.rs` | NEW — `run_all`-level tests where two sensors share a node shape |
| `tests/parsers_adversarial.rs` | NEW — malformed / truncated / deeply-nested input per parser |

---

## P0 — Blockers (do these first)

### Task 1: Track `payload_schema.rs` and make HEAD compile again

**Status: DONE in the protocol-sensor commit.** Recorded here because it was a real incident: `pub mod payload_schema;` was committed at `673afb37` while the file was untracked, and `diff.rs` was committed at `543291dc` referencing `ContractKey::WebSocket`/`Table` that only exist in the uncommitted `model.rs`. **HEAD did not compile and was bisect-broken.**

- [x] `git add src/server/sensors/payload_schema.rs`
- [x] Land `model.rs` in the same commit as the `diff.rs` that references its variants

**Guard so it cannot recur:** every commit must pass a clean-checkout build.

- [ ] **Step 1:** add `scripts/check-clean-build.sh`:

```bash
#!/usr/bin/env bash
# Build from a pristine export of HEAD, so a commit that stages half
# of a coupled change cannot land. Catches untracked `mod` files and
# cross-file references committed without their definitions.
set -euo pipefail
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
git archive HEAD | tar -x -C "$tmp"
(cd "$tmp" && cargo check --all-targets --quiet)
echo "clean HEAD builds"
```

- [ ] **Step 2:** wire it into `.github/workflows/ci.yml` next to `check-mod-resolution.sh`, and add it to `scripts/check-mod-resolution.sh`'s doc so agents know the gate exists.

---

### Task 2: Sensor coexistence — shared owners retract each other's output

**Problem.** `sensor_owner_of` maps several sensors to one owner, and `replace_sensor_output(owner, …)` retracts *every* node with that owner. So a later sensor wipes an earlier one's output on every scan. Three concrete cases, all currently live:

- `FieldRef` catch-all → `FieldAccessSensor`. `graphql_consumer` mints `FieldRef` nodes at `src/server/sensors/graphql_consumer_sensor.rs:149-171` and `:213-235`; `field_access_sensor.rs:223` then retracts all of them on every run. The GraphQL field-lineage feature is dead in production and its test (`tests/graphql_resolution.rs:641+`) never runs `field_access`, so it stays green.
- `GraphqlProvider`/`GraphqlHandler`/`GraphqlConsumer` all → `GraphqlSensor`. Run order is `(phase, name)`: `graphql_provider` (p0) then `graphql_consumer` (p1) then `graphql_resolver_link` (p1). `graphql_consumer_sensor.rs:242` retracts the provider's `Module` **and its new Schema/Field nodes + `HasField`/`ResponseSchema` edges**. Same for the gRPC family (`grpc_consumer_sensor.rs:132`, `grpc_handler_link_sensor.rs:121` vs `grpc_provider_sensor.rs:206`).
- `SensorOwner::WebSocketSensor` is returned by `sensor_owner_of` but **no sensor ever passes it to `replace_sensor_output`**, so WebSocket provider nodes are never retracted (stale routes persist forever).

**Files:** `src/server/graph/mod.rs:118-175`, `src/server/sensors/{graphql_consumer,graphql_provider,graphql_resolver_link,grpc_consumer,grpc_handler_link,grpc_provider,websocket}_sensor.rs`

- [ ] **Step 1: write the failing coexistence test** — `tests/sensor_coexistence.rs`:

```rust
//! Two sensors that share a node shape must both survive `run_all`.
//! Ownership bugs are invisible in single-sensor tests and fatal in
//! production: an unrelated sensor's rescan silently retracts the
//! other's output.

mod support;
#[path = "support/contracts_snapshot_harness.rs"]
mod harness;

#[test]
fn graphql_provider_schemas_survive_a_full_run_all() {
    // Workspace with BOTH schema.graphql (provider) and gql`` queries
    // (consumer) plus a resolver — the shape where the two sensors
    // collide.
    let ws = write_graphql_workspace();
    let graph = GraphDatabase::new(&ws.join("db.bin")).unwrap();
    let ns = RepoNamespace::for_test();
    crate::server::sensors::run_all(&graph, &ws, &ns, "svc");

    let schemas = graph.get_nodes_by_types(&[NodeType::Schema]).unwrap();
    assert!(
        !schemas.is_empty(),
        "graphql_provider's Schema nodes were retracted by a later sensor"
    );
    let refs = graph.get_nodes_by_types(&[NodeType::FieldRef]).unwrap();
    assert!(
        !refs.is_empty(),
        "graphql_consumer's FieldRef nodes were retracted by field_access"
    );
}

#[test]
fn websocket_consumers_survive_http_client_rescan() {
    // `WebSocketConsumer` rides on an `HttpClientCall` node. If the
    // node-type catch-all claims it, `http_client_sensor`'s
    // `replace_sensor_output` deletes it on every scan.
    let ws = write_websocket_workspace();
    let graph = GraphDatabase::new(&ws.join("db.bin")).unwrap();
    crate::server::sensors::run_all(&graph, &ws, &RepoNamespace::for_test(), "svc");

    let ws_consumers: Vec<_> = graph
        .get_all_nodes()
        .into_iter()
        .filter(|n| matches!(n.contract, Some(ContractFact::WebSocketConsumer(_))))
        .collect();
    assert!(!ws_consumers.is_empty(), "WebSocketConsumer nodes vanished after run_all");
}

#[test]
fn openapi_json_spec_schemas_survive_event_sensor() {
    // `openapi.json` Schema/Field nodes must not be owned by EventSensor.
    let ws = write_openapi_json_workspace();
    let graph = GraphDatabase::new(&ws.join("db.bin")).unwrap();
    crate::server::sensors::run_all(&graph, &ws, &RepoNamespace::for_test(), "svc");
    let schemas = graph.get_nodes_by_types(&[NodeType::Schema]).unwrap();
    assert!(!schemas.is_empty(), "openapi.json schemas were retracted by event_sensor");
}
```

- [ ] **Step 2: run it and confirm all three fail**

Run: `cargo test --test sensor_coexistence -- --nocapture`
Expected: all three FAIL (the `.json` case now passes — it was fixed in the protocol-sensor commit; the other two should still fail).

- [ ] **Step 3: give each protocol sensor its own owner**

In `src/server/graph/mod.rs`, replace the family-wide arms with per-sensor owners keyed on the `ContractFact` variant (which is already unique per emitting sensor):

```rust
        (_, Some(ContractFact::GraphqlProvider(_))) => Some(SensorOwner::GraphqlProviderSensor),
        (_, Some(ContractFact::GraphqlHandler(_))) => Some(SensorOwner::GraphqlResolverLinkSensor),
        (_, Some(ContractFact::GraphqlConsumer(_))) => Some(SensorOwner::GraphqlConsumerSensor),
        (_, Some(ContractFact::RpcProvider(_))) => Some(SensorOwner::GrpcProviderSensor),
        (_, Some(ContractFact::RpcHandler(_))) => Some(SensorOwner::GrpcHandlerLinkSensor),
        (_, Some(ContractFact::RpcConsumer(_))) => Some(SensorOwner::GrpcConsumerSensor),
        // A `FieldRef` from the GraphQL consumer must not be owned by
        // `field_access`, or the latter's rescan deletes it. Key on the
        // read fact's origin id.
        (NodeType::FieldRef, Some(ContractFact::FieldRead(r))) if r.id.starts_with("graphql-read:") => {
            Some(SensorOwner::GraphqlConsumerSensor)
        }
        (NodeType::FieldRef, _) => Some(SensorOwner::FieldAccessSensor),
```

(Add the matching `SensorOwner` variants; update `websocket_sensor.rs` to call `replace_sensor_output(SensorOwner::WebSocketSensor, …)` rather than upsert-only.)

- [ ] **Step 4: run the tests to verify they pass**

Run: `cargo test --test sensor_coexistence && cargo test --quiet --lib 'graph' && cargo test --quiet --lib 'sensors'`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add src/server/graph/mod.rs src/server/sensors/ tests/sensor_coexistence.rs
git commit -m "fix(sensors): per-sensor node owners so a rescan stops deleting peer output"
```

---

### Task 3: Ambiguity refusal and host evidence for WebSocket consumers

**Problem.** `resolve_websocket_consumer` (`src/server/federation/contracts/joiner/consumer_protocol.rs:365-384`) builds its key from `consumer.route` only and uses `AmbiguityPolicy::NoMatch`, which binds on *any* candidate count at confidence 1.0 `Exact`. `WebSocketConsumerFact.url` is never read. Consequences:

- `wss://api.thirdparty.com/feed` binds to whatever internal service declares `/feed` — an invented `Binds` edge, with no `External` terminal state.
- Two services declaring `/ws` → the consumer multi-binds to both at 1.0 instead of `Unresolved{reason}`.

**Files:** `src/server/federation/contracts/joiner/consumer_protocol.rs`, `src/server/federation/contracts/model.rs` (`WebSocketConsumerFact`)

- [ ] **Step 1: write the failing tests** in `src/server/federation/contracts/joiner_tests.rs`, mirroring the existing `resolve_rpc_consumer` cases:

```rust
#[test]
fn ws_consumer_with_foreign_host_does_not_bind() {
    // url host is a literal that matches no configured service.
    let out = run_joiner_with_ws_consumer("wss://api.thirdparty.com/feed", "/feed");
    let res = &out.index.consumers[&call_id];
    assert!(
        matches!(res.target, Some(ConsumerTarget::Unresolved { .. })),
        "an external WS host must not invent a Binds edge: {:?}",
        res.target
    );
}

#[test]
fn ws_consumer_with_two_matching_providers_stays_unresolved() {
    let out = run_joiner_with_two_ws_providers("/ws");
    let res = &out.index.consumers[&call_id];
    assert!(
        matches!(res.target, Some(ConsumerTarget::Unresolved { .. })),
        "ambiguity must refuse, not bind to both: {:?}",
        res.target
    );
}

#[test]
fn ws_consumer_with_single_matching_provider_binds() {
    let out = run_joiner_with_ws_consumer("wss://orders.internal/ws", "/ws");
    let res = &out.index.consumers[&call_id];
    assert!(matches!(res.target, Some(ConsumerTarget::Binds { .. })));
}
```

- [ ] **Step 2: run and confirm they fail**

Run: `cargo test --lib joiner_tests::ws_consumer -- --nocapture`
Expected: first two FAIL (they currently bind).

- [ ] **Step 3: implement** — mirror `resolve_rpc_consumer`:

```rust
fn resolve_websocket_consumer(...) -> ... {
    // 1. If the URL has a literal host, restrict candidates to services
    //    whose hosts/env match it. No match -> Unresolved (or External
    //    when the host is outside every configured service).
    // 2. Only fall back to route-only matching when HostPart::None.
    // 3. Use the GraphQL-style exact-one policy: n != 1 -> Unresolved.
}
```

- [ ] **Step 4: verify**

Run: `cargo test --lib joiner_tests && cargo test --quiet --test federation_contracts_e2e`
Expected: all PASS, T1 precision/recall still 1.0.

- [ ] **Step 5: Commit**

```bash
git add src/server/federation/contracts/
git commit -m "fix(joiner): WebSocket consumers require host evidence and refuse ambiguity"
```

---

## P1 — Soundness

### Task 4: `field_join` schema keying must match `build_endpoints`

**Problem.** Two branches build `EndpointId`s that never match the endpoints the joiner actually created, so `repos.yaml#schemas` validates, appears in `config_hash`, and does nothing:

- `field_join.rs:163-196` uses `p.template` raw for Topic keys; `joiner/endpoints.rs:67,180` applies `endpoint_template_for` (prepends `base_path` / `route_prefixes`). Any service with `base_path` configured → schema attaches to a nonexistent endpoint. Broker fallback also diverges (`split('/').next()` → `""` vs `default_broker_for` → `"kafka"`).
- `field_join.rs:247-250` treats `SchemaDecl.repo` as a **service name** while `config.rs:428-436` validates it as a **repo id**. Works only when they coincide. Broker hardcoded `"kafka"`.

**Files:** `src/server/federation/contracts/field_join.rs`, `joiner/endpoints.rs`

- [ ] **Step 1: failing test** — a service with `base_path: /api` and a `schemas:` entry; assert the payload schema binds to the endpoint `build_endpoints` produced.
- [ ] **Step 2:** extract the key derivation into one shared function used by both `build_endpoints` and `field_join` (including `endpoint_template_for` and `default_broker_for`).
- [ ] **Step 3:** make `SchemaDecl` carry an explicit `service:` (falling back to repo→service assignment), and validate it against `services`.
- [ ] **Step 4:** `cargo test --lib joiner_tests && cargo test --test federation_contracts_e2e`
- [ ] **Step 5:** `git commit -m "fix(contracts): field_join keys match build_endpoints so schemas: actually binds"`

---

### Task 5: `payload_schema` parsers must be total and report failure

**Problem.** `parse_proto_messages` (`src/server/sensors/payload_schema.rs:242-339`) emits wrong or missing facts on ordinary proto:

| input | current behavior |
|---|---|
| `optional string id = 1;` | field named `"string"`, type Unknown |
| `string id = 1; // the id` | field silently dropped |
| `oneof { … }` | closing `}` ends the message; rest lost |
| nested `message` | outer message flushed early, remainder dropped |
| empty message | dropped entirely |

None of it surfaces through `ScanReport`, so the coverage ledger cannot see the gap. The parser also ignores the comment-strip / continuation-join pipeline that `parse_proto_providers` already uses.

**Files:** `src/server/sensors/payload_schema.rs`, `src/server/sensors/grpc_provider_sensor.rs:98`

- [ ] **Step 1: adversarial test file** — `tests/parsers_adversarial.rs`, one case per row above plus empty file and 10k-line file, asserting either correct facts or a recorded `ScanReport::unresolved`.
- [ ] **Step 2:** reuse `util_tokenize` + the grpc provider's `strip_comments` / `join_continued_lines` instead of re-lexing.
- [ ] **Step 3:** handle `optional`/`required`/`repeated`/`map<…>`/`oneof`/nesting; keep a depth cap.
- [ ] **Step 4:** override `scan_with_report` on the grpc/graphql/sql sensors so unparseable files produce `UnresolvedRecord` (never silent).
- [ ] **Step 5:** `cargo test --test parsers_adversarial && cargo test --test grpc_resolution && cargo test --quiet --lib payload_schema`
- [ ] **Step 6:** `git commit -m "fix(sensors): proto payload parser is total and reports what it could not parse"`

---

### Task 6: GraphQL parser — fragments and `implements`/`@key`

**Problem.**
- Fragment spreads are parsed as top-level fields: `query { orders { id ...orderFields } }` yields a selected field `"orderFields"`; `... on Paid { total }` yields fields `"on"` and `"Paid"` (`graphql_consumer_sensor.rs:592-605`, push at `:703-704`). Invented consumer facts and `FieldRef`s → over-claiming.
- `type X implements Node @key(fields: "id") { … }` is silently skipped because the parser requires `{` immediately after the name (`graphql_provider_sensor.rs:446-448`). That is *exactly* the federation SDL LAIN targets, so schema lineage is missing where it matters most.

**Files:** `src/server/sensors/graphql_consumer_sensor.rs`, `graphql_provider_sensor.rs`

- [ ] **Step 1: failing tests** — a query with `...spread`, `... on Type`, and a top-level `...frag`; and an SDL with `implements` + `@key`. Assert no field named `"on"`/`"Paid"` and that the directive-bearing type still yields Schema/Field nodes.
- [ ] **Step 2:** explicitly detect `...` and skip the spread name + optional `on Type` + its selection set.
- [ ] **Step 3:** after the type name, skip an `implements …` clause and `@directive(…)` argument list (balanced parens) before expecting `{`.
- [ ] **Step 4:** bound the recursion in `top_level_fields_with_selections` (`:700`) with a depth cap — adversarial `a{a{a{…` currently blows the stack.
- [ ] **Step 5:** `cargo test --test graphql_resolution --test parsers_adversarial`
- [ ] **Step 6:** `git commit -m "fix(sensors): GraphQL fragments and directive-bearing SDL parse correctly"`

---

### Task 7: `databases` config — consume or reject

**Problem.** `DatabaseDecl.shared_with` and `db.name` are declared, validated (`config.rs:438-461`) and hashed into `config_hash`, but **consumed nowhere**. Table→service ownership is `config.databases.iter().find(|d| d.tables.iter().any(|t| t == &tbl.name))` — first declaration wins, matching **table name only**. Two databases listing the same table name silently attribute all such `Table` facts to the first one's service.

**Files:** `src/server/federation/contracts/config.rs`, `joiner/endpoints.rs:154-163`

- [ ] **Step 1: failing tests** in `config_tests.rs` — duplicate `databases[].name`, a table listed in two databases, and a `shared_with` entry that must actually widen ownership.
- [ ] **Step 2:** reject duplicates at `validate()`; key table ownership on `(db.name, table)`; implement `shared_with` (a table shared with service B is owned by both) or delete the field.
- [ ] **Step 3:** align `databases[].service` validation with `http_clients.service` (which uses `known_services`, including implicit repo ids).
- [ ] **Step 4:** `cargo test --lib config_tests && cargo test --test sql_tables`
- [ ] **Step 5:** `git commit -m "fix(config): databases.shared_with is consumed, and duplicate table names are rejected"`

---

## P2 — Quality and the registry migration

### Task 8: Make the protocol sensors templated — three tiers, not "rewrite them"

**Origin.** Reviewer question: *"didn't we make a template format for these sensors? why are they implemented as .rs?"*

The first answer to this was wrong in a useful way: it framed the split as "parsers belong in Rust, idioms belong in data." That is a false dichotomy. **Rust + template is exactly what `patterns/` already is** — a Rust engine consuming data. The shipped registry is not regex tables:

- `frameworks.yaml` — 218 lines, **35 framework entries**
- **38 `.scm` files** across **11 language dirs** (python, rust, go, java, kotlin, csharp, ruby, tsjs)
- `<repo>/.lain/patterns/*.yaml` overrides, via `Patterns::with_overrides`

And a `.scm` file *is* structured parsing as data — `patterns/rust/axum-route.scm` declares named captures (`@path`, `@verb`, `@handler`) over a tree-sitter grammar. `tests/sensors/patterns_new_framework.rs` already asserts the design goal: *"No production code in `src/server/sensors/*.rs` was touched."*

So the real question is **what is the unit of templating**. A sensor does three distinct jobs:

| Layer | Job | Belongs in | Why |
|---|---|---|---|
| **Recognise** | "this call site is a Kafka publish" | **data** | many instances, one shape |
| **Extract** | "what fields does this proto / SDL / SQL declare?" | **data where a grammar exists** (`.scm`), else one shared lexer | tree-sitter grammars exist for proto, GraphQL and SQL |
| **Project** | mint `ContractFact`, normalize `/api/orders/:id` → `/api/orders/{}`, derive `ContractKey`, resolve `import "common.proto"`, apply ambiguity policy | **Rust** — but *one shared emitter* | semantic, cross-file, policy-laden |

Only the third layer must be Rust. What the current code got wrong is not "it is in Rust" but **all three layers are in Rust, once per sensor**.

**Evidence of the cost** (measured):

| | lines | covers |
|---|---|---|
| Shared pattern engine (`http_sensor` + `http_client` + `entry_point` + `field_access`) | ~7,600 | 11 languages × 35 frameworks |
| 7 hand-written protocol sensors | ~5,700 | 5 protocols, with internal duplication |

Duplication is verbatim. `grpc_provider_sensor.rs:100` and `graphql_provider_sensor.rs:102` mint `Schema` nodes with the identical `generate_id` / `GraphNode::new` / `contract = Schema{direction}` sequence; the ~25-line `FieldRef` emission block is copy-pasted inside `graphql_consumer_sensor.rs` at `:149` and `:213`; `parse_proto_messages` re-lexes from scratch instead of reusing `util_tokenize` and the comment-strip / continuation-join that `parse_proto_providers` already has.

**Where the line actually is** (stated honestly — there is a real trade-off):

- **Same shape, many instances → data.** kafkajs vs aiokafka vs rdkafka vs kafka-go vs `@app.task` vs `@Cron` are six idioms, one shape. `new WebSocket(…)` vs `app.ws(…)` vs `WebSocketGateway` likewise. A seventh must not require a Rust diff.
- **Structurally unique artifacts → code, but *shared* code.** A hand-rolled `.proto` tokenizer (~150 lines, zero deps) is a defensible choice for one format — just not one lexer per protocol. Where a tree-sitter grammar exists, prefer `.scm`.
- **Semantic projection → always Rust, always shared.**

Rough residual that genuinely must stay Rust: cross-file type resolution, `ContractKey` derivation, ambiguity policy — about 20% of the 5,700 lines.

**Files:** `src/server/sensors/patterns/{mod.rs,frameworks.yaml}`, `src/server/sensors/util.rs`, the 7 protocol sensors

- [ ] **Step 1: Tier 1 — recognition as data.** Add an `idioms:` section (or extend `FrameworkKind` with `TopicProducer`, `TopicConsumer`, `Scheduled`, `WebSocket` — prefer extending so `with_overrides` and `<repo>/.lain/patterns/*.yaml` keep working). Entries carry `lib_match` / `annotation_regex` / `path_regex` and name the capture they yield (topic, url, handler). Move these out of Rust:

  | file | hardcoded idioms |
  |---|---|
  | `event_sensor.rs` | `.run(` `.subscribe(` `KafkaConsumer(` `@app.task` `@shared_task` `@Cron`, kafkajs / aiokafka / rdkafka / kafka-go |
  | `websocket_sensor.rs` | `wss?://…`, `on( open\|message\|close\|error)`, `new WebSocket(…)`, `app.ws\|router.ws\|WebSocketGateway` |

  Consume them from one generic line-idiom walker in `util.rs`, not per sensor.

- [ ] **Step 2: Tier 2 — extraction as data.** Where a tree-sitter grammar exists (proto, GraphQL, SQL), replace the hand-rolled lexers with `.scm` queries declaring named captures. Where it does not (`payload_schema.rs` Avro / JSON-Schema), keep a small parser but route it through the shared tokenizer. Always reuse `util_tokenize` + the existing comment-strip / continuation-join rather than re-lexing.

- [ ] **Step 3: Tier 3 — one shared emitter.** Extract `emit_schema_with_fields`, `emit_field_ref`, `emit_table` into `util.rs` and collapse the duplicated blocks above. Every sensor then reports parse failure the same way (`ScanReport::{error, unresolved}`) — see Task 5.

- [ ] **Step 4:** hoist any remaining `Regex::new` out of scan functions into `OnceLock`/`LazyLock` statics **regardless** of the migration — `websocket_sensor.rs` compiles four regexes per scan call today, which is a real per-file cost.

- [ ] **Step 5: the acceptance test.** `tests/sensors/patterns_protocol_idioms.rs`, modelled on `tests/sensors/patterns_new_framework.rs` (`django-route` is the proof case):

  ```rust
  //! Adding support for a new Kafka client or WebSocket framework must
  //! be a YAML/`.scm` diff. If this test needs a change under
  //! `src/server/sensors/*.rs`, the templating has regressed.
  #[test]
  fn a_new_idiom_is_a_data_change() {
      // 1. append one entry to frameworks.yaml (or drop a .scm in)
      // 2. assert the walker emits the expected topic / url / handler
      // 3. assert `git diff -- src/server/sensors/*.rs` is empty
  }
  ```

- [ ] **Step 6:** `cargo test --quiet --lib sensors && cargo test --test patterns_new_framework --test patterns_protocol_idioms`
- [ ] **Step 7:** `git commit -m "refactor(sensors): three-tier templating — idioms and extraction as data, one shared emitter"`

**Acceptance:** adding `kafka-python` or `socket.io` support is a YAML/`.scm` diff with **zero** `src/server/sensors/*.rs` changes. That is the only test that proves the template is real.

---

### Task 9: Test-quality debt in the protocol batch

All flagged by review; each is a small test, not a redesign.

- [ ] `tests/grpc_resolution.rs:544-566` hand-builds the consumer `FieldRef` + `ReadsFrom` edge — no gRPC sensor emits those in production. Replace with a fixture that runs the real sensors.
- [ ] `tests/graphql_resolution.rs:641+` (F7) and `tests/grpc_resolution.rs:479+` (E6) assert only `edges.any(|e| e.edge_type == ResponseSchema)`. Assert the edge connects the *specific* provider to the *specific* schema.
- [ ] `tests/sql_tables.rs:501` asserts `orm.count >= 1` where the fixture produces exactly 2. Assert the exact count and the sample ids.
- [ ] `tests/property_join_pipeline.rs` gained only `databases: vec![]`. Extend `arb_call` to generate WebSocket / Topic / Table consumers so I2/I5/I6 cover the new branches generatively.
- [ ] `joiner_tests.rs::contract_key_display_round_trip` is HTTP-only. Add `websocket:` and `table:` round-trips (`model.rs:711-775`).
- [ ] `field_join.rs:228` step 4b picks the first matching Schema node with `find` — input-order dependent. Tie-break by node id.

- [ ] **Commit:** `test: adversarial and negative coverage for the protocol sensors`

---

### Task 10: Small cleanups

- [ ] `src/server/mcp/handler.rs:1530-1549` — new `LainMcpServer::call_tool` has zero callers and duplicates `call_tool_embedded` (`:1273`). Delete it, or make `call_tool_embedded` delegate so there is one path.
- [ ] `src/server/sensors/{grpc_provider,graphql_provider,graphql_consumer}.rs` — `let _ = graph.replace_sensor_output(...)` swallows write errors; propagate with `?` like `sql`/`http`/`event`.
- [ ] Same three files — the empty-output guard (`if !all_nodes.is_empty()`) defeats retraction when a rescan finds zero files (all deleted). Replace unconditionally, as `sql_sensor.rs:162` does.
- [ ] `grpc_provider_sensor.rs:114` hardcodes `Direction::Response` on request-side messages. Use `Direction::Request` from the `RequestSchema` lookup.
- [ ] `sql_sensor.rs:273-300` — the ORM 4-line lookahead attributes a later `cursor.execute` literal to the ORM call's line and then reports no `OrmDynamicQuery`. Skip the lookahead when `shape.is_orm`. Also narrow `.filter(` (`:373`), which matches pandas and keeps `RepoCoverage::is_complete` false for ordinary Python repos.
- [ ] `services.rs:788-794` comment says `owners` is "Always present" but the code `continue`s when `ref.id` is missing; `get_service.out.json` correctly marks it optional. Align the comment.
- [ ] DRY: the ~25-line `FieldRef` emission block is duplicated at `graphql_consumer_sensor.rs:149-171` and `:213-235`; the Schema+Field+HasField emission is identical in `grpc_provider_sensor.rs:99-141` and `graphql_provider_sensor.rs:95-150`. Share one helper.

---

## P3 — Carried over from the command-center review

These were identified in the integration review and are still open.

- [ ] **`tests/real_federation/*` is red.** `ground_truth.sh` / `soundness.sh` / `metrics.rs` pass `serde` to `prepare_snapshot` but `scripts/demo-federation-fixture.sh` clones only `bytes` + `tokio` → `repo_not_registered`. `tools_smoke.sh` has 4 stale argument shapes (`trace_impact` `from` form, `check_binding` `"GET /x"` vs `http:GET /x`). Fix the scripts or the fixture, then assert *verdicts* (currently only joiner precision is asserted — never "this change breaks X").
- [x] **Defect K — ownership / entry points / env bindings write no graph nodes.** RULING 2026-10-08 (human): **closed, split three ways.**
  - **Ownership (CODEOWNERS) — WONTFIX.** Not a product requirement. CODEOWNERS is a static `path → team` declaration file, not git history/"who pushed" — it answers "who do I notify", which is routing, not impact analysis. Neither use case (architecture workbench, PR intelligence) is load-bearing on it. The costly half (graph-traversable owners + `FEDERATION_GRAPH_VERSION` bump + `lain reindex` + CHANGELOG) is **cancelled** — no schema migration. The existing sensor stays as harmless annotation on `get_service`'s `used_by` entries (`services.rs:797`); do not extend it.
  - **Entry points — REQUIRED, already done, not a defect.** `entry_point_sensor` writes `GraphNode.entry` (`schema.rs:748`) and it is queryable end-to-end: `used_by` emits `"kind": "http_handler"|"scheduled"|"cli"|"main"` with a resolvable `ref` (`used_by.rs:107-110`, `kind_wire` at `:187`), plus `list_entry_points` (`architecture.rs:124`). This serves the trace requirement ("field read, entry point, source evidence") directly. Keep it.
  - **Env bindings — leave as-is.** Deliberately a joiner-side axis, not graph content (spec §6). Feeds Phase C resolution (`federated_index.rs:1060`), which is the upstream/downstream use case, but does not need to be graph-traversable.
- [ ] **gRPC / GraphQL / WebSocket / SQL end-to-end through the contract tools is unproven.** Sensors and joiner dispatch exist and unit tests pass, but no run drives `get_contract` / `trace_impact` over a fixture containing those protocols. Build a 4-repo fixture with one of each and assert provider + consumer + `Binds` per protocol.
- [ ] **`ChangedWithoutSchema` on a schema-less endpoint with zero bound consumers** can fall through to `NoKnownImpact` under complete coverage (`diff.rs` final `else`). Untested. Add the fixture tag and assert it is *not* `NoKnownImpact`.
- [ ] **`could_match` returns `false` for every unresolved consumer against a `Topic`/`Rpc`/`Graphql`/`Table` endpoint** (`diff.rs:2190-2208`) — an unresolved topic consumer never blocks `NoKnownImpact` when its producer changes. Pre-existing I3 gap in the committed baseline; a soundness hole for every non-HTTP protocol.
- [ ] **Known limit from the command-center fixes, by design:** a real behaviour change in a *shared* handler file is still unreported (`HandlerChanged` requires single-endpoint attribution). Closing it needs line- or hunk-level attribution.

  **MEASURED 2026-10-08 — this is blocked on a data-model gap, not on effort.** I attempted the obvious fix (drop the `owners.len() == 1` gate so every endpoint claiming a changed handler file gets `HandlerChanged`) and measured it:

  - `PR13_METRICS_JSON` goes `diff_precision: 1.0 → 0.9090909090909091` and **T1 FAILS**. The gate is precision-load-bearing; the existing test `handler_change_in_a_shared_file_is_not_attributed_to_one_endpoint` records exactly this ("reporting it three times was the precision regression this rule caused on the T1 fixture's `s6-rename-path` scenario"). Reverted.
  - The real fix needs data that does not exist in the model:
    - `RepoDiffResult::Changed(BTreeSet<String>)` is **paths only** (`changed_files.rs:28-35`), and `diff_impl` collects them with `DiffFormat::NameOnly` (`changed_files.rs:155-161`) — hunks are never read, so no changed line ranges exist.
    - `SymbolKey` (`model.rs:576-581`) is `{repo, path, container, name}` — **no line range**, so there is nothing to intersect a hunk against even if hunks were collected.
  - So closing it properly is a two-sided change: (a) carry per-file changed line ranges out of `diff_impl` and through `RepoDiffResult` + the `ChangedFilesSource` trait and its four impls (`MirrorChangedFiles`, `MultiRepoChangedFiles`, `RepoScopedChangedFiles`, `StaticChangedFiles`); (b) give `SymbolKey` a handler line range populated by the sensors; (c) intersect in `diff.rs` so the change is attributed to the one handler the hunk touches. Until then, per-endpoint precision and per-file soundness genuinely trade off.

  **What is still unsound today:** a schema-bearing endpoint whose shared handler file changed produces *no* change record, so it never reaches the verdict machinery and `NoKnownImpact` stands while behaviour may have moved. `ChangedWithoutSchema` (schema-less endpoints) is NOT affected — `source_changed` has no ownership gate (`diff.rs:644-651`), so that rule already fires for shared files.

  Suggested shape for whoever takes it: do the hunk work as its own task with T1 held at 1.0 as the acceptance gate. Do **not** retry the gate removal — it is measured to fail.

---

## Out of scope

- New transports, new MCP tools, or new HTTP routes. Everything here is reachable through the existing 13 contract tools.
- Org-wide safety claims. `scope.configured_only` stays `true` and `caveats.unconfigured_scope` must remain rendered on every response — no code change is "a fix" for that; it is the contract.
- Version bumps and releases (`AGENTS.md`: release PRs only).
