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
//! - `config` (PR 7): `ContractFederationConfig` (§7.1) — the
//!   parsed-and-validated form of the contract-federation sections
//!   in `repos.yaml`. Pure data + validation; no graph or sensor
//!   dependencies.
//! - `index` (PR 7): `ContractIndex` (§4.3) — the per-federation
//!   shape derived by the joiner. Pure data.
//! - `joiner` (PR 7): `ContractJoiner::run(nodes, config)` computing
//!   the desired `Binds` edge set and `ContractIndex`.

pub mod config;
pub mod index;
pub mod joiner;
pub mod model;
pub mod normalize;
pub mod route_match;

#[cfg(test)]
mod config_tests;
#[cfg(test)]
mod index_tests;
#[cfg(test)]
mod joiner_tests;
#[cfg(test)]
mod model_tests;
#[cfg(test)]
mod scenario_tests;
