//! Contract federation — types and operations for cross-repo
//! provider/consumer/field joins.
//!
//! The split is by concern:
//!
//! - `model` (PR 3): the per-repo, sensor-written facts and the
//!   federation-level key types. No sensor logic, no joiner logic —
//!   just the data shapes every later task speaks.
//! - `normalize` (PR 5): URL → `NormalizedUrl` (§4.5). Pure
//!   functions; no graph or config dependencies.
//! - `route_match` (PR 5): provider template ↔ consumer template
//!   matching (§7.4). Pure logic; used by the joiner in PR 7.
//! - `joiner` (PR 7): `ContractJoiner::run(nodes, config)` computing
//!   the desired `Binds` edge set and `ContractIndex`.
//! - `index` (later): the per-federation `ContractIndex` derived by
//!   `rejoin_contracts` and never persisted (§4.3).

pub mod model;
pub mod normalize;
pub mod route_match;

#[cfg(test)]
mod model_tests;