# Protocol coverage

Where every `ContractKey` kind comes from (sensor), how it joins
(joiner resolver), whether an operator can confirm it by hand
(`check_binding` + `repos.yaml#bindings`), and where it surfaces in
the dashboard graph view. This is the audit the owner asked for —
"revisar que no se nos escapen otros protocolos" — rendered as
prose. The machine-readable twin lives in
`src/server/federation/contracts/coverage_matrix.rs`
(`coverage_matrix()`), and its `#[cfg(test)]` exhaustive `match`
over `ContractKey` makes a new kind fail to compile until its row is
declared here and there.

The audit was taken 2026-10-18 against source
(`docs/superpowers/plans/2026-10-18-topic-hub-and-protocol-coverage.md`,
"Protocol coverage audit") and updated by the work that followed:
topic hubs became canonical (Task 1), the dashboard view learned to
show them (Task 2), and `check_binding` / `bindings` grew from
HTTP+GraphQL to all six kinds (Task 3).

## The matrix

| kind | sensors | resolver | bindings | dashboard view edges |
|---|---|---|---|---|
| `http` | `http_sensor`, `http_client_sensor`, `openapi_sensor` | HTTP ladder (`consumer_http.rs`) | yes | `Calls` |
| `graphql` | `graphql_consumer_sensor`, `graphql_provider_sensor`, `graphql_resolver_link_sensor` | `resolve_graphql_consumer` | yes | — (nodes only) |
| `topic` | `event_sensor` | `resolve_topic_consumer` | yes | `Produces`, `Consumes`, `PayloadSchema` |
| `rpc` | `grpc_provider_sensor`, `grpc_consumer_sensor`, `grpc_handler_link_sensor`, `proto_sensor` | `resolve_rpc_consumer` | yes | — (nodes only) |
| `websocket` | `websocket_sensor` | `resolve_websocket_consumer` | yes | — (nodes only) |
| `table` | `sql_sensor` | `resolve_table_consumer` | yes | — (nodes only) |

"Dashboard view edges" lists the workspace-graph edge types
(`WORKSPACE_GRAPH_EDGE_TYPES` — `Calls`, `Imports`,
`CrossRepoSameSymbol`, `Produces`, `Consumes`, `PayloadSchema`)
through which the kind's contract surface reaches
`get_workspace_graph`. An empty entry means the kind's dedicated
legs (`ReadsFrom`/`ReadsField`, `RequestSchema`/`ResponseSchema`,
`Uses`, `ReadsTable`/`WritesTable`, …) are outside that filter: the
nodes render, the contract edges do not. Those gaps are named under
each kind and are backlog, not silence.

## http

- **Sensors.** `http_sensor` (routes), `http_client_sensor` (call
  sites), `openapi_sensor` (spec-derived routes and schemas).
- **Resolver.** The HTTP ladder (`consumer_http.rs`) — §7.3 tier by
  tier: exact route, prefix-stripped template, `http_clients`
  wrapper registry, host evidence, env-var dispatch, unbound-host
  heuristic.
- **Bindings.** `http:<METHOD> <template>` (the shortened
  `<METHOD> <template>` form is accepted in `provider.key`
  position). Same-kind template bridges are supported
  (`GET /v1/api/orders/{}` → `GET /api/orders/{}` — §15.2
  scenario 10); methods must be compatible (`ANY`/`UNKNOWN`
  wildcard).
- **Dashboard.** `Calls` — the call/import spine shows callers and
  handlers as ordinary functions. The dedicated legs (`SendsHttp`,
  `CallsHttp`, `ReadsField`/`ReadsFrom`) are outside the dashboard
  filter.
- **OpenAPI gap.** An OpenAPI spec that references a foreign
  `$ref` (e.g. `$ref: 'other.yaml#/components/schemas/Foo'`)
  emits the route and the `Schema{Response}` but no fields
  downstream of the `$ref` (the body lives outside the
  workspace). The `OpenApiSensor`'s `unresolved_refs` accumulator
  records each foreign URL per spec; the per-repo coverage
  ledger folds it into `coverage.unresolved_refs` (sibling of
  `unnormalized`, not the same bucket — an external `$ref` is not
  a consumer-shaped gap). `claims_from_coverage` surfaces it as
  `missing` evidence with the foreign URL as the file path so the
  operator sees a `Coverage.unresolved_refs` entry they can act
  on (inline the schema or onboard the foreign file).
- **Status.** Complete: detected, joined, confirmable, visible.

## graphql

