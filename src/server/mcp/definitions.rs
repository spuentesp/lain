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

use crate::server::mcp::envelope::tool_arg_property_schema;
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
    /// Argument keys the tool *accepts* but does not require.
    ///
    /// Undeclared optional args are unusable: a schema-respecting
    /// client (Claude Code among them) sends only what the schema
    /// declares, so `list_active_agents`'s documented
    /// `include_background` and `detect_overlap`'s documented `head`
    /// could be described in prose and never actually passed.
    pub optional_args: &'static [&'static str],
    /// Optional JSON Schema for the tool's input, loaded with
    /// `include_str!` from `mcp/contract_tools/schemas/<tool>.in.json`
    /// (contract tools; `§10.1`). `None` for tools whose input
    /// schema is built dynamically from `required_args` /
    /// `optional_args` (the 0.8 inventory tools).
    pub input_schema: Option<&'static str>,
    /// Optional JSON Schema for the tool's output (`§10.1`). The
    /// schema describes the `data` field of the envelope, not the
    /// envelope itself. Used by `lain schema dump` so the
    /// `schema-drift` CI job covers the contract surface.
    pub output_schema: Option<&'static str>,
}

impl ToolDef {
    /// Convenience for tools that do not advertise a static JSON
    /// schema. Mirrors the 0.8 inventory shape: only `name`,
    /// `description`, `required_args`, `optional_args`.
    pub const fn legacy(
        name: &'static str,
        description: &'static str,
        required_args: &'static [&'static str],
        optional_args: &'static [&'static str],
    ) -> Self {
        ToolDef {
            name,
            description,
            required_args,
            optional_args,
            input_schema: None,
            output_schema: None,
        }
    }
}

impl ToolDef {
    /// Build the `ToolInputSchema` (JSON Schema `properties` +
    /// `required`) for this tool by mapping each key in
    /// `required_args` through `arg_property_schema`.
    pub fn to_input_schema(&self) -> ToolInputSchema {
        let mut props = std::collections::BTreeMap::new();
        for req in self.required_args {
            props.insert((*req).to_string(), tool_arg_property_schema(self.name, req));
        }
        for opt in self.optional_args {
            props.insert((*opt).to_string(), tool_arg_property_schema(self.name, opt));
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
    ToolDef::legacy(
        "list_repos",
        "List every repository currently registered in the federation, with id, path, health, and graph stats.",
        &[],
        &[],
    ),
    ToolDef::legacy(
        "get_repo_info",
        "Get info about a single repository in the federation by id.",
        &["repo_id"],
        &[],
    ),
    ToolDef::legacy(
        "get_federation_health",
        "Aggregate health counts and total node/edge counts across the federation, plus a rough memory estimate.",
        &[],
        &[],
    ),
    ToolDef::legacy(
        "search_org",
        "Case-insensitive substring search across every repo's symbols (matched on name or path). Args: query (substring), limit (max results, parsed as usize). Returns matches sorted by (repo_id, name).",
        &["query", "limit"],
        &[],
    ),
    ToolDef::legacy(
        "get_cross_repo_blast_radius",
        "Use this when you want to know what breaks across repositories: resolve a symbol, traverse INCOMING Calls edges (its callers — \"if I change this, what breaks?\") in [min_depth, max_depth), and group visited nodes by repo. depth is a string range like \"1..3\", not a number. Returns {by_repo: {repo_id: [global_ids...]}, total_count, truncated}. Caps at 1000 nodes; truncated=true when the cap is hit. When the symbol matches several repos, pass repo_id to choose one.",
        &["symbol", "depth"],
        &["repo_id"],
    ),
];

/// Workspace-aware MCP tools, registered when the server was constructed
/// with a `WorkspacesFile` (i.e., when a workspace may be active). These
/// are additive to the federation tools — they don't replace anything.
pub const WORKSPACE_TOOL_DEFS: &[ToolDef] = &[
    ToolDef::legacy(
        "list_workspaces",
        "List all known workspaces from workspaces.yaml. Returns [{name, description?, source?, member_count, is_active}].",
        &[],
        &[],
    ),
    ToolDef::legacy(
        "get_active_workspace",
        "Return the workspace the server is currently holding (the one whose repos were loaded). Errors with NoActiveWorkspace if the server was started without --workspace or no workspace matches the loaded repos.",
        &[],
        &[],
    ),
    ToolDef::legacy(
        "get_workspace",
        "Full detail on one workspace by name: description?, source?, members: [{repo_id, path, health}]. Errors with NotFound if name is unknown.",
        &["name"],
        &[],
    ),
    ToolDef::legacy(
        "get_workspace_graph",
        "Per-workspace graph for the dashboard. Returns {nodes: [...], edges: [...], truncated: bool}. Filters to Function/Method/Class + Calls/Imports. Optional filter: substring match against node name + path. Cross-repo Calls edges are marked cross_repo: true.",
        &[],
        &["filter"],
    ),
];

/// Contract-federation service-view tools (PR 16, `docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md`
/// §10.1 / §12). Dispatched via the `ContractToolEntry` inventory
/// (`mcp/contract_tools/mod.rs`) and advertised only when (a) the
/// server was constructed with a `FederatedIndex` and (b) the
/// `contracts` package is enabled (`LAIN_TOOL_PROFILE=contracts` or
/// `load_package contracts`). Advertising is not dispatch — the
/// tools are callable whenever a federation is configured, package
/// enabled or not (per `§10.1`'s explicit note).
///
/// The static JSON Schemas are loaded with `include_str!` so the
/// `schema-drift` CI job (`tests/schema_dump_smoke.rs`) covers the
/// contract surface.
pub const CONTRACT_TOOL_DEFS: &[ToolDef] = &[
    ToolDef {
        name: "list_services",
        description: "Every service in the federation with its repo, paths, endpoint count and consumer counts. Works on `live` and named snapshots (§12).",
        required_args: &["snapshot"],
        optional_args: &["repo", "cursor", "limit"],
        input_schema: Some(include_str!("contract_tools/schemas/list_services.in.json")),
        output_schema: Some(include_str!("contract_tools/schemas/list_services.out.json")),
    },
    ToolDef {
        name: "get_service",
        description: "Consumers of one service: endpoints, calling code, fields used, and the `used_by` walk that explains why each caller runs (§10.9).",
        required_args: &["snapshot", "service"],
        optional_args: &["depth", "cursor", "limit"],
        input_schema: Some(include_str!("contract_tools/schemas/get_service.in.json")),
        output_schema: Some(include_str!("contract_tools/schemas/get_service.out.json")),
    },
    ToolDef {
        name: "prepare_snapshot",
        description: "Pin every repo to a commit, index once, reuse the result. Same inputs after ref resolution produce the same id (§8.4); `from` derives a snapshot from an existing base (§11).",
        required_args: &[],
        optional_args: &["repos", "exclude", "from", "max_base_age_s", "wait_ms"],
        input_schema: Some(include_str!("contract_tools/schemas/prepare_snapshot.in.json")),
        output_schema: Some(include_str!("contract_tools/schemas/prepare_snapshot.out.json")),
    },
    ToolDef {
        name: "get_snapshot",
        description: "Read a snapshot's current state. The `snapshot` argument is required; same shape as `prepare_snapshot` returns. On `live` returns a readiness-shaped answer (§8.7).",
        required_args: &["snapshot"],
        optional_args: &["wait_ms"],
        input_schema: Some(include_str!("contract_tools/schemas/get_snapshot.in.json")),
        output_schema: Some(include_str!("contract_tools/schemas/get_snapshot.out.json")),
    },
    ToolDef {
        name: "list_contracts",
        description: "Endpoints the federation exposes, filterable by service, repo, and kind (http/topic).",
        required_args: &["snapshot"],
        optional_args: &["service", "repo", "kind", "cursor", "limit"],
        input_schema: Some(include_str!("contract_tools/schemas/list_contracts.in.json")),
        output_schema: Some(include_str!("contract_tools/schemas/list_contracts.out.json")),
    },
    ToolDef {
        name: "get_contract",
        description: "Providers, schema fields, and bound consumers for one endpoint (§12).",
        required_args: &["snapshot", "key"],
        optional_args: &["service"],
        input_schema: Some(include_str!("contract_tools/schemas/get_contract.in.json")),
        output_schema: Some(include_str!("contract_tools/schemas/get_contract.out.json")),
    },
    ToolDef {
        name: "list_unresolved",
        description: "Every ambiguous and unresolved consumer with the candidate endpoints that could match (§9.7).",
        required_args: &["snapshot"],
        optional_args: &["repo", "service", "cursor", "limit"],
        input_schema: Some(include_str!("contract_tools/schemas/list_unresolved.in.json")),
        output_schema: Some(include_str!("contract_tools/schemas/list_unresolved.out.json")),
    },
    ToolDef {
        name: "check_binding",
        description: "Validate a proposed consumer→endpoint link and emit the YAML for `repos.yaml#bindings` (§12). Pure; creates nothing.",
        required_args: &["snapshot", "consumer", "endpoint"],
        optional_args: &[],
        input_schema: Some(include_str!("contract_tools/schemas/check_binding.in.json")),
        output_schema: Some(include_str!("contract_tools/schemas/check_binding.out.json")),
    },
    ToolDef {
        name: "diff_contracts",
        description: "Provider and consumer changes between two snapshots, classified, with impact paths and coverage (§12). `live` is not allowed as base/head.",
        required_args: &["base", "head"],
        optional_args: &["repo", "service", "min_impact", "cap"],
        input_schema: Some(include_str!("contract_tools/schemas/diff_contracts.in.json")),
        output_schema: Some(include_str!("contract_tools/schemas/diff_contracts.out.json")),
    },
    ToolDef {
        name: "trace_impact",
        description: "Impact paths from an endpoint, field, or symbol (§5.2 + §12). Exactly one of endpoint/field/symbol is required.",
        required_args: &["snapshot", "from"],
        optional_args: &["depth", "min_confidence", "cap"],
        input_schema: Some(include_str!("contract_tools/schemas/trace_impact.in.json")),
        output_schema: Some(include_str!("contract_tools/schemas/trace_impact.out.json")),
    },
    ToolDef {
        name: "get_coverage",
        description: "What the view saw and what it could not resolve (§9.7). With an `endpoint`, `complete` reflects the could-match gate.",
        required_args: &["snapshot"],
        optional_args: &["endpoint"],
        input_schema: Some(include_str!("contract_tools/schemas/get_coverage.in.json")),
        output_schema: Some(include_str!("contract_tools/schemas/get_coverage.out.json")),
    },
    ToolDef {
        name: "resolve_evidence",
        description: "Check refs exist at their commit; return context. Bad refs are `exists: false` with a reason, never an error (§12).",
        required_args: &["snapshot", "refs"],
        optional_args: &["context_lines"],
        input_schema: Some(include_str!("contract_tools/schemas/resolve_evidence.in.json")),
        output_schema: Some(include_str!("contract_tools/schemas/resolve_evidence.out.json")),
    },
    ToolDef {
        name: "read_source",
        description: "A bounded line range of a file at a view's commit (§10.7). Refuses secrets and binaries; reads snapshot content from the mirror's git2 blob.",
        required_args: &["snapshot", "repo", "path", "start", "end"],
        optional_args: &[],
        input_schema: Some(include_str!("contract_tools/schemas/read_source.in.json")),
        output_schema: Some(include_str!("contract_tools/schemas/read_source.out.json")),
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
    ToolDef::legacy(
        "get_server_status",
        "Returns the server's run-time status: pid, transport, port, started_at, last_sync_at, last_error, repo_count, workspace_count.",
        &[],
        &[],
    ),
    ToolDef::legacy(
        "list_recent_projects",
        "List projects the operator has used recently, with per-project workspace_count and repo_count pulled from each project's repos.yaml/workspaces.yaml.",
        &[],
        &[],
    ),
    ToolDef::legacy(
        "get_reload_status",
        "Returns the current reload subsystem state: state (idle | rebuilding | failed), started_at, last_reload_at, last_error, pending_changes.",
        &[],
        &[],
    ),
    ToolDef::legacy(
        "request_reload",
        "Schedule a hot-reload of repos.yaml and workspaces.yaml. The actual rebuild runs on a background task; the call returns immediately after queueing the signal.",
        &[],
        &[],
    ),
    ToolDef::legacy(
        "register_agent",
        "Register this agent with lain. Returns an agent_id and session_token to use on every subsequent call. `kind` and `mode` are optional but are stored and reported to other agents by `list_active_agents`, so passing them makes this agent legible to its peers.",
        &["name"],
        // These were accepted and stored but never declared, so an agent
        // that followed the schema silently lost its own type and mode
        // metadata — with nothing in any response to indicate a field
        // had been dropped. Same class as the undeclared `path` on
        // `list_occupancy`.
        &["kind", "mode"],
    ),
    ToolDef::legacy(
        "heartbeat",
        "Refresh the session. lain expires sessions 60 seconds after the last heartbeat.",
        &["agent_id", "session_token"],
        &[],
    ),
    ToolDef::legacy(
        "list_active_agents",
        "List agents currently connected. Set include_background=true to also see cron/CI agents.",
        &[],
        &["include_background"],
    ),
    ToolDef::legacy(
        "who_am_i",
        "Resolve a session_token to the agent it belongs to, plus their current claims.",
        &["session_token"],
        &[],
    ),
    ToolDef::legacy(
        "list_subagents",
        "List active subagents whose parent_session_id matches the caller's session. Args: session_token. Returns: { parent, subagents: [{ agent_id, name, kind, mode, started_at_unix, last_heartbeat_unix }] }",
        &["session_token"],
        &[],
    ),
    ToolDef::legacy(
        "claim_files",
        "Announce intent to edit (or read) files/symbols. Returns `granted`, `conflicts` (other agents already holding an edit claim — your claim was refused), and `advisories` (your claim WAS granted, but another agent holds an edit claim on that file; a read is never blocked, so re-read before you patch).",
        &["agent_id", "session_token", "files"],
        &[],
    ),
    ToolDef::legacy(
        "release_files",
        "Release claims. Other agents get a notification.",
        &["agent_id", "session_token", "files"],
        &[],
    ),
    ToolDef::legacy(
        "list_occupancy",
        "Show which agents are in a file or the whole workspace. Pass `path` to scope to one file, or omit it for everything. Each entry carries `holders`: [{agent_id, name, intent, inferred}] — `edit` blocks other edits, `read` never blocks.",
        &[],
        &["path"],
    ),
    ToolDef::legacy(
        "my_claims",
        "List files this agent has claimed.",
        &["agent_id", "session_token"],
        &[],
    ),
    ToolDef::legacy(
        "detect_overlap",
        "Detect symbol-level overlap between two git refs in a workspace. Args: base (required), head (defaults to HEAD), workspace (required). Returns { base, head, total_overlaps, files: [{ repo, path, symbols_base, symbols_head, overlap, severity }] } — a non-empty overlap means both refs edited the same definition. `severity` is none | low | medium | high, graded by the kinds of the shared symbols (a shared function weighs more than a shared module).",
        &["base", "workspace"],
        &["head"],
    ),
    ToolDef::legacy(
        "get_audit_log",
        "Read the server's audit log (per-write events appended by the claim_files handler when a claim is granted). Args: since_unix (drop events whose ts_unix is strictly less than this), path_glob (filter to events whose path matches this glob — see src/server/glob_match.rs for the supported subset), limit (the most recent N matching events, default 200). Returns an array of AuditEvent objects, oldest first (ts_unix, agent_id, path, claim_set, racers, plan_revision, landed_revision). The on-disk file is `<state_dir>/audit.jsonl`, rotation-capped at 50 MB.",
        &[],
        &["since_unix", "path_glob", "limit"],
    ),
    ToolDef::legacy(
        "get_world_state",
        "Read-only companion to claim_files: returns the same WorldState shape (retracted symbols, overlay-delta changed_symbols filtered to the requested set, and a resync note for BeyondCurrent/TooOld) without taking a claim. Lets an LLM ask 'is this symbol still in the graph?' or 'what's the world state for these symbols?' before deciding whether to claim. Args: symbols (list of symbol names to check for retract — empty list yields a no-op WorldState), plan_revision (the agent's last-seen overlay revision; omit for current). Returns { current, plan, changed_symbols: [{ name, change_kind, at_revision }], note } where change_kind is Edited | Retracted and note is set only on BeyondCurrent ('plan_revision beyond current — server may have restarted') or TooOld ('plan_revision too old for delta; resync required').",
        &[],
        &[],
    ),
    ToolDef::legacy(
        "get_recent_activity",
        "Compact digest of the audit log: groups recent edit_landed events by path (default), agent, or hour and returns a count + sample per group. Designed for LLM session compaction — instead of re-reading every audit.jsonl line, the agent gets a navigable summary and can call get_audit_log with a specific path_glob for full detail. Args: since_unix (filter by ts_unix), group_by ('path' (default) | 'agent' | 'hour'), path_glob (pre-filter by path before grouping), limit (max groups returned, default 20). Returns { groups: [{ key, count, first_ts, last_ts, sample_event }], total_events, total_groups, truncated, group_by }. truncated=true when total_groups > limit.",
        &[],
        &["since_unix", "group_by", "path_glob", "limit"],
    ),
    ToolDef::legacy(
        "add_annotation",
        "Add an annotation (note | warning | todo | investigation | fix) targeting a symbol, file, repo, or edge. Body is 1..=4096 bytes. Returns { id, created_at_unix_ms, repo_id }. Author defaults to the authenticated session_token if `author` is omitted.",
        &["target", "kind", "body"],
        &["author", "session_token", "refs"],
    ),
    ToolDef::legacy(
        "list_annotations",
        "List annotations with optional filters: target (symbol | file | repo | edge), author, kind, status, limit (default 100). Live-staleness pass marks targets that no longer exist in the graph as status='stale'. Returns { annotations: [Annotation] }.",
        &[],
        &["target", "author", "kind", "status", "limit"],
    ),
    ToolDef::legacy(
        "resolve_annotation",
        "Mark an open annotation as resolved by the calling agent (or by `resolved_by`). Returns { resolved: Annotation }. Annotations already resolved return an error so a typo'd id is loud. `session_token` is optional — omit unless you have one from register_agent.",
        &["id"],
        &["session_token", "resolved_by"],
    ),
    ToolDef::legacy(
        "leave_handoff_note",
        "Leave a workspace-scoped note for the next agent that registers. Stored as an open Note annotation; expires after 24h. Body may carry a `[scope:<s>]` prefix for filtering on the read side. Returns { id, expires_at_unix_ms }. Single-repo mode only for now. `session_token` is optional — omit unless you have one from register_agent.",
        &["body"],
        &["scope", "refs", "author", "session_token"],
    ),
    ToolDef::legacy(
        "get_pending_handoffs",
        "List open handoff notes from prior agents. Filters: scope (workspace | repo:<id> | agent_kind:<k>), since_unix_ms. Returns { handoffs: [Annotation] } — only kind=note, status=open rows within the 24h TTL.",
        &[],
        &["scope", "since_unix_ms"],
    ),
    // Intent layer (PR 1 of `docs/INTENT_AND_OBSERVABILITY_PLAN.md`).
    // Listed here (in addition to the inventory-registered entries
    // in `handler.rs`) so the multiplayer tools are advertised in
    // `tools/list` regardless of whether the inventory crate
    // collected the static-linker section in a given build.
    ToolDef::legacy(
        "unregister_agent",
        "Tear down an agent session: releases every claim, drops intent + activity entries, removes the presence session.",
        &["agent_id", "session_token"],
        &[],
    ),
    ToolDef::legacy(
        "lain_intent",
        "Declare or update an intent (goal + scopes + status). The response carries the live coordination level (GREEN / YELLOW / RED) and a `coordination` block with related activity. Returns {intent_id, revision, coordination, intent}.",
        &["agent_id", "session_token"],
        &["goal", "scopes", "status", "add_scopes", "remove_scopes", "intent_id"],
    ),
    ToolDef::legacy(
        "list_active_intents",
        "Per-agent activity feed: for each connected agent, returns intent {goal, scopes, status}, focus (most-recent observation target), observed_reads (deduped), and last_tool {tool, target, at_unix}. Optional `agent_id` filters to one agent.",
        &[],
        &["agent_id"],
    ),
];

/// Map a `&[ToolDef]` to the Vec<Tool> shape the MCP `tools/list`
/// response expects. The stdio path calls this; the HTTP path uses
/// [`defs_to_value_tools`] because its response is a `serde_json::Value`
/// (not a `rust_mcp_schema::Tool`).
///
/// `output_schema` on `ToolDef` is parsed once and surfaced on `Tool`.
/// When `input_schema` is set on `ToolDef` it overrides the auto-built
/// one (PR 16 contract tools ship a static `input_schema` JSON).
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
            output_schema: d.output_schema.and_then(|s| serde_json::from_str(s).ok()),
            title: None,
        })
        .collect()
}

/// Map a `&[ToolDef]` to the `serde_json::Value` shape the HTTP
/// JSON-RPC `tools/list` arm of `handle_request` uses. Mirrors
/// [`defs_to_tools`] so the two transports don't diverge on tool
/// shape, descriptions, or argument schemas.
///
/// When `input_schema` is `Some(s)` on a `ToolDef`, `s` overrides the
/// auto-built one. `output_schema` is included under the `outputSchema`
/// key when present (PR 16 contract tools).
pub fn defs_to_value_tools(defs: &[ToolDef]) -> Vec<serde_json::Value> {
    defs.iter()
        .map(|d| {
            let input_schema = if let Some(raw) = d.input_schema {
                serde_json::from_str(raw).unwrap_or_else(|e| {
                    panic!("invalid JSON Schema in input_schema for {}: {e}", d.name)
                })
            } else {
                let mut props = serde_json::Map::new();
                for req in d.required_args {
                    props.insert(
                        (*req).to_string(),
                        serde_json::Value::Object(tool_arg_property_schema(d.name, req)),
                    );
                }
                for opt in d.optional_args {
                    props.insert(
                        (*opt).to_string(),
                        serde_json::Value::Object(tool_arg_property_schema(d.name, opt)),
                    );
                }
                serde_json::json!({
                    "type": "object",
                    "properties": props,
                    "required": d.required_args,
                })
            };
            let mut obj = serde_json::json!({
                "name": d.name,
                "description": d.description,
                "inputSchema": input_schema,
            });
            if let Some(raw) = d.output_schema {
                let parsed: serde_json::Value = serde_json::from_str(raw).unwrap_or_else(|e| {
                    panic!("invalid JSON Schema in output_schema for {}: {e}", d.name)
                });
                obj["outputSchema"] = parsed;
            }
            obj
        })
        .collect()
}

/// Build the on-disk schema dump — the JSON array `lain schema dump`
/// writes to `docs/tool-schema.json`. The shape must match the
/// HTTP `tools/list` arm in `handler.rs:1824-1856` byte-for-byte
/// (see the drift-detection test in `tests/schema_dump_smoke.rs`).
///
/// Six sources, in the order `tools/list` appends them:
///   1. `ToolRegistry::definitions()` — `inventory`-discovered tools.
///   2. `special_tool_definitions()` — tools that bypass
///      `ToolHandler` (get_health, get_agent_strategy, etc.).
///   3. `FEDERATION_TOOL_DEFS` — federation-mode MCP tools (only
///      included here so the doc surface is the *maximum* an agent
///      could see).
///   4. `WORKSPACE_TOOL_DEFS` — workspace-aware MCP tools.
///   5. `CONTRACT_TOOL_DEFS` — contract-federation service view (PR 16).
///   6. `SERVER_TOOL_DEFS` — always-on server-status tools.
///
/// The HTTP `tools/list` arm conditionalizes (3), (4), (5) on whether
/// the server was constructed with a federation, workspaces, or
/// contracts; the dump always emits all six. The drift-detection test
/// in Task 6 boots the server with `--workspace auto` so the live
/// response includes all six, and the on-disk artifact then byte-matches.
///
/// `inert` is the same list `tools/list` filters with
/// (`inert_tool_names(&embedder)`). Apply it uniformly so a doc
/// produced against a stub embedder and a live `tools/list` from
/// the same stub embedder byte-match.
pub fn dump_tools_schema(inert: &[&str]) -> Vec<serde_json::Value> {
    let not_inert = |name: &str| !inert.contains(&name);
    let mut tools: Vec<serde_json::Value> =
        crate::server::tools::registry::ToolRegistry::definitions()
            .iter()
            .filter(|def| not_inert(def.name))
            .map(|def| {
                serde_json::json!({
                    "name": def.name,
                    "description": def.description,
                    "inputSchema": def.input_schema,
                })
            })
            .collect();
    for def in crate::server::mcp::handler::special_tool_definitions() {
        if !not_inert(def.name) {
            continue;
        }
        tools.push(serde_json::json!({
            "name": def.name,
            "description": def.description,
            "inputSchema": def.input_schema,
        }));
    }
    let drop_inert = |t: &serde_json::Value| -> bool {
        t.get("name")
            .and_then(|v| v.as_str())
            .is_none_or(&not_inert)
    };
    tools.extend(
        defs_to_value_tools(FEDERATION_TOOL_DEFS)
            .into_iter()
            .filter(|t| drop_inert(t)),
    );
    tools.extend(
        defs_to_value_tools(WORKSPACE_TOOL_DEFS)
            .into_iter()
            .filter(|t| drop_inert(t)),
    );
    tools.extend(
        defs_to_value_tools(CONTRACT_TOOL_DEFS)
            .into_iter()
            .filter(|t| drop_inert(t)),
    );
    tools.extend(
        defs_to_value_tools(SERVER_TOOL_DEFS)
            .into_iter()
            .filter(|t| drop_inert(t)),
    );
    tools
}

