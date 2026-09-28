//! Tool profile — what `tools/list` advertises.
//!
//! Advertising is not dispatch: the dispatcher serves every
//! registered tool regardless of profile, so hook scripts keep
//! calling hidden tools. The profile only decides what a *model*
//! sees.
//!
//! The default is `core` — the always-on comprehension + impact set,
//! plus the active mode's Q&A tools. Everything else is a package
//! (see [`crate::server::tools::capabilities`]): opt in per process
//! with `LAIN_TOOL_PROFILE=<package>,...` or per session with the
//! `load_package` tool. `full` advertises every registered tool.

use crate::server::tools::capabilities::{capability, load_package, Package};
use std::env;

/// Non-inventory tool families the dispatcher appends at runtime.
/// Kept here because `special_advertised_count` has to count them
/// without the registry side.
pub struct SemanticProfileFamlies;

impl SemanticProfileFamlies {
    /// Federation-mode reads a coding agent needs.
    pub const FEDERATION_QA: &'static [&'static str] =
        &["search_org", "get_cross_repo_blast_radius"];
    /// Workspace-mode reads.
    pub const WORKSPACE_QA: &'static [&'static str] = &["get_workspace_graph"];
}

/// Whether a name is part of the always-visible Q&A half of a
/// mode family (as opposed to the package's admin half).
fn is_mode_qa(tool_name: &str) -> bool {
    SemanticProfileFamlies::FEDERATION_QA.contains(&tool_name)
        || SemanticProfileFamlies::WORKSPACE_QA.contains(&tool_name)
}

/// The active advertise-set: `core` plus opted-in packages, or
/// `full`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolProfile {
    /// Advertise every registered tool. Supersedes the packages.
    pub full: bool,
    /// Packages opted in beyond `core`.
    pub packages: Vec<Package>,
}

impl ToolProfile {
    /// Read the active profile from the environment.
    ///
    /// `LAIN_TOOL_PROFILE` accepts one value or a comma list of
    /// package names (`arch`, `raw`, `verify`, `session`, `social`,
    /// `notes`, `ops`, `federation`, `workspace`), the legacy aliases
    /// `semantic` (default, no-op) and `full`, or `session`/`ops`
    /// which map to the packages of the same name. Unknown values are
    /// ignored with a warning so a typo is visible to the operator
    /// without surprising the agent.
    pub fn from_env() -> Self {
        let raw = match env::var("LAIN_TOOL_PROFILE") {
            Ok(v) => v,
            Err(_) => return Self::default(),
        };
        let mut profile = Self::default();
        let mut known = false;
        for part in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "semantic" | "core" => known = true,
                "full" => {
                    profile.full = true;
                    known = true;
                }
                name => match Package::parse(name) {
                    Some(package) => {
                        if !profile.packages.contains(&package) {
                            profile.packages.push(package);
                        }
                        known = true;
                    }
                    None => tracing::warn!(
                        "LAIN_TOOL_PROFILE={name:?} is not a known package; ignoring it. \
                         Valid values: core, arch, raw, verify, session, social, notes, ops, \
                         federation, workspace, full (comma-listable)."
                    ),
                },
            }
        }
        if !known && !profile.full && profile.packages.is_empty() {
            tracing::warn!(
                "LAIN_TOOL_PROFILE={raw:?} is not a known profile; defaulting to core. \
                 Run `list_packages` for the valid names."
            );
        }
        profile
    }

    /// Stable name for diagnostics and `get_capabilities`. A pure
    /// default is `"semantic"`; otherwise the enabled package names
    /// join with `+`, and `full` wins.
    pub fn as_str(&self) -> String {
        if self.full {
            return "full".to_string();
        }
        if self.packages.is_empty() {
            return "semantic".to_string();
        }
        let mut names: Vec<&str> = self.packages.iter().map(|p| p.name()).collect();
        names.sort_unstable();
        names.join("+")
    }

    /// True when the package is part of this profile.
    pub fn enables(&self, package: Package) -> bool {
        self.full || package == Package::Core || self.packages.contains(&package)
    }

    /// Opt a package into this session's profile (runtime half of the
    /// skill model — `load_package`). Forwards to the session set the
    /// tools/list filter reads.
    pub fn load_package(package: Package) -> bool {
        load_package(package)
    }
}

