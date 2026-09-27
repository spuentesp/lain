//! Tool profile — wire-level filter for `tools/list`.
//!
//! The underlying MCP dispatcher still has every specialised tool
//! registered; what changes per profile is what `tools/list` returns.
//! That split (advertise ≠ dispatch) is what lets hook scripts keep
//! calling `claim_files` / `heartbeat` while the model's surface stays
//! small.
//!
//! The default `Semantic` profile is the curated comprehension +
//! impact layer — the tools a coding agent reaches for while reading
//! and changing code. Everything else is opt-in per audience:
//!
//! - `session` — multiplayer plumbing (claims, heartbeat, occupancy).
//!   Owned by hooks in most setups; advertised only when asked for.
//! - `ops` — server/status/reload controls and federation-admin reads.
//! - `full` — the entire registered surface.
//!
//! `LAIN_TOOL_PROFILE` accepts one value or a comma list
//! (`session,ops`); the default is `semantic`. The choice is
//! self-discoverable through `get_capabilities` and
//! `get_agent_strategy`, so an agent can ask to see more rather than
//! guessing at hidden names.

use std::env;

/// The comprehension + impact core, advertised under every non-`full`
/// profile. The list is hand-curated to match `get_agent_strategy`'s
/// recommended flow: bootstrap → find → understand → assess → act on
/// impact. Order is preserved in the JSON Schema dump for diagnostic
/// tools that want it (the runtime filtering doesn't depend on order).
///
/// Adding a tool here is the *only* code change needed to expose it
/// through the small semantic surface; everything else is data-driven.
pub const SEMANTIC_PROFILE: &[&str] = &[
    // Bootstrap
    "understand_repository",
    // Comprehension
    "find_symbol",
    "get_context",
    "search_code",
    "explain_dispatch",
    // Impact / architecture
    "assess_change",
    "get_blast_radius",
    "get_call_chain",
    "find_related",
    "find_anchors",
    "list_entry_points",
    "get_coupling_radar",
    "find_dead_code",
    // Readiness / self-discovery
    "get_health",
    "get_capabilities",
    // Escape hatch — how to get more tools, on demand.
    "get_agent_strategy",
];

/// Multiplayer plumbing. The hooks layer (`hooks/<agent>/`) calls
/// these directly — the dispatcher serves them whether or not they
/// are advertised — so hiding them from the default surface costs a
/// hook-driven agent nothing. An agent without hook support opts in
/// with `LAIN_TOOL_PROFILE=session` to claim files manually.
pub const SESSION_PROFILE: &[&str] = &[
    "register_agent",
    "heartbeat",
    "claim_files",
    "release_files",
    "list_occupancy",
    "get_world_state",
];

/// Server controls. Opt in with `LAIN_TOOL_PROFILE=ops`.
pub const OPS_PROFILE: &[&str] = &[
    "get_server_status",
    "list_recent_projects",
    "get_reload_status",
    "request_reload",
];

/// Tools in the special-case families (server-status, federation,
/// workspace) are appended to `tools/list` by the dispatcher at
/// runtime. They're not in [`SEMANTIC_PROFILE`] itself; each family
/// splits into a Q&A half (visible under every profile — "what's
/// around me?") and an admin half (only under `ops`).
///
/// Each list matches a `*_TOOL_DEFS` array in
/// `crate::server::mcp::definitions` and lives next to the dispatch
/// site so adding a new tool there doesn't drift the profile.
pub struct SemanticProfileFamlies;

impl SemanticProfileFamlies {
    /// Federation-mode reads a coding agent needs (org-wide search and
    /// cross-repo impact).
    pub const FEDERATION_QA: &'static [&'static str] =
        &["search_org", "get_cross_repo_blast_radius"];
    /// Federation admin — repository inventory and health of the
    /// federation itself.
    pub const FEDERATION_ADMIN: &'static [&'static str] =
        &["list_repos", "get_repo_info", "get_federation_health"];
    /// Workspace-mode reads.
    pub const WORKSPACE_QA: &'static [&'static str] = &["get_workspace_graph"];
    /// Workspace admin — which workspaces exist and which is active.
    pub const WORKSPACE_ADMIN: &'static [&'static str] =
        &["list_workspaces", "get_active_workspace", "get_workspace"];
}

/// The active advertise-set. Composable: `session` and `ops` are
/// flags on top of the semantic core, `full` is everything.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToolProfile {
    /// Advertise the multiplayer plumbing (`SESSION_PROFILE`).
    pub session: bool,
    /// Advertise server controls and admin reads (`OPS_PROFILE` +
    /// `*_ADMIN` families).
    pub ops: bool,
    /// Advertise every registered tool. Supersedes the flags.
    pub full: bool,
}

impl ToolProfile {
    /// Read the active profile from the environment.
    ///
    /// Resolution order:
    ///   1. `LAIN_TOOL_PROFILE` env var (case-insensitive). Accepts a
    ///      single value or a comma list of `semantic`, `session`,
    ///      `ops`, `full`. Unknown values fall back to the default and
    ///      log a warning so the operator learns the typo without the
    ///      agent being surprised.
    ///   2. Default `semantic`.
    pub fn from_env() -> Self {
        let raw = match env::var("LAIN_TOOL_PROFILE") {
            Ok(v) => v,
            Err(_) => return Self::default(),
        };
        let mut profile = Self::default();
        let mut known = false;
        for part in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "semantic" => known = true,
                "session" => {
                    profile.session = true;
                    known = true;
                }
                "ops" => {
                    profile.ops = true;
                    known = true;
                }
                "full" => {
                    profile.full = true;
                    known = true;
                }
                other => {
                    tracing::warn!(
                        "LAIN_TOOL_PROFILE={other:?} is not a known profile value; ignoring it. \
                         Valid values: semantic, session, ops, full (comma-listable)."
                    );
                }
            }
        }
        if !known && !profile.session && !profile.ops && !profile.full {
            tracing::warn!(
                "LAIN_TOOL_PROFILE={raw:?} is not a known profile; defaulting to semantic. \
                 Valid values: semantic, session, ops, full (comma-listable)."
            );
        }
        profile
    }

    /// Stable string name for diagnostics and `get_capabilities`.
    pub fn as_str(self) -> &'static str {
        if self.full {
            "full"
        } else {
            match (self.session, self.ops) {
                (true, true) => "session+ops",
                (true, false) => "session",
                (false, true) => "ops",
                (false, false) => "semantic",
            }
        }
    }
}

/// Whether `tool_name` is advertised under `profile`.
///
/// The semantic core and the mode Q&A families are visible under
/// every non-`full` profile; `session` and `ops` add their own sets.
pub(crate) fn profile_allows_inner(profile: ToolProfile, tool_name: &str) -> bool {
    if profile.full {
        return true;
    }
    if SEMANTIC_PROFILE.contains(&tool_name)
        || SemanticProfileFamlies::FEDERATION_QA.contains(&tool_name)
        || SemanticProfileFamlies::WORKSPACE_QA.contains(&tool_name)
    {
        return true;
    }
    if profile.session && SESSION_PROFILE.contains(&tool_name) {
        return true;
    }
    if profile.ops
        && (OPS_PROFILE.contains(&tool_name)
            || SemanticProfileFamlies::FEDERATION_ADMIN.contains(&tool_name)
            || SemanticProfileFamlies::WORKSPACE_ADMIN.contains(&tool_name))
    {
        return true;
    }
    false
}

/// Count of `tools/list` entries that come from the *non-inventory*
/// sources under `profile`: the always-visible Q&A families plus the
/// admin families under `ops`. The caller adds the inventory-side
/// count themselves with the same profile filter so we don't
/// double-count.
///
/// All three flags are present on the signature even though
/// `ToolContext` currently doesn't carry workspace state — the
/// parameter is wired through `false` from both `get_capabilities`
/// and `doctor.json` today, and a future PR that plumbs
/// workspaces into `ToolContext` just swaps the call sites
/// without changing the helper.
pub fn special_advertised_count(
    profile: ToolProfile,
    federation_active: bool,
    workspace_active: bool,
) -> usize {
    use SemanticProfileFamlies as Fam;
    if profile.full {
        // The caller treats `full` as "everything registered"; the
        // non-inventory sources are all counted there.
        return Fam::FEDERATION_QA.len()
            + Fam::FEDERATION_ADMIN.len()
            + Fam::WORKSPACE_QA.len()
            + Fam::WORKSPACE_ADMIN.len()
            + OPS_PROFILE.len();
    }
    let mut count = 0;
    if federation_active {
        count += Fam::FEDERATION_QA.len();
    }
    if workspace_active {
        count += Fam::WORKSPACE_QA.len();
    }
    if profile.ops {
        count += OPS_PROFILE.len();
        if federation_active {
            count += Fam::FEDERATION_ADMIN.len();
        }
        if workspace_active {
            count += Fam::WORKSPACE_ADMIN.len();
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Per-test serial env-var guard. `LAIN_TOOL_PROFILE` is process-wide
    // and `from_env` reads it on every call, so tests that mutate it
    // need to either hold this guard or accept the flake risk.
    // SAFETY: the env is process-global; tests that touch it must
    // serialise through `ENV_LOCK` below. Without this guard, two
    // tests running in parallel would race and pin each other's
    // profile.
    #[allow(dead_code)]
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn semantic_profile_is_small_and_curated() {
        let set = SEMANTIC_PROFILE;
        // 16 hand-curated entries. Pinning a count catches "I added one
        // more without realising" — if you add a tool, the change should
        // be conscious, not silent. The default must stay small enough
        // that a cold agent reads every description.
        assert_eq!(set.len(), 16, "SEMANTIC_PROFILE drifted; review the list");

        // Sanity: every name is non-empty and the list has no
        // duplicates (Set semantics).
        let mut sorted = set.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            set.len(),
            "SEMANTIC_PROFILE contains duplicates"
        );
        for name in set {
            assert!(!name.is_empty());
            assert!(
                !name.contains(' '),
                "tool names cannot contain spaces: {name:?}"
            );
        }
    }

    #[test]
    fn session_and_ops_do_not_overlap_the_core() {
        for t in SESSION_PROFILE.iter().chain(OPS_PROFILE.iter()) {
            assert!(
                !SEMANTIC_PROFILE.contains(t),
                "{t} is advertised twice; the core and the opt-in sets must be disjoint"
            );
        }
    }

    #[test]
    fn profile_from_env_defaults_to_semantic() {
        let def = ToolProfile::default();
        assert_eq!(def.as_str(), "semantic");
        assert!(!def.session && !def.ops && !def.full);
    }

    #[test]
    fn profile_names_are_stable() {
        // These exact strings end up in `get_capabilities.tool_profile`
        // output. A rename here is a wire change.
        assert_eq!(ToolProfile::default().as_str(), "semantic");
        assert_eq!(
            (ToolProfile {
                session: true,
                ..Default::default()
            })
            .as_str(),
            "session"
        );
        assert_eq!(
            (ToolProfile {
                ops: true,
                ..Default::default()
            })
            .as_str(),
            "ops"
        );
        assert_eq!(
            (ToolProfile {
                session: true,
                ops: true,
                ..Default::default()
            })
            .as_str(),
            "session+ops"
        );
        assert_eq!(
            (ToolProfile {
                full: true,
                ..Default::default()
            })
            .as_str(),
            "full"
        );
    }

    #[test]
    fn default_hides_session_and_ops_tools() {
        let def = ToolProfile::default();
        assert!(profile_allows_inner(def, "find_symbol"));
        assert!(profile_allows_inner(def, "get_blast_radius"));
        assert!(profile_allows_inner(def, "get_agent_strategy"));
        // Mode Q&A families stay visible under the default.
        assert!(profile_allows_inner(def, "search_org"));
        assert!(profile_allows_inner(def, "get_workspace_graph"));
        // Session and ops plumbing do not.
        assert!(!profile_allows_inner(def, "claim_files"));
        assert!(!profile_allows_inner(def, "heartbeat"));
        assert!(!profile_allows_inner(def, "get_server_status"));
        assert!(!profile_allows_inner(def, "list_repos"));
    }

    #[test]
    fn opt_in_profiles_compose() {
        let session = ToolProfile {
            session: true,
            ..Default::default()
        };
        assert!(profile_allows_inner(session, "claim_files"));
        assert!(!profile_allows_inner(session, "request_reload"));

        let both = ToolProfile {
            session: true,
            ops: true,
            ..Default::default()
        };
        assert!(profile_allows_inner(both, "claim_files"));
        assert!(profile_allows_inner(both, "request_reload"));
        assert!(profile_allows_inner(both, "list_repos"));

        let full = ToolProfile {
            full: true,
            ..Default::default()
        };
        assert!(profile_allows_inner(full, "query_graph"));
    }

    #[test]
    fn default_surface_stays_small_in_every_mode() {
        // The promise: a cold agent in the busiest mode sees at most
        // 18 advertised tools (16 core + the federation Q&A pair).
        let federation_mode = SEMANTIC_PROFILE.len() + SemanticProfileFamlies::FEDERATION_QA.len();
        let workspace_mode = SEMANTIC_PROFILE.len() + SemanticProfileFamlies::WORKSPACE_QA.len();
        assert!(
            federation_mode <= 18,
            "default federation surface grew to {federation_mode}; the point of the profile is that it stays readable"
        );
        assert!(
            workspace_mode <= 18,
            "default workspace surface grew to {workspace_mode}"
        );
    }

    #[test]
    fn user_manual_surface_table_matches_the_profiles() {
        // The docs table is only useful if it is true. Derive every
        // number from the same lists the dispatcher filters with and
        // from the generated schema dump, then require the manual to
        // state exactly those numbers.
        let manual = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/USER_MANUAL.md"),
        )
        .expect("docs/USER_MANUAL.md must be readable");
        let dump = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/tool-schema.json"),
        )
        .expect("docs/tool-schema.json must exist (run `make schema`)");
        let dump: serde_json::Value = serde_json::from_str(&dump).expect("dump is JSON");
        let full_count = dump.as_array().expect("dump root is an array").len();

        for needle in [
            format!("| `semantic` (default) | {} |", SEMANTIC_PROFILE.len()),
            format!("| `session` | adds {} |", SESSION_PROFILE.len()),
            format!("| `ops` | adds {} |", OPS_PROFILE.len()),
            format!("| `full` | {} |", full_count),
        ] {
            assert!(
                manual.contains(&needle),
                "docs/USER_MANUAL.md tool-surface table is stale; expected a row containing {needle:?}"
            );
        }
    }

    #[test]
    fn special_advertised_count_without_modes_matches_qa_families_only() {
        let def = ToolProfile::default();
        // No federation, no workspace: the Q&A families are empty and
        // the default advertises no non-inventory tools at all.
        assert_eq!(special_advertised_count(def, false, false), 0);
    }

    #[test]
    fn special_advertised_count_with_federation() {
        let def = ToolProfile::default();
        let n = special_advertised_count(def, true, false);
        assert_eq!(n, SemanticProfileFamlies::FEDERATION_QA.len());

        let ops = ToolProfile {
            ops: true,
            ..Default::default()
        };
        let n_ops = special_advertised_count(ops, true, false);
        assert_eq!(
            n_ops,
            SemanticProfileFamlies::FEDERATION_QA.len()
                + SemanticProfileFamlies::FEDERATION_ADMIN.len()
                + OPS_PROFILE.len()
        );
    }
}