#[cfg(test)]
mod dump_tools_schema_tests {
    use super::*;

    #[test]
    fn every_advertised_tool_has_an_explicit_readiness_classification() {
        use crate::server::tools::definitions::readiness_requirement;
        for tool in dump_tools_schema(&[]) {
            let name = tool["name"].as_str().expect("tool name");
            assert!(
                readiness_requirement(name).is_some(),
                "unclassified MCP tool: {name}"
            );
        }
        assert!(readiness_requirement("future_unreviewed_tool").is_none());

        for definition in crate::server::tools::registry::ToolRegistry::definitions()
            .into_iter()
            .chain(crate::server::mcp::handler::special_tool_definitions())
        {
            assert_eq!(
                Some(definition.readiness),
                readiness_requirement(definition.name)
            );
        }
    }

    /// The dump must contain every subset the HTTP `tools/list` arm
    /// appends. The integration test in `tests/schema_dump_smoke.rs`
    /// pins the wire shape; this unit test pins the function-level
    /// invariant that the function does not silently drop a subset.
    #[test]
    fn dump_contains_all_five_subsets() {
        // Pass `&[]` (no inert tools filtered) so the unit test
        // exercises the full surface.
        let tools = dump_tools_schema(&[]);
        let names: std::collections::HashSet<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();

        // (1) ToolRegistry (inventory).
        assert!(
            names.contains("query_graph"),
            "ToolRegistry subset missing `query_graph`: {names:?}"
        );
        // (2) special_tool_definitions.
        assert!(
            names.contains("get_health"),
            "special subset missing `get_health`: {names:?}"
        );
        // (3) FEDERATION_TOOL_DEFS.
        assert!(
            names.contains("list_repos"),
            "federation subset missing `list_repos`: {names:?}"
        );
        // (4) WORKSPACE_TOOL_DEFS.
        assert!(
            names.contains("list_workspaces"),
            "workspace subset missing `list_workspaces`: {names:?}"
        );
        // (5) SERVER_TOOL_DEFS.
        assert!(
            names.contains("get_server_status"),
            "server subset missing `get_server_status`: {names:?}"
        );
    }