/// Whether `tool_name` is advertised under `profile`. The session
/// set (`load_package`) is consulted in addition to the env profile.
pub(crate) fn profile_allows_inner(profile: &ToolProfile, tool_name: &str) -> bool {
    if profile.full {
        return true;
    }
    let Some(cap) = capability(tool_name) else {
        // Unregistered names (legacy tests, aliases) stay visible —
        // the registry pin guarantees nothing on the real surface is
        // missing from it.
        return is_mode_qa(tool_name);
    };
    match cap.package {
        Package::Core => true,
        Package::Federation | Package::Workspace if is_mode_qa(tool_name) => true,
        package => {
            profile.enables(package)
                || crate::server::tools::capabilities::loaded_packages().contains(&package)
        }
    }
}

/// Count of `tools/list` entries that come from the *non-inventory*
/// sources under `profile` (the mode Q&A families). The caller adds
/// the inventory-side count themselves with the same profile filter
/// so we don't double-count.
///
/// `federation_active` / `workspace_active` are wired through from
/// `get_capabilities` and `doctor.json`; the parameter exists so a
/// future PR that plumbs workspaces into `ToolContext` just swaps the
/// call sites.
pub fn special_advertised_count(
    profile: &ToolProfile,
    federation_active: bool,
    workspace_active: bool,
) -> usize {
    let _ = profile; // Mode Q&A is always visible; packages gate only
                     // the admin halves, which live in the registry
                     // side and are counted by the caller.
    let mut count = 0;
    if federation_active {
        count += SemanticProfileFamlies::FEDERATION_QA.len();
    }
    if workspace_active {
        count += SemanticProfileFamlies::WORKSPACE_QA.len();
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::tools::capabilities::{package_tools, Package};

    #[test]
    fn default_profile_is_core_only() {
        let def = ToolProfile::default();
        assert_eq!(def.as_str(), "semantic");
        assert!(profile_allows_inner(&def, "find_symbol"));
        assert!(profile_allows_inner(&def, "list_packages"));
        assert!(profile_allows_inner(&def, "load_package"));
        // Mode Q&A stays visible under the default.
        assert!(profile_allows_inner(&def, "search_org"));
        assert!(!profile_allows_inner(&def, "claim_files"));
        assert!(!profile_allows_inner(&def, "run_tests"));
        assert!(!profile_allows_inner(&def, "query_graph"));
    }

    #[test]
    fn packages_opt_in_one_at_a_time() {
        let verify = ToolProfile {
            packages: vec![Package::Verify],
            ..Default::default()
        };
        assert!(profile_allows_inner(&verify, "run_tests"));
        assert!(!profile_allows_inner(&verify, "query_graph"));
        assert_eq!(verify.as_str(), "verify");

        let two = ToolProfile {
            packages: vec![Package::Verify, Package::Notes],
            ..Default::default()
        };
        assert!(profile_allows_inner(&two, "add_annotation"));
        assert_eq!(two.as_str(), "notes+verify");
    }

    #[test]
    fn full_profile_is_everything() {
        let full = ToolProfile {
            full: true,
            ..Default::default()
        };
        assert!(profile_allows_inner(&full, "query_graph"));
        assert!(profile_allows_inner(&full, "debug_sleep"));
        assert_eq!(full.as_str(), "full");
    }

    #[test]
    fn package_names_are_valid_wire_strings() {
        for p in Package::ALL {
            assert_eq!(Package::parse(p.name()), Some(*p));
        }
        assert_eq!(Package::parse("ARCH"), Some(Package::Architecture));
        assert_eq!(Package::parse("nonsense"), None);
    }

    #[test]
    fn every_package_has_tools() {
        for p in Package::ALL {
            assert!(
                !package_tools(*p).is_empty(),
                "package {} is empty",
                p.name()
            );
        }
    }

    #[test]
    fn user_manual_surface_table_matches_the_profiles() {
        // The docs table is only useful if it is true. Derive every
        // number from the same registry the dispatcher filters with
        // and from the generated schema dump.
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

        for p in Package::ALL {
            let needle = format!("| `{}` | {} |", p.name(), package_tools(*p).len());
            assert!(
                manual.contains(&needle),
                "docs/USER_MANUAL.md tool-surface table is stale; expected a row containing {needle:?}"
            );
        }
        assert!(
            manual.contains(&format!("| `full` | {full_count} |")),
            "docs/USER_MANUAL.md full-surface row is stale; expected {full_count}"
        );
    }
}
