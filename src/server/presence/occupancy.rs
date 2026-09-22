//! Occupancy map: per-path claim state.
//!
//! `OccupancyMap` is the in-memory `Arc<Mutex<OccupancyState>>` keyed on
//! per-path file occupancy (with per-symbol occupancy as a sub-map).
//! `claim`, `release`, `list_for_path`, `list_for_agent`, and the
//! TTL/heartbeat expiry paths live here. The canonical claim-path
//! resolver (`canonical_claim_path`) and the body-hash helper
//! (`compute_symbol_hash`) live in this file because they only feed
//! the claim pipeline.
//!
//! Persistence (`save_pair` / `load_pair`) lives in `persistence.rs`
//! and reads these fields directly through `pub(crate)` visibility.
//! Persistence callers are responsible for holding the inner lock.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use parking_lot::Mutex;

use super::agent::AgentId;
use super::claim::{Claim, ClaimIntent, ConflictEntry, Holder, OccupancyEntry, SymbolHash, SymbolOccupancy};
use super::registry::{AgentSession, PersistFn, PresenceRegistry};
use crate::server::path_util::{canonical_form, lexical_normalize, posix_string};
use crate::server::revision_log::RevisionId;


#[derive(Debug, Clone, serde::Serialize)]
pub struct ClaimRequest {
    pub path: PathBuf,
    pub symbols: Vec<String>,
    pub intent: ClaimIntent,
    /// Optional explicit TTL in seconds. When `Some(n)`, the resulting
    /// `Claim` carries `expires_at = claimed_at + n` and the expiry
    /// loop in `LainServer` will release the claim once `expires_at`
    /// passes regardless of heartbeat. When `None`, the claim has no
    /// TTL of its own and is only released explicitly or when the
    /// owning agent's session expires.
    pub ttl_seconds: Option<u64>,
    /// Last plan revision the caller saw when issuing this claim
    /// (Task 1.4). Threads onto the resulting `Claim` so the value
    /// survives persistence and reachability-checks against the
    /// overlay can flag stale claims. `None` for callers that don't
    /// supply a revision.
    pub plan_revision: Option<RevisionId>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ClaimResult {
    pub granted: Vec<ClaimRequest>,
    pub conflicts: Vec<ConflictEntry>,
    /// Non-blocking notices about claims that were *granted anyway*.
    ///
    /// A read claim never conflicts — readers shouldn't block on
    /// writers. But returning `{"conflicts": [], "granted": [...]}` and
    /// nothing else told a reader nothing about the agent rewriting the
    /// file underneath it, which is the most common way agent teams
    /// actually collide: B reads, reasons for two minutes, and patches
    /// a version A already replaced. Same shape as `conflicts`, but
    /// advisory: proceed, and re-read before you patch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub advisories: Vec<ConflictEntry>,
    /// Snapshot of (current_revision, plan_revision) at claim time, plus
    /// the symbols that changed since the caller's `plan_revision` and a
    /// free-form `note` for `BeyondCurrent` / `TooOld` error paths.
    /// `None` when the caller didn't supply a `plan_revision` and no
    /// staleness info applies (omitted from the wire JSON by
    /// `skip_serializing_if`). Populated by the static-graph retract
    /// detector (Task 1.6, PR 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world_state: Option<WorldState>,
}

#[derive(Debug, Default)]
pub(crate) struct FileOccupancy {
    pub(crate) agents: HashSet<AgentId>,
    /// Per-symbol agent set. An entry exists only if any agent has claimed
    /// that specific symbol. If no agent has claimed a symbol, the entry is
    /// absent — not present with an empty set.
    pub(crate) symbols: HashMap<String, HashSet<AgentId>>,
    /// Per-symbol intent tracking. Outer key is the symbol name (or
    /// the `__file_level__` sentinel for file-level claims); inner
    /// map records the `ClaimIntent` each agent recorded when they
    /// claimed that scope. Powers the read-vs-edit conflict filter:
    /// a Read claim is non-conflicting against any existing intent;
    /// only Edit-vs-Edit (or Edit vs file-level Edit) yields a
    /// conflict.
    pub(crate) intents: HashMap<String, HashMap<AgentId, ClaimIntent>>,
    /// Per-symbol last-touched timestamp, in the same shape as
    /// `intents`. Used to populate the `last_seen_unix` field on
    /// `ConflictEntry` so callers can tell when the conflicting
    /// claim was first (or most recently) recorded.
    pub(crate) last_touched: HashMap<String, HashMap<AgentId, SystemTime>>,
    /// Agents whose presence on this file was inferred from filesystem
    /// activity rather than declared. Mirrored onto `ConflictEntry` so
    /// a conflicting agent can tell a guess from a declaration.
    pub(crate) inferred: HashSet<AgentId>,
}

impl FileOccupancy {
    /// Intent that `agent` recorded for `sym` (or `__file_level__`).
    /// Returns `None` if the agent never claimed that scope — which is
    /// the same condition that drives the existing agent/symbol
    /// bookkeeping, so the two never disagree.
    fn intent_for(&self, agent: &AgentId, sym: &str) -> Option<ClaimIntent> {
        self.intents.get(sym).and_then(|m| m.get(agent)).cloned()
    }

    /// Resolve `agent`'s strongest intent at *any* symbol scope
    /// (excluding `__file_level__`). Returns `Some(Edit)` if the agent
    /// has any symbol-level Edit claim, `Some(Read)` if every
    /// symbol-level claim is Read, and `None` if the agent has no
    /// symbol-level claims at all. Used by the file-level Edit
    /// conflict branch to decide whether a holder with only
    /// symbol-level claims should be treated as "actively editing
    /// here" (Edit → conflict) or "just observing" (Read → no
    /// conflict per wishlist #5).
    fn any_symbol_intent(&self, agent: &AgentId) -> Option<ClaimIntent> {
        let mut saw_read = false;
        for (sym, per_agent) in &self.intents {
            if sym == "__file_level__" {
                continue;
            }
            if let Some(intent) = per_agent.get(agent) {
                if *intent == ClaimIntent::Edit {
                    return Some(ClaimIntent::Edit);
                }
                saw_read = true;
            }
        }
        if saw_read {
            Some(ClaimIntent::Read)
        } else {
            None
        }
    }

