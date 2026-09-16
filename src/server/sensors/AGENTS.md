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

If you're here to add a sixth sensor, the canonical shape is the
existing five files — copy their structure, not their walker.