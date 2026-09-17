//! Tool definitions and schemas
//!
//! `ToolDefinition` struct used by `ToolRegistry::definitions()` to build MCP schema.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadinessRequirement {
    GraphIndependent,
    GraphRequired,
    SemanticRequired,
}

/// Tool definition for MCP registration
pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub readiness: ReadinessRequirement,
}

/// Mandatory classification table for every advertised MCP tool. Returning
/// `None` makes schema validation fail when a new tool has not been reviewed.
pub fn readiness_requirement(name: &str) -> Option<ReadinessRequirement> {
    use ReadinessRequirement::*;
    if name == "semantic_search" {
        return Some(SemanticRequired);
    }
    if matches!(
        name,
        "get_health"
            | "get_capabilities"
            | "understand_repository"
            | "find_symbol"
            | "search_code"
            | "get_agent_strategy"
            | "get_server_status"
            | "list_recent_projects"
            | "get_reload_status"
            | "request_reload"
            | "register_agent"
            | "heartbeat"
            | "list_active_agents"
            | "who_am_i"
            | "list_subagents"
            | "claim_files"
            | "release_files"
            | "list_occupancy"
            | "my_claims"
            | "detect_overlap"
            | "get_audit_log"
            | "register_job_webhook"
            | "get_job_status"
            | "debug_sleep"
            | "install_language_server"
            | "list_repos"
            | "get_repo_info"
            | "get_federation_health"
            | "list_workspaces"
            | "get_active_workspace"
            | "get_workspace"
            | "get_file_diff"
            | "get_commit_history"
            | "get_branch_status"
            | "run_build"
            | "run_tests"
            | "run_clippy"
            | "get_world_state"
            | "get_recent_activity"
            | "add_annotation"
            | "list_annotations"
            | "resolve_annotation"
            | "leave_handoff_note"
            | "get_pending_handoffs"
    ) {
        return Some(GraphIndependent);
    }
    if matches!(
        name,
        "explore_architecture"
            | "list_entry_points"
            | "compare_modules"
            | "architectural_observations"
            | "trace_dependency"
            | "get_call_chain"
            | "navigate_to_anchor"
            | "get_layered_map"
            | "get_master_map"
            | "get_blast_radius"
            | "get_coupling_radar"
            | "find_anchors"
            | "get_anchor_score"
            | "get_context_depth"
            | "find_dead_code"
            | "explain_symbol"
            | "suggest_refactor_targets"
            | "query_graph"
            | "describe_schema"
            | "get_cross_runtime_callers"
            | "run_enrichment"
            | "sync_state"
            | "get_context_for_prompt"
            | "get_context"
            | "find_related"
            | "assess_change"
            | "get_code_snippet"
            | "get_call_sites"
            | "find_untested_functions"
            | "get_test_template"
            | "get_coverage_summary"
            | "search_org"
            | "get_cross_repo_blast_radius"
            | "get_cross_repo_blast_radius_for_repo"
            | "get_workspace_graph"
    ) {
        return Some(GraphRequired);
    }
    None
}