    /// Last-touched timestamp for `agent` on `sym`. Mirrors
    /// `intent_for`. Falls back to `UNIX_EPOCH` when absent — callers
    /// turn this directly into a `ConflictEntry.last_seen_unix`
    /// via `Option::unwrap_or_default()`-style plumbing.
    fn last_touched_for(&self, agent: &AgentId, sym: &str) -> Option<SystemTime> {
        self.last_touched
            .get(sym)
            .and_then(|m| m.get(agent))
            .copied()
    }

    /// Most recent `last_touched` timestamp for `agent` on this file
    /// across **all** scopes (file-level + every symbol they claimed).
    /// Used to populate the `last_seen_unix` field on a conflict entry
    /// so the caller can tell "the other agent is actively here"
    /// (`> UNIX_EPOCH`) from "no claim" (`== UNIX_EPOCH`). Wishlist #5
    /// fix: previously this only looked up the file-level key, so an
    /// agent that only had symbol-level claims reported `1970` and the
    /// staleness signal was useless exactly when it mattered.
    fn last_touched_unix_for(&self, agent: &AgentId) -> SystemTime {
        self.last_touched
            .values()
            .filter_map(|per_agent| per_agent.get(agent).copied())
            .max()
            .unwrap_or(SystemTime::UNIX_EPOCH)
    }
}

#[derive(Debug, Default)]
pub(crate) struct OccupancyState {
    pub(crate) by_file: HashMap<PathBuf, FileOccupancy>,
    pub(crate) by_agent: HashMap<AgentId, Vec<Claim>>,
}

/// Canonical key for a claim path.
///
/// Claims used to be keyed on the caller's raw spelling, so
/// `/ws/src/a.rs`, `src/a.rs`, `./src/a.rs` and `src/../src/a.rs` were
/// four independent claims on one file and never conflicted with each
/// other. That split ran straight down the middle of the product:
/// `lain hooks claim` writes absolute paths while MCP callers write
/// repo-relative ones, so the CLI and the MCP surface could never
/// collide.
///
/// Resolution runs in two steps.
///
/// First the path is made absolute: an absolute path is normalized as
/// given; a relative one is anchored to the first root under which the
/// file actually exists, falling back to the primary root for a file
/// the agent is about to create.
///
/// Then it is presented workspace-relative when it lives under the
/// primary workspace root, and absolute when it does not. That keeps
/// the common single-repo case on the short, readable key agents
/// already send, while federation — where the primary root is a `/tmp`
/// staging placeholder that no real file lives under — falls through to
/// absolute keys, so `src/main.rs` in two federated repos stays two
/// distinct claims instead of colliding.
///
/// With no roots configured at all the path is normalized and left as
/// it came in; it still collides with itself, which is the best
/// available answer.
/// Both branches pass through `canonical_form` in `path_util` so
/// symlinks and Windows extended-length prefixes collapse to the same string.
///
/// `pub` so the fuzz target in `fuzz/fuzz_targets/path_canonicalize.rs`
/// can drive it with adversarial input; the function is otherwise
/// internal and was `fn` before the fuzz target existed (PR #60).
pub fn canonical_claim_path(roots: &[PathBuf], path: &Path) -> PathBuf {
    // Both branches go through the same canonical form so
    // symlinks and Windows extended-length prefixes don't
    // produce divergent absolute vs. relative keys.
    let absolute = if path.is_absolute() {
        canonical_form(path)
    } else {
        let anchored = roots
            .iter()
            .map(|root| canonical_form(&root.join(path)))
            .find(|candidate| candidate.exists());
        match anchored.or_else(|| roots.first().map(|root| canonical_form(&root.join(path)))) {
            Some(p) => p,
            None => return PathBuf::from(posix_string(path)),
        }
    };

    let relative = match roots.first() {
        Some(primary) => {
            // The stored `roots.first()` is the canonical form (the
            // set_workspace_root path calls canonicalize_path, and
            // any added claim roots should be canonical too). Strip
            // the same `\\?\` prefix off the stored form before
            // matching so absolute and relative keys end up in the
            // same string form.
            let stripped_primary = primary
                .to_string_lossy()
                .strip_prefix(r"\\?\")
                .map(PathBuf::from)
                .unwrap_or_else(|| primary.clone());
            match absolute.strip_prefix(&stripped_primary) {
                Ok(rel) => rel.to_path_buf(),
                Err(_) => absolute,
            }
        }
        None => absolute,
    };
    PathBuf::from(posix_string(&relative))
}

#[derive(Clone)]
pub struct OccupancyMap {
    pub(crate) inner: std::sync::Arc<Mutex<OccupancyState>>,
    /// Optional persist callback. Same shape as the registry's
    /// `persist_cb`; fires on `claim`, `release`, and `release_all_for`
    /// when the call actually mutates state (calls that grant no claims
    /// or release no paths do not fire).
    pub(crate) persist_cb: std::sync::Arc<parking_lot::Mutex<Option<PersistFn>>>,
    /// Workspace root for the filesystem-as-lock side-effect
    /// (`presence_lock::try_lock`). Set via `set_workspace_root` after
    /// construction; `None` means "no filesystem layer" (used in tests
    /// and by anything that doesn't have a workspace to anchor).
    /// `claim` reads this under a small lock so the side-effect
    /// doesn't race with a `set_workspace_root` swap.
    pub(crate) workspace_root: std::sync::Arc<parking_lot::Mutex<Option<PathBuf>>>,
    /// Roots a relative claim path may be anchored to, in priority
    /// order. Seeded with the workspace root; federation servers extend
    /// it with every registered repo path, because there the workspace
    /// is a staging placeholder and the real files live under the repo
    /// roots. Read by `canonical_claim_path`.
    pub(crate) claim_roots: std::sync::Arc<parking_lot::Mutex<Vec<PathBuf>>>,
    /// Active advisory filesystem lock leases: (AgentId, CanonicalClaimPath) -> LockFilePath.
    pub(crate) lock_leases: std::sync::Arc<parking_lot::Mutex<HashMap<(AgentId, PathBuf), PathBuf>>>,
}

impl std::fmt::Debug for OccupancyMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Manual Debug impl: see `PresenceRegistry` for rationale.
        let s = self.inner.lock();
        f.debug_struct("OccupancyMap")
            .field("files", &s.by_file.len())
            .field("agents", &s.by_agent.len())
            .finish()
    }
}

impl OccupancyMap {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(Mutex::new(OccupancyState::default())),
            persist_cb: std::sync::Arc::new(parking_lot::Mutex::new(None)),
            workspace_root: std::sync::Arc::new(parking_lot::Mutex::new(None)),
            claim_roots: std::sync::Arc::new(parking_lot::Mutex::new(Vec::new())),
            lock_leases: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
        }
    }

    #[cfg(test)]
    pub(crate) fn lock_leases_count(&self) -> usize {
        self.lock_leases.lock().len()
    }

    #[cfg(test)]
    pub(crate) fn has_lock_lease(&self, agent_id: &AgentId, path: &Path) -> bool {
        let roots = self.claim_roots_snapshot();
        let canonical = canonical_claim_path(&roots, path);
        self.lock_leases
            .lock()
            .contains_key(&(agent_id.clone(), canonical))
    }

    /// Install a callback fired on every mutation that should be
    /// persisted. Same semantics as
    /// `PresenceRegistry::set_persist_callback`.
    pub fn set_persist_callback<F>(&self, cb: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        let mut slot = self.persist_cb.lock();
        *slot = Some(std::sync::Arc::new(cb));
    }

    /// Atomically replace the current persist callback with one that
    /// records its `Result<(), String>` into the supplied cell, and
    /// return the previous callback. See
    /// [`PresenceRegistry::swap_persist_capture`] for the rationale.
    pub fn swap_persist_capture(
        &self,
        cell: std::sync::Arc<parking_lot::Mutex<Option<Result<(), String>>>>,
        path: std::path::PathBuf,
        presence: std::sync::Arc<PresenceRegistry>,
        occupancy: std::sync::Arc<OccupancyMap>,
        intent: std::sync::Arc<crate::server::intent::IntentRegistry>,
        activity: std::sync::Arc<crate::server::activity::ActivityTracker>,
    ) -> Option<crate::server::presence::PersistFn> {
        let cell_for_cb = std::sync::Arc::clone(&cell);
        let path_for_cb = path;
        let presence_for_cb = std::sync::Arc::clone(&presence);
        let occupancy_for_cb = std::sync::Arc::clone(&occupancy);
        let intent_for_cb = std::sync::Arc::clone(&intent);
        let activity_for_cb = std::sync::Arc::clone(&activity);
        let new_cb: crate::server::presence::PersistFn = std::sync::Arc::new(move || {
            let result = crate::server::presence::save_pair(
                &path_for_cb,
                &presence_for_cb,
                &occupancy_for_cb,
                &intent_for_cb,
                &activity_for_cb,
            );
            let mut slot = cell_for_cb.lock();
            *slot = Some(result);
        });
        let mut slot = self.persist_cb.lock();
        let prev = slot.take();
        *slot = Some(new_cb);
        prev
    }

    /// Restore a callback previously captured by
    /// [`Self::swap_persist_capture`].
    pub fn restore_persist_callback(&self, cb: crate::server::presence::PersistFn) {
        let mut slot = self.persist_cb.lock();
        *slot = Some(cb);
    }

    /// Set the workspace root so `claim` can write the
    /// filesystem-as-lock side-effect under
    /// `<workspace>/.lain/locks/<file>.json`. Called once per
    /// `LainServer` constructor, mirroring `set_persist_callback`.
    /// When unset (e.g. unit tests, federation paths without a
    /// workspace anchor), `claim` skips the filesystem write entirely.
    pub fn set_workspace_root(&self, workspace_root: &Path) {
        // Canonicalize the workspace root so an absolute claim
        // (`/var/folders/.../src/a.rs` on macOS, where the tempdir
        // is a symlink to `/private/var/folders/...`) and a
        // relative claim anchored against `roots.first()` (the
        // un-symlinked canonical root, once stored here) collapse
        // to the same canonical key. Without this the relative
        // claim strips to `src/a.rs` while the absolute claim keeps
        // its symlinked prefix, the two never collide, and the
        // presence tests in `tests/presence.rs` panic on
        // `/var/folders/...` paths that `/private/var/folders/...`
        // is a symlink for. Falls back to the un-symlinked form
        // only when canonicalize itself fails (e.g. the path was
        // removed between `tempfile::tempdir` and the test body).
        let canonical_root =
            std::fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
        let mut slot = self.workspace_root.lock();
        *slot = Some(canonical_root.clone());
        drop(slot);
        // The workspace is also the first anchor for relative claim
        // paths. Kept at the front so it wins over repo roots added
        // later by `add_claim_roots`. Use the canonical form so the
        // anchored key (canonical_root joined with the relative path
        // and stripped again) lands on the same string as the
        // absolute key's stripped form.
        let mut roots = self.claim_roots.lock();
        let root = lexical_normalize(&canonical_root);
        roots.retain(|r| r != &root);
        roots.insert(0, root);
    }

    /// Register additional roots that a relative claim path may be
    /// anchored to. Federation servers call this with every registered
    /// repo path: there `config.workspace` is a `/tmp` staging
    /// placeholder, so the repo roots are the only anchors that can
    /// turn `src/server/presence.rs` into the same key the CLI produces
    /// from an absolute path.
    pub fn add_claim_roots(&self, paths: &[PathBuf]) {
        let mut roots = self.claim_roots.lock();
        for p in paths {
            let root = lexical_normalize(p);
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
    }

    /// Snapshot the claim-path anchors. Taken before the occupancy lock
    /// so normalization never runs under it.
    fn claim_roots_snapshot(&self) -> Vec<PathBuf> {
        self.claim_roots.lock().clone()
    }

    /// Snapshot the workspace root, if configured. Used by
    /// `OccupancyMap::claim` to fetch the path under the small lock
    /// rather than holding the lock across the `try_lock` call.
    fn workspace_root_snapshot(&self) -> Option<PathBuf> {
        self.workspace_root.lock().clone()
    }

    /// Clone the (optional) persist callback out of the slot. Returns
    /// `None` when no callback has been installed; callers always
    /// no-op in that case.
    fn cloned_persist_cb(&self) -> Option<PersistFn> {
        self.persist_cb.lock().clone()
    }

    pub fn claim(&self, agent_id: &AgentId, requests: Vec<ClaimRequest>) -> ClaimResult {
        self.claim_in_memory(agent_id, requests, false)
    }

    /// Claim on behalf of an agent that never asked — the attribution
    /// watcher saw a write and guessed who made it. Marked `inferred`
    /// so every consumer can tell it apart from a declared claim.
    pub fn claim_inferred(&self, agent_id: &AgentId, requests: Vec<ClaimRequest>) -> ClaimResult {
        self.claim_in_memory(agent_id, requests, true)
    }

    /// Same as [`Self::claim`] but additionally writes the filesystem
    /// lock side-effect for each granted path. Preferred entry point
    /// when the full `AgentSession` is available (e.g. the MCP
    /// `claim_files` handler has already resolved the session via
    /// `by_token`). The lock write is best-effort: failures are logged
    /// and the in-memory claim stands regardless — the in-memory
    /// `OccupancyMap` remains authoritative when a `lain` server is
    /// running.
    pub fn claim_with_session(
        &self,
        session: &AgentSession,
        requests: Vec<ClaimRequest>,
    ) -> ClaimResult {
        let result = self.claim_in_memory(&session.id, requests, false);
        if !result.granted.is_empty() {
            self.write_lock_files(session, &result.granted);
        }
        result
    }

    /// In-memory-only claim implementation. Extracted so both
    /// `claim` (no FS side-effect, agent-id-only callers) and
    /// `claim_with_session` (FS side-effect + full session) share
    /// the same conflict / book-keeping logic.
    fn claim_in_memory(
        &self,
        agent_id: &AgentId,
        requests: Vec<ClaimRequest>,
        inferred: bool,
    ) -> ClaimResult {
        // Canonicalize before anything is keyed, so two agents naming
        // one file in two spellings land on the same entry. Done
        // outside the occupancy lock: the relative-path branch stats
        // the filesystem.
        let roots = self.claim_roots_snapshot();
        let requests: Vec<ClaimRequest> = requests
            .into_iter()
            .map(|mut r| {
                r.path = canonical_claim_path(&roots, &r.path);
                r
            })
            .collect();
        let (granted, conflicts, advisories) = {
            let mut s = self.inner.lock();
            let mut granted = Vec::new();
            let mut conflicts = Vec::new();
            let mut advisories = Vec::new();

            for req in requests {
                let entry = s.by_file.entry(req.path.clone()).or_default();
                let mut req_conflicts: Vec<ConflictEntry> = Vec::new();

                // Read claims never produce a conflict — wishlist
                // item #5. They still update the agent/symbol
                // bookkeeping below so the granting agent becomes
                // observable for occupancy listings.
                if req.intent == ClaimIntent::Read {
                    // A read is granted regardless, but the reader
                    // deserves to know someone is rewriting the file
                    // while it reads. Advisory, never blocking.
                    for other in entry.agents.iter().filter(|a| *a != agent_id) {
                        let holder_intent = entry
                            .intent_for(other, "__file_level__")
                            .or_else(|| entry.any_symbol_intent(other));
                        if holder_intent == Some(ClaimIntent::Edit) {
                            advisories.push(ConflictEntry {
                                agent_id: other.clone(),
                                inferred: entry.inferred.contains(other),
                                path: req.path.clone(),
                                symbols: entry
                                    .symbols
                                    .iter()
                                    .filter(|(sym, agents)| {
                                        sym.as_str() != "__file_level__" && agents.contains(other)
                                    })
                                    .map(|(sym, _)| sym.clone())
                                    .collect(),
                                intent: ClaimIntent::Edit,
                                last_seen_unix: entry.last_touched_unix_for(other),
                            });
                        }
                    }
                }

                if req.intent == ClaimIntent::Edit {
                    // File-level Edit collision: only conflicts with
                    // another agent's Edit-intent claim — at any scope
                    // (file-level OR symbol-level). A Read claim is a
                    // non-event per wishlist #5; if alice has only
                    // symbol-level Read claims and bob (us) wants to do
                    // file-level Edit, alice's observation isn't
                    // invalidated by our edit. (This was a residual
                    // defect after the first read-vs-edit pass: the
                    // lookup fell back to `Edit` for symbol-only
                    // holders, which both blocked a legitimate edit and
                    // reported a wrong intent on the conflict entry.)
                    if req.symbols.is_empty() {
                        for other in entry.agents.iter().filter(|a| *a != agent_id) {
                            // Resolve the holder's *strongest* intent
                            // at any scope: file-level first, then any
                            // symbol-level. Read everywhere → Read
                            // (no conflict). Any Edit → Edit (conflict,
                            // and the reported intent is the actual
                            // holder intent, not a synthetic default).
                            let other_intent = entry
                                .intent_for(other, "__file_level__")
                                .or_else(|| entry.any_symbol_intent(other))
                                .unwrap_or(ClaimIntent::Edit);
                            if other_intent != ClaimIntent::Edit {
                                continue;
                            }
                            req_conflicts.push(ConflictEntry {
                                agent_id: other.clone(),
                                inferred: entry.inferred.contains(other),
                                path: req.path.clone(),
                                symbols: vec![],
                                intent: other_intent,
                                last_seen_unix: entry.last_touched_unix_for(other),
                            });
                        }
                    } else {
                        // Symbol-level Edit: per-symbol conflict with
                        // existing Edit claims on the same symbol, and a
                        // single file-level conflict with any other agent
                        // whose file-level claim is Edit (they're
                        // rewriting the whole file).
                        for sym in &req.symbols {
                            if let Some(others) = entry.symbols.get(sym) {
                                for other in others.iter().filter(|a| *a != agent_id) {
                                    if entry.intent_for(other, sym) == Some(ClaimIntent::Edit) {
                                        req_conflicts.push(ConflictEntry {
                                            agent_id: other.clone(),
                                            inferred: entry.inferred.contains(other),
                                            path: req.path.clone(),
                                            symbols: vec![sym.clone()],
                                            intent: ClaimIntent::Edit,
                                            last_seen_unix: entry
                                                .last_touched_for(other, sym)
                                                .unwrap_or(SystemTime::UNIX_EPOCH),
                                        });
                                    }
                                }
                            }
                        }
                        // File-level existing claim: treat as a single
                        // file-level conflict (symbols: vec![]) so the
                        // caller sees one entry instead of one per
                        // requested symbol. Only fire when the existing
                        // file-level intent is Edit — a file-level Read
                        // is non-conflicting just like a symbol-level
                        // Read.
                        if let Some(file_level_agents) = entry.symbols.get("__file_level__") {
                            for other in file_level_agents
                                .iter()
                                .filter(|a| *a != agent_id)
                                .cloned()
                                .collect::<Vec<_>>()
                            {
                                if entry.intent_for(&other, "__file_level__")
                                    == Some(ClaimIntent::Edit)
                                {
                                    req_conflicts.push(ConflictEntry {
                                        agent_id: other.clone(),
                                        inferred: entry.inferred.contains(&other),
                                        path: req.path.clone(),
                                        symbols: vec![],
                                        intent: ClaimIntent::Edit,
                                        last_seen_unix: entry.last_touched_unix_for(&other),
                                    });
                                }
                            }
                        }
                    }
                }

                if req_conflicts.is_empty() {
                    let now = SystemTime::now();
                    // Apply: add agent to file; add to symbol sets; record
                    // intent and last-touched under each scope (real
                    // symbol name or the `__file_level__` sentinel).
                    // A declaration always wins over a guess; a guess
                    // never downgrades a declaration. So `inferred`
                    // marks only claims the agent did not already hold,
                    // while an explicit claim clears the marker outright
                    // — the agent has now said out loud what the watcher
                    // had only inferred.
                    let already_held = entry.agents.contains(agent_id);
                    entry.agents.insert(agent_id.clone());
                    if !inferred {
                        entry.inferred.remove(agent_id);
                    } else if !already_held {
                        entry.inferred.insert(agent_id.clone());
                    }
                    // Read the resolved flag now: `entry` borrows
                    // `s.by_file`, and the `Claim` below writes through
                    // `s.by_agent`.
                    let claim_is_inferred = entry.inferred.contains(agent_id);
                    if req.symbols.is_empty() {
                        entry
                            .symbols
                            .entry("__file_level__".into())
                            .or_default()
                            .insert(agent_id.clone());
                        entry
                            .intents
                            .entry("__file_level__".into())
                            .or_default()
                            .insert(agent_id.clone(), req.intent.clone());
                        entry
                            .last_touched
                            .entry("__file_level__".into())
                            .or_default()
                            .insert(agent_id.clone(), now);
                    } else {
                        for sym in &req.symbols {
                            entry
                                .symbols
                                .entry(sym.clone())
                                .or_default()
                                .insert(agent_id.clone());
                            entry
                                .intents
                                .entry(sym.clone())
                                .or_default()
                                .insert(agent_id.clone(), req.intent.clone());
                            entry
                                .last_touched
                                .entry(sym.clone())
                                .or_default()
                                .insert(agent_id.clone(), now);
                        }
                    }
                    // File-level claim (no specific symbols) carries no
                    // content hash; symbol-level claims hash the symbol's
                    // body bytes via the tree-sitter extractor. When the
                    // symbol can't be located (unsupported file type,
                    // unreadable file, etc.) we fall back to the all-zero
                    // placeholder so existing consumers still see
                    // `Some(SymbolHash)`.
                    let content_hash = if req.symbols.is_empty() {
                        None
                    } else {
                        let sym = req.symbols.first().map(|s| s.as_str()).unwrap_or("");
                        compute_symbol_hash(&req.path, sym).or_else(|| Some(SymbolHash::zero()))
                    };
                    // Translate the request's optional TTL into an absolute
                    // expiry timestamp. `None` means "no expiry set" and the
                    // claim is only released explicitly or when the agent's
                    // session expires.
                    let expires_at = req
                        .ttl_seconds
                        .map(|s| now + std::time::Duration::from_secs(s));
                    // Re-claiming a scope replaces the previous entry
                    // rather than appending beside it. Without this,
                    // an agent that claimed the same file twice — or
                    // whose declared claim was re-observed by the
                    // attribution watcher — accumulated duplicate rows
                    // in `my_claims`, inflating `claims_count` and
                    // leaving a stale `inferred` flag behind the fresh
                    // one.
                    let agent_claims = s.by_agent.entry(agent_id.clone()).or_default();
                    agent_claims.retain(|c| !(c.path == req.path && c.symbols == req.symbols));
                    agent_claims.push(Claim {
                        agent_id: agent_id.clone(),
                        path: req.path.clone(),
                        symbols: req.symbols.clone(),
                        content_hash,
                        intent: req.intent.clone(),
                        claimed_at: now,
                        last_touched_unix: now,
                        expires_at,
                        plan_revision: req.plan_revision,
                        inferred: claim_is_inferred,
                    });
                    granted.push(req);
                } else {
                    conflicts.extend(req_conflicts);
                }
            }

            (granted, conflicts, advisories)
        };
        if !granted.is_empty() {
            if let Some(cb) = self.cloned_persist_cb() {
                cb();
            }
        }
        ClaimResult {
            granted,
            conflicts,
            advisories,
            world_state: None,
        }
    }

    /// Refresh the `last_touched` timestamp on every claim this agent
    /// holds. Wired up by the MCP `heartbeat` handler so the staleness
    /// clock advances on each heartbeat instead of being frozen at
    /// `claimed_at`. Wishlist #5 fix: without this, conflict entries'
    /// `last_seen_unix` is identical to when the agent first claimed,
    /// and a "long-held" claim looks identical to a "just-stale" one.
    /// Separate from `PresenceRegistry::heartbeat` because the
    /// `OccupancyMap` has its own lock; the handler in `mcp/handler.rs`
    /// calls both under a single `Arc<LainServer>` coordination.
    pub fn touch(&self, agent_id: &AgentId) {
        let now = SystemTime::now();
        {
            let mut s = self.inner.lock();
            for entry in s.by_file.values_mut() {
                for per_agent in entry.last_touched.values_mut() {
                    if per_agent.contains_key(agent_id) {
                        per_agent.insert(agent_id.clone(), now);
                    }
                }
            }
        }
        self.refresh_locks_for_agent(agent_id);
    }

    pub(crate) fn refresh_locks_for_agent(&self, agent_id: &AgentId) {
        let locks: Vec<(PathBuf, PathBuf)> = {
            let leases = self.lock_leases.lock();
            leases
                .iter()
                .filter(|((a, _), _)| a == agent_id)
                .map(|((_, claim_path), lock_path)| (claim_path.clone(), lock_path.clone()))
                .collect()
        };
        for (claim_path, lock_path) in locks {
            match crate::server::presence_lock::refresh_lock_if_owned(&lock_path, agent_id) {
                crate::server::presence_lock::RefreshOutcome::Refreshed => {}
                crate::server::presence_lock::RefreshOutcome::Missing => {
                    tracing::warn!(
                        "advisory lock file {} was missing during heartbeat refresh for {}",
                        lock_path.display(),
                        agent_id.as_str(),
                    );
                    self.lock_leases
                        .lock()
                        .remove(&(agent_id.clone(), claim_path));
                }
                crate::server::presence_lock::RefreshOutcome::StolenBy(other) => {
                    tracing::warn!(
                        "advisory lock file {} was stolen by {} during heartbeat refresh for {}",
                        lock_path.display(),
                        other.as_str(),
                        agent_id.as_str(),
                    );
                    self.lock_leases
                        .lock()
                        .remove(&(agent_id.clone(), claim_path));
                }
                crate::server::presence_lock::RefreshOutcome::Error(e) => {
                    tracing::warn!(
                        "error refreshing advisory lock file {}: {e}",
                        lock_path.display(),
                    );
                }
            }
        }
    }

    pub fn release(&self, agent_id: &AgentId, paths: &[PathBuf]) -> Vec<PathBuf> {
        // Same canonicalization as `claim_in_memory`, so a release
        // spelled differently from the claim still finds it.
        let roots = self.claim_roots_snapshot();
        let paths: Vec<PathBuf> = paths
            .iter()
            .map(|p| canonical_claim_path(&roots, p))
            .collect();
        let released = {
            let mut s = self.inner.lock();
            let mut released = Vec::new();
            for path in &paths {
                if let Some(entry) = s.by_file.get_mut(path) {
                    entry.agents.remove(agent_id);
                    entry.inferred.remove(agent_id);
                    let syms_to_remove: Vec<String> = entry
                        .symbols
                        .iter()
                        .filter(|(_, agents)| agents.contains(agent_id))
                        .map(|(s, _)| s.clone())
                        .collect();
                    for s in syms_to_remove {
                        if let Some(set) = entry.symbols.get_mut(&s) {
                            set.remove(agent_id);
                            if set.is_empty() {
                                entry.symbols.remove(&s);
                            }
                        }
                        // Mirror the same key into the parallel
                        // intent / timestamp tracks so they don't
                        // outlive a now-empty symbol set. Without
                        // this, `intents_for(other, sym)` could
                        // return a stale intent for a scope the
                        // agent no longer holds.
                        if let Some(m) = entry.intents.get_mut(&s) {
                            m.remove(agent_id);
                            if m.is_empty() {
                                entry.intents.remove(&s);
                            }
                        }
                        if let Some(m) = entry.last_touched.get_mut(&s) {
                            m.remove(agent_id);
                            if m.is_empty() {
                                entry.last_touched.remove(&s);
                            }
                        }
                    }
                    if entry.agents.is_empty() && entry.symbols.is_empty() {
                        s.by_file.remove(path);
                    }
                    released.push(path.clone());
                }
            }
            if let Some(claims) = s.by_agent.get_mut(agent_id) {
                claims.retain(|c| !released.contains(&c.path));
            }
            released
        };
        for path in &paths {
            let maybe_lock = self
                .lock_leases
                .lock()
                .remove(&(agent_id.clone(), path.clone()));
            if let Some(lock_path) = maybe_lock {
                if let Err(e) =
                    crate::server::presence_lock::release_lock_if_owned(&lock_path, agent_id)
                {
                    tracing::warn!("failed to release lock file {}: {e}", lock_path.display());
                }
            }
        }
        if !released.is_empty() {
            if let Some(cb) = self.cloned_persist_cb() {
                cb();
            }
        }
        released
    }

    pub fn release_all_for(&self, agent_id: &AgentId) -> Vec<PathBuf> {
        let paths: Vec<PathBuf> = {
            let s = self.inner.lock();
            s.by_agent
                .get(agent_id)
                .map(|cs| cs.iter().map(|c| c.path.clone()).collect())
                .unwrap_or_default()
        };
        let released = self.release(agent_id, &paths);
        let remaining_locks: Vec<PathBuf> = {
            let mut leases = self.lock_leases.lock();
            let keys: Vec<(AgentId, PathBuf)> = leases
                .keys()
                .filter(|(a, _)| a == agent_id)
                .cloned()
                .collect();
            keys.into_iter().filter_map(|k| leases.remove(&k)).collect()
        };
        for lock_path in remaining_locks {
            if let Err(e) =
                crate::server::presence_lock::release_lock_if_owned(&lock_path, agent_id)
            {
                tracing::warn!("failed to release lock file {}: {e}", lock_path.display());
            }
        }
        // `self.release` already fired the persist callback when
        // `released` is non-empty, so we don't double-fire here.
        let _ = paths;
        released
    }

    /// Drop every claim whose `expires_at` is in the past and return the
    /// `(agent_id, path)` pairs that were removed so callers can fire
    /// `ClaimReleased` events. Mirrors the bookkeeping that `release`
    /// does: agent is unlinked from `by_file`'s agent set and the
    /// relevant symbol sets, and `by_file` entries are dropped when
    /// empty. Returns an empty vec when nothing expired.
    ///
    /// The persist callback fires (at most once) when the result vec
    /// is non-empty, matching the contract of `release` and
    /// `release_all_for`.
    pub fn expire_by_ttl(&self) -> Vec<(AgentId, PathBuf)> {
        let now = SystemTime::now();
        let released = {
            let mut s = self.inner.lock();
            let mut released: Vec<(AgentId, PathBuf)> = Vec::new();

            // Collect the claims to drop first so we don't mutate
            // `by_agent` while iterating it. Also capture the symbol
            // sets each released claim touched so we can clean up
            // `by_file`.
            let mut to_drop: Vec<(AgentId, PathBuf, Vec<String>)> = Vec::new();
            for (agent_id, claims) in s.by_agent.iter() {
                for c in claims.iter() {
                    let Some(exp) = c.expires_at else { continue };
                    // Wall-clock skew guard: `expires_at` is a
                    // `SystemTime`, which is non-monotonic — an NTP
                    // correction or container suspend can jump it
                    // backwards. The normal expiry check is `exp <=
                    // now`; the additional `now < claimed_at` arm
                    // fails-secure when the wall clock has jumped
                    // backwards past this claim's creation time
                    // (otherwise the claim would live forever until
                    // the clock catches up).
                    //
                    // The right long-term fix is to migrate
                    // `Claim::expires_at` to `Option<Instant>` (mono-
                    // tonic) and store `expires_at_unix` separately
                    // for serialization. That's a structural change
                    // touching every Claim constructor; the guard
                    // below is the surgical mitigation.
                    let expired = exp <= now || now < c.claimed_at;
                    if expired {
                        to_drop.push((agent_id.clone(), c.path.clone(), c.symbols.clone()));
                    }
                }
            }

            for (agent_id, path, symbols) in &to_drop {
                if let Some(entry) = s.by_file.get_mut(path) {
                    entry.agents.remove(agent_id);
                    entry.inferred.remove(agent_id);
                    // Remove the agent from any symbol set it claimed.
                    // For file-level claims (`symbols` empty) the
                    // bookkeeping lives under the `__file_level__`
                    // sentinel.
                    let symbol_keys: Vec<String> = if symbols.is_empty() {
                        vec!["__file_level__".into()]
                    } else {
                        symbols.clone()
                    };
                    for sym in &symbol_keys {
                        if let Some(set) = entry.symbols.get_mut(sym) {
                            set.remove(agent_id);
                            if set.is_empty() {
                                entry.symbols.remove(sym);
                            }
                        }
                        // Same shadow cleanup as in `release`: the
                        // intent / timestamp tracks must agree with
                        // `symbols` or risk leaving stale (agent,
                        // scope) pairs reachable to
                        // `intent_for` / `last_touched_for`.
                        if let Some(m) = entry.intents.get_mut(sym) {
                            m.remove(agent_id);
                            if m.is_empty() {
                                entry.intents.remove(sym);
                            }
                        }
                        if let Some(m) = entry.last_touched.get_mut(sym) {
                            m.remove(agent_id);
                            if m.is_empty() {
                                entry.last_touched.remove(sym);
                            }
                        }
                    }
                    if entry.agents.is_empty() && entry.symbols.is_empty() {
                        s.by_file.remove(path);
                    }
                }
                if let Some(claims) = s.by_agent.get_mut(agent_id) {
                    claims.retain(|c| {
                        !(c.path == *path && c.expires_at.map(|e| e <= now).unwrap_or(false))
                    });
                    if claims.is_empty() {
                        s.by_agent.remove(agent_id);
                    }
                }
                released.push((agent_id.clone(), path.clone()));
            }

            released
        };
        for (agent_id, path) in &released {
            let maybe_lock = self
                .lock_leases
                .lock()
                .remove(&(agent_id.clone(), path.clone()));
            if let Some(lock_path) = maybe_lock {
                if let Err(e) =
                    crate::server::presence_lock::release_lock_if_owned(&lock_path, agent_id)
                {
                    tracing::warn!("failed to release lock file {}: {e}", lock_path.display());
                }
            }
        }
        if !released.is_empty() {
            if let Some(cb) = self.cloned_persist_cb() {
                cb();
            }
        }
        released
    }

    /// Best-effort write of `<workspace>/.lain/locks/<hash>.json` for
    /// each path that was just granted with `ClaimIntent::Edit`. Called by
    /// `claim_with_session` after the in-memory bookkeeping settles.
    /// No-op when no workspace root is configured (unit tests,
    /// federation paths).
    ///
    /// The in-memory state is *not* rolled back if `try_lock` reports
    /// a conflict or an I/O error — both are logged via `tracing::warn`
    /// and the claim stands. Granted edit locks are registered in
    /// `lock_leases` and cleaned up on `release`, `release_all_for`,
    /// or `expire_by_ttl`.
    fn write_lock_files(&self, session: &AgentSession, granted: &[ClaimRequest]) {
        let Some(workspace) = self.workspace_root_snapshot() else {
            return;
        };
        for req in granted {
            if req.intent != ClaimIntent::Edit {
                continue;
            }
            let key = (session.id.clone(), req.path.clone());
            let existing_lock = self.lock_leases.lock().get(&key).cloned();
            if let Some(lp) = existing_lock {
                if let crate::server::presence_lock::RefreshOutcome::Refreshed =
                    crate::server::presence_lock::refresh_lock_if_owned(&lp, &session.id)
                {
                    continue;
                }
            }
            match crate::server::presence_lock::try_lock(
                &workspace,
                &req.path,
                &session.id,
                session.kind.clone(),
                req.intent.clone(),
            ) {
                Ok(lock) => {
                    self.lock_leases.lock().insert(key, lock.path);
                }
                Err(conflict) => {
                    if conflict.agent_id() == session.id {
                        let lp = crate::server::presence_lock::lock_path_for(&workspace, &req.path);
                        if let crate::server::presence_lock::RefreshOutcome::Refreshed =
                            crate::server::presence_lock::refresh_lock_if_owned(&lp, &session.id)
                        {
                            self.lock_leases.lock().insert(key, lp);
                            continue;
                        }
                    }
                    tracing::warn!(
                        "filesystem lock for {:?} already held by {} (k={:?}); in-memory claim stands",
                        req.path,
                        conflict.agent_id().as_str(),
                        conflict.kind(),
                    );
                }
            }
        }
    }

    pub fn list_for_path(&self, path: &Path) -> Option<OccupancyEntry> {
        // Readers canonicalize on the same rule as `claim`, so asking
        // "who is in this file?" with an absolute path finds a claim
        // taken with a relative one, and vice versa.
        let path = &canonical_claim_path(&self.claim_roots_snapshot(), path);
        let s = self.inner.lock();
        s.by_file.get(path).map(|entry| {
            let mut symbols: Vec<SymbolOccupancy> = entry
                .symbols
                .iter()
                .filter(|(s, _)| s.as_str() != "__file_level__")
                .map(|(sym, agents)| SymbolOccupancy {
                    symbol: sym.clone(),
                    agents: agents.iter().cloned().collect(),
                })
                .collect();
            symbols.sort_by(|a, b| a.symbol.cmp(&b.symbol));
            let mut holders: Vec<Holder> = entry
                .agents
                .iter()
                .map(|a| Holder {
                    agent_id: a.clone(),
                    // Strongest intent at any scope: file-level first,
                    // then any symbol-level. Read everywhere means read.
                    intent: entry
                        .intent_for(a, "__file_level__")
                        .or_else(|| entry.any_symbol_intent(a))
                        .unwrap_or(ClaimIntent::Read),
                    inferred: entry.inferred.contains(a),
                })
                .collect();
            holders.sort_by(|x, y| x.agent_id.as_str().cmp(y.agent_id.as_str()));
            OccupancyEntry {
                path: path.to_path_buf(),
                agents: entry.agents.iter().cloned().collect(),
                holders,
                symbols,
            }
        })
    }

    pub fn list_all(&self) -> Vec<OccupancyEntry> {
        // Snapshot the path set under the lock, then drop it before calling
        // `list_for_path`, which acquires the lock for itself. Mutex is not
        // reentrant, so calling back into `self.list_for_path` while holding
        // `s` would deadlock on the first iteration.
        let paths: Vec<std::path::PathBuf> = {
            let s = self.inner.lock();
            s.by_file.keys().cloned().collect()
        };
        paths.iter().filter_map(|p| self.list_for_path(p)).collect()
    }

    pub fn list_for_agent(&self, agent_id: &AgentId) -> Vec<Claim> {
        let s = self.inner.lock();
        s.by_agent.get(agent_id).cloned().unwrap_or_default()
    }
}

