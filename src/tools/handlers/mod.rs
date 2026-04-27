//! Modular handlers for different tool domains

pub mod architecture;
pub mod impact;
pub mod navigation;
pub mod search;
pub mod metrics;
pub mod enrichment;
pub mod decoration;
pub mod filesystem;
pub mod execution;
pub mod context;
pub mod gitops;
pub mod testing;
pub mod query;
pub mod cross_runtime;
pub mod registry_impl;

#[cfg(test)]
mod filesystem_tests;
#[cfg(test)]
mod gitops_tests;
#[cfg(test)]
mod metrics_tests;
#[cfg(test)]
mod context_tests;
#[cfg(test)]
mod testing_tests;
#[cfg(test)]
mod query_tests;
#[cfg(test)]
mod cross_runtime_tests;
#[cfg(test)]
mod enrichment_tests;
#[cfg(test)]
mod architecture_tests;
