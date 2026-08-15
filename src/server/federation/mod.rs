//! Federation engine — multi-repo coordination.
//!
//! Moved from src/federation/ in PR 1 of the consolidation plan.

pub mod config;
pub mod federated_index;
pub mod graph_backend;
pub mod health;
pub mod loader;
pub mod manifest;
pub mod matching;
pub mod repo_id;
pub mod repo_index;
pub mod repo_source;
pub mod workspace;

#[cfg(test)]
mod federated_index_tests;
#[cfg(test)]
mod graph_backend_tests;
#[cfg(test)]
mod loader_tests;
#[cfg(test)]
mod manifest_tests;
#[cfg(test)]
mod matching_tests;
#[cfg(test)]
mod repo_source_tests;