impl Default for OccupancyMap {
    fn default() -> Self {
        Self::new()
    }
}

/// Broadcast events emitted by the presence layer. `LainServer` owns the
/// sender; SSE handlers (Task 6) and any in-process subscribers clone the
/// receiver to stream these to clients.
///
/// Variants:
/// - `AgentJoined` — a new session was registered.
/// - `AgentLeft` — a session was explicitly removed (not via expiry).
/// - `HeartbeatExpired` — the expiry loop dropped a stale session.
/// - `ClaimGranted` / `ClaimReleased` — occupancy map changes.
/// - `ConflictDetected` — an occupancy claim came back with conflicts.
/// - `EditLanded` — a successful write path appended an `AuditEvent`
///   (PR 2 / Task 2.4). The wire JSON for this variant carries the
///   `EditLanded` tag wrapping the inner `AuditEvent`'s fields
///   (serde's external-tag default). Downstream consumers read the
///   audit data from `data["EditLanded"]`. The SSE frame's `event:`
///   field is set to `"edit_landed"`, so the stream shape is symmetric
///   with `get_audit_log`'s responses — both serialize the seven
///   `AuditEvent` fields under the same JSON keys.

/// Compute the BLAKE3-256 hash of a symbol's body bytes so the
/// occupancy layer can tell when the source under a claimed symbol
/// has changed. Reads the file, asks the tree-sitter extractor for
/// the symbol's `byte_start..byte_end`, and hashes that slice. Returns
/// `None` if the file is unreadable, non-UTF-8, or doesn't define the
/// symbol — callers fall back to `SymbolHash::zero()` so a stale
/// content_hash can never block a `claim`.
pub(crate) fn compute_symbol_hash(path: &Path, symbol: &str) -> Option<SymbolHash> {
    let bytes = std::fs::read(path).ok()?;
    let src = std::str::from_utf8(&bytes).ok()?;
    let defs = crate::server::treesitter::extract_definitions(path, src);
    let def = defs.into_iter().find(|d| d.name == symbol)?;
    let start = def.byte_start as usize;
    let end = def.byte_end as usize;
    if start > end || end > bytes.len() {
        return None;
    }
    Some(SymbolHash::from_bytes(&bytes[start..end]))
}
// ── WorldState / ChangedSymbol / ChangedKind (Task 1.5, PR 1) ────────────────
//
// The claim response carries a `world_state` snapshot so the caller can
// tell whether its plan is stale without a second round-trip. The shapes
// here are populated by the static-graph retract detector (Task 1.6)
// and surfaced on `ClaimResult`. `LookupResult` lives in
// `crate::server::revision_log` and is re-exported from `revision_log`
// for callers that want to reason about `diffs_since` outcomes.

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum ChangedKind {
    Edited,
    /// The symbol was in the graph and is not any more — something the
    /// caller was working on disappeared under it.
    Retracted,
    /// The graph has no record of this symbol at all. Distinct from
    /// `Retracted`, which used to cover both cases: asking about a name
    /// that is a match arm rather than a definition, or one added since
    /// the last index, returned `Retracted` and told the agent its
    /// target had been deleted. "I have never seen this" and "this was
    /// removed" call for opposite reactions.
    NotIndexed,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ChangedSymbol {
    pub name: String,
    pub change_kind: ChangedKind,
    pub at_revision: RevisionId,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct WorldState {
    pub current: RevisionId,
    pub plan: RevisionId,
    #[serde(default)]
    pub changed_symbols: Vec<ChangedSymbol>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl ChangedSymbol {
    /// Collapse a stream of `OverlayDiff`s into one `ChangedSymbol` per
    /// name, keeping the *latest* `at_revision` we saw for that name.
    ///
    /// The brief leaves `plan` unused in the helper — the caller in
    /// `run_claim_files` filters by the claim's paths/symbols after
    /// construction, so this just does the structural dedup. Returns
    /// `ChangedKind::Edited` for every entry: distinguishing retracted
    /// from edited is the static-graph retract detector's job
    /// (Task 1.6), which compares the diff against the indexed graph.
    pub fn from_diffs(
        diffs: &[crate::server::overlay::stream::OverlayDiff],
        _plan: RevisionId,
        _current: RevisionId,
    ) -> Vec<ChangedSymbol> {
        use std::collections::BTreeMap;
        let mut by_name: BTreeMap<String, RevisionId> = BTreeMap::new();
        for d in diffs {
            for n in &d.added {
                // `BTreeMap::insert` keeps the *latest* `d.revision`
                // because we iterate `diffs` in order; later diffs on
                // the same symbol overwrite earlier ones.
                by_name.insert(n.name.clone(), d.revision);
            }
            for n in &d.updated {
                by_name.insert(n.name.clone(), d.revision);
            }
        }
        by_name
            .into_iter()
            .map(|(name, at)| ChangedSymbol {
                name,
                change_kind: ChangedKind::Edited,
                at_revision: at,
            })
            .collect()
    }
}