- **Sensors.** `graphql_consumer_sensor` (operation documents and
  tagged templates), `graphql_provider_sensor` (SDL root fields),
  `graphql_resolver_link_sensor` (resolver-map → handler links).
- **Resolver.** `resolve_graphql_consumer` — exact `(op, field)`
  match. Per spec §8.3, several services exposing the same root
  field (federation/gateway) is ambiguous and never single-bound;
  the consumer lands `GraphqlNoOp` instead.
- **Bindings.** `graphql:<op>:<field>` — same `op` and same `field`
  on both sides; field bridging is not a thing.
- **Dashboard.** The call sites (`graphql-call:{op}:{field}`
  `Function` nodes) and the SDL (`Schema` nodes) render, but the
  GraphQL legs (`ReadsFrom`/`ReadsField` on selected fields,
  `HasField`/`ResponseSchema` on the provider) are outside the
  dashboard filter. **Backlog.**
- **Status.** Detected, joined, confirmable. The round trip
  (`entry → load_from_str → validate → apply_confirmed_binding →
  Binds/Confirmed`) is pinned by
  `task6_check_binding_yaml_round_trip` in
  `tests/federation_contracts_e2e.rs`.

## topic

- **Sensors.** `event_sensor`, across kafkajs, confluent-kafka,
  `confluent_kafka`, aiokafka, rdkafka, kafka-go, pika, amqplib,
  Celery, and NestJS `@Cron`.
- **Resolver.** `resolve_topic_consumer` — exact `(broker, name)`
  match. No host/template tier: topics are identified by the pair
  alone. Same §7.8 in-service rule as HTTP — a consumer never binds
  its own service's producer.
- **Bindings.** `topic:<broker>:<name>` (colon form) or the wire
  form `topic:<broker>/<name>`. The wire form splits at the first
  `/` (the name may carry further `/` and `:` — Redis
  `orders:created`, MQTT `sensors/temp`); the colon form takes
  exactly two non-empty components. A string that would read under
  both shapes (`topic:a:b/c`) is rejected as ambiguous rather than
  guessed. `check_binding` round trip pinned by
  `pr15_bindings_entry_round_trip_confirms_topic_consumer`.
- **Dashboard.** One canonical hub node per `(broker, topic)` per
  repo (site fragments collapse onto it), with `Produces` and
  `Consumes` legs per **site** — two publishers in one file are two
  legs — and `PayloadSchema` to the payload `Schema` when one is
  linked. All three are in the dashboard filter. The same
  `broker/topic` in two repos is two hubs: per-repo identity is
  kept, and the contract layer is what links them cross-repo.
- **Status.** Complete. One caveat: same-repo cross-service pairs may
  need a hand-written binding when the hub's path lands in the
  consumer's service.

## rpc

- **Sensors.** `grpc_provider_sensor` (services and methods),
  `grpc_consumer_sensor` (generated-stub call sites),
  `grpc_handler_link_sensor` (server registration → handler),
  `proto_sensor` (message schemas).
- **Resolver.** `resolve_rpc_consumer` — resolve the channel
  address to candidate services (`services[].hosts` for a literal
  `HostPart`, `services[].env` for an env-var dial), then exact
  `(package.Service, method)` match inside that service. No
  URL-prefix tolerance (§8.2). An in-service stub call must not
  bind (I5). A channel that names no configured service is
  `RpcStubUnknown`; an env var no service declares is `EnvUnmapped`
  and counted in the unresolved ledger.
- **Bindings.** `rpc:<system>:<service>:<method>` (system is
  `grpc`) or the wire form `rpc:<service>/<method>`. Exact key
  equality — no bridge. Round trip pinned by
  `pr15_bindings_entry_round_trip_confirms_rpc_ws_table_consumers`.
- **Dashboard.** The call sites (`rpc-call:{service}:{method}`
  `Function` nodes) and message `Schema` nodes render; the RPC legs
  (`RequestSchema`/`ResponseSchema`/`HasField`) are outside the
  dashboard filter. **Backlog.**
- **Status.** Detected, joined, confirmable.

## websocket

- **Sensors.** `websocket_sensor` — client dials, server routes, and
  handler bindings (Tier-1 line-idiom walker over
  `sensors/patterns/frameworks.yaml`).
- **Resolver.** `resolve_websocket_consumer` — host evidence first,
  exactly like the RPC resolver: a literal host that names no
  configured service means the dial left the federation
  (`Unresolved`, never a guessed bind), then exact `route` match
  among the candidate services. Ambiguity refuses (`GraphqlNoOp`
  policy): two services exposing `/ws` must not multi-bind.
- **Bindings.** `ws:<route>` (or the wire form
  `websocket:<route>`) — exact route equality. Round trip pinned by
  `pr15_bindings_entry_round_trip_confirms_rpc_ws_table_consumers`.
- **Dashboard.** Client dials (`HttpClientCall`) and server routes
  (`HttpRoute`) render as nodes; the handler `Uses` leg is outside
  the dashboard filter. **Backlog.**
- **Status.** Detected, joined, confirmable.

## table

- **Sensors.** `sql_sensor` — literal SQL and ORM call sites, one
  `TableConsumer` fact per site listing every table the statement
  touches (SELECT, CTEs, subselects, joins, and the DML verbs).
- **Resolver.** `resolve_table_consumer` — tables are identified by
  name alone; ownership (which service's endpoint table a `Table`
  lands in) comes from `databases[]` in `repos.yaml`, widened by
  `shared_with`. No own-service skip: a service reading its own
  table is the canonical case (§7 handler → function → table
  traversal). All-or-nothing: every table a site reads must have an
  owning endpoint or the whole consumer is `Unresolved { NoMatch }`
  — a reader of an unowned table must be visible to
  `list_unresolved`, because silence is what lets `NoKnownImpact`
  be claimed while a possible reader exists.
- **Bindings.** `table:<name>` — exact name equality, same spelling
  in binding and wire form. Round trip pinned by
  `pr15_bindings_entry_round_trip_confirms_rpc_ws_table_consumers`.
- **Dashboard.** The reader sites (`sql-read:{path}:{line}`
  `Function` nodes) and `Table` nodes render; the `ReadsTable` /
  `WritesTable` legs are outside the dashboard filter. **Backlog.**
- **Status.** Detected, joined, confirmable.

## Reading the escape-proof test

`src/server/federation/contracts/coverage_matrix.rs` pins all of
the above mechanically:

- `every_contract_kind_has_sensor_resolver_and_declared_binding_support`
  holds an exhaustive `match` over `ContractKey` whose arms bind each
  variant to its coverage row. Adding a seventh variant breaks the
  match at compile time, and the new arm's assert — driven by the
  per-variant exemplars — fails the test until its coverage row is
  declared (a bare `=> ()` arm would satisfy only the compiler).
  A new protocol cannot skip detection, joining, or confirmation
  by accident.
- `coverage_view_edges_reference_the_real_workspace_filter_set`
  checks every `view_edges` name against the real
  `WORKSPACE_GRAPH_EDGE_TYPES` export (a copied literal cannot drift
  from the shipped filter) and pins the Topic row to
  `Produces`/`Consumes`/`PayloadSchema`.
- The per-kind round-trip evidence for the `binding_supported`
  column is `pr15_bindings_entry_round_trip_confirms_topic_consumer`
  (topic) and
  `pr15_bindings_entry_round_trip_confirms_rpc_ws_table_consumers`
  (rpc, ws, table) in `tests/federation_contracts_e2e.rs`, plus
  `task6_check_binding_yaml_round_trip` (graphql). HTTP's
  `binding_supported` evidence predates this plan — pinned by the
  `pr13_check_binding_*` HTTP binding tests in the same file, with no
  dedicated round-trip test name like the four above.

## Backlog

### Dashboard view edges

The dedicated contract edges of four kinds are outside
`WORKSPACE_GRAPH_EDGE_TYPES` and therefore invisible in
`get_workspace_graph`: GraphQL field reads (`ReadsFrom`,
`ReadsField`, `HasField`, `ResponseSchema`), RPC schema links
(`RequestSchema`, `ResponseSchema`, `HasField`), the WebSocket
handler leg (`Uses`), and the table legs (`ReadsTable`,
`WritesTable`). The nodes render; the contracts do not. Extending
the filter set is backlog by decision — the shipped set is pinned
by name in `src/server/mcp/federation_tools/workspace.rs` and
referenced (never copied) by the coverage matrix.

### Dynamic topic names

A publish/subscribe site whose name is neither a string literal nor a
same-file constant is dropped at the producer side (no `(broker, name)`
to anchor a `Topic` hub on — a hub with the sentinel would falsely
advertise a provider that never resolves). On the consumer side it IS
recorded: the synthetic per-site consumer node carries a
`TopicConsumerFact` whose `name` is `TOPIC_DYNAMIC_SENTINEL`
(`<dynamic>`); `TopicDispatch` recognises it and returns
`Unresolved { reason: DynamicTopic, target_service: None }`, which the
joiner pushes onto `coverage.unnormalized` next to the existing HTTP
`Unnormalized` arm. So a dynamic topic name is visible today: every
subscriber whose topic argument was not a literal lands in
`coverage.unnormalized` with `reason: DynamicTopic`. The producer side
stays a pure drop.