    /// Per-tool shape must be `{name, description, inputSchema}` —
    /// the same camelCase keys the HTTP `tools/list` arm emits.
    #[test]
    fn dump_per_tool_shape_matches_wire() {
        let tools = dump_tools_schema(&[]);
        for t in &tools {
            assert!(
                t.get("name").and_then(|n| n.as_str()).is_some(),
                "tool missing `name`: {t}"
            );
            assert!(
                t.get("description").is_some(),
                "tool missing `description`: {t}"
            );
            assert!(
                t.get("inputSchema").is_some(),
                "tool missing `inputSchema`: {t}"
            );
        }
    }

    /// `inert` filtering must be applied across all five subsets.
    /// Drop `semantic_search` from the registry subset and
    /// `list_repos` from the federation subset; both should be
    /// absent from the result, and every other tool should remain.
    #[test]
    fn inert_filter_applies_across_all_subsets() {
        let inert: &[&str] = &["semantic_search", "list_repos"];
        let tools = dump_tools_schema(inert);
        let names: std::collections::HashSet<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(
            !names.contains("semantic_search"),
            "registry inert filter did not drop `semantic_search`: {names:?}"
        );
        assert!(
            !names.contains("list_repos"),
            "federation inert filter did not drop `list_repos`: {names:?}"
        );
        // The remaining subsets must still be present.
        assert!(
            names.contains("get_health"),
            "non-inert tools dropped: {names:?}"
        );
        assert!(
            names.contains("list_workspaces"),
            "non-inert tools dropped: {names:?}"
        );
        assert!(
            names.contains("get_server_status"),
            "non-inert tools dropped: {names:?}"
        );
    }
}
