//! Typed MCP tool definitions. The stdio `tools/list` handler and the
//! HTTP `tools/list` handler both need to enumerate the same tool
//! surface; before this module the two paths iterated three tuple
//! arrays (`SERVER_TOOL_DEFS`, `FEDERATION_TOOL_DEFS`,
//! `WORKSPACE_TOOL_DEFS`) inline, with the same `for (name, description,
//! required) in *_DEFS { … build Tool }` block copy-pasted in both
//! paths. Moving the data here gives:
//!
//! - One place to add/remove a tool (no more editing two dispatchers).
//! - A typed `ToolDef` instead of `(&str, &str, &[&str])` tuples, so a
//!   typo in a tool name or arg name is a compile error.
//! - A single `defs_to_tools(&[ToolDef])` helper that the two
//!   `tools/list` implementations both call.
//!
//! Special-case tools (the 6 tools that `ToolExecutor::call_inner`
//! handles directly without going through the per-domain handler
//! machinery) stay in their own module — see
//! [`crate::tools::definitions::ToolDefinition`], the type the
//! `ToolRegistry` already enumerates.

use crate::server::mcp::envelope::arg_property_schema;
use rust_mcp_schema::{Tool, ToolInputSchema};

/// A typed tool declaration. `name`, `description`, and
/// `required_args` are the three things every MCP tool must
/// describe; `required_args` is also the source of truth for the
/// per-argument JSON Schema fragment (see [`arg_property_schema`]).
#[derive(Debug, Clone, Copy)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    /// Argument keys the tool *requires*; the JSON schema fragment for
    /// each is generated from [`arg_property_schema`].
    pub required_args: &'static [&'static str],
}

impl ToolDef {
    /// Build the `ToolInputSchema` (JSON Schema `properties` +
    /// `required`) for this tool by mapping each key in
    /// `required_args` through `arg_property_schema`.
    pub fn to_input_schema(&self) -> ToolInputSchema {
        let mut props = std::collections::BTreeMap::new();
        for req in self.required_args {
            props.insert((*req).to_string(), arg_property_schema(req));
        }
        ToolInputSchema::new(
            self.required_args.iter().map(|s| s.to_string()).collect(),
            if props.is_empty() { None } else { Some(props) },
            None,
        )
    }
}

/// Federation-mode MCP tools. Registered in `tools/list` only when the
/// server was constructed with a `FederatedIndex` (see
/// `LainMcpServer::with_federation`).
pub const FEDERATION_TOOL_DEFS: &[ToolDef] = &[
    ToolDef {
        name: "list_repos",
        description: "List every repository currently registered in the federation, with id, path, health, and graph stats.",
        required_args: &[],
    },
    ToolDef {
        name: "get_repo_info",
        description: "Get info about a single repository in the federation by id.",
        required_args: &["id"],
    },
    ToolDef {
        name: "get_federation_health",
        description: "Aggregate health counts and total node/edge counts across the federation, plus a rough memory estimate.",
        required_args: &[],
    },
    ToolDef {
        name: "search_org",
        description: "Case-insensitive substring search across every repo's symbols (matched on name or path). Args: query (substring), limit (max results, parsed as usize). Returns matches sorted by (repo_id, name).",
        required_args: &["query", "limit"],
    },
    ToolDef {
        name: "get_cross_repo_blast_radius",
        description: "Resolve a symbol across the federation, traverse outgoing Calls edges in [min_depth, max_depth) (depth is a u32 range like \"1..3\"), and group visited nodes by repo. Returns {by_repo: {repo_id: [global_ids...]}, total_count, truncated}. Caps at 1000 nodes; truncated=true when the cap is hit.",
        required_args: &["symbol", "depth"],
    },
    ToolDef {
        name: "get_cross_repo_blast_radius_for_repo",
        description: "Same as get_cross_repo_blast_radius but the caller disambiguates the repo explicitly via repo_id, bypassing symbol resolution. Args: repo_id, symbol, depth (u32 range like \"1..3\"). Returns {by_repo: {repo_id: [global_ids...]}, total_count, truncated}.",
        required_args: &["repo_id", "symbol", "depth"],
    },
];

