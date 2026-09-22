//! On-disk persistence for `PresenceRegistry` + `OccupancyMap`.
//!
//! Why free functions (not methods):
//! - Both `PresenceRegistry` and `OccupancyMap` are `Arc<Mutex<...>>` wrappers.
//!   Adding a method that takes a path clutters the type's contract with a
//!   filesystem concern; the persistence layer is genuinely orthogonal to the
//!   in-memory data structure.
//! - `LainServer` is the natural owner of the state path (it knows the
//!   workspace) and the natural caller; it can either drive the helpers
//!   explicitly via `save_state`/`load_state` or hand a closure that captures
//!   the path to the registries' `set_persist_callback` setters.
//!
//! Why the persist hooks don't capture `LainServer`:
//! - The hook closures need to be `'static + Send + Sync`. Capturing an
//!   `Arc<LainServer>` works in principle but creates a ref cycle (server ->
//!   registry -> closure -> server). Holding just the `Path` + clones of the
//!   `Arc<PresenceRegistry>` / `Arc<OccupancyMap>` keeps the lifecycle
//!   straightforward: as long as the registries live, the closure is valid.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{FileOccupancy, OccupancyMap, OccupancyState};
use super::claim::{Claim, ClaimIntent, SymbolHash};
use super::registry::{AgentSession, PresenceRegistry};

/// On-disk schema for `PresenceRegistry` + `OccupancyMap`. Fields are
/// `Vec<(K, V)>` rather than maps because serde-json's `HashMap`
/// representation is non-deterministic across runs; with tuples the
/// emitted file is stable to hand-inspection.
type OccupancySnapshot = Vec<(PathBuf, Vec<String>, Vec<(String, Vec<String>)>)>;

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedState {
    /// `(agent_id_string, session)`.
    sessions: Vec<(String, AgentSession)>,
    /// `(path, file_level_agents, [(symbol, agents)])`. The
    /// `__file_level__` sentinel that lives in the in-memory symbol
    /// map is filtered out before serialization; the file-level agents
    /// list is derived directly from `FileOccupancy::agents`.
    occupancy_by_file: OccupancySnapshot,
    /// `(path, [(agent_id, intent)])`. File-level `ClaimIntent`
    /// records — the only intents that survive a save/load round-trip.
    /// Symbol-level intents (`claims` whose `symbols` field names a
    /// specific definition) are not persisted because the
    /// `(sym, agents)` shape in `occupancy_by_file` already records
    /// which agent touched which symbol; the file-level intent is the
    /// only one the cross-process presence layer can't reconstruct
    /// from `agents` alone. Without this, an edit claim loaded from
    /// disk looks intentless to a peer's read claim, and the
    /// advisory branch of `OccupancyMap::claim_in_memory` skips
    /// the warning (P1 bug surfaced by `tests/multi_agent_concurrency`).
    #[serde(default)]
    occupancy_file_intents: Vec<(PathBuf, Vec<(String, ClaimIntent)>)>,
    /// `(agent_id_string, [claim])`. Mirrored into `by_file` on load.
    occupancy_by_agent: Vec<(String, Vec<Claim>)>,
    /// Offset (in bytes) into `audit.jsonl` at which the next audit
    /// append should start on the next restart. Task 2.6 reads this
    /// out of the audit module on save and writes it back on load so
    /// crash-safe append continuation crosses process boundaries.
    #[serde(default)]
    audit_offset_bytes: u64,
    /// Unix-epoch seconds at which `audit.jsonl` was last reset
    /// because it was missing or corrupt on load. `None` until
    /// Task 2.6 wires up the loader's reset detection.
    #[serde(default)]
    audit_reset_at_unix: Option<f64>,
    /// Per-agent intent declarations. `(agent_id, intent)`. The
    /// registry invariant is "one intent per agent", but the on-disk
    /// shape is a flat list so a stale state file with a duplicate
    /// entry does not crash the loader — the loader picks the most
    /// recent entry per agent and drops the rest. `#[serde(default)]`
    /// keeps backward-compat with state files written before the
    /// intent layer landed (PR 1 of `docs/INTENT_AND_OBSERVABILITY_PLAN.md`).
    #[serde(default)]
    intents: Vec<(String, crate::server::intent::Intent)>,
    /// Per-agent observed tool-call activity. `(agent_id, activity)`.
    /// The `Activity::recent_tools` ring buffer is FIFO-capped at
    /// 100 entries, so a stale entry's payload stays bounded even
    /// after a long-running session. `#[serde(default)]` for the
    /// same backward-compat reason as `intents`.
    #[serde(default)]
    activities: Vec<(String, crate::server::activity::Activity)>,
}

/// Serialize the in-memory presence registry + occupancy map to a JSON
/// file at `path`. The write is atomic: serialise to `path.tmp` first,
/// then `rename` over `path`. Returns a string error on any IO / JSON
/// failure; callers wrap as needed.
///
/// The `audit_offset_bytes` field is populated from the live
/// `audit.jsonl` file (sibling of `path` under the same state
/// directory) at save time — Task 2.6 wiring. The state file is
/// always co-located with the audit log on disk (see
/// `LainServer::state_dir_for_audit`), so `path.parent()` is the
/// correct audit directory in every production code path. A bare
/// filename with no parent (which `LainServer::state_path` never
/// produces, but tests might) falls back to the current dir, which
/// at worst yields a `0` offset for a missing audit log.
pub fn save_pair(
    path: &Path,
    reg: &PresenceRegistry,
    occ: &OccupancyMap,
    intent: &crate::server::intent::IntentRegistry,
    activity: &crate::server::activity::ActivityTracker,
) -> Result<(), String> {
    // Task 2.6 — read the live audit log size now so the value
    // persisted on this save reflects "how much audit data was on
    // disk at the moment of this write," not a placeholder. The
    // sibling relationship between the state file and the audit log
    // holds in production; the parent-unwrap_or("") fallback keeps
    // this safe even for synthetic test paths with no parent.
    let audit_dir: PathBuf = path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from(""));
    let audit_offset_bytes = crate::server::audit::current_offset_bytes(&audit_dir);

    let state = {
        let s = reg.inner.lock();
        let o = occ.inner.lock();
        let intents_snapshot = intent.snapshot();
        let activities_snapshot = activity.snapshot();
        PersistedState {
            sessions: s
                .sessions
                .iter()
                .map(|(k, v)| (k.0.clone(), v.clone()))
                .collect(),
            occupancy_by_file: o
                .by_file
                .iter()
                .map(|(p, fo)| {
                    let agents: Vec<String> = fo.agents.iter().map(|a| a.0.clone()).collect();
                    let symbols: Vec<(String, Vec<String>)> = fo
                        .symbols
                        .iter()
                        .filter(|(sym, _)| sym.as_str() != "__file_level__")
                        .map(|(sym, agents)| {
                            (sym.clone(), agents.iter().map(|a| a.0.clone()).collect())
                        })
                        .collect();
                    (p.clone(), agents, symbols)
                })
                .collect(),
            // Save file-level intents. `__file_level__` is the only
            // sentinel key on `intents`; symbol-level entries are
            // reconstructed on demand from the `(sym, agents)`
            // entries above and the agents' recorded `claim_set`
            // (see `load_pair`). Mirrors the comment on
            // `PersistedState::occupancy_file_intents`.
            occupancy_file_intents: o
                .by_file
                .iter()
                .map(|(p, fo)| {
                    let entries: Vec<(String, ClaimIntent)> = fo
                        .intents
                        .get("__file_level__")
                        .map(|per_agent| {
                            per_agent
                                .iter()
                                .map(|(a, i)| (a.0.clone(), i.clone()))
                                .collect()
                        })
                        .unwrap_or_default();
                    (p.clone(), entries)
                })
                .filter(|(_, entries)| !entries.is_empty())
                .collect(),
            occupancy_by_agent: o
                .by_agent
                .iter()
                .map(|(k, v)| (k.0.clone(), v.clone()))
                .collect(),
            // Task 2.6 — these fields are now driven by the audit
            // module instead of placeholders. `audit_offset_bytes`
            // is the live size of `audit.jsonl`; `audit_reset_at_unix`
            // is set by `load_pair` when it detects a missing or
            // unreadable audit log on the way in, and simply
            // round-trips here on the way out. Additive-compat
            // (state files from before Task 2.2 still load via
            // `#[serde(default)]`).
            audit_offset_bytes,
            audit_reset_at_unix: None,
            // Intent layer (PR 1 of `docs/INTENT_AND_OBSERVABILITY_PLAN.md`):
            // a flat `(agent_id, intent)` list. Each agent has at
            // most one intent in memory; if a stale state file
            // somehow has duplicates, the loader picks the most
            // recent per agent.
            intents: intents_snapshot
                .into_iter()
                .map(|(id, i)| (id.0, i))
                .collect(),
            // Activity layer: per-agent observed tool calls. The
            // ring buffer on `Activity` is bounded to ~100 entries
            // so a single agent's payload stays small.
            activities: activities_snapshot
                .into_iter()
                .map(|(id, a)| (id.0, a))
                .collect(),
        }
    };
    let json = serde_json::to_string_pretty(&state)
        .map_err(|e| format!("serialize PersistedState: {e}"))?;
    crate::cli::io::write_file_atomic(path, json.as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

/// Hydrate `reg` and `occ` from a JSON file previously written by
/// `save_pair`. When `path` does not exist this is a no-op (the
/// registries stay untouched).
///
/// On a successful read, prior contents of `reg` / `occ` are replaced
/// with the persisted snapshot, ensuring ghost sessions and stale
/// claims do not survive across reloads. If reading or parsing fails,
/// live state remains untouched.
///
/// Task 2.6: after a successful parse, if the live `audit.jsonl` is
/// missing or unreadable in the state directory (`path.parent()`),
/// the loader rewrites the state file with `audit_offset_bytes = 0`
/// and `audit_reset_at_unix = Some(now)`. The spec calls for a WARN
/// here; we surface it through `tracing::warn!` so operators see it
/// in the server log. The next `save_pair` then persists the reset
/// timestamp out to the world; subsequent restarts see the marker
/// and don't re-warn.
///
/// Returns the list of `PresenceEvent::ClaimRevoked { reason:
/// "stale_owner" }` events the caller must publish on the
/// presence broadcast channel. These are claims whose owner is no
/// longer in `PresenceRegistry::sessions` after a fresh load — i.e.
/// the agent's process is gone but its claims were never released.
/// Without this cross-check the new server would refuse every
/// competing claim on those scopes (linearizability violation across
/// server crashes), so the load itself reclaims them and tells the
/// world via SSE.
pub fn load_pair(
    path: &Path,
    reg: &PresenceRegistry,
    occ: &OccupancyMap,
    intent: &crate::server::intent::IntentRegistry,
    activity: &crate::server::activity::ActivityTracker,
) -> Result<Vec<crate::server::presence::PresenceEvent>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let json =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut state: PersistedState =
        serde_json::from_str(&json).map_err(|e| format!("parse {}: {e}", path.display()))?;

    // Task 2.6 — audit log present-or-not check + reset rewrite,
    // before we start consuming `state`'s `Vec` fields below. The
    // same `path.parent()` rule from `save_pair` applies: the state
    // file and audit log are siblings under the state directory,
    // and a bare path with no parent falls back to the current dir
    // for the check (which yields a fresh "missing" verdict,
    // triggering the reset — correct, since no audit log is
    // colocated there). Doing the rewrite here keeps `state` fully
    // owned so we can `&state` for the on-disk rewrite; the on-disk
    // marker is independent of the in-memory hydration that follows
    // so the order doesn't matter for the data flow.
    let audit_dir: PathBuf = path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from(""));
    if !crate::server::audit::audit_log_present_and_readable(&audit_dir) {
        tracing::warn!(
            "audit log missing or unreadable at {}; resetting audit_offset_bytes and stamping audit_reset_at_unix",
            audit_dir.join(crate::server::audit::AUDIT_LOG_FILENAME).display(),
        );
        state.audit_offset_bytes = 0;
        state.audit_reset_at_unix = Some(crate::server::time::now_unix_f64());
        // Persist the reset marker immediately so a crash between
        // load and the first save doesn't lose it. The write goes
        // through the same atomic-rename path as `save_pair` so a
        // half-written state file can't be observed by a concurrent
        // reader. A concurrent mutator racing the rewrite would
        // still write its own (possibly newer) state on top of ours
        // — that's the same race the regular save path already
        // accepts, so it doesn't widen the surface here.
        let json = serde_json::to_string_pretty(&state)
            .map_err(|e| format!("serialize PersistedState (reset): {e}"))?;
        crate::cli::io::write_file_atomic(path, json.as_bytes())
            .map_err(|e| format!("write {}: {e}", path.display()))?;
    }

    // Stage parsed snapshot into temporary collections before locking
    // or mutating live state. If parsing or validation fails, live state
    // remains untouched.
    let mut new_sessions = HashMap::new();
    let mut new_by_token = HashMap::new();
    for (k, sess) in state.sessions {
        new_sessions.insert(super::agent::AgentId(k.clone()), sess.clone());
        new_by_token.insert(sess.session_token, super::agent::AgentId(k));
    }
    let mut new_by_file: HashMap<PathBuf, FileOccupancy> = HashMap::new();
    for (path_str, agents, symbols) in state.occupancy_by_file {
        let pb = path_str;
        let entry = new_by_file.entry(pb).or_default();
        for a in agents {
            entry.agents.insert(super::agent::AgentId(a));
        }
        for (sym, agent_ids) in symbols {
            let set = entry.symbols.entry(sym).or_default();
            for a in agent_ids {
                set.insert(super::agent::AgentId(a));
            }
        }
    }
    // Restore file-level intents so a peer's edit claim is visible
    // as Edit (not as "no intent recorded") after a load.
    for (path_str, intents) in state.occupancy_file_intents {
        let pb = path_str;
        let entry = new_by_file.entry(pb).or_default();
        let per_agent = entry
            .intents
            .entry("__file_level__".to_string())
            .or_default();
        for (agent_id, intent) in intents {
            per_agent.insert(super::agent::AgentId(agent_id), intent);
        }
    }
    let mut new_by_agent = HashMap::new();
    for (k, claims) in state.occupancy_by_agent {
        let agent_id = super::agent::AgentId(k.clone());
        for claim in &claims {
            let entry = new_by_file.entry(claim.path.clone()).or_default();
            if claim.symbols.is_empty() {
                entry
                    .intents
                    .entry("__file_level__".to_string())
                    .or_default()
                    .insert(agent_id.clone(), claim.intent.clone());
                entry
                    .last_touched
                    .entry("__file_level__".to_string())
                    .or_default()
                    .insert(agent_id.clone(), claim.last_touched_unix);
            } else {
                for sym in &claim.symbols {
                    entry
                        .intents
                        .entry(sym.clone())
                        .or_default()
                        .insert(agent_id.clone(), claim.intent.clone());
                    entry
                        .last_touched
                        .entry(sym.clone())
                        .or_default()
                        .insert(agent_id.clone(), claim.last_touched_unix);
                }
            }
        }
        new_by_agent.insert(agent_id, claims);
    }

    let mut s = reg.inner.lock();
    let mut o = occ.inner.lock();
    s.sessions = new_sessions;
    s.by_token = new_by_token;
    o.by_file = new_by_file;
    o.by_agent = new_by_agent;
    occ.lock_leases.lock().retain(|(agent, path), _| {
        o.by_agent
            .get(agent)
            .map(|cs| cs.iter().any(|c| &c.path == path))
            .unwrap_or(false)
    });
    drop(s);
    drop(o);

    // Intent layer (PR 1 of
    // `docs/INTENT_AND_OBSERVABILITY_PLAN.md`). The on-disk shape is
    // `(agent_id, Intent)`. The registry's `replace_all` handles
    // deduplication per agent (most-recent `updated_at` wins) so a
    // stale state file with duplicates is reconciled.
    let intents: Vec<crate::server::intent::Intent> = state
        .intents
        .into_iter()
        .map(|(id_str, i)| {
            let mut i = i;
            // Defensive: state-file entries carry `agent_id` inside
            // the Intent; use the on-disk agent_id (the tuple key)
            // to overwrite any drift in the inner field.
            i.agent_id = super::agent::AgentId(id_str);
            i
        })
        .collect();
    intent.replace_all(intents);

    // Activity layer: each `(agent_id, Activity)` pair is restored
    // verbatim. The `replace_all` helper overwrites the entry's
    // agent_id with the map key so the two stay in sync.
    let activities: Vec<(super::agent::AgentId, crate::server::activity::Activity)> = state
        .activities
        .into_iter()
        .map(|(id_str, a)| (super::agent::AgentId(id_str), a))
        .collect();
    activity.replace_all(activities);

    // Linearizability across server crashes (variant 1 of
    // `scripts/agy_chaos.sh`): every claim whose `agent_id` is not
    // in `s.sessions` is an orphan — its owner is gone but the claim
    // survived the persistence round-trip. The lock layer's
    // stale-after-takeover window would eventually let a competing
    // agent in via the filesystem sentinel, but the in-memory
    // `OccupancyMap` is checked first and the orphan claim would
    // block the competing agent indefinitely. So drop the orphans
    // here and emit one `ClaimRevoked` per reclaimed path so SSE
    // subscribers see the same view the new server has.
    //
    // The cross-check happens after both `o.by_file` and `o.by_agent`
    // are populated so we can prune consistently. The `lock_leases`
    // retain above already drops filesystem lock entries that no
    // longer match a live `o.by_agent` claim, so it falls into line.
    let stale_events: Vec<crate::server::presence::PresenceEvent> = {
        let s_guard = reg.inner.lock();
        let mut o_guard = occ.inner.lock();
        let mut revoked: Vec<crate::server::presence::PresenceEvent> = Vec::new();
        let orphan_agents: Vec<super::agent::AgentId> = o_guard
            .by_agent
            .keys()
            .filter(|agent_id| !s_guard.sessions.contains_key(agent_id))
            .cloned()
            .collect();
        for agent_id in orphan_agents {
            // Take the orphan's claims out of `by_agent` first; the
            // claim list is what we iterate to clean up `by_file`.
            if let Some(claims) = o_guard.by_agent.remove(&agent_id) {
                for claim in &claims {
                    if let Some(entry) = o_guard.by_file.get_mut(&claim.path) {
                        entry.agents.remove(&agent_id);
                        // Drop every symbol-level entry the agent
                        // touched. Empty file-level agents means
                        // `__file_level__` stays around only if
                        // another agent still holds the file.
                        for sym in claim
                            .symbols
                            .iter()
                            .chain(std::iter::once(&"__file_level__".to_string()))
                        {
                            if let Some(set) = entry.symbols.get_mut(sym) {
                                set.remove(&agent_id);
                                if set.is_empty() {
                                    entry.symbols.remove(sym);
                                }
                            }
                            if let Some(intents) = entry.intents.get_mut(sym) {
                                intents.remove(&agent_id);
                                if intents.is_empty() {
                                    entry.intents.remove(sym);
                                }
                            }
                            if let Some(touched) = entry.last_touched.get_mut(sym) {
                                touched.remove(&agent_id);
                                if touched.is_empty() {
                                    entry.last_touched.remove(sym);
                                }
                            }
                        }
                        if entry.agents.is_empty()
                            && entry.symbols.is_empty()
                            && entry.intents.is_empty()
                            && entry.last_touched.is_empty()
                        {
                            o_guard.by_file.remove(&claim.path);
                        }
                    }
                    revoked.push(crate::server::presence::PresenceEvent::ClaimRevoked {
                        agent_id: agent_id.clone(),
                        path: claim.path.clone(),
                        reason: "stale_owner".to_string(),
                    });
                }
            }
        }
        revoked
    };

    Ok(stale_events)
}

/// Compute the BLAKE3-256 `SymbolHash` of the body bytes for `symbol`
/// in `path`. The body is the exact byte range of the symbol's
/// tree-sitter definition (`byte_start..byte_end`), sliced directly
/// from the file's raw bytes — no line splitting, no CRLF normalization,
/// no `String` round-trip. This way two symbols on one line get
/// distinct hashes, and editing one symbol doesn't shift another
/// symbol's hash.
///
/// Returns `None` when the file is unreadable, not valid UTF-8, the
/// language isn't supported by the tree-sitter extractor, the symbol
/// isn't defined in the file, or the recorded byte range falls
/// outside the file (which shouldn't happen for a freshly parsed
/// file but is defended against anyway). Callers fall back to
/// `Some(SymbolHash::zero())` when they need a non-None hash for
/// `Claim.content_hash`.
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

#[cfg(test)]
mod audit_persistence_tests {
    //! Round-trip tests for the new `audit_offset_bytes` /
    //! `audit_reset_at_unix` fields on `PersistedState` (Task 2.2).
    //!
    //! These live alongside the type so the on-disk shape can't drift
    //! from the implementation without a test failure. The struct
    //! fields are private to the module, so we test from inside rather
    //! than via the `tests/` integration tree — that way we can assert
    //! on the field values directly.
    use super::*;

