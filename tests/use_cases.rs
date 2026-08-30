//! Proving-grounds test suite — one file per use case.
//!
//! Each `mod` declaration pulls in a per-use-case file from
//! `tests/use_cases/`. The `#[path]` attribute is needed because
//! `tests/use_cases/` is a subdirectory, not a Rust module path
//! (cargo 2021's auto-discovery only treats top-level `tests/*.rs`
//! files as test targets). Keeping one file per use case means
//! a single regression can be bisected to the specific capability
//! that broke, and the failure message names the use case, not
//! just the test function.

#[path = "use_cases/cross_repo_peers_match.rs"]
mod cross_repo_peers_match;

#[path = "use_cases/find_dead_code.rs"]
mod find_dead_code;

#[path = "use_cases/find_anchors.rs"]
mod find_anchors;

#[path = "use_cases/get_call_sites.rs"]
mod get_call_sites;

#[path = "use_cases/get_code_snippet_paths.rs"]
mod get_code_snippet_paths;

#[path = "use_cases/watcher_reindex.rs"]
mod watcher_reindex;

#[path = "use_cases/workspace_graph_peers.rs"]
mod workspace_graph_peers;

#[path = "use_cases/battery_mcp_tools.rs"]
mod battery_mcp_tools;

#[path = "use_cases/battery_federation.rs"]
mod battery_federation;

#[path = "use_cases/battery_cli.rs"]
mod battery_cli;

#[path = "use_cases/battery_hooks.rs"]
mod battery_hooks;

#[path = "use_cases/battery_presence.rs"]
mod battery_presence;

#[path = "use_cases/battery_audit.rs"]
mod battery_audit;