/// Workspace-aware MCP tools, registered when the server was constructed
/// with a `WorkspacesFile` (i.e., when a workspace may be active). These
/// are additive to the federation tools — they don't replace anything.
pub const WORKSPACE_TOOL_DEFS: &[ToolDef] = &[
    ToolDef {
        name: "list_workspaces",
        description: "List all known workspaces from workspaces.yaml. Returns [{name, description?, source?, member_count, is_active}].",
        required_args: &[],
    },
    ToolDef {
        name: "get_active_workspace",
        description: "Return the workspace the server is currently holding (the one whose repos were loaded). Errors with NoActiveWorkspace if the server was started without --workspace or no workspace matches the loaded repos.",
        required_args: &[],
    },
    ToolDef {
        name: "get_workspace",
        description: "Full detail on one workspace by name: description?, source?, members: [{repo_id, path, health}]. Errors with NotFound if name is unknown.",
        required_args: &["name"],
    },
    ToolDef {
        name: "get_workspace_graph",
        description: "Per-workspace graph for the dashboard. Returns {nodes: [...], edges: [...], truncated: bool}. Filters to Function/Method/Class + Calls/Imports. Optional filter: substring match against node name + path. Cross-repo Calls edges are marked cross_repo: true.",
        required_args: &["filter?"],
    },
];

/// Server-status, recent-projects, reload, and multiplayer (presence)
/// tools — always available regardless of whether the server is in
/// federation mode or which workspaces (if any) are loaded. The
/// multiplayer entries (register_agent, claim_files, etc.) are the
/// canonical argument names; `arg_property_schema` returns `{"type":
/// "string"}` for any arg it doesn't special-case, so unknown keys
/// (e.g. `name`, `session_token`) become string-typed in the JSON
/// Schema output. The handler-side `ToolExecutor::call_inner`
/// dispatches these by name; the tool list returned to MCP clients
/// here is just the metadata.
pub const SERVER_TOOL_DEFS: &[ToolDef] = &[
    ToolDef {
        name: "get_server_status",
        description: "Returns the server's run-time status: pid, transport, port, started_at, last_sync_at, last_error, repo_count, workspace_count.",
        required_args: &[],
    },
    ToolDef {
        name: "list_recent_projects",
        description: "List projects the operator has used recently, with per-project workspace_count and repo_count pulled from each project's repos.yaml/workspaces.yaml.",
        required_args: &[],
    },
    ToolDef {
        name: "get_reload_status",
        description: "Returns the current reload subsystem state: state (idle | rebuilding | failed), started_at, last_reload_at, last_error, pending_changes.",
        required_args: &[],
    },
    ToolDef {
        name: "request_reload",
        description: "Schedule a hot-reload of repos.yaml and workspaces.yaml. The actual rebuild runs on a background task; the call returns immediately after queueing the signal.",
        required_args: &[],
    },
    ToolDef {
        name: "register_agent",
        description: "Register this agent with lain. Returns an agent_id and session_token to use on every subsequent call.",
        required_args: &["name"],
    },
    ToolDef {
        name: "heartbeat",
        description: "Refresh the session. lain expires sessions 60 seconds after the last heartbeat.",
        required_args: &["agent_id", "session_token"],
    },
    ToolDef {
        name: "list_active_agents",
        description: "List agents currently connected. Set include_background=true to also see cron/CI agents.",
        required_args: &[],
    },
    ToolDef {
        name: "who_am_i",
        description: "Resolve a session_token to the agent it belongs to, plus their current claims.",
        required_args: &["session_token"],
    },
    ToolDef {
        name: "list_subagents",
        description: "List active subagents whose parent_session_id matches the caller's session. Args: session_token. Returns: { parent, subagents: [{ agent_id, name, kind, mode, started_at_unix, last_heartbeat_unix }] }",
        required_args: &["session_token"],
    },
    ToolDef {
        name: "claim_files",
        description: "Announce intent to edit (or read) files/symbols. Returns conflicts: other agents already holding claims.",
        required_args: &["agent_id", "session_token", "files"],
    },
    ToolDef {
        name: "release_files",
        description: "Release claims. Other agents get a notification.",
        required_args: &["agent_id", "session_token", "files"],
    },
    ToolDef {
        name: "list_occupancy",
        description: "Show which agents are in a file or the whole workspace.",
        required_args: &[],
    },
    ToolDef {
        name: "my_claims",
        description: "List files this agent has claimed.",
        required_args: &["agent_id", "session_token"],
    },
    ToolDef {
        name: "detect_overlap",
        description: "Detect symbol-level overlap between two git refs in a workspace. Args: base (required), head (defaults to HEAD), workspace (required). Returns { base, head, total_overlaps, files: [{ repo, path, symbols_base, symbols_head, overlap, severity }] } — a non-empty overlap means both refs edited the same definition. `severity` is none | low | medium | high, graded by the kinds of the shared symbols (a shared function weighs more than a shared module).",
        required_args: &["base", "workspace"],
    },
    ToolDef {
        name: "get_audit_log",
        description: "Read the server's audit log (per-write events appended by the claim_files handler when a claim is granted). Args: since_unix (drop events whose ts_unix is strictly less than this), path_glob (filter to events whose path matches this glob — see src/server/glob_match.rs for the supported subset). Returns an array of AuditEvent objects (ts_unix, agent_id, path, claim_set, racers, plan_revision, landed_revision). The on-disk file is `<state_dir>/audit.jsonl`, rotation-capped at 50 MB.",
        required_args: &[],
    },
    ToolDef {
        name: "get_world_state",
        description: "Read-only companion to claim_files: returns the same WorldState shape (retracted symbols, overlay-delta changed_symbols filtered to the requested set, and a resync note for BeyondCurrent/TooOld) without taking a claim. Lets an LLM ask 'is this symbol still in the graph?' or 'what's the world state for these symbols?' before deciding whether to claim. Args: symbols (list of symbol names to check for retract — empty list yields a no-op WorldState), plan_revision (the agent's last-seen overlay revision; omit for current). Returns { current, plan, changed_symbols: [{ name, change_kind, at_revision }], note } where change_kind is Edited | Retracted and note is set only on BeyondCurrent ('plan_revision beyond current — server may have restarted') or TooOld ('plan_revision too old for delta; resync required').",
        required_args: &[],
    },
    ToolDef {
        name: "get_recent_activity",
        description: "Compact digest of the audit log: groups recent edit_landed events by path (default), agent, or hour and returns a count + sample per group. Designed for LLM session compaction — instead of re-reading every audit.jsonl line, the agent gets a navigable summary and can call get_audit_log with a specific path_glob for full detail. Args: since_unix (filter by ts_unix), group_by ('path' (default) | 'agent' | 'hour'), path_glob (pre-filter by path before grouping), limit (max groups returned, default 20). Returns { groups: [{ key, count, first_ts, last_ts, sample_event }], total_events, total_groups, truncated, group_by }. truncated=true when total_groups > limit.",
        required_args: &[],
    },
];

/// Map a `&[ToolDef]` to the Vec<Tool> shape the MCP `tools/list`
/// response expects. Both the stdio and the HTTP paths call this
/// with their respective slice; the function itself is shared.
pub fn defs_to_tools(defs: &[ToolDef]) -> Vec<Tool> {
    defs.iter()
        .map(|d| Tool {
            name: d.name.to_string(),
            description: Some(d.description.to_string()),
            input_schema: d.to_input_schema(),
            annotations: None,
            execution: None,
            icons: vec![],
            meta: None,
            output_schema: None,
            title: None,
        })
        .collect()
}