    /// Thin test wrappers around `save_pair` / `load_pair` so the
    /// existing audit tests don't have to thread empty intent /
    /// activity registries through every call site. PR 1 of
    /// `docs/INTENT_AND_OBSERVABILITY_PLAN.md` extended the saver
    /// and loader signatures to take the two new registries; the
    /// intent and activity payload is irrelevant for these tests
    /// because they assert on presence + occupancy fields only.
    fn save_pair_legacy(
        path: &Path,
        reg: &PresenceRegistry,
        occ: &OccupancyMap,
    ) -> Result<(), String> {
        save_pair(
            path,
            reg,
            occ,
            &crate::server::intent::IntentRegistry::new(),
            &crate::server::activity::ActivityTracker::new(),
        )
    }

    fn load_pair_legacy(
        path: &Path,
        reg: &PresenceRegistry,
        occ: &OccupancyMap,
    ) -> Result<Vec<crate::server::presence::PresenceEvent>, String> {
        load_pair(
            path,
            reg,
            occ,
            &crate::server::intent::IntentRegistry::new(),
            &crate::server::activity::ActivityTracker::new(),
        )
    }
    use std::fs;

    #[test]
    fn audit_offset_and_reset_round_trip_through_persisted_state() {
        // Task 2.2: `audit_offset_bytes` + `audit_reset_at_unix` are new
        // additive fields on `PersistedState`. They must round-trip
        // through serde so the audit module can resume append safely
        // after a restart.
        let json = r#"{
            "sessions": [],
            "occupancy_by_file": [],
            "occupancy_by_agent": [],
            "audit_offset_bytes": 12345,
            "audit_reset_at_unix": 1700000000.5
        }"#;
        let state: PersistedState =
            serde_json::from_str(json).expect("PersistedState should accept audit fields");
        assert_eq!(state.audit_offset_bytes, 12345);
        assert_eq!(state.audit_reset_at_unix, Some(1700000000.5));
    }

    #[test]
    fn pre_task_2_2_state_loads_with_defaults() {
        // State files written before Task 2.2 don't have the audit
        // fields. `#[serde(default)]` lets them load with `0` / `None`
        // instead of failing the parser — no migration required.
        let json = r#"{
            "sessions": [],
            "occupancy_by_file": [],
            "occupancy_by_agent": []
        }"#;
        let state: PersistedState = serde_json::from_str(json)
            .expect("Legacy state files without audit fields must still load");
        assert_eq!(state.audit_offset_bytes, 0);
        assert_eq!(state.audit_reset_at_unix, None);
    }

    #[test]
    fn save_pair_writes_audit_fields_with_placeholder_defaults() {
        // For Task 2.2 the audit module isn't wired up yet, so the
        // values written to disk are placeholders (`0` / `None`). Task
        // 2.6 swaps these for live audit-module values. We still want
        // the round-trip through `save_pair` / a JSON re-parse to
        // succeed and emit both fields — that way the on-disk shape is
        // stable from this commit onward.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let reg = PresenceRegistry::new();
        let occ = OccupancyMap::new();
        save_pair_legacy(&path, &reg, &occ).expect("save_pair");
        let written = fs::read_to_string(&path).unwrap();
        assert!(
            written.contains("\"audit_offset_bytes\""),
            "save_pair must emit audit_offset_bytes; got:\n{written}"
        );
        assert!(
            written.contains("\"audit_reset_at_unix\""),
            "save_pair must emit audit_reset_at_unix; got:\n{written}"
        );

        // Round-trip back through `load_pair` -> PersistedState with no
        // parse error, then double-check we read what we wrote.
        load_pair_legacy(&path, &reg, &occ).expect("load_pair");
        let parsed: PersistedState = serde_json::from_str(&written).unwrap();
        assert_eq!(parsed.audit_offset_bytes, 0);
        assert_eq!(parsed.audit_reset_at_unix, None);
    }

    /// Task 2.6 / brief: `save_pair` must read the current size of
    /// `audit.jsonl` (its sibling under the same state directory) and
    /// emit that as `audit_offset_bytes`, not the placeholder `0`.
    /// Pre-create the audit log with a known size, call `save_pair`,
    /// re-parse the state file, and assert the offset matches.
    #[test]
    fn offset_round_trips_across_state_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let audit_path = dir.path().join(crate::server::audit::AUDIT_LOG_FILENAME);

        // 12345 bytes of known sentinel content. The exact byte
        // count is what the test pins — `save_pair` must surface
        // this on disk, not a placeholder.
        const EXPECTED: u64 = 12_345;
        std::fs::write(&audit_path, vec![b'x'; EXPECTED as usize]).unwrap();

        let reg = PresenceRegistry::new();
        let occ = OccupancyMap::new();
        save_pair_legacy(&state_path, &reg, &occ).expect("save_pair");

        let written = fs::read_to_string(&state_path).unwrap();
        let parsed: PersistedState =
            serde_json::from_str(&written).expect("state file must round-trip after save");
        assert_eq!(
            parsed.audit_offset_bytes, EXPECTED,
            "save_pair must read audit.jsonl size and emit it as audit_offset_bytes; \
             got {} expected {} (state file:\n{written})",
            parsed.audit_offset_bytes, EXPECTED,
        );
    }

    /// Task 2.6 / spec: if `audit.jsonl` is missing on load, the
    /// loader must mark `audit_reset_at_unix` with a recent timestamp
    /// so the next save persists the reset, and `get_audit_log`
    /// consumers can report the gap. This test pre-writes a state
    /// file with `audit_reset_at_unix: None`, runs `load_pair` with
    /// no audit file present, and asserts the state file now carries
    /// a reset timestamp.
    #[test]
    fn load_pair_marks_reset_when_audit_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        // No `audit.jsonl` is created — the missing-file case is
        // the entire point of the test.
        assert!(!dir
            .path()
            .join(crate::server::audit::AUDIT_LOG_FILENAME)
            .exists());

        // Seed a state file with a prior offset and no reset marker
        // (the "pre-reset" state: we thought we had an audit log
        // pointing at byte 9999, but it's gone).
        let seeded = serde_json::json!({
            "sessions": [],
            "occupancy_by_file": [],
            "occupancy_by_agent": [],
            "audit_offset_bytes": 9_999_u64,
            "audit_reset_at_unix": serde_json::Value::Null,
        });
        std::fs::write(&state_path, serde_json::to_string_pretty(&seeded).unwrap()).unwrap();

        let reg = PresenceRegistry::new();
        let occ = OccupancyMap::new();
        load_pair_legacy(&state_path, &reg, &occ).expect("load_pair");

        // The state file on disk must now have `audit_reset_at_unix`
        // set to a recent timestamp (not null). The loader rewrites
        // the file when it detects the missing audit log.
        let after = fs::read_to_string(&state_path).unwrap();
        let parsed: PersistedState = serde_json::from_str(&after)
            .expect("state file must round-trip after load-induced reset");
        let reset = parsed
            .audit_reset_at_unix
            .expect("load_pair must set audit_reset_at_unix when audit.jsonl is missing");
        let now = crate::server::time::now_unix_f64();
        assert!(
            (now - reset).abs() < 5.0,
            "reset timestamp should be recent: reset={reset} now={now}",
        );
        // The offset is also reset to 0 (the spec says "reset offset
        // to 0" when the audit log is missing).
        assert_eq!(
            parsed.audit_offset_bytes, 0,
            "load_pair must reset audit_offset_bytes to 0 when audit.jsonl is missing",
        );
    }

    /// Counterpart of the previous test: when `audit.jsonl` IS
    /// present on load, `load_pair` must not clobber the persisted
    /// offset or stamp a spurious reset. Existing offset survives.
    #[test]
    fn load_pair_preserves_offset_when_audit_file_present() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let audit_path = dir.path().join(crate::server::audit::AUDIT_LOG_FILENAME);
        // Create a 100-byte audit log so the file exists and is
        // readable; the loader must not flag a reset.
        std::fs::write(&audit_path, vec![b'x'; 100]).unwrap();

        let seeded = serde_json::json!({
            "sessions": [],
            "occupancy_by_file": [],
            "occupancy_by_agent": [],
            "audit_offset_bytes": 100_u64,
            "audit_reset_at_unix": serde_json::Value::Null,
        });
        std::fs::write(&state_path, serde_json::to_string_pretty(&seeded).unwrap()).unwrap();

        let reg = PresenceRegistry::new();
        let occ = OccupancyMap::new();
        load_pair_legacy(&state_path, &reg, &occ).expect("load_pair");

        let after = fs::read_to_string(&state_path).unwrap();
        let parsed: PersistedState = serde_json::from_str(&after).unwrap();
        assert_eq!(
            parsed.audit_offset_bytes, 100,
            "load_pair must preserve the persisted offset when audit.jsonl exists",
        );
        assert!(
            parsed.audit_reset_at_unix.is_none(),
            "load_pair must not stamp a reset when audit.jsonl is present",
        );
    }

    #[test]
    fn load_pair_restores_snapshot_and_drops_unpersisted_sessions_and_claims() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let audit_path = dir.path().join(crate::server::audit::AUDIT_LOG_FILENAME);
        std::fs::write(&audit_path, b"audit").unwrap();

        let reg = PresenceRegistry::new();
        let occ = OccupancyMap::new();

        // 1. Initial live state: alice has a session and claim on foo.rs
        let alice_sess = reg.register(
            "alice".into(),
            super::registry::AgentKind::ClaudeCode,
            super::registry::AgentMode::Interactive,
            None,
            None,
        );
        let alice_id = alice_sess.id.clone();
        occ.claim(
            &alice_id,
            vec![crate::server::presence::ClaimRequest {
                path: PathBuf::from("foo.rs"),
                symbols: vec![],
                intent: ClaimIntent::Edit,
                ttl_seconds: None,
                plan_revision: None,
            }],
        );
        assert!(reg.get(&alice_id).is_some());
        assert!(occ.list_for_path(Path::new("foo.rs")).is_some());

        // 2. Prepare state on disk representing another snapshot: only bob has a session and claim on bar.rs
        let bob_sess = AgentSession::new(
            super::agent::AgentId("bob-123".into()),
            "bob".into(),
            super::registry::AgentKind::ClaudeCode,
            super::registry::AgentMode::Interactive,
            None,
            None,
        );
        let disk_reg = PresenceRegistry::new();
        let disk_occ = OccupancyMap::new();
        {
            let mut s = disk_reg.inner.lock();
            s.sessions.insert(bob_sess.id.clone(), bob_sess.clone());
            s.by_token
                .insert(bob_sess.session_token.clone(), bob_sess.id.clone());
        }
        disk_occ.claim(
            &bob_sess.id,
            vec![crate::server::presence::ClaimRequest {
                path: PathBuf::from("bar.rs"),
                symbols: vec![],
                intent: ClaimIntent::Edit,
                ttl_seconds: None,
                plan_revision: None,
            }],
        );
        save_pair_legacy(&state_path, &disk_reg, &disk_occ).expect("save_pair");

        // 3. Load snapshot into live reg & occ
        load_pair_legacy(&state_path, &reg, &occ).expect("load_pair");

        // Stale session and claim for alice must be GONE (restored snapshot, not additive merge)
        assert!(
            reg.get(&alice_id).is_none(),
            "alice should have disappeared after loading snapshot"
        );
        assert!(
            occ.list_for_path(Path::new("foo.rs")).is_none(),
            "foo.rs claim should have disappeared"
        );

        // Bob's session and claim must be present
        assert!(reg.get(&bob_sess.id).is_some(), "bob should be restored");
        assert!(
            occ.list_for_path(Path::new("bar.rs")).is_some(),
            "bar.rs claim should be restored"
        );
    }

    #[test]
    fn load_pair_preserves_live_state_on_malformed_json() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        std::fs::write(&state_path, b"{ not valid json").unwrap();

        let reg = PresenceRegistry::new();
        let occ = OccupancyMap::new();
        let alice_sess = reg.register(
            "alice".into(),
            super::registry::AgentKind::ClaudeCode,
            super::registry::AgentMode::Interactive,
            None,
            None,
        );
        let alice_id = alice_sess.id.clone();
        occ.claim(
            &alice_id,
            vec![crate::server::presence::ClaimRequest {
                path: PathBuf::from("foo.rs"),
                symbols: vec![],
                intent: ClaimIntent::Edit,
                ttl_seconds: None,
                plan_revision: None,
            }],
        );

        let res = load_pair_legacy(&state_path, &reg, &occ);
        assert!(res.is_err(), "load_pair must error on invalid json");

        // Live state must be completely untouched
        assert!(reg.get(&alice_id).is_some());
        assert!(occ.list_for_path(Path::new("foo.rs")).is_some());
    }

    #[test]
    fn load_pair_preserves_live_state_on_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let missing_path = dir.path().join("does_not_exist.json");

        let reg = PresenceRegistry::new();
        let occ = OccupancyMap::new();
        let alice_sess = reg.register(
            "alice".into(),
            super::registry::AgentKind::ClaudeCode,
            super::registry::AgentMode::Interactive,
            None,
            None,
        );
        let alice_id = alice_sess.id.clone();
        occ.claim(
            &alice_id,
            vec![crate::server::presence::ClaimRequest {
                path: PathBuf::from("foo.rs"),
                symbols: vec![],
                intent: ClaimIntent::Edit,
                ttl_seconds: None,
                plan_revision: None,
            }],
        );

        let res = load_pair_legacy(&missing_path, &reg, &occ);
        assert!(
            res.is_ok(),
            "load_pair on missing file must be a no-op Ok(())"
        );

        // Live state must be completely untouched
        assert!(reg.get(&alice_id).is_some());
        assert!(occ.list_for_path(Path::new("foo.rs")).is_some());
    }

    #[test]
    fn cross_process_refresh_removes_stale_local_ghosts() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let audit_path = dir.path().join(crate::server::audit::AUDIT_LOG_FILENAME);
        std::fs::write(&audit_path, b"audit").unwrap();

        // Process 1 and Process 2 registries
        let reg1 = PresenceRegistry::new();
        let occ1 = OccupancyMap::new();
        let reg2 = PresenceRegistry::new();
        let occ2 = OccupancyMap::new();

        // Process 1 creates a session and claims a file, then persists
        let sess1 = reg1.register(
            "worker-1".into(),
            super::registry::AgentKind::ClaudeCode,
            super::registry::AgentMode::Interactive,
            None,
            None,
        );
        occ1.claim(
            &sess1.id,
            vec![crate::server::presence::ClaimRequest {
                path: PathBuf::from("job.rs"),
                symbols: vec![],
                intent: ClaimIntent::Edit,
                ttl_seconds: None,
                plan_revision: None,
            }],
        );
        save_pair_legacy(&state_path, &reg1, &occ1).unwrap();

        // Process 2 reloads and sees Process 1's work
        load_pair_legacy(&state_path, &reg2, &occ2).unwrap();
        assert!(reg2.get(&sess1.id).is_some());
        assert!(occ2.list_for_path(Path::new("job.rs")).is_some());

        // Process 1 finishes work: releases claim and session, then persists
        occ1.release(&sess1.id, &[PathBuf::from("job.rs")]);
        reg1.remove(&sess1.id);
        save_pair_legacy(&state_path, &reg1, &occ1).unwrap();

        // Process 2 refreshes from disk: ghost session and ghost claim must be gone
        load_pair_legacy(&state_path, &reg2, &occ2).unwrap();
        assert!(
            reg2.get(&sess1.id).is_none(),
            "ghost session should not survive cross-process refresh"
        );
        assert!(
            occ2.list_for_path(Path::new("job.rs")).is_none(),
            "ghost claim should not survive cross-process refresh"
        );
    }

    #[test]
    fn lease_ledger_tracks_edit_claims_and_cleans_up_on_release() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let occ = OccupancyMap::new();
        occ.set_workspace_root(ws);

        let sess = AgentSession::new(
            super::agent::AgentId("alice-edit".into()),
            "alice".into(),
            super::registry::AgentKind::ClaudeCode,
            super::registry::AgentMode::Interactive,
            None,
            None,
        );

        // 1. Claim Edit creates lease and file
        let edit_req = crate::server::presence::ClaimRequest {
            path: PathBuf::from("src/main.rs"),
            symbols: vec![],
            intent: ClaimIntent::Edit,
            ttl_seconds: None,
            plan_revision: None,
        };
        occ.claim_with_session(&sess, vec![edit_req]);
        assert_eq!(occ.lock_leases_count(), 1);
        assert!(occ.has_lock_lease(&sess.id, Path::new("src/main.rs")));
        let lock_path = crate::server::presence_lock::lock_path_for(ws, Path::new("src/main.rs"));
        assert!(
            lock_path.exists(),
            "filesystem lock file must be written for Edit claim"
        );

        // 2. Claim Read does NOT create lease or lock file
        let read_req = crate::server::presence::ClaimRequest {
            path: PathBuf::from("src/lib.rs"),
            symbols: vec![],
            intent: ClaimIntent::Read,
            ttl_seconds: None,
            plan_revision: None,
        };
        occ.claim_with_session(&sess, vec![read_req]);
        assert_eq!(occ.lock_leases_count(), 1);
        assert!(!occ.has_lock_lease(&sess.id, Path::new("src/lib.rs")));
        let read_lock_path =
            crate::server::presence_lock::lock_path_for(ws, Path::new("src/lib.rs"));
        assert!(
            !read_lock_path.exists(),
            "filesystem lock must NOT be written for Read claim"
        );

        // 3. Release removes lease and lock file
        occ.release(&sess.id, &[PathBuf::from("src/main.rs")]);
        assert_eq!(occ.lock_leases_count(), 0);
        assert!(
            !lock_path.exists(),
            "filesystem lock must be deleted on release"
        );
    }

    #[test]
    fn lease_ledger_cleans_up_on_expire_by_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let occ = OccupancyMap::new();
        occ.set_workspace_root(ws);

        let sess = AgentSession::new(
            super::agent::AgentId("alice-ttl".into()),
            "alice".into(),
            super::registry::AgentKind::ClaudeCode,
            super::registry::AgentMode::Interactive,
            None,
            None,
        );

        let req = crate::server::presence::ClaimRequest {
            path: PathBuf::from("src/temp.rs"),
            symbols: vec![],
            intent: ClaimIntent::Edit,
            ttl_seconds: Some(0),
            plan_revision: None,
        };
        occ.claim_with_session(&sess, vec![req]);
        assert_eq!(occ.lock_leases_count(), 1);
        let lock_path = crate::server::presence_lock::lock_path_for(ws, Path::new("src/temp.rs"));
        assert!(lock_path.exists());

        std::thread::sleep(std::time::Duration::from_millis(50));
        let expired = occ.expire_by_ttl();
        assert!(!expired.is_empty());
        assert_eq!(occ.lock_leases_count(), 0);
        assert!(
            !lock_path.exists(),
            "expired lease lock file must be removed"
        );
    }

    #[test]
    fn touch_refreshes_active_leases() {
        use std::time::SystemTime;

        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let occ = OccupancyMap::new();
        occ.set_workspace_root(ws);

        let sess = AgentSession::new(
            super::agent::AgentId("alice-touch".into()),
            "alice".into(),
            super::registry::AgentKind::ClaudeCode,
            super::registry::AgentMode::Interactive,
            None,
            None,
        );

        let req = crate::server::presence::ClaimRequest {
            path: PathBuf::from("src/touched.rs"),
            symbols: vec![],
            intent: ClaimIntent::Edit,
            ttl_seconds: None,
            plan_revision: None,
        };
        occ.claim_with_session(&sess, vec![req]);
        let lock_path =
            crate::server::presence_lock::lock_path_for(ws, Path::new("src/touched.rs"));
        assert!(lock_path.exists());

        // Backdate mtime
        let past = SystemTime::now() - std::time::Duration::from_secs(2);
        {
            let f = std::fs::OpenOptions::new()
                .write(true)
                .open(&lock_path)
                .unwrap();
            f.set_modified(past).unwrap();
        }
        let mtime_past = std::fs::metadata(&lock_path).unwrap().modified().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(50));
        occ.touch(&sess.id);

        let mtime_refreshed = std::fs::metadata(&lock_path).unwrap().modified().unwrap();
        assert!(
            mtime_refreshed > mtime_past,
            "touch must refresh lock file mtime"
        );
    }

    #[test]
    fn remove_invokes_on_remove_callback_and_cleans_up_occupancy() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let reg = PresenceRegistry::new();
        let occ = OccupancyMap::new();
        occ.set_workspace_root(ws);

        // Wire on_remove_callback
        let occ_clone = occ.clone();
        reg.set_on_remove_callback(move |id| {
            occ_clone.release_all_for(id);
        });

        let sess = reg.register(
            "worker".into(),
            super::registry::AgentKind::ClaudeCode,
            super::registry::AgentMode::Interactive,
            None,
            None,
        );

        let req = crate::server::presence::ClaimRequest {
            path: PathBuf::from("src/worker.rs"),
            symbols: vec![],
            intent: ClaimIntent::Edit,
            ttl_seconds: None,
            plan_revision: None,
        };
        occ.claim_with_session(&sess, vec![req]);
        assert_eq!(occ.lock_leases_count(), 1);
        let lock_path = crate::server::presence_lock::lock_path_for(ws, Path::new("src/worker.rs"));
        assert!(lock_path.exists());
        assert!(occ.list_for_path(Path::new("src/worker.rs")).is_some());

        // Call reg.remove: callback must fire, releasing occupancy and lock file
        let removed = reg.remove(&sess.id);
        assert!(removed.is_some());
        assert!(reg.get(&sess.id).is_none());

        assert!(occ.list_for_path(Path::new("src/worker.rs")).is_none());
        assert_eq!(occ.lock_leases_count(), 0);
        assert!(
            !lock_path.exists(),
            "lock file must be deleted when session is removed"
        );
    }
}
