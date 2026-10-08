# `src/server/sensors/` — for AI coding agents

You're editing one of the five protocol detectors
(`http_sensor.rs`, `openapi_sensor.rs`, `proto_sensor.rs`,
`graphql_sensor.rs`, `websocket_sensor.rs`) or adding a new one.

**Before you write any code, read
[`docs/CONTRIBUTING_AGENTS.md`](../../../docs/CONTRIBUTING_AGENTS.md#sensor-pattern-one-concern-per-file-one-trait-shared).**

The short version:

- Each sensor is one concern. Don't mix protocol detection with graph
  mutation.
- The walker shell, the `is_read_only` guard, the `to_snake_case` /
  `to_camel_case` helpers, and the `find_handler_in_graph` resolver
  all live in `util.rs` and the `Sensor` trait default impls. **Import
  them; do not copy.**
- New sensors must `inventory::submit!(SensorEntry(&XSensor))` once.
  `scripts/check-no-duplicate-sensors.py` rejects free functions
  named `scan_workspace_*` / `enrich_with_*` that aren't paired with
  an inventory submission in the same file.

## Never put a `ContractFact` on a node another sensor owns

`replace_sensor_output(owner, …)` removes every node whose
`sensor_owner_of` matches `owner` **and its incident edges**
(`graph/mod.rs:896`). So a node shared between two sensors means one
sensor's rescan silently deletes the other's edges — the fact comes
back, the edges do not.

Rule: a node carrying a `ContractFact` must be owned by exactly one
sensor, and that sensor must be the only one that retracts it.

- Per-site consumer facts ride a **synthetic node** named
  `<prefix><path>:<line>` — see `SQL_READ_PREFIX` / `TOPIC_READ_PREFIX`
  in `util.rs`. Never reuse the enclosing symbol's id.
- Synthetic nodes keep `line_end: None`. `util::enclosing_symbol`
  requires both bounds, so this is what stops a later scan from
  resolving the synthetic node as "the enclosing symbol" and re-creating
  the collision.
- `sensor_owner_of` arms for these facts are **name-guarded**, so a
  pre-fix graph whose symbol node carries the fact is not retracted —
  deleting it would take real function nodes and their edges with it.
- Every edge you emit must have a **materialized** source and target.
  `insert_edges_batch` drops an edge whose endpoints aren't in the
  graph (`graph/mod.rs:1156`) without failing, so an unmaterialized
  endpoint makes the edge vanish rather than error.

If you're here to add a sixth sensor, the canonical shape is the
existing five files — copy their structure, not their walker.