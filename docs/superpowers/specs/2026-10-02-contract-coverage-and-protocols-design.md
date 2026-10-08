# Contract federation — coverage ledger, wrapper resolution, env aliases, SQL tables, gRPC + GraphQL (2026-10-02)

Status: implemented. Branch: `feat/contract-coverage-and-protocols` →
PR #272 → squash `24010be5` on `dev`. The `Status: DRAFT` line above
this one is the original spec's pre-merge marker; it was stale at
merge time and is replaced by the implementation note below.

## 1. Intent

PR #270 gives LAIN a cross-repo contract map. Two weaknesses limit trust in it:

1. **A missing edge is ambiguous.** "Not found" and "not analyzed" look the same, so
   `NoKnownImpact` can be claimed for a repo LAIN could not read.
2. **Recall gaps.** Wrapped HTTP clients, env-configured hosts, non-HTTP protocols and
   data access are invisible or silently dropped.

Goal: make every verdict *sound with respect to what was analyzed*, then widen what is analyzed.
Soundness first, recall second, and never invent a binding.

## 2. Findings that shape the design (verified in code)

- `SensorCounts` / `RepoCoverage.sensor_counts` are counts only. Walkers `continue` silently on
  unsupported extensions and unreadable files; `run_all` converts sensor errors into `warn!`.
- §9.7 `complete` = no unreviewed repos AND no could-match unresolved consumer. Nothing models
  "repo has source we cannot analyze".
- `joiner.rs` rule 1: a `CallVia::Receiver` with no `http_clients` match is `continue`d — it is
  not recorded in `consumers` at all. **Bug**: silent drop, fixed in Phase A.
- Base-URL tracking exists only for Python `httpx/requests/aiohttp` ctor clients. TS/JS context
  is per-file plain assignments. All other languages: none. Wrapper modules are imported across
  files, so per-file context cannot work.
- `ContractKey` = `Http | Topic`. `graphql_sensor`, `websocket_sensor` are line scanners that emit
  `Variable`/`Uses`, not `ContractFact`. `proto_sensor` likewise.
- No `Table` node, no table edges.
- `FEDERATION_GRAPH_VERSION = 3` and v3 is unreleased: new node/edge types fold into v3 (no new
  bump). CHANGELOG still names the new types and `lain reindex`. Graphs built by earlier dev
  builds lack the new facts until reindexed (loader header unchanged).

## 3. Invariants (the "solid" part)

I1 **No invented bindings.** A `Binds` edge exists only with provenance recorded; an unresolvable
   call stays a candidate with a reason.
I2 **Every discovered call/operation lands in exactly one terminal state** in `ContractIndex`:
   `Binds | External | Unresolved{reason}`. Nothing is dropped.
I3 **Verdict soundness.** `NoKnownImpact(change)` ⇒ every repo in scope is `Analyzed` for every
   language present that can carry a consumer, and no unresolved consumer could match.
I4 **Determinism / order independence** of join output (extends §7.8).
I5 **No same-service Binds** (extends §7.8 to RPC and GraphQL). Data endpoints
   (`ContractKey::Table`) are exempt: a service reading its own warehouse table is
   not API self-consumption, and §7.8's rule targets a service calling its own
   published endpoints.
I6 **Resolution precedence is a total order**; adding evidence never lowers a bind's rank.
I7 **Index generation consistency.** A tool call reads one generation of `ContractIndex`, never a
   mix across live rejoin / snapshot swap.
I8 **Scan ownership / no peer retraction.** `replace_sensor_output(s)` retracts only the nodes
   `sensor_owner_of` assigns to `s`, then inserts `s`'s current scan output — a scan never removes
   a peer sensor's nodes. Modeled in `docs/formal/ScanRetract.tla`; enforced by
   `src/server/graph/mod.rs::replace_sensor_output` / `sensor_owner_of`.

I3 and I7 get TLA+ models (§9). I2, I4–I6 get property tests.

## 4. Phase A — Coverage ledger (soundness; lands first)

### 4.1 Model
Per repo, per sensor, per language:

```
SensorLedger {
  files_seen, files_analyzed,
  files_skipped: [{ reason: unsupported_language | unreadable | parse_error | size_cap, count, sample_paths(≤5) }],
  emitted,
  unresolved: [{ reason: dynamic_url | wrapper_unconfigured | base_unknown | env_unmapped | dynamic_topic | external_ref, count, sample_ids }],
  error: Option<String>
}
```
`CoverageLedger` = map `(sensor, lang) -> SensorLedger`, plus repo-level `languages_present`
(source files by extension, regardless of sensor support).

### 4.2 Mechanics
- Central file classification in `sensors::util::walk_workspace`: every file is classified once
  (language by extension, ignored/size-capped). Sensors report outcome per file through a small
  `ScanCtx` recorder instead of `continue`. `Sensor::scan` keeps its signature via a default
  method; new `scan_with_report(...) -> ScanReport` default-delegates, so the five legacy sensors
  are migrated incrementally (unmigrated sensors are reported as `ledger: unknown`, never as
  clean).
- `run_all` returns `(SensorCounts, CoverageLedger)`; a sensor `Err` becomes `ledger.error`.
- Persist the ledger in the per-commit index cache (§8.3) so pinned snapshot views carry it.
  `RepoCoverage` gains `ledger`; existing `sensor_counts` stays (derived).
- Tri-state query result for service/endpoint/consumer lookups:
  `found | not_found_analyzed | not_analyzed(reasons)`.

### 4.3 Verdict change
`evaluate()` downgrades `NoKnownImpact` → `NeedsInvestigation` when any in-scope repo has
(a) `languages_present` with no consumer-capable sensor, (b) `files_skipped` of reason
`parse_error|unreadable`, or (c) unresolved candidates that could match (existing §9.7 rule,
extended to the new reasons). The envelope `coverage.complete` is derived from the same predicate.
Golden tests update deliberately; each changed golden is called out in the PR.

### 4.4 Rule-1 fix
Wrapper candidates without an `http_clients` match become
`ConsumerTarget::Unresolved{ reason: WrapperUnconfigured }` and are listed in coverage.
Counted in `unresolved`, never bound (I1, I2).

### Acceptance
- Fixture repo containing a language with no sensor ⇒ `not_analyzed`, change classified
  `NeedsInvestigation`, not `NoKnownImpact`.
- Corrupt/oversized/unreadable file ⇒ appears in `files_skipped` with reason.
- Wrapper call with no config ⇒ present in `unresolved` with `wrapper_unconfigured`.
- Snapshot round-trip preserves the ledger.

## 5. Phase B — Wrapper & base-URL resolution

### 5.1 Client registry (per repo pre-pass)
Collect `ClientDef { name, module, base: Vec<UrlPart>, library, site }` from:
TS/JS `axios.create({baseURL})`, `ky.create({prefixUrl})`, `got.extend({prefixUrl})`,
`new Foo({baseUrl})` where `Foo` is a locally defined thin wrapper; Python existing ctor clients;
then, per language in later increments, Java `WebClient.builder().baseUrl`, Kotlin Ktor
`defaultRequest { url }`, C# `HttpClient{BaseAddress}`, Go `http.Client` + base const.
Cross-file: resolve `import { ordersClient } from "./clients"` and one re-export hop; deeper
chains stay unresolved (`base_unknown`). Exported factories returning a client are out of scope v1.

### 5.2 Composition
Final URL = `normalize(base_parts ++ call_path_parts)`; host derives from base via the existing
`host_for` (`Literal | Env | Expr`). `CallVia::Receiver` gains `base: Option<BaseOrigin{ client, module, site }>`
for evidence.

### 5.3 Resolution precedence (I6, total order, highest first)
1. Confirmed binding
2. Code-derived base + host/env → known service, route match (Static)
3. `http_clients` config pattern
4. `operationId` match (Heuristic 0.9)
5. Unbound-host heuristic (0.6) / ambiguous (0.3)
6. Unresolved candidate (reason recorded)

### Acceptance
`ordersClient.get("/123")` with a known base binds to the Orders endpoint with provenance naming the
client definition; with unknown/ambiguous base it is an unresolved candidate and **no** `Binds`
edge exists. Two clients of the same name in different modules do not cross-bind.

## 6. Phase C — Env aliases

New config-file sensor (`env_sensor`, phase 0) reads `.env*`, docker-compose `environment:`,
helm `values.yaml`, k8s `env:` → `EnvBinding{ var, host, source }`. Join: `HostPart::Env(vars)` →
host → `services[].hosts`, in addition to the existing `services[].env` match. Unmapped vars are listed
in the ledger as `env_unmapped`. Conflicting values for one var in one repo → ambiguous, not bound.

Acceptance: `process.env.ORDERS_API_URL` with `ORDERS_API_URL=http://orders:8080` in compose binds to
the service whose `hosts` contains `orders`; with no mapping, appears in `env_unmapped`.

## 7. Phase D — SQL tables

- Schema (folded into v3): `NodeType::Table`, `EdgeType::ReadsTable`, `EdgeType::WritesTable`;
  update `describe_schema`, `is_indexed`, `tool-schema.json`, schema-drift check.
- `sql_sensor` (phase 1): recognized call shapes (sqlx, rusqlite, `cursor.execute`, `db.query`,
  JDBC `prepareStatement`) with a **literal** statement ⇒ parse with a SQL parser (crate choice
  checked against dependency/vuln policy before adoption) ⇒ edges from enclosing function to
  `Table{service, name}`. Non-literal SQL ⇒ ledger `dynamic_sql`. CTEs, subselects and joins
  produce reads; INSERT/UPDATE/DELETE/MERGE produce writes. ORMs and column lineage: out of scope.
- Impact: handler → function → table is reachable through existing typed traversal (§5.2 table
  extended).

## 8. Phase E — gRPC and GraphQL

### 8.1 Model extension (v3)
`ContractKey::Rpc { system: Grpc, service, method }` and
`ContractKey::Graphql { op: Query|Mutation|Subscription, field }`. Provider/consumer facts reuse
`ProviderFact`/`ConsumerFact` shape with a protocol-specific key; `Endpoint.method/template` are
projected from the key for display. Same-service rule I5 applies.

### 8.2 gRPC (first)
- Provider: rewrite `proto_sensor` onto a real proto grammar (tree-sitter-proto if acceptable, else a
  small tokenizer with fixtures) emitting `service Foo { rpc Bar }` as providers; `package` is part
  of the identity (`pkg.Foo/Bar`). Server registration (`RegisterFooServer`, `add_FooServicer_to_server`,
  `@GrpcService`) links the provider to its implementing handler.
- Consumer: generated stub calls — `FooClient.Bar(...)`/`stub.Bar(...)` where the stub type or
  channel construction resolves to `pkg.Foo` through an import of the generated module. Target
  service via channel address (`grpc.NewClient("orders:50051")`, env) reusing Phase B/C host
  resolution. A stub call that cannot be tied to a proto service is a candidate
  (`rpc_stub_unknown`).
- Join: exact `(pkg.Service, method)` match within target service; no URL prefix tolerance.

### 8.3 GraphQL (second)
- Provider: SDL (`type Query { orders(...): [Order] }`) via a real parser (graphql-parser crate,
  subject to the same policy check) replacing the line scanner; resolver linkage by naming convention
  per framework (Apollo resolver maps, graphql-java `DataFetcher`, gqlgen, Strawberry) recorded with
  `Heuristic` provenance where by-name.
- Consumer: `gql`/`graphql` tagged templates, `.graphql` documents, persisted operations. Extract
  top-level selection fields of each operation → one consumer per root field. Fragment-only or
  interpolated documents ⇒ `dynamic_operation`.
- Endpoint target: `/graphql` HTTP route resolution via Phase B/C selects the service; join on
  `(op, field)` there. If several services expose the same root field (federation/gateway) ⇒ ambiguous,
  never single-bound.
- Field-level read tracking is out of scope v1 (operation level only).

### Acceptance (both)
Provider+consumer in two fixture repos bind; renaming an rpc/field produces
`ConsumerEndpointUnmatched`-class diff; unknown stub/dynamic document is a ledger candidate; no
same-service bind.

## 9. Formal verification (TLA+) and property tests

Setup: needs `tla2tools.jar` (Java present; the download requires approval). Specs live in
`docs/formal/` with a short action → code-line mapping per spec so they do not drift; CI runs TLC
only when a spec changes. Scale: 2–3 repos, 2 writers, 2 readers, 2 snapshots (races show up at tiny
sizes). Order: models 1 and 2 first (they target code that already exists); a counterexample is
confirmed, then fixed on this branch as its own small commit. Model 3 gates Phase A.

The findings below come from reading the code, not from running it. TLC confirms or kills each.

### 9.1 `RejoinProtocol.tla` (highest value)

Code: `federated_index.rs` `rejoin_contracts_if_dirty`, `rejoin_contracts`, `contracts_dirty`,
`projection_lock`, `contract_index`.

Suspected problems:
- **Lost dirty flag.** `rejoin_contracts` reads inputs, joins, writes `Binds`, swaps the index, and
  only then `contracts_dirty.store(false)`. `projection_lock` is taken at three sites (293, 333, 511);
  the setters at ~875/959 and the public `mark_contracts_dirty` do not reference it (refresh loop and
  hot reload not yet traced). A set landing mid-join is overwritten by the clear.
- **Torn input.** Nodes (`get_node`) and edges (`all_edges`) are read separately; a concurrent
  projection can land in between.
- **Two views of one generation.** `Binds` edges are applied before `contract_index` is swapped.

Model: writers (project nodes, project edges, add/remove repo, config reload); rejoin at its real step
granularity; readers that call `rejoin_if_dirty` then read both index and `Binds` edges.
Variants checked: (a) current code, (b) clear flag before reading and re-set on error, (c) epoch-stamped
atomic publish of `(index, binds)`.
Invariants: convergence (quiescent ⇒ `index == join(inputs)`); no lost update (an input change is
reflected or the flag is set); reader consistency (no mixed generation, I7).
Liveness: a dirty flag is eventually rejoined.

### 9.2 `SnapshotResidency.tla`

Code: `snapshots/manager.rs` `from_snapshot_with_wait_ms`, `install_resident`,
`try_evict_one_lru_unheld`, `HoldGuard`, `residency_notify`.

Suspected problems:
- `held` is an `AtomicBool`, not a count: two concurrent calls on one resident snapshot share it, the
  first drop clears it, and the snapshot can leave `resident` while still in use (the `|resident| ≤ cap`
  bound then stops meaning anything).
- `install_resident` checks `len() < cap` and inserts under separate lock acquisitions (cap overrun).
- `try_evict_one_lru_unheld` selects and removes under separate locks (a hold can land in between).
- No single-flight: two builders for one snapshot id both build; the second overwrites the first.
- The condvar uses a different mutex from the state it guards; the 50 ms poll may hide a lost wakeup.
  The model decides whether that is latency only or a hang.

Invariants: a held snapshot is never evicted; `|resident| ≤ cap`; at most one federation per snapshot
id; cache pins cover every live federation; `busy` is returned only when every slot is held.

### 9.3 `CoverageClaim.tla` (gates Phase A)

State: repos, languages, sensor support, repo states, unresolved candidates, cache entries.
Actions: reindex, snapshot, sensor failure, config change, analyzer upgrade.
Invariants: I3 (`NoKnownImpact ⇒` all in-scope repos analyzed and no could-match unresolved) and
`complete` never true while a scoped repo is unanalyzed.
Cache validity: `CacheKey` includes `analyzer_version`. If the ledger shape or a new sensor ships
without a bump, old entries read as "analyzed" when they are not. The model must show a bump is
required; the resulting rule is enforced in Phase A (ledger presence is part of cache validity).

### 9.4 Not modeled (property tests instead)

Pure functions: `match_route`, `compare_specificity`, URL normalization, `field_join`, resolution
precedence (I6, total order), determinism (I4), partition (I2), no same-service bind (I5), and
no-panic fuzz for each new parser. Sensor phase ordering is also left out: `replace_sensor_output`
is one write-locked swap and phases are sorted, so a model would be trivial.

## 10. Delivery

Order: spec + TLA+ → A → B → C → E-gRPC → E-GraphQL → D (D is independent; may move earlier).
One PR per phase against `dev` once #270 lands, otherwise stacked on #270. No version bumps. Every
phase: `cargo test`, `clippy --all-targets` 0/0, fmt, fresh-named ci-probe full battery plus the
acceptance harness before "done". Bugs found en route are fixed in place.

Branch hygiene: merge `feat/contract-federation` into this branch at each phase start and before
opening any PR; re-run the battery after the merge.

## 11. Risks

- Soundness change (A) alters verdicts and goldens — mitigated by calling out each golden diff.
- Cross-file client resolution may over-bind on name collisions — keyed by `(module, export)`, tests
  for collisions.
- New parser crates: license/vuln policy (`docs/VULNS.md`) before adoption.
- Schedule: 0.9.0 target 2026-10-12, hackathon 2026-10-30. Only A (and rule-1 fix) is a 0.9 gate;
  B is stretch; the rest follows.
