//! Cross-repo symbol resolver used by the federation's ingest resolve phase.
//!
//! When a `Calls` or static name reference does not resolve against the
//! *source* repo's per-repo `GraphDatabase`, the resolve phase asks a
//! [`CrossRepoResolver`] for help. The resolver, supplied by the
//! federation, looks up the target symbol across every registered
//! repo. On success it returns the canonical global id
//! (`repo_id:Kind:path:name`); the resolve phase writes that id
//! directly into the per-repo edge, and [`crate::federation::federated_index::FederatedIndex::project_repo`]
//! passes it through unchanged into the federated backend.
//!
//! This trait is opt-in. The single-workspace ingest pipeline passes
//! `None` and the resolve phase is bit-identical to the pre-resolver
//! behavior, so the new wiring is a no-op for non-federation servers.
//!
//! See `docs/wish-list.md` #13 for the original gap.

use crate::federation::repo_id::{GlobalId, RepoId};
use std::path::Path;

/// Federation-aware lookup the resolve phase calls when a reference
/// does not resolve against the calling repo's own `GraphDatabase`.
///
/// Implementations receive whatever hints the resolve phase has. LSP
/// refs carry an absolute target path plus a target line; tree-sitter
/// refs carry a target name only. Implementations are free to use any
/// combination, but must return `None` when the target cannot be
/// narrowed to a single non-source repo — the resolve phase treats
/// `None` as "leave the reference as a gap", which matches the
/// same-file preference already established by
/// [`crate::server::ingest::resolve::resolve_static_edges`].
pub trait CrossRepoResolver: Send + Sync {
    /// Resolve a cross-repo reference to its global id.
    ///
    /// `source_repo` is the repo doing the indexing; the resolver must
    /// skip it when picking candidate owning repos, so a reference
    /// from `billing-svc` to `billing-svc` is never reported as a
    /// cross-repo hit (the local DB already handles that case).
    ///
    /// Hints (any combination may be `Some`; the resolver picks the
    /// strongest available):
    /// - `name` — the symbol name being referenced (tree-sitter refs).
    /// - `hint_path` — LSP-reported absolute path of the target's
    ///   source file (LSP refs).
    /// - `hint_line` — line number within `hint_path` where the
    ///   reference resolves (LSP refs).
    fn resolve_cross_repo(
        &self,
        source_repo: &RepoId,
        name: Option<&str>,
        hint_path: Option<&Path>,
        hint_line: Option<u32>,
    ) -> Option<GlobalId>;

    /// Refresh any cached index the resolver holds so the just-indexed
    /// repo's symbols become visible to subsequent cross-repo lookups.
    /// Called by the ingest pipeline after nodes are inserted into the
    /// per-repo DB but before the resolve phase runs, so cross-repo
    /// refs in this repo's own source can find targets in other repos
    /// the federation has indexed.
    ///
    /// Default implementation is a no-op for resolvers that don't
    /// maintain a cache (test fakes, single-repo shims). The
    /// federation's implementation rebuilds `symbol_to_repos` from
    /// every registered repo's `nodes()`.
    fn refresh(&self) {}
}