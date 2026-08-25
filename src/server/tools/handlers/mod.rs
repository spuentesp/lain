//! Modular handlers for different tool domains

pub mod architecture;
pub mod impact;
pub mod navigation;
pub mod search;
pub mod metrics;
pub mod enrichment;
pub mod decoration;
pub mod execution;
// `filesystem` (read_file / list_directory / find_files) was removed:
// 112 lines of handler and 151 lines of tests for three functions that
// were never registered as MCP tools, so no agent could reach them. The
// capability is covered — `get_code_snippet` reads source through the
// graph, and every MCP client already ships native file tools. Keeping an
// unreachable surface alive is what turned `FileWatcher`, the protocol
// sensors and `run_background_sync` into silent no-ops.
pub mod context;
pub mod gitops;
pub mod testing;
pub mod query;
pub mod cross_runtime;
pub mod registry_impl;

#[cfg(test)]
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
#[cfg(test)]
mod search_tests;