### Bus and job-queue systems (detection patterns only)

**Status (0.13.0):** implemented as YAML pattern entries in
`src/server/sensors/patterns/frameworks.yaml`, each with a `broker:`
label. Covered: Redis (pub/sub, Streams), MQTT, AWS SQS, SNS, Kinesis,
EventBridge, NATS, Pulsar, GCP Pub/Sub, Temporal, Dramatiq, Inngest,
Sidekiq, Resque, Kafka (kafka-python, Spring Kafka), and AMQP (already
present). Tests: `tests/bus_idioms_e2e.rs`. Not implemented in 0.13.0,
with reasons: **Quartz** (`@Scheduled` defines a schedule, not a named
topic; needs a design for scheduled sites), **ZeroMQ** (subscription
prefixes, not call-shaped topics), **Debezium** (connector configuration,
not code), **Huey** (Python, not yet covered by idiom entries),
**Redis Streams consumers in TS/Go**.

None of these is a `ContractKey` problem — `Topic { broker, name }`
is broker-generic, the binding grammar (`topic:<broker>:<name>`) is
already there, and the hub model (one canonical hub per
`(broker, topic)`, per-site `Produces`/`Consumes`, `PayloadSchema`)
is broker-agnostic. Every entry below is **detection pattern only —
no new contract kind needed**; each maps to `Topic { broker, name }`.
The recipe for adding any of them without new design is the next
section.

- **MQTT** (paho-mqtt / Eclipse Paho, mqtt.js) —
  `client.publish(topic, …)`, `client.subscribe(topic)` /
  `subscribe([topic, …])`, `client.unsubscribe(topic)`, and paho's
  positional `publish(topic, payload, qos, retain)`.
  *(detection pattern only — no new contract kind needed)*
- **NATS** (nats.py / `nats.aio`, nats.js, nats.go) —
  `nc.publish(subject, …)`, `nc.subscribe(subject, …)`,
  `nc.request(subject, …)`; JetStream `js.publish(subj, …)` /
  `js.subscribe(subj, …)`. Subject is the topic name.
  *(detection pattern only — no new contract kind needed)*
- **Redis pub/sub and Streams** (redis-py / `redis`, ioredis,
  go-redis) — `r.publish(channel, …)`, `pubsub.subscribe(channel)` /
  `psubscribe(pattern)`; Streams `r.xadd(stream, …)`,
  `r.xread({stream: …})`, `r.xreadgroup(group, consumer, …)`,
  `r.xgroup_create(stream, group, …)`.
  *(detection pattern only — no new contract kind needed)*
- **AWS SQS / SNS** (boto3, aws-sdk v2/v3) —
  `sqs.send_message(QueueUrl=…)` / `send_message_batch` /
  `receive_message(QueueUrl=…)`, `sns.publish(TopicArn=…)` /
  `sns.subscribe(TopicArn=…)`; JS `SendMessageCommand` /
  `ReceiveMessageCommand` / `PublishCommand` with `QueueUrl` /
  `TopicArn`. Queue URL or topic ARN is the name.
  *(detection pattern only — no new contract kind needed)*
- **Amazon Kinesis** (boto3, aws-sdk) —
  `kinesis.put_record(StreamName=…)` / `put_records`,
  `get_shard_iterator(StreamName=…)` / `get_records`; JS
  `PutRecordCommand({ StreamName })`. Stream name is the topic.
  *(detection pattern only — no new contract kind needed)*
- **Amazon EventBridge** (boto3, aws-sdk) —
  `events.put_events(Entries=[{Source, DetailType, EventBusName}])`;
  JS `PutEventsCommand({ Entries })`. Bus identity is `EventBusName`
  (default bus when absent); event identity is `Source`/`DetailType`.
  *(detection pattern only — no new contract kind needed)*
- **Apache Pulsar** (pulsar-client Python/Node/Java/Go) —
  `client.create_producer(topic=…)` / `producer.send(…)`,
  `client.subscribe(topic, subscription_name=…)`; Java
  `newProducer().topic("…").create()` /
  `newConsumer().topic("…").subscribe()`.
  *(detection pattern only — no new contract kind needed)*
- **ZeroMQ** (pyzmq, zeromq.js, zmq, cppzmq) — topics are PUB/SUB
  filter prefixes, not broker names:
  `sock.setsockopt(zmq.SUBSCRIBE, prefix)`,
  `sock.send_multipart([topic, …])`, `sock.send_string("topic …")`.
  *(detection pattern only — no new contract kind needed)*
- **Google Cloud Pub/Sub** (google-cloud-pubsub, `@google-cloud/pubsub`)
  — `publisher.publish(topic_path, data)`,
  `publisher.topic_path(project, topic)`,
  `subscriber.subscribe(subscription_path, callback)`; Node
  `pubsub.topic(name).publishMessage({ data })`.
  *(detection pattern only — no new contract kind needed)*
- **Temporal** (temporalio, `@temporalio/*`, Go SDK) —
  `Worker(client, task_queue=…)`,
  `client.start_workflow(…, task_queue=…)` /
  `execute_workflow(…)`; Go `worker.New(client, "task-queue", …)`,
  `StartWorkflowOptions{TaskQueue: …}`. Task queue is the topic.
  *(detection pattern only — no new contract kind needed)*
- **Sidekiq** (Ruby) — `Sidekiq::Client.push('class' => …, 'queue' =>
  …)`, `MyWorker.perform_async(…)` / `perform_in` / `perform_at`,
  `sidekiq_options queue: '…'`. Queue is the topic.
  *(detection pattern only — no new contract kind needed)*
- **Resque** (Ruby) — `Resque.enqueue(MyJob, …)`,
  `Resque.enqueue_to('queue', MyJob, …)`, `@queue = '…'` in the job
  class. *(detection pattern only — no new contract kind needed)*
- **Dramatiq** (Python) — `@dramatiq.actor(queue_name="…")`,
  `actor.send(…)` / `send_with_countdown(…)`; broker ctor
  (`RabbitmqBroker`, `RedisBroker`) names the transport.
  *(detection pattern only — no new contract kind needed)*
- **Huey** (Python) — `@huey.task()` /
  `@huey.periodic_task(crontab(…))`, `task(…)`,
  `huey.enqueue(task)`; `RedisHuey('queue-name')`. The Huey
  instance name is the topic.
  *(detection pattern only — no new contract kind needed)*
- **Quartz** (Java `@Scheduled`) — `@Scheduled(cron = "…")` /
  `fixedRate` / `fixedDelay` (Spring), `scheduler.scheduleJob(job,
  trigger)`, `JobBuilder.newJob(…).withIdentity("…")`. This is the
  `scheduled` shape (Celery / NestJS `@Cron` arm), not produce/
  consume. *(detection pattern only — no new contract kind needed)*
- **Inngest** — `inngest.send({ name: "…", data })`,
  `inngest.createFunction({ triggers: [{ event: "…" }] })` / `on: {
  event: "…" }`. The event name is the topic.
  *(detection pattern only — no new contract kind needed)*
- **Debezium / CDC** — connector configs (`topic.prefix`,
  `table.include.list`), Kafka Connect REST `POST /connectors` with
  `connector.class=io.debezium.…`, embedded
  `DebeziumEngine.create(…)`. Output topics follow
  `<topic.prefix>.<db>.<table>`; consuming them is an ordinary
  consumer idiom on the underlying broker, so the gap is the naming
  convention and connector-config-derived names.
  *(detection pattern only — no new contract kind needed)*

## Recipe: adding a bus without new design

Adding one of the systems above is a pattern entry, a name rule, and
tests. None of the six `ContractKey` kinds, the joiner, or the hub
model changes — `coverage_matrix()`'s exhaustive match stays closed.

**(a) Pattern entries, not code.** Detection lives in the
declarative pattern list `src/server/sensors/patterns/frameworks.yaml`
— the `topic_producer` / `topic_consumer` / `scheduled` entries under
each language key, consumed by `event_sensor` through
`Patterns::topic_producer_patterns` / `topic_consumer_patterns` /
`scheduled_patterns` and `util::topic_idioms_for`. Two real entries
as the template:

```yaml
    - id: pika-publish
      kind: topic_producer
      path_regex: '\.basic_publish\s*\([^)]*?routing_key\s*=\s*(?:["'']([^"'']+)["'']|([A-Za-z_]\w*))'
    - id: kafkajs-consumer
      kind: topic_consumer
      # Group 1: string-literal topic name in the topics array.
      path_regex: '\.(?:run|subscribe)\s*\(\s*\{[^}]*?topics?\s*:\s*\[\s*["'']([^"'']+)["'']'
```

The `kind` is the site kind: `topic_producer` →
`SiteKind::Produces`, `topic_consumer` → `SiteKind::Consumes`,
`scheduled` → `SiteKind::Scheduled` (emitted as a `Consumes` leg with
`TopicConsumerKind::Scheduled`). Capture group 1 is the name; an
optional group 2 is a bare identifier (see (b)). The broker label
follows the matched framework id, never the filename (`broker_for`
in `event_sensor.rs`): `pika*` / `amqplib*` mint `amqp/<queue>`,
everything else `kafka/<topic>`. A bus with its own broker label
extends that one mapping today; the follow-up below moves the label
onto the YAML entry.

**(b) Names from string literals or same-file constants.**
`resolve_idiom_capture` in `event_sensor.rs` applies §6.7: a quoted
string is the name as-is; a bare identifier resolves against the
file's `const` / `let` table; anything else is a **dynamic** name.
The producer side of a dynamic name is dropped (no `(broker, name)`
to anchor a hub on); the consumer side is recorded as a
`TopicConsumerFact` carrying the `TOPIC_DYNAMIC_SENTINEL`
(`<dynamic>`) name, and `TopicDispatch` returns
`Unresolved { reason: DynamicTopic, target_service: None }` for it
— the joiner pushes that call onto `coverage.unnormalized` so the
operator sees the gap (pinned by
`topic_join_dynamic_sentinel_is_unnormalized`, which asserts both
the verdict and the bucket; the producer drop stays pinned by
`dynamic_topic_arg_does_not_emit_topic_node`). Shape the regex to
capture literal-or-identifier (as `pika-publish` above); a pattern
that captures an expression contributes nothing for the producer
side and an unnormalized entry for the consumer side.

**(c) The hub model needs nothing new.** Any broker is
`Topic { broker, name }`: hub identity (label alone, `TOPIC_ANCHOR`),
the per-site `Produces` / `Consumes` legs, and the payload arm
(`*.avsc` / `*.schema.json` → `Schema` + `PayloadSchema`) are all
broker-agnostic. The joiner parses the label prefix back into
`ContractKey::Topic { broker, .. }` (`default_broker_for`), and
`topic:<broker>:<name>` / `topic:<broker>/<name>` already confirm any
broker by hand — Redis `orders:created`, MQTT `sensors/temp` are named
in the topic section's grammar above. No new `ContractKey` kind, no
joiner change, no binding-grammar change.

**(d) Tests follow `tests/event_hub_e2e.rs`.** Put the new call shape
in a fixture file (`fixture_repo_with` / `write_file`), run
`scan_workspace_event` (`scan_events`), and assert the hub contract
that file pins: one `Topic` node per `(broker, topic)`
(`two_sites_one_topic_mint_exactly_one_hub_node`), one
`Produces` / `Consumes` edge per site
(`hub_keeps_one_produces_edge_per_top_level_site`), and the canonical
hub id (`hub_id` / `TOPIC_ANCHOR` literal). Name resolution (literal,
same-file constant, dynamic drop) is pinned in `event_sensor`'s
`#[cfg(test)]` module (`ts_constant_topic_name_is_resolved`,
`dynamic_topic_arg_does_not_emit_topic_node`) — extend that shape for
the new entry.

### Standing follow-up: patterns as config

The event sensor now loads `Patterns::with_overrides` once per scan,
like `entry_point_sensor`, `http_client_sensor`, `http_sensor`, and
`field_access_sensor`. Repository-local `.lain/patterns/*.yaml` files
replace matching framework ids or append fresh ones, without changing
the bundled singleton. Event and HTTP regression tests exercise custom
patterns and isolation between repositories; malformed event overrides
leave the last successful scan intact.

One follow-up remains: bus knowledge still leaks into code, and the
standing directive is that
detection patterns must be **data, not scattered literals**
(`src/server/sensors/AGENTS.md`, "Detection patterns belong in
config/templates, not in code"):

`broker_for`'s framework-id → broker mapping (`pika*` / `amqplib*` →
`amqp`, else `kafka`) is a hardcoded arm list. Move the broker label
onto the YAML entry so pattern and broker travel as one data record.

Until that lands, adding a bus with a new broker label is a YAML entry
plus at most one `broker_for` arm — still no new design, but a code change
rather than a data change. Extracting the patterns to the
`<repo>/.lain/patterns` surface makes adding a bus data end to end.
