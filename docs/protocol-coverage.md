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
- **Status.** Complete.

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
  holds an exhaustive `match` over `ContractKey`. Adding a seventh
  variant fails to compile there until its coverage row is declared
  — a new protocol cannot skip detection, joining, or confirmation
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
  `task6_check_binding_yaml_round_trip` (graphql).

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

### Bus and job-queue systems (detection patterns only)

None of these is a `ContractKey` problem — `Topic { broker, name }`
is broker-generic, and the binding grammar is already there. Each
is a pure detection-pattern addition to `event_sensor`'s
declarative pattern list:

- MQTT (paho, mqtt.js)
- NATS (nats.py, nats.js)
- Redis pub/sub and Streams (redis-py, ioredis)
- AWS SQS / SNS (boto3, aws-sdk)
- Amazon Kinesis
- Amazon EventBridge
- Apache Pulsar
- ZeroMQ
- Google Cloud Pub/Sub
- Job queues: Temporal, Sidekiq, Resque, Dramatiq, Huey
- Quartz (Java `@Scheduled`)
- Inngest
- Debezium / CDC output topics

Adding a system is data, not code: a pattern entry whose captures
map to `SiteKind::Produces` / `SiteKind::Consumes`. Names read from
literals or same-file constants; dynamic names land in
`coverage.unnormalized`. The hub model needs nothing new — any
broker is `Topic { broker, name }`. Adding systems is backlog by
design.
