//! Modular handlers for different tool domains

pub mod architecture;
pub mod decoration;
pub mod enrichment;
pub mod execution;
pub mod explain_dispatch;
pub mod impact;
pub mod metrics;
pub mod navigation;
pub mod search;
pub mod semantic;
// `filesystem` was removed: handlers + tests for three functions that
// were never registered as MCP tools, so no agent could reach them.
// File reads are served via `get_code_snippet` and the clients' native
// tools.
pub mod context;
pub mod cross_runtime;
pub mod gitops;
pub mod query;
pub mod registry_impl;
pub mod testing;

#[cfg(test)]
mod architecture_tests;
#[cfg(test)]
mod context_tests;
#[cfg(test)]
mod cross_runtime_tests;
#[cfg(test)]
mod enrichment_tests;
#[cfg(test)]
#[cfg(test)]
mod gitops_tests;
#[cfg(test)]
mod metrics_tests;
#[cfg(test)]
mod query_tests;
#[cfg(test)]
mod search_tests;
#[cfg(test)]
mod testing_tests;
