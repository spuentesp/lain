//! Sensor test aggregator.
//!
//! `tests/sensors/patterns.rs` is a subdirectory test target. Cargo
//! only auto-discovers `.rs` files directly under `tests/` as
//! integration test crates, so this thin file uses `#[path]` to wire
//! the per-sensor test files in. Mirror the `use_cases.rs` shape.

#[path = "sensors/entry_point_patterns.rs"]
mod entry_point_patterns;

#[path = "sensors/field_access_deny.rs"]
mod field_access_deny;

#[path = "sensors/http_client_sensor_patterns.rs"]
mod http_client_sensor_patterns;

#[path = "sensors/http_sensor_patterns.rs"]
mod http_sensor_patterns;

#[path = "sensors/patterns.rs"]
mod patterns;

#[path = "sensors/patterns_build.rs"]
mod patterns_build;

#[path = "sensors/patterns_override.rs"]
mod patterns_override;

#[path = "sensors/patterns_new_framework.rs"]
mod patterns_new_framework;
