//! Capability packages — the skill layer over the tool surface.
//!
//! Every advertised tool has exactly one row here: which package it
//! belongs to, how heavy it is, what it does, why it is (or isn't) in
//! the default core, and when to reach for it. The package cards
//! rendered by `get_agent_strategy` / `list_packages` and the
//! generated section in `docs/quickstart-tools.md` all read this one
//! table, so the documentation cannot drift from the surface: a pin
//! test compares the registry against the schema dump.
//!
//! Levels:
//! - `Core`      — read-only, cheap, universally useful. Default.
//! - `Power`     — composes core ideas for a specific job; still safe.
//! - `Advanced`  — expensive, low-level, or answers questions the
//!   core tools already answer more cheaply.
//! - `Plumbing`  — setup, coordination, server mechanics. Owned by
//!   hooks or operators in most sessions.

/// The skill packages. `Core` is always advertised; the rest are
/// opt-in per session (`load_package`) or per process
/// (`LAIN_TOOL_PROFILE=<name,...>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Package {
    Core,
    Federation,
    Workspace,
    Architecture,
    Primitives,
    Verify,
    Session,
    Social,
    Notes,
    Ops,
    /// Contract federation service view (`docs/CONTRACT_FEDERATION.md` PR 16).
    /// Opt-in via `LAIN_TOOL_PROFILE=contracts` (combinable) or `load_package contracts`.
    /// The default profile is unchanged and stays at 18 tools or fewer; the
    /// two contract tools (`list_services`, `get_service`) live behind this
    /// package so they are not advertised unless explicitly requested.
    Contracts,
}

impl Package {
    pub const ALL: &'static [Package] = &[
        Package::Core,
        Package::Federation,
        Package::Workspace,
        Package::Architecture,
        Package::Primitives,
        Package::Verify,
        Package::Session,
        Package::Social,
        Package::Notes,
        Package::Ops,
        Package::Contracts,
    ];

    /// Name used in `LAIN_TOOL_PROFILE` and `load_package`.
    pub fn name(self) -> &'static str {
        match self {
            Package::Core => "core",
            Package::Federation => "federation",
            Package::Workspace => "workspace",
            Package::Architecture => "arch",
            Package::Primitives => "raw",
            Package::Verify => "verify",
            Package::Session => "session",
            Package::Social => "social",
            Package::Notes => "notes",
            Package::Ops => "ops",
            Package::Contracts => "contracts",
        }
    }

    /// One-line pitch: what this package is a skill *for*.
    pub fn pitch(self) -> &'static str {
        match self {
            Package::Core => "Orient, understand, and assess impact before touching code",
            Package::Federation => "Org-wide questions across multiple repositories",
            Package::Workspace => "Workspace-grouped views when repos are managed as sets",
            Package::Architecture => {
                "Map the system: layered views, dependency traces, module comparison"
            }
            Package::Primitives => "The raw layer: direct graph queries, snippets, low-level reads",
            Package::Verify => "Ship-it checks: build, test, lint, coverage, and git state",
            Package::Session => "Multiplayer claiming: register, claim files, heartbeat, occupancy",
            Package::Social => "Who else is here: agent roster, overlap, audit trail",
            Package::Notes => "Team memory: annotations, handoff notes, intents",
            Package::Ops => "Server health and setup: reload, status, LSP install, re-enrichment",
            Package::Contracts => {
                "Service view across the federation: who provides what, who consumes it and why"
            }
        }
    }

    /// Why this package is not part of the default surface.
    pub fn why_off_by_default(self) -> &'static str {
        match self {
            Package::Core => "",
            Package::Federation => {
                "only meaningful when the server runs a federation; shown automatically then"
            }
            Package::Workspace => {
                "needs a workspaces.yaml; shown automatically when one is configured"
            }
            Package::Architecture => {
                "heavy graph walks — core answers most shape questions more cheaply"
            }
            Package::Primitives => {
                "building blocks the core tools already compose; `query_graph` needs schema literacy"
            }
            Package::Verify => {
                "executes real builds and tests — slow and side-effecting, never what a read-only session wants"
            }
            Package::Session => {
                "hook scripts drive claims in most setups (advertising is not dispatch — they work either way)"
            }
            Package::Social => "coordination awareness, not code work",
            Package::Notes => "team memory layer — noise for a solo session",
            Package::Ops => "server mechanics and one-time setup chores",
            Package::Contracts => {
                "only meaningful when a federation is configured; opt in for cross-repo service questions"
            },
        }
    }

    pub fn level(self) -> Level {
        match self {
            Package::Core => Level::Core,
            Package::Federation | Package::Workspace | Package::Verify | Package::Contracts => {
                Level::Power
            }
            Package::Architecture | Package::Primitives => Level::Advanced,
            Package::Session | Package::Social | Package::Notes | Package::Ops => Level::Plumbing,
        }
    }

    pub fn parse(name: &str) -> Option<Package> {
        Package::ALL
            .iter()
            .copied()
            .find(|p| p.name() == name.trim().to_ascii_lowercase())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Core,
    Power,
    Advanced,
    Plumbing,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Level::Core => "core",
            Level::Power => "power",
            Level::Advanced => "advanced",
            Level::Plumbing => "plumbing",
        }
    }
}

/// One capability: everything an agent needs to decide whether to
/// reach for this tool, and everything an operator needs to decide
/// whether to enable its package.
pub struct Capability {
    pub tool: &'static str,
    pub package: Package,
    pub level: Level,
    /// What the tool does, in one sentence, from the caller's view.
    pub what: &'static str,
    /// When to reach for it — the trigger, phrased as an intent.
    pub when: &'static str,
}

const fn c(
    tool: &'static str,
    package: Package,
    level: Level,
    what: &'static str,
    when: &'static str,
) -> Capability {
    Capability {
        tool,
        package,
        level,
        what,
        when,
    }
}

/// Every tool on the surface, exactly once. Pinned against the
/// schema dump by `registry_matches_schema_dump`.
pub const CAPABILITIES: &[Capability] = &[
    // ── Core ───────────────────────────────────────────────────────
    c(
        "list_packages",
        Package::Core,
        Level::Core,
        "the skill menu: every package with its tools and when to use it",
        "the default surface does not cover the task at hand",
    ),
    c(
        "load_package",
        Package::Core,
        Level::Core,
        "opt a package into this session's tools/list (then refetch it)",
        "you know which skill you need (verify, arch, notes, ...)",
    ),
    c(
        "understand_repository",
        Package::Core,
        Level::Core,
        "one-call bootstrap: identity, top anchors, entry points, capability states",
        "you just connected and have not explored yet",
    ),
    c(
        "find_symbol",
        Package::Core,
        Level::Core,
        "every graph node matching a name, narrowed by path/type",
        "you know the symbol's name and want where it lives",
    ),
    c(
        "get_context",
        Package::Core,
        Level::Core,
        "one-call dossier: definition, callers, callees, source excerpt",
        "you need to understand one symbol well enough to quote it",
    ),
    c(
        "search_code",
        Package::Core,
        Level::Core,
        "find code by name, intent, or pattern (lexical or semantic)",
        "you know roughly what you are looking for, not its exact name",
    ),
    c(
        "explain_dispatch",
        Package::Core,
        Level::Core,
        "synthesises every caller signal and returns one honest verdict",
        "an empty blast radius looks wrong and you need to know why",
    ),
    c(
        "assess_change",
        Package::Core,
        Level::Core,
        "pre-edit impact: dependents, untested dependents, risk verdict",
        "about to edit a symbol and want the blast radius in risk form",
    ),
    c(
        "get_blast_radius",
        Package::Core,
        Level::Core,
        "who breaks if this symbol changes: direct and transitive dependents",
        "you want the concrete list of affected code",
    ),
    c(
        "get_call_chain",
        Package::Core,
        Level::Core,
        "the exact call path between two symbols",
        "you need how A reaches B, not just that it does",
    ),
    c(
        "find_related",
        Package::Core,
        Level::Core,
        "graph neighbours, co-change partners, semantic neighbours",
        "asking what is connected to X before committing to a change",
    ),
    c(
        "find_anchors",
        Package::Core,
        Level::Core,
        "the most foundational, stable components by anchor score",
        "asking what to read first in an unfamiliar codebase",
    ),
    c(
        "list_entry_points",
        Package::Core,
        Level::Core,
        "where execution starts: mains, routes, top-level handlers",
        "asking where a program or request begins",
    ),
    c(
        "get_coupling_radar",
        Package::Core,
        Level::Core,
        "hidden coupling between files from git co-change history",
        "planning a refactor and want what tends to change together",
    ),
    c(
        "find_dead_code",
        Package::Core,
        Level::Core,
        "nodes with zero incoming callers, tests excluded",
        "cleaning up and want what is actually unused",
    ),
    c(
        "get_health",
        Package::Core,
        Level::Core,
        "operational status: repos, nodes, edges, readiness",
        "checking the server is alive and the graph is fresh",
    ),
    c(
        "get_capabilities",
        Package::Core,
        Level::Core,
        "readiness snapshot per capability plus the active tool profile",
        "checking what can answer right now and which profile is active",
    ),
    c(
        "get_agent_strategy",
        Package::Core,
        Level::Core,
        "the operating manual: intent-to-tool map and package cards",
        "unsure which tool fits, or how to get more of them",
    ),
    // ── Federation ─────────────────────────────────────────────────
    c(
        "search_org",
        Package::Federation,
        Level::Power,
        "symbol and path search across every repository in the federation",
        "asking which repos know about X",
    ),
    c(
        "get_cross_repo_blast_radius",
        Package::Federation,
        Level::Power,
        "what breaks across repos if a symbol changes, grouped by repo",
        "the change crosses a repository boundary (pin repo_id when names collide)",
    ),
    c(
        "list_repos",
        Package::Federation,
        Level::Power,
        "the federation's repositories and their sources",
        "asking what repos this server indexes",
    ),
    c(
        "get_repo_info",
        Package::Federation,
        Level::Power,
        "one repository's identity, source, and index state",
        "asking about a specific repo's health or origin",
    ),
    c(
        "get_federation_health",
        Package::Federation,
        Level::Power,
        "federation-wide health: load errors, per-repo readiness",
        "the federation looks incomplete or slow",
    ),
    // ── Workspace ──────────────────────────────────────────────────
    c(
        "get_workspace_graph",
        Package::Workspace,
        Level::Power,
        "nodes and edges of one configured workspace group",
        "working against a named set of repos and want the combined view",
    ),
    c(
        "list_workspaces",
        Package::Workspace,
        Level::Power,
        "configured workspace groups and their members",
        "asking what workspace groups exist",
    ),
    c(
        "get_active_workspace",
        Package::Workspace,
        Level::Power,
        "which workspace group is currently active",
        "before scoped calls that depend on the active group",
    ),
    c(
        "get_workspace",
        Package::Workspace,
        Level::Power,
        "one workspace group's membership and description",
        "asking what belongs to a group",
    ),
    // ── Architecture ───────────────────────────────────────────────
    c(
        "explore_architecture",
        Package::Architecture,
        Level::Advanced,
        "a high-level tree of files and modules to a chosen depth",
        "mapping the shape of a subsystem",
    ),
    c(
        "compare_modules",
        Package::Architecture,
        Level::Advanced,
        "stability and coupling metrics for two modules side by side",
        "deciding which of two modules to refactor first",
    ),
    c(
        "architectural_observations",
        Package::Architecture,
        Level::Advanced,
        "patterns, boundary violations, and high-fan-out hot spots",
        "reviewing architecture rather than a single change",
    ),
    c(
        "trace_dependency",
        Package::Architecture,
        Level::Advanced,
        "everything a symbol depends on, recursively",
        "the question is upstream (what it needs), not downstream",
    ),
    c(
        "get_layered_map",
        Package::Architecture,
        Level::Advanced,
        "a slice of the architecture at one depth from an entry point",
        "visualising layers under a specific entry point",
    ),
    c(
        "get_master_map",
        Package::Architecture,
        Level::Advanced,
        "staleness report: when each module last synced from LSP/git",
        "wondering which parts of the graph are trustworthy",
    ),
    c(
        "navigate_to_anchor",
        Package::Architecture,
        Level::Advanced,
        "the controlling anchor node for a leaf function",
        "climbing from a leaf to the thing that owns it",
    ),
    c(
        "get_anchor_score",
        Package::Architecture,
        Level::Advanced,
        "architectural stability score for one symbol",
        "quantifying how load-bearing a symbol is",
    ),
    c(
        "get_context_depth",
        Package::Architecture,
        Level::Advanced,
        "abstraction layers between an entry point and a symbol",
        "asking how deep in the stack a symbol sits",
    ),
    c(
        "suggest_refactor_targets",
        Package::Architecture,
        Level::Advanced,
        "god objects and high-debt targets from complexity and stability",
        "looking for where refactoring pays off",
    ),
    // ── Primitives ─────────────────────────────────────────────────
    c(
        "query_graph",
        Package::Primitives,
        Level::Advanced,
        "run a JSON ops-array query directly against the graph",
        "no high-level tool answers it and you know the schema",
    ),
    c(
        "describe_schema",
        Package::Primitives,
        Level::Advanced,
        "node types, edge types, and example queries",
        "before writing query_graph ops",
    ),
    c(
        "explain_symbol",
        Package::Primitives,
        Level::Advanced,
        "signature, docstring, and metrics combined into one summary",
        "wanting a description rather than a graph answer",
    ),
    c(
        "get_context_for_prompt",
        Package::Primitives,
        Level::Advanced,
        "LLM-optimised context block for a symbol",
        "assembling a prompt about one symbol",
    ),
    c(
        "get_code_snippet",
        Package::Primitives,
        Level::Advanced,
        "file text with surrounding context around a line",
        "you have a location and want the source, not analysis",
    ),
    c(
        "get_call_sites",
        Package::Primitives,
        Level::Advanced,
        "every caller of a symbol with the line each call sits on",
        "the raw call-site list, not a summary",
    ),
    c(
        "get_cross_runtime_callers",
        Package::Primitives,
        Level::Advanced,
        "protocol-level callers: HTTP routes, RPC methods, resolvers",
        "asking who calls this across an API boundary",
    ),
    c(
        "semantic_search",
        Package::Primitives,
        Level::Advanced,
        "concept search over local embeddings (requires a loaded model)",
        "lexical search misses and a model is installed",
    ),
    // ── Verify ─────────────────────────────────────────────────────
    c(
        "run_build",
        Package::Verify,
        Level::Power,
        "compile the workspace and return output and status",
        "verifying the tree still builds after a change",
    ),
    c(
        "run_tests",
        Package::Verify,
        Level::Power,
        "run the test suite, optionally filtered",
        "verifying behaviour after a change",
    ),
    c(
        "run_clippy",
        Package::Verify,
        Level::Power,
        "lint the workspace, optionally auto-fixing",
        "checking style and common mistakes before committing",
    ),
    c(
        "find_untested_functions",
        Package::Verify,
        Level::Power,
        "functions with no test coverage signal in the call graph",
        "deciding what to test next",
    ),
    c(
        "get_coverage_summary",
        Package::Verify,
        Level::Power,
        "structural coverage estimate from call-graph connectivity",
        "a fast sense of coverage without running a coverage tool",
    ),
    c(
        "get_test_template",
        Package::Verify,
        Level::Power,
        "a test scaffold for a function or type",
        "starting a test and wanting the boilerplate",
    ),
    c(
        "get_file_diff",
        Package::Verify,
        Level::Power,
        "uncommitted changes, staged and unstaged",
        "reviewing what is about to be committed",
    ),
    c(
        "get_commit_history",
        Package::Verify,
        Level::Power,
        "recent commits with authors and messages",
        "asking who changed this and why, recently",
    ),
    c(
        "get_branch_status",
        Package::Verify,
        Level::Power,
        "current branch and working-tree status",
        "before starting work on a dirty tree",
    ),
    // ── Session (multiplayer plumbing) ─────────────────────────────
    c(
        "register_agent",
        Package::Session,
        Level::Plumbing,
        "join the session as a named agent",
        "starting a coordinated session without hook support",
    ),
    c(
        "heartbeat",
        Package::Session,
        Level::Plumbing,
        "renew this agent's liveness",
        "keeping presence alive in a long session",
    ),
    c(
        "claim_files",
        Package::Session,
        Level::Plumbing,
        "claim files for editing with intent and TTL",
        "about to edit files in a multi-agent session",
    ),
    c(
        "release_files",
        Package::Session,
        Level::Plumbing,
        "release claims when edits are done",
        "finished with claimed files",
    ),
    c(
        "list_occupancy",
        Package::Session,
        Level::Plumbing,
        "who currently holds which files",
        "checking for collisions before editing",
    ),
    c(
        "get_world_state",
        Package::Session,
        Level::Plumbing,
        "one snapshot: agents, claims, occupancy",
        "orienting in a multi-agent session",
    ),
    c(
        "my_claims",
        Package::Session,
        Level::Plumbing,
        "the claims this agent currently holds",
        "resuming work and unsure what is still held",
    ),
    // ── Social ─────────────────────────────────────────────────────
    c(
        "who_am_i",
        Package::Social,
        Level::Plumbing,
        "this agent's identity and registration state",
        "unsure which identity the server sees",
    ),
    c(
        "list_active_agents",
        Package::Social,
        Level::Plumbing,
        "the agent roster with liveness",
        "asking who else is working here",
    ),
    c(
        "list_subagents",
        Package::Social,
        Level::Plumbing,
        "subagents spawned by this session",
        "tracking delegated workers",
    ),
    c(
        "unregister_agent",
        Package::Social,
        Level::Plumbing,
        "leave the session cleanly",
        "ending a coordinated session",
    ),
    c(
        "detect_overlap",
        Package::Social,
        Level::Plumbing,
        "who else touches the files or symbols you are about to",
        "checking for concurrent work before starting",
    ),
    c(
        "get_audit_log",
        Package::Social,
        Level::Plumbing,
        "recent coordination events for this workspace",
        "reviewing what happened while you were away",
    ),
    // ── Notes ──────────────────────────────────────────────────────
    c(
        "add_annotation",
        Package::Notes,
        Level::Plumbing,
        "attach a note to a file, symbol, or commit",
        "leaving knowledge behind for the next agent",
    ),
    c(
        "list_annotations",
        Package::Notes,
        Level::Plumbing,
        "annotations, optionally only open ones",
        "checking for notes left on the code you are in",
    ),
    c(
        "resolve_annotation",
        Package::Notes,
        Level::Plumbing,
        "mark an annotation handled",
        "acting on a note and closing it",
    ),
    c(
        "leave_handoff_note",
        Package::Notes,
        Level::Plumbing,
        "write a handoff note for the next agent",
        "ending work someone else will continue",
    ),
    c(
        "get_pending_handoffs",
        Package::Notes,
        Level::Plumbing,
        "handoff notes waiting for an agent",
        "starting work and checking for briefs",
    ),
    c(
        "lain_intent",
        Package::Notes,
        Level::Plumbing,
        "declare what you intend to work on",
        "signalling your plan so others can route around it",
    ),
    c(
        "list_active_intents",
        Package::Notes,
        Level::Plumbing,
        "what other agents have said they will do",
        "checking the plan board before starting",
    ),
    c(
        "get_recent_activity",
        Package::Notes,
        Level::Plumbing,
        "the recent activity feed for this workspace",
        "catching up on what just happened",
    ),
    // ── Ops ────────────────────────────────────────────────────────
    c(
        "get_server_status",
        Package::Ops,
        Level::Plumbing,
        "transport, uptime, and configuration summary",
        "diagnosing the server itself",
    ),
    c(
        "get_reload_status",
        Package::Ops,
        Level::Plumbing,
        "hot-reload state and the last reload outcome",
        "a config change does not seem to have applied",
    ),
    c(
        "request_reload",
        Package::Ops,
        Level::Plumbing,
        "trigger a configuration hot reload",
        "changed repos.yaml and want it picked up",
    ),
    c(
        "list_recent_projects",
        Package::Ops,
        Level::Plumbing,
        "recently used projects on this machine",
        "resuming work across projects",
    ),
    c(
        "get_job_status",
        Package::Ops,
        Level::Plumbing,
        "status of a registered long-running job",
        "checking on background work",
    ),
    c(
        "register_job_webhook",
        Package::Ops,
        Level::Plumbing,
        "be notified when a background job finishes",
        "wanting a callback instead of polling",
    ),
    c(
        "debug_sleep",
        Package::Ops,
        Level::Plumbing,
        "sleep N seconds (test harness helper)",
        "testing timeout behaviour — never in real work",
    ),
    c(
        "install_language_server",
        Package::Ops,
        Level::Plumbing,
        "download and install an optional language server",
        "setting up richer indexing on this machine",
    ),
    c(
        "run_enrichment",
        Package::Ops,
        Level::Plumbing,
        "force a full architectural enrichment pass",
        "graph metadata looks stale",
    ),
    c(
        "sync_state",
        Package::Ops,
        Level::Plumbing,
        "re-sync the graph with the current git HEAD",
        "the graph and the checkout disagree after a branch change",
    ),
    // ── Contracts (PR 16) ────────────────────────────────────────────
    c(
        "list_services",
        Package::Contracts,
        Level::Power,
        "every service in the federation with repo, paths, endpoint count and consumer counts",
        "asking which services exist and how many endpoints each owns",
    ),
    c(
        "get_service",
        Package::Contracts,
        Level::Power,
        "consumers of one service: endpoints, calling code, fields used, used_by walk",
        "asking who consumes a service and why a function runs",
    ),
];

/// Lookup one capability by tool name.
pub fn capability(tool: &str) -> Option<&'static Capability> {
    CAPABILITIES.iter().find(|c| c.tool == tool)
}

/// Tools belonging to one package, in registry order.
pub fn package_tools(package: Package) -> Vec<&'static str> {
    CAPABILITIES
        .iter()
        .filter(|c| c.package == package)
        .map(|c| c.tool)
        .collect()
}

/// Render one package as the card `get_agent_strategy` / `list_packages`
/// show: pitch, level, why it is off by default, and its tools with
/// their "when" triggers.
pub fn package_card(package: Package) -> String {
    let mut out = format!(
        "## Package `{}` — {}\n\nLevel: {}.\n",
        package.name(),
        package.pitch(),
        package.level().label()
    );
    if package != Package::Core {
        out.push_str(&format!(
            "Not enabled by default: {}.\n",
            package.why_off_by_default()
        ));
        out.push_str(&format!(
            "Enable with `LAIN_TOOL_PROFILE={}` or call `load_package(\"{}\")`.\n",
            package.name(),
            package.name()
        ));
    }
    out.push('\n');
    for c in CAPABILITIES.iter().filter(|c| c.package == package) {
        out.push_str(&format!("- **{}** — {} _({})_\n", c.tool, c.what, c.when));
    }
    out
}

/// Every package card — the full skill menu.
pub fn all_package_cards() -> String {
    Package::ALL
        .iter()
        .map(|p| package_card(*p))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Packages opted in for this process session (`load_package`).
/// Process-wide: one operator per server, which matches how
/// `LAIN_TOOL_PROFILE` works. Advertising is still not dispatch —
/// this set only grows `tools/list`.
pub fn session_packages() -> &'static std::sync::Mutex<std::collections::HashSet<Package>> {
    use std::sync::OnceLock;
    static SESSION: OnceLock<std::sync::Mutex<std::collections::HashSet<Package>>> =
        OnceLock::new();
    SESSION.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

/// Opt a package into the session. Returns false when already loaded.
pub fn load_package(package: Package) -> bool {
    session_packages()
        .lock()
        .map(|mut s| s.insert(package))
        .unwrap_or(false)
}

/// Packages currently opted in (sorted, stable order).
pub fn loaded_packages() -> Vec<Package> {
    let set = session_packages()
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default();
    Package::ALL
        .iter()
        .copied()
        .filter(|p| set.contains(p))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_exactly_one_capability_row() {
        let mut names: Vec<&str> = CAPABILITIES.iter().map(|c| c.tool).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate capability rows");
    }

    #[test]
    fn packages_are_sized_like_skills() {
        // Opt-in packages are skills: small enough to read in one
        // sitting. Core is the always-on base and is exempt.
        // `Contracts` ships PR 16 with 2 tools (`list_services`,
        // `get_service`); PR 13 adds the rest of the package. Until
        // then the package is intentionally below the 3-tool floor.
        for p in Package::ALL {
            if *p == Package::Core || *p == Package::Contracts {
                continue;
            }
            let n = package_tools(*p).len();
            assert!(
                (3..=12).contains(&n),
                "package {} has {n} tools; skills should be 3-12",
                p.name()
            );
        }
    }

    #[test]
    fn core_is_the_eighteen_tool_default() {
        // 16 comprehension/impact tools plus the two skill-layer
        // tools (list_packages / load_package) that make every other
        // package discoverable.
        assert_eq!(package_tools(Package::Core).len(), 18);
    }

    #[test]
    fn registry_matches_schema_dump() {
        // The generated dump is the canonical registered surface;
        // `semantic_search` is the one tool that exists but is filtered
        // when no model is loaded, so it is allowed to be the only
        // difference.
        let dump = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/tool-schema.json"),
        )
        .expect("docs/tool-schema.json must exist (run `make schema`)");
        let dump: serde_json::Value = serde_json::from_str(&dump).expect("dump is JSON");
        let mut surface: Vec<String> = dump
            .as_array()
            .expect("dump root is an array")
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        surface.push("semantic_search".into());
        surface.sort_unstable();

        let mut registered: Vec<String> = CAPABILITIES.iter().map(|c| c.tool.to_string()).collect();
        registered.sort_unstable();

        assert_eq!(
            surface, registered,
            "capability registry and the schema dump disagree; update both"
        );
    }

    #[test]
    fn package_names_are_stable_wire_strings() {
        // These end up in LAIN_TOOL_PROFILE parsing and load_package
        // arguments. Renaming one is a wire change.
        assert_eq!(Package::Core.name(), "core");
        assert_eq!(Package::Architecture.name(), "arch");
        assert_eq!(Package::Primitives.name(), "raw");
        assert_eq!(Package::Verify.name(), "verify");
        assert_eq!(Package::Session.name(), "session");
        assert_eq!(Package::Contracts.name(), "contracts");
    }

    #[test]
    fn every_row_says_what_it_does_and_when() {
        for c in CAPABILITIES {
            assert!(!c.what.is_empty(), "{} has no what", c.tool);
            assert!(!c.when.is_empty(), "{} has no when", c.tool);
            assert!(
                c.when
                    .chars()
                    .next()
                    .map(|ch| ch.is_lowercase())
                    .unwrap_or(false),
                "{}: `when` should read as a trigger phrase",
                c.tool
            );
        }
    }
}
