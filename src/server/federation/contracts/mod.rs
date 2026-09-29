//! Contract federation — types and operations for cross-repo
//! provider/consumer/field joins.
//!
//! The split is by concern:
//!
//! - `model` (this PR, task 3): the per-repo, sensor-written facts
//!   and the federation-level key types. No sensor logic, no joiner
//!   logic — just the data shapes every later task speaks.
//! - `normalize` (later): URL → `NormalizedUrl` (§4.5).
//! - `joiner` (later, task 7): `ContractJoiner::run(nodes, config)`
//!   computing the desired `Binds` edge set and `ContractIndex`.
//! - `index` (later): the per-federation `ContractIndex` derived by
//!   `rejoin_contracts` and never persisted (§4.3).
//!
//! This PR only ships `model`. The other submodules land in later PRs;
//! touching them here is out of scope.

pub mod model;

#[cfg(test)]
mod model_tests;
