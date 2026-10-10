# Phase D — SQL tables — Report

Spec: `docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §7
Plan: `docs/superpowers/plans/2026-10-02-coverage-and-protocols.md` §"Phase D"

## Per-task commit hashes

| Task | Commit | Subject |
|---|---|---|
| 1 (`NodeType::Table` + `EdgeType::{ReadsTable, WritesTable}` folded into v3) | `b7681d3d` | feat(schema): NodeType::Table + EdgeType::{ReadsTable, WritesTable} (folded into v3) |
| 2 (`Table { service, name }` contract payload) | `f52acd99` | feat(contracts): Table node type with service+name |
| 3 (`sql_sensor` literal-statement shapes + hand-rolled SQL parser) | `aef8dc59` | feat(sensors): sql_sensor — literal-statement shapes (sqlx / rusqlite / cursor.execute / JDBC) |
| 4 (function→Table edge wiring via `enclosing_symbol` → file fallback) | `aef8dc59` | (folded into Task 3 commit; same `sql_sensor::build_graph` path) |
| 5 (D1–D6 acceptance scenarios) | `b91bd0af` | test(contracts): Phase D acceptance scenarios (D1–D6) |
| 6 (clippy + fmt cleanups) | `5afd4250` | chore(contracts): clippy cleanups after Phase D |

## Acceptance outcomes (D1–D6)

`tests/sql_tables.rs` runs all 7 scenarios (D1–D6 plus the
parser re-exercise) under `cargo +nightly test --test sql_tables`:

```
running 7 tests
test d1_sqlx_query_emits_reads_table ... ok
test d2_rusqlite_execute_emits_writes_table ... ok
test d3_python_cursor_execute_emits_writes_table ... ok
test d4_cte_subselect_join_emit_reads_to_every_table ... ok
test d5_non_literal_sql_is_dynamic ... ok
test d6_orm_usage_emits_no_edges ... ok
test d4_parser_re_exercised ... ok
test result: ok. 7 passed; 0 failed
```

- **D1** (spec §7): `sqlx::query("SELECT id FROM orders WHERE id = ?")`
  emits a `Table { service: "", name: "orders" }` node and one
  `ReadsTable` edge from the enclosing function (`list_order`)
  to the table. `service` is the empty string at scan time; the
  joiner fills it from `repos.yaml` the same way Phase B
  resolves an HTTP-client wrapper to its owning service.

- **D2** (spec §7): `conn.execute("UPDATE orders SET status = 'paid' WHERE id = ?", [...])`
  emits a `WritesTable` edge. The sensor matches `conn.execute(`
  directly (more specific than the generic `.execute(` needle)
  and dedupes overlapping needles per line.

- **D3** (spec §7): `cursor.execute("INSERT INTO orders (id) VALUES (?)")`
  emits a `WritesTable` edge. The shape matches both
  `cursor.execute(` and `.execute(`; the dedup picks the longer
  (more specific) needle so the site appears once.

- **D4** (spec §7): the fixture runs three SQL statements —
  CTE / subselect / JOIN — and emits one `Table` node per
  distinct name and one `ReadsTable` edge per
  `(function, table)`. The hand-rolled parser walks balanced
  parens for CTE / subselect bodies and reads JOIN partners.
  The graph's `insert_edges_batch` dedups on
  `(source, target, edge_type)`, so multiple statements
  referencing the same table collapse to one edge per
  `(function, table)` pair — the semantic the typed
  traversal asks is "the function reads from this table",
  not "the function reads from this table N times".

- **D5** (spec §7): non-literal SQL
  (`sqlx::query(&format!("SELECT * FROM {}", table))`) records
  the call site on the per-repo coverage ledger's `unresolved`
  bucket with `reason: UnresolvedReason::DynamicSql`. No
  `Table` nodes or `ReadsTable` / `WritesTable` edges are
  emitted. The ledger bucket is wired today via the
  `CoverageLedger.by_repo[repo].ledger[sensor][lang]` map;
  the per-lang fill-in ships with the Phase A
  `scan_with_report` migration (the legacy `scan` only
  returns a single integer count, so per-lang `unresolved`
  is populated by the orchestrator from `take_unmapped_records`
  on the existing sensors).

- **D6** (spec §7): ORM calls
  (`session.query(Order).all()`, `session.save(obj)`) produce
  no SQL literal so no needle matches; no edges, no ledger
  entry. ORMs are out of scope per spec.

## Gate outcomes

| Gate | Outcome |
|---|---|
| `cargo clippy --all-targets -- -D warnings` (stable) | clean |
| `cargo fmt --check` | clean |
| `python3 scripts/check-no-duplicate-sensors.py` | clean |
| `python3 scripts/check-mcp-dispatch-shape.py` | clean |
| `python3 scripts/check-no-mirror-dtos.py` | clean |
| `python3 scripts/check-format-duration-once.py` | clean |
| `bash scripts/check-mod-resolution.sh` | clean (`ok: all mod declarations resolve`) |
| `pr13_hermetic_precision_recall_over_t1_fixture` (1.000 × 6) | passes |
| `committed_digest_matches_fresh_recomputation` (no digest drift) | passes |
| `digest_is_deterministic_across_two_runs` | passes |
| `cargo +nightly test --lib` | 2029 passed; 0 failed |
| `cargo +nightly test --test sql_tables` | 7 passed; 0 failed |
| `cargo +nightly test --test env_aliases` | 6 passed; 0 failed |
| `cargo +nightly test --test coverage_ledger` | 7 passed; 0 failed |
| `cargo +nightly test --test contract_federation_integration` | 12 passed; 0 failed |
| `cargo +nightly test --test federation_contracts_e2e` | 40 passed; 0 failed |

Notes on the digest: the fixture
(`tests/fixtures/contracts/analyzer_digest.txt`) pins
`digest: 65916faf5b0841ef9e7b3afd5d8f117d971042ae1303c987fd9c01bffbb331ae`
under `analyzer_rev: 3`. Adding `NodeType::Table` (variant
22) and `EdgeType::{ReadsTable, WritesTable}` (variants 22,
23) does not change the bincode encoding of existing
variants — bincode serialises enums by their positional
discriminant, and existing values stay at their old
discriminants. The fixture's indexed repos have no SQL
calls (sqlx / rusqlite / cursor.execute / JDBC), so the
sensor emits zero `Table` nodes there; the digest
therefore stays at the committed value. `FEDERATION_GRAPH_VERSION`,
`PATH_FORMAT_VERSION`, and `CONTRACT_ANALYZER_REV` are all
unchanged per spec §2 "v3 is unreleased: new node/edge
types fold into v3".

Notes on `sqlparser-rs` / `sqlparser`: the spec lists them
as the parser to use, but the hard rules forbid
`Cargo.toml [dependencies]` runtime version bumps and the
CONTRIBUTING_AGENTS guide says agents are not expected to
add dependencies. The sensor ships a focused hand-rolled
parser instead: it covers the spec's required SQL shapes
(SELECT with CTE / subselect / JOIN; INSERT / UPDATE /
DELETE / MERGE) and treats anything else (DDL, PRAGMA, …)
as `DynamicSql` so the operator sees the call site exists
even when the statement can't be classified.

## PR13_METRICS_JSON

```
PR13_METRICS_JSON {"diff_precision":1.0,"diff_recall":1.0,"binds_precision":1.0,"binds_recall":1.0,"reads_field_precision":1.0,"reads_field_recall":1.0}
```

## Files changed

- `src/server/schema.rs` — new `NodeType::Table`, new
  `EdgeType::{ReadsTable, WritesTable}`, description +
  is_indexed + all + source_types + target_types entries.
- `src/server/federation/graph_backend.rs` —
  `impact_propagation` maps ReadsTable / WritesTable to
  Incoming.
- `src/server/mcp/presence_tools.rs` — `symbol_weight` adds
  `NodeType::Table` at weight 1 (leaf).
- `src/server/tools/handlers/semantic.rs` —
  `node_type_label` adds `NodeType::Table` as `table`.
- `src/server/federation/contracts/model.rs` —
  `ContractFact::Table(Table)` variant + `Table { service,
  name }` struct.
- `src/server/federation/contracts/index.rs` —
  `node_type_of` maps `ContractFact::Table(_)` to
  `NodeType::Table`.
- `src/server/federation/contracts/coverage.rs` — new
  `UnresolvedReason::DynamicSql`.
- `src/server/sensors/sql_sensor.rs` (new) — the
  literal-statement sensor; recognises sqlx / rusqlite /
  PEP 249 / JDBC / node-postgres & friends; hand-rolled SQL
  parser; emits `Table` nodes + `ReadsTable` / `WritesTable`
  edges via `replace_sensor_output(SensorOwner::SqlSensor, …)`.
- `src/server/sensors/mod.rs` — registers `sql_sensor`
  module; adds `SensorCounts::sql_tables`,
  `SensorCountField::SqlTables`.
- `src/server/graph/mod.rs` — new `SensorOwner::SqlSensor`;
  `sensor_owner_of` maps `NodeType::Table` /
  `ContractFact::Table(_)` to it.
- `tests/sql_tables.rs` (new) — D1–D6 acceptance + parser
  re-exercise.
- `CHANGELOG.md` — Phase D schema fold-in note.

## Phase D — complete

```
PHASE D COMPLETE — SQL tables live, hermetic 1.000
```