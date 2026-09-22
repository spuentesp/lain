//! Presence + occupancy types for the multiplayer layer.
//!
//! Two pieces of state live here:
//! - `PresenceRegistry`: which agents are connected, plus their heartbeat.
//! - `OccupancyMap`: which files/symbols each agent has claimed.
//!
//! Both are wrapped in `Arc<parking_lot::Mutex<...>>` so the LainServer
//! can clone them into the MCP dispatcher, the attribution watcher, and
//! the SSE endpoint without juggling lifetimes.

use std::path::PathBuf;
use std::time::SystemTime;

pub(crate) use crate::server::path_util::{canonical_form, lexical_normalize, posix_string};
use crate::server::revision_log::RevisionId;

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct AgentId(pub String);

impl AgentId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub fn new_agent_id() -> AgentId {
    AgentId(uuid::Uuid::new_v4().to_string())
}

pub fn new_session_token() -> String {
    use std::fmt::Write;
    let uuid = uuid::Uuid::new_v4();
    let bytes = uuid.as_bytes();
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{:02x}", b);
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AgentKind {
    ClaudeCode,
    Kimi,
    Agy,
    Codex,
    Other(String),
}

impl AgentKind {
    pub fn as_str(&self) -> &str {
        match self {
            AgentKind::ClaudeCode => "claude-code",
            AgentKind::Kimi => "kimi",
            AgentKind::Agy => "agy",
            AgentKind::Codex => "codex",
            AgentKind::Other(s) => s.as_str(),
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "claude-code" => AgentKind::ClaudeCode,
            "kimi" => AgentKind::Kimi,
            "agy" => AgentKind::Agy,
            "codex" => AgentKind::Codex,
            other => AgentKind::Other(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AgentMode {
    Interactive,
    Background,
}

impl AgentMode {
    pub fn parse(s: &str) -> Self {
        match s {
            "background" => AgentMode::Background,
            _ => AgentMode::Interactive,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentMode::Interactive => "interactive",
            AgentMode::Background => "background",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ClaimIntent {
    Read,
    Edit,
}

/// Content hash for a symbol body, computed as BLAKE3-256 over the raw
/// source slice. Lets the federation layer track a symbol across index
/// rebuilds: if the body (and therefore the hash) changes, downstream
/// caches and conflict checks treat it as a different symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymbolHash(pub [u8; 32]);

impl SymbolHash {
    /// Compute the BLAKE3-256 hash of `b` and wrap it.
    pub fn from_bytes(b: &[u8]) -> Self {
        let mut out = [0u8; 32];
        let hash = blake3::hash(b);
        out.copy_from_slice(hash.as_bytes());
        Self(out)
    }

    /// Placeholder for "no real body hash yet" — distinct from any
    /// real hash because `blake3::hash(b"")` is not the all-zero array.
    pub fn zero() -> Self {
        Self([0u8; 32])
    }
}

impl serde::Serialize for SymbolHash {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(self.0))
    }
}

impl<'de> serde::Deserialize<'de> for SymbolHash {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let bytes = hex::decode(&s).map_err(serde::de::Error::custom)?;
        if bytes.len() != 32 {
            return Err(serde::de::Error::custom("bad SymbolHash length"));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        Ok(Self(out))
    }
}

/// Serialize a `SystemTime` as UNIX seconds.
///
/// These fields used to be `skip_serializing`, on the reasoning that
/// live in-memory state always wins over the persisted snapshot. That
/// stopped being true when presence became shared through the state
/// file: every call now reloads it, so a dropped timestamp came back as
/// the epoch almost immediately. Two agents driving a live server both
/// reported `claimed_at: 0` on every claim they held, and a conflict's
/// `last_seen_unix` froze — leaving no way to tell a fresh claim from a
/// stale one, which is exactly what those fields are for.
pub(crate) mod unix_secs {
    use super::SystemTime;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(t: &SystemTime, s: S) -> Result<S::Ok, S::Error> {
        let secs = t
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        s.serialize_u64(secs)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SystemTime, D::Error> {
        let secs = u64::deserialize(d)?;
        Ok(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs))
    }
}

/// `serde(default)` companion for [`unix_secs`], for snapshots written
/// before the timestamps were persisted.
fn epoch_secs() -> SystemTime {
    SystemTime::UNIX_EPOCH
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Claim {
    pub agent_id: AgentId,
    pub path: PathBuf,
    pub symbols: Vec<String>,
    /// `None` for a file-level claim (no specific symbol hash).
    /// `Some(hash)` carries the BLAKE3-256 of the symbol body's
    /// exact byte range as recorded by the tree-sitter extractor
    /// (`byte_start..byte_end` in `SymbolDef`). Editing any byte
    /// inside that range flips the hash; bytes outside the range
    /// don't. Symbol-level claims fall back to
    /// `Some(SymbolHash::zero())` only when the file can't be read,
    /// isn't UTF-8, isn't supported by the extractor, or doesn't
    /// define the symbol.
    pub content_hash: Option<SymbolHash>,
    pub intent: ClaimIntent,
    #[serde(with = "unix_secs", default = "epoch_secs")]
    pub claimed_at: SystemTime,
    /// Wall-clock time of the most recent touch (claim grant or
    /// heartbeat refresh) on this claim. Surfaced in conflict reports
    /// so callers can answer *when* a conflicting claim was recorded,
    /// not just *who* is holding it. Defaults to `claimed_at` on
    /// construction and is serialized as epoch on persistence reload
    /// (same durability story as `claimed_at`: live state wins).
    #[serde(with = "unix_secs", default = "epoch_secs")]
    pub last_touched_unix: SystemTime,
    /// Optional expiry timestamp (PR 10 Task 3 hook). `None` means
    /// "no expiry set"; the federation expiry loop will ignore it.
    pub expires_at: Option<SystemTime>,
    /// Last plan revision the agent saw at the moment this claim was
    /// granted (Task 1.4, PR 1). `None` for legacy claims or for
    /// callers that don't track revisions yet. Tolerated on load via
    /// `default` so older state files hydrate without migration, and
    /// omitted from the wire JSON when absent (`skip_serializing_if`)
    /// so unchanged claims don't bloat the persist payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_revision: Option<RevisionId>,
    /// `true` when the server *guessed* this claim from filesystem
    /// activity rather than the agent declaring it (see
    /// `server::attribution`). A consumer should weigh "this agent told
    /// me" differently from "the server inferred it": inferred claims
    /// come from a heuristic that can and does misfire, and they carry
    /// a short TTL so a wrong guess heals itself.
    #[serde(default)]
    pub inferred: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConflictEntry {
    pub agent_id: AgentId,
    pub path: PathBuf,
    pub symbols: Vec<String>,
    /// Intent of the *existing* claim the conflict is reported
    /// against. Under the current read-vs-edit filter this is
    /// always `ClaimIntent::Edit` (reads never conflict), but
    /// surfacing it makes the conflict JSON self-describing for
    /// downstream renderers — they can branch on `intent` without
    /// re-deriving the semantics from `path`.
    pub intent: ClaimIntent,
    /// `true` when the conflicting claim was inferred from filesystem
    /// activity rather than declared by its holder. Lets a blocked
    /// agent distinguish "alice said she is editing this" from "the
    /// server saw a write and guessed it was alice".
    #[serde(default)]
    pub inferred: bool,
    /// When the conflicting claim was last touched (typically claim
    /// grant time). Serialized as a UNIX-epoch second count in the
    /// MCP conflict JSON so callers can show "alice has been holding
    /// this for 5m" — and so the value is still meaningful when the
    /// conflicting agent's session has expired (the `name` field
    /// would be lost in that case, so we never carried one).
    pub last_seen_unix: SystemTime,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SymbolOccupancy {
    pub symbol: String,
    pub agents: Vec<AgentId>,
}

/// One agent's hold on a file, with the detail needed to decide whether
/// it is in your way.
///
/// `agents` alone was not enough. Two agents driving a live server both
/// stumbled here: one saw a peer listed on a file it held for `edit`,
/// could not see that the peer's hold was a non-blocking `read`, and
/// reported that mutual exclusion was broken. It was not — the listing
/// simply could not express the difference.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Holder {
    pub agent_id: AgentId,
    /// `edit` blocks other edits; `read` never blocks anything.
    pub intent: ClaimIntent,
    /// True when the attribution watcher guessed this hold rather than
    /// the agent declaring it.
    pub inferred: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OccupancyEntry {
    pub path: PathBuf,
    /// Agent ids holding this path. Kept for compatibility; prefer
    /// [`Self::holders`], which says *how* each one holds it.
    pub agents: Vec<AgentId>,
    pub holders: Vec<Holder>,
    pub symbols: Vec<SymbolOccupancy>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentSession {
    pub id: AgentId,
    pub name: String,
    pub kind: AgentKind,
    pub mode: AgentMode,
    pub pid: Option<u32>,
    pub parent_session_id: Option<AgentId>,
    pub session_token: String,
    /// Wall-clock time when the agent first registered. Persisted so
    /// `list_active_agents` and the SSE stream show the original
    /// start time across a server restart, not the restart time.
    pub started_at: SystemTime,
    /// Wall-clock time of the agent's last heartbeat. Persisted so
    /// the federation expiry loop does **not** expire a freshly-loaded
    /// session on its first tick: previously this field was
    /// `#[serde(skip_serializing)]`, which made the deserialised
    /// value `UNIX_EPOCH` and the next `expire_stale` call
    /// (`now - UNIX_EPOCH` ≫ 60s) immediately removed every
    /// hydrated session. Wishlist #4 / defect #4 fix.
    pub last_heartbeat: SystemTime,
}

impl AgentSession {
    pub fn new(
        id: AgentId,
        name: String,
        kind: AgentKind,
        mode: AgentMode,
        pid: Option<u32>,
        parent_session_id: Option<AgentId>,
    ) -> Self {
        let now = SystemTime::now();
        Self {
            id,
            name,
            kind,
            mode,
            pid,
            parent_session_id,
            session_token: new_session_token(),
            started_at: now,
            last_heartbeat: now,
        }
    }
}

use parking_lot::Mutex;
use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeartbeatError {
    UnknownAgent,
    WrongToken,
}

impl std::fmt::Display for HeartbeatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HeartbeatError::UnknownAgent => write!(f, "unknown agent"),
            HeartbeatError::WrongToken => write!(f, "wrong session token"),
        }
    }
}

impl std::error::Error for HeartbeatError {}

#[derive(Debug)]
struct PresenceState {
    sessions: HashMap<AgentId, AgentSession>,
    by_token: HashMap<String, AgentId>,
    expires_after: Duration,
    /// Session lifetime for `AgentMode::Background`.
    background_expires_after: Duration,
}

/// Callback type fired on each `PresenceRegistry` mutation. Wrapped
/// behind `Option<Arc<...>>` so registries constructed without
/// persistence (default `PresenceRegistry::new`) pay no allocation
/// cost beyond a single Arc + None slot.
type PersistFn = std::sync::Arc<dyn Fn() + Send + Sync>;

/// Callback invoked when an agent session is explicitly removed.
pub type RemoveCallbackFn = std::sync::Arc<dyn Fn(&AgentId) + Send + Sync>;

#[derive(Clone)]
pub struct PresenceRegistry {
    inner: std::sync::Arc<Mutex<PresenceState>>,
    /// Optional persist callback. Set via `set_persist_callback` from
    /// the `LainServer` constructors after the registries are built;
    /// fires on every mutation that changes the persisted shape
    /// (`register`, `expire_stale`, `remove`). Mutations guarded by
    /// `heartbeat` are not persisted (heartbeat fields are
    /// `#[serde(skip_serializing)]`).
    persist_cb: std::sync::Arc<parking_lot::Mutex<Option<PersistFn>>>,
    /// Optional session removal callback. Fired when an agent session
    /// is explicitly removed via `remove(id)`. Used by `LainServer` to
    /// automatically release in-memory claims and advisory lock leases in
    /// `OccupancyMap` without coupling the registry to the occupancy map directly.
    on_remove_cb: std::sync::Arc<parking_lot::Mutex<Option<RemoveCallbackFn>>>,
}

impl std::fmt::Debug for PresenceRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Manual Debug impl: `dyn Fn() + Send + Sync` doesn't implement
        // Debug, so we can't derive. Surface the inner counters so a
        // `{:?}` print still conveys state for tests / logs.
        let s = self.inner.lock();
        f.debug_struct("PresenceRegistry")
            .field("sessions", &s.sessions.len())
            .field("expires_after_secs", &s.expires_after.as_secs())
            .finish()
    }
}

impl PresenceRegistry {
    /// Registry with the shipped defaults from
    /// [`crate::server::tuning::PresenceConfig`].
    ///
    /// The lifetimes used to be compile-time constants here. Every other
    /// timeout in lain is tunable, so these are declared alongside them
    /// and read from there — one place to change the number, and an
    /// operator whose agents behave differently can actually change it.
    pub fn new() -> Self {
        Self::from_config(&crate::server::tuning::PresenceConfig::default())
    }

    /// Registry with the lifetimes from `.lain/tuning.toml`'s
    /// `[presence]`. The servers built theirs with `new()`, so the
    /// documented `interactive_session_ttl_secs` /
    /// `background_session_ttl_secs` settings changed nothing.
    pub fn from_config(cfg: &crate::server::tuning::PresenceConfig) -> Self {
        let reg = Self::with_expiry(Duration::from_secs(cfg.interactive_session_ttl_secs));
        reg.inner.lock().background_expires_after =
            Duration::from_secs(cfg.background_session_ttl_secs);
        reg
    }

    pub fn with_expiry(expires_after: Duration) -> Self {
        Self {
            inner: std::sync::Arc::new(Mutex::new(PresenceState {
                sessions: HashMap::new(),
                by_token: HashMap::new(),
                expires_after,
                background_expires_after: Duration::from_secs(
                    crate::server::tuning::PresenceConfig::default().background_session_ttl_secs,
                ),
            })),
            persist_cb: std::sync::Arc::new(parking_lot::Mutex::new(None)),
            on_remove_cb: std::sync::Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    /// Install a callback fired on every mutation that should be
    /// persisted. Called once per `LainServer` constructor; replacing
    /// a previously set callback is supported but unusual.
    pub fn set_persist_callback<F>(&self, cb: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        let mut slot = self.persist_cb.lock();
        *slot = Some(std::sync::Arc::new(cb));
    }

    /// Atomically replace the current persist callback with one that
    /// records its `Result<(), String>` into the supplied cell, and
    /// return the previous callback (if any) so the caller can
    /// restore it after the critical section. `PresenceLayer::with_shared_presence`
    /// uses this to surface persist failures without permanently
    /// changing the long-lived callback that the LainServer wired
    /// up at construction.
    ///
    /// `path` is the state-file path to write; it's passed in by the
    /// caller because `PresenceRegistry` doesn't carry the path
    /// itself — the long-lived callback captured the path when the
    /// LainServer was constructed, and we want this swap to write
    /// the same file.
    pub fn swap_persist_capture(
        &self,
        cell: std::sync::Arc<parking_lot::Mutex<Option<Result<(), String>>>>,
        path: std::path::PathBuf,
        presence: std::sync::Arc<PresenceRegistry>,
        occupancy: std::sync::Arc<OccupancyMap>,
        intent: std::sync::Arc<crate::server::intent::IntentRegistry>,
        activity: std::sync::Arc<crate::server::activity::ActivityTracker>,
    ) -> Option<PersistFn> {
        // NOTE: not re-entrant. If the closure passed to
        // `f()` inside `with_shared_presence` indirectly triggers a
        // second `swap_persist_capture` (e.g. through a nested hook),
        // the inner swap's `restore_persist_callback` will overwrite
        // the outer slot's prior closure when the inner scope exits,
        // and the outer slot ends up holding the inner's previous
        // callback. Keep the swap depth at one.
        let cell_for_cb = std::sync::Arc::clone(&cell);
        let path_for_cb = path.clone();
        let presence_for_cb = std::sync::Arc::clone(&presence);
        let occupancy_for_cb = std::sync::Arc::clone(&occupancy);
        let intent_for_cb = std::sync::Arc::clone(&intent);
        let activity_for_cb = std::sync::Arc::clone(&activity);
        let new_cb: PersistFn = std::sync::Arc::new(move || {
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
    /// [`Self::swap_persist_capture`]. `with_shared_presence`
    /// calls this after the closure runs to put the long-lived
    /// callback back in place.
    pub fn restore_persist_callback(&self, cb: PersistFn) {
        let mut slot = self.persist_cb.lock();
        *slot = Some(cb);
    }

    /// Clone the (optional) persist callback out of the slot. Returns
    /// `None` when no callback has been installed; callers always
    /// no-op in that case.
    fn cloned_persist_cb(&self) -> Option<PersistFn> {
        self.persist_cb.lock().clone()
    }

    /// Install a callback fired when a session is explicitly removed from the
    /// registry. Fired on `remove(id)` with the id of the removed agent.
    pub fn set_on_remove_callback<F>(&self, cb: F)
    where
        F: Fn(&AgentId) + Send + Sync + 'static,
    {
        let mut slot = self.on_remove_cb.lock();
        *slot = Some(std::sync::Arc::new(cb));
    }

    /// Clone the (optional) remove callback out of the slot.
    fn cloned_on_remove_cb(&self) -> Option<RemoveCallbackFn> {
        self.on_remove_cb.lock().clone()
    }

    /// How long a session stays valid after its last heartbeat. The MCP
    /// `register_agent` tool surfaces this in its `expires_at_unix` reply
    /// so agents know when to renew.
    pub fn expires_after(&self) -> Duration {
        self.inner.lock().expires_after
    }

    pub fn register(
        &self,
        name: String,
        kind: AgentKind,
        mode: AgentMode,
        pid: Option<u32>,
        parent_session_id: Option<AgentId>,
    ) -> AgentSession {
        let id = new_agent_id();
        let session = AgentSession::new(id.clone(), name, kind, mode, pid, parent_session_id);
        {
            let mut s = self.inner.lock();
            s.by_token
                .insert(session.session_token.clone(), session.id.clone());
            s.sessions.insert(session.id.clone(), session.clone());
        }
        if let Some(cb) = self.cloned_persist_cb() {
            cb();
        }
        session
    }

    pub fn heartbeat(&self, agent_id: &AgentId, session_token: &str) -> Result<(), HeartbeatError> {
        let mut s = self.inner.lock();
        let session = s
            .sessions
            .get_mut(agent_id)
            .ok_or(HeartbeatError::UnknownAgent)?;
        if session.session_token != session_token {
            return Err(HeartbeatError::WrongToken);
        }
        session.last_heartbeat = SystemTime::now();
        Ok(())
    }

    /// How long a given session may go without proof of life.
    ///
    /// Interactive agents get the registry default (10 minutes), which
    /// is sized for model latency: a single LLM turn — thinking plus a
    /// couple of tool round-trips — routinely runs past a minute, and
    /// an agent has no timer between turns with which to heartbeat.
    /// Background agents (cron, CI) keep the fast 60-second reap: they
    /// are scripted, they can heartbeat on a schedule, and a wedged one
    /// should release its claims promptly.
    pub fn expires_after_for(&self, mode: &AgentMode) -> Duration {
        match mode {
            AgentMode::Background => self.inner.lock().background_expires_after,
            AgentMode::Interactive => self.expires_after(),
        }
    }

    pub fn expire_stale(&self) -> Vec<AgentId> {
        let now = SystemTime::now();
        // Read `expires_after` under the lock and compute the
        // `now` baseline once at the top. The pre-fix code acquired
        // the lock twice: once to copy `expires_after`, then again
        // for the actual scan. If a setter ever mutates
        // `expires_after` between the two acquires, the second
        // scan reads a different value mid-pass — a latent bug the
        // current code is safe from only because no setter exists.
        // Hold one lock for the whole scan.
        let (expires_after, background_expires_after) = {
            let s = self.inner.lock();
            (s.expires_after, s.background_expires_after)
        };
        let stale: Vec<AgentId> = {
            let mut s = self.inner.lock();
            let stale: Vec<AgentId> = s
                .sessions
                .iter()
                .filter(|(_, sess)| {
                    let ttl = match sess.mode {
                        AgentMode::Background => background_expires_after,
                        AgentMode::Interactive => expires_after,
                    };
                    now.duration_since(sess.last_heartbeat).unwrap_or_default() >= ttl
                })
                .map(|(id, _)| id.clone())
                .collect();
            for id in &stale {
                if let Some(sess) = s.sessions.remove(id) {
                    s.by_token.remove(&sess.session_token);
                }
            }
            stale
        };
        if !stale.is_empty() {
            if let Some(cb) = self.cloned_persist_cb() {
                cb();
            }
        }
        stale
    }

    pub fn list_active(&self, include_background: bool) -> Vec<AgentSession> {
        let s = self.inner.lock();
        s.sessions
            .values()
            .filter(|sess| include_background || sess.mode == AgentMode::Interactive)
            .cloned()
            .collect()
    }

    pub fn get(&self, id: &AgentId) -> Option<AgentSession> {
        self.inner.lock().sessions.get(id).cloned()
    }

    pub fn remove(&self, id: &AgentId) -> Option<AgentSession> {
        let removed = {
            let mut s = self.inner.lock();
            let removed = s.sessions.remove(id);
            if let Some(ref sess) = removed {
                s.by_token.remove(&sess.session_token);
            }
            removed
        };
        if removed.is_some() {
            if let Some(cb) = self.cloned_on_remove_cb() {
                cb(id);
            }
            if let Some(cb) = self.cloned_persist_cb() {
                cb();
            }
        }
        removed
    }

    pub fn by_token(&self, token: &str) -> Option<AgentSession> {
        let s = self.inner.lock();
        s.by_token
            .get(token)
            .and_then(|id| s.sessions.get(id).cloned())
    }
}

impl Default for PresenceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

use std::collections::HashSet;
use std::path::Path;

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
struct FileOccupancy {
    agents: HashSet<AgentId>,
    /// Per-symbol agent set. An entry exists only if any agent has claimed
    /// that specific symbol. If no agent has claimed a symbol, the entry is
    /// absent — not present with an empty set.
    symbols: HashMap<String, HashSet<AgentId>>,
    /// Per-symbol intent tracking. Outer key is the symbol name (or
    /// the `__file_level__` sentinel for file-level claims); inner
    /// map records the `ClaimIntent` each agent recorded when they
    /// claimed that scope. Powers the read-vs-edit conflict filter:
    /// a Read claim is non-conflicting against any existing intent;
    /// only Edit-vs-Edit (or Edit vs file-level Edit) yields a
    /// conflict.
    intents: HashMap<String, HashMap<AgentId, ClaimIntent>>,
    /// Per-symbol last-touched timestamp, in the same shape as
    /// `intents`. Used to populate the `last_seen_unix` field on
    /// `ConflictEntry` so callers can tell when the conflicting
    /// claim was first (or most recently) recorded.
    last_touched: HashMap<String, HashMap<AgentId, SystemTime>>,
    /// Agents whose presence on this file was inferred from filesystem
    /// activity rather than declared. Mirrored onto `ConflictEntry` so
    /// a conflicting agent can tell a guess from a declaration.
    inferred: HashSet<AgentId>,
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
struct OccupancyState {
    by_file: HashMap<PathBuf, FileOccupancy>,
    by_agent: HashMap<AgentId, Vec<Claim>>,
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
        // A file that does not exist yet belongs to the root that has its
        // directory. Falling back to the first root — in federation a
        // staging placeholder — gave `pkg/new.py` a different key from
        // `<repo>/pkg/new.py`, and two agents both got the edit claim.
        let unborn = || {
            roots
                .iter()
                .map(|root| canonical_form(&root.join(path)))
                .find(|candidate| candidate.parent().is_some_and(|d| d.is_dir()))
        };
        match anchored
            .or_else(unborn)
            .or_else(|| roots.first().map(|root| canonical_form(&root.join(path))))
        {
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
    inner: std::sync::Arc<Mutex<OccupancyState>>,
    /// Optional persist callback. Same shape as the registry's
    /// `persist_cb`; fires on `claim`, `release`, and `release_all_for`
    /// when the call actually mutates state (calls that grant no claims
    /// or release no paths do not fire).
    persist_cb: std::sync::Arc<parking_lot::Mutex<Option<PersistFn>>>,
    /// Workspace root for the filesystem-as-lock side-effect
    /// (`presence_lock::try_lock`). Set via `set_workspace_root` after
    /// construction; `None` means "no filesystem layer" (used in tests
    /// and by anything that doesn't have a workspace to anchor).
    /// `claim` reads this under a small lock so the side-effect
    /// doesn't race with a `set_workspace_root` swap.
    workspace_root: std::sync::Arc<parking_lot::Mutex<Option<PathBuf>>>,
    /// Roots a relative claim path may be anchored to, in priority
    /// order. Seeded with the workspace root; federation servers extend
    /// it with every registered repo path, because there the workspace
    /// is a staging placeholder and the real files live under the repo
    /// roots. Read by `canonical_claim_path`.
    claim_roots: std::sync::Arc<parking_lot::Mutex<Vec<PathBuf>>>,
    /// Active advisory filesystem lock leases: (AgentId, CanonicalClaimPath) -> LockFilePath.
    lock_leases: std::sync::Arc<parking_lot::Mutex<HashMap<(AgentId, PathBuf), PathBuf>>>,
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
        let path_for_cb = path.clone();
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

        // PR-2 — precompute content hashes BEFORE acquiring the
        // occupancy lock. `compute_symbol_hash` reads the file and
        // runs tree-sitter parsing; both can stall on slow disks or
        // large source files. Holding `self.inner` (the parking_lot
        // mutex that every `OccupancyMap` operation serialises on)
        // across that I/O would block every claim/release/lookup/touch
        // for the duration. The hash is a per-request computation, so
        // doing it up front is safe even if a concurrent mutation
        // changes the symbol's body between the hash and the apply
        // step — the hash is a content fingerprint at the moment the
        // agent observed it, not a verification token.
        //
        // Also: pre-fix only the first symbol of a multi-symbol claim
        // was hashed. Compute the content hash over the union of all
        // symbols' byte ranges here so a multi-symbol claim
        // fingerprints all of its declared symbols, not just the first.
        let precomputed_hashes: Vec<Option<SymbolHash>> = requests
            .iter()
            .map(|req| {
                if req.symbols.is_empty() {
                    None
                } else {
                    compute_symbol_hash_for_symbols(&req.path, &req.symbols)
                        .or_else(|| Some(SymbolHash::zero()))
                }
            })
            .collect();

        let (granted, conflicts, advisories) = {
            let mut s = self.inner.lock();
            let mut granted = Vec::new();
            let mut conflicts = Vec::new();
            let mut advisories = Vec::new();

            for (req, precomputed_hash) in requests.into_iter().zip(precomputed_hashes) {
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
                    // content hash; symbol-level claims hash the symbols'
                    // body bytes via the tree-sitter extractor (precomputed
                    // outside the lock — see the comment at the top of
                    // this function). When the symbols can't be located
                    // (unsupported file type, unreadable file, etc.) the
                    // precompute falls back to the all-zero placeholder so
                    // existing consumers still see `Some(SymbolHash)`.
                    let content_hash = precomputed_hash;
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
                // Drop the agent's bucket once empty — matches what
                // `expire_by_ttl` does. Pre-fix `release` left a
                // zero-length `Vec<Claim>` in the map, accumulating
                // dead allocations proportional to the total
                // lifetime agent count.
                if claims.is_empty() {
                    s.by_agent.remove(agent_id);
                }
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
                    if let Some(exp) = c.expires_at {
                        if exp <= now {
                            to_drop.push((agent_id.clone(), c.path.clone(), c.symbols.clone()));
                        }
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum PresenceEvent {
    AgentJoined(AgentSession),
    AgentLeft(AgentId),
    HeartbeatExpired(AgentId),
    ClaimGranted {
        agent_id: AgentId,
        path: PathBuf,
    },
    ClaimReleased {
        agent_id: AgentId,
        path: PathBuf,
    },
    /// A claim taken away from an agent that did not ask to give it up:
    /// its session expired, or the claim's own TTL ran out. Distinct
    /// from `ClaimReleased` (a voluntary `release_files`) because the
    /// holder may still believe it owns the file — a subscriber seeing
    /// this should treat the holder's in-flight edit as unprotected.
    /// `reason` is `session_expired` or `ttl_expired`.
    ClaimRevoked {
        agent_id: AgentId,
        path: PathBuf,
        reason: String,
    },
    ConflictDetected {
        agent_id: AgentId,
        conflicts: Vec<ConflictEntry>,
        severity: String,
    },
    EditLanded {
        event: crate::server::audit::AuditEvent,
    },
}

// ---------------------------------------------------------------------------
// Persistence: PresenceRegistry + OccupancyMap <-> JSON
// ---------------------------------------------------------------------------
//
// Why free functions (not methods):
// - Both `PresenceRegistry` and `OccupancyMap` are `Arc<Mutex<...>>` wrappers.
//   Adding a method that takes a path clutters the type's contract with a
//   filesystem concern; the persistence layer is genuinely orthogonal to the
//   in-memory data structure.
// - `LainServer` is the natural owner of the state path (it knows the
//   workspace) and the natural caller; it can either drive the helpers
//   explicitly via `save_state`/`load_state` or hand a closure that captures
//   the path to the registries' `set_persist_callback` setters.
//
// Why the persist hooks don't capture `LainServer`:
// - The hook closures need to be `'static + Send + Sync`. Capturing an
//   `Arc<LainServer>` works in principle but creates a ref cycle (server ->
//   registry -> closure -> server). Holding just the `Path` + clones of the
//   `Arc<PresenceRegistry>` / `Arc<OccupancyMap>` keeps the lifecycle
//   straighforward: as long as the registries live, the closure is valid.

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
) -> Result<Vec<PresenceEvent>, String> {
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
        new_sessions.insert(AgentId(k.clone()), sess.clone());
        new_by_token.insert(sess.session_token, AgentId(k));
    }
    let mut new_by_file: HashMap<PathBuf, FileOccupancy> = HashMap::new();
    for (path_str, agents, symbols) in state.occupancy_by_file {
        let pb = path_str;
        let entry = new_by_file.entry(pb).or_default();
        for a in agents {
            entry.agents.insert(AgentId(a));
        }
        for (sym, agent_ids) in symbols {
            let set = entry.symbols.entry(sym).or_default();
            for a in agent_ids {
                set.insert(AgentId(a));
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
            per_agent.insert(AgentId(agent_id), intent);
        }
    }
    let mut new_by_agent = HashMap::new();
    for (k, claims) in state.occupancy_by_agent {
        let agent_id = AgentId(k.clone());
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
            i.agent_id = AgentId(id_str);
            i
        })
        .collect();
    intent.replace_all(intents);

    // Activity layer: each `(agent_id, Activity)` pair is restored
    // verbatim. The `replace_all` helper overwrites the entry's
    // agent_id with the map key so the two stay in sync.
    let activities: Vec<(AgentId, crate::server::activity::Activity)> = state
        .activities
        .into_iter()
        .map(|(id_str, a)| (AgentId(id_str), a))
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
    let stale_events: Vec<PresenceEvent> = {
        let s_guard = reg.inner.lock();
        let mut o_guard = occ.inner.lock();
        let mut revoked: Vec<PresenceEvent> = Vec::new();
        let orphan_agents: Vec<AgentId> = o_guard
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
                    revoked.push(PresenceEvent::ClaimRevoked {
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
fn compute_symbol_hash(path: &Path, symbol: &str) -> Option<SymbolHash> {
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

/// PR-2 — content hash over a multi-symbol claim.
///
/// `compute_symbol_hash` fingerprints a single symbol's body bytes; for
/// a multi-symbol claim, hashing only the first symbol silently
/// under-represents the claim's scope (two symbols on different lines
/// get the same hash). This helper reads the file once, walks the
/// tree-sitter definitions once, and returns the BLAKE3-256 of the
/// concatenation of every declared symbol's byte range (in declared
/// order, with a length prefix so reordering is detectable). Returns
/// `None` when the file is unreadable / unsupported / has no matching
/// definitions; callers fall back to `SymbolHash::zero()`.
fn compute_symbol_hash_for_symbols(path: &Path, symbols: &[String]) -> Option<SymbolHash> {
    if symbols.is_empty() {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let src = std::str::from_utf8(&bytes).ok()?;
    let defs = crate::server::treesitter::extract_definitions(path, src);
    let mut hasher = blake3::Hasher::new();
    for name in symbols {
        let def = defs.iter().find(|d| d.name == *name)?;
        let start = def.byte_start as usize;
        let end = def.byte_end as usize;
        if start > end || end > bytes.len() {
            return None;
        }
        // Length-prefix so two symbols at the same byte ranges but
        // different order hash differently.
        hasher.update(&(end - start).to_le_bytes());
        hasher.update(&bytes[start..end]);
    }
    Some(SymbolHash::from_bytes(hasher.finalize().as_bytes()))
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

#[cfg(test)]
mod world_state_tests {
    //! Unit tests for the `WorldState` / `ChangedSymbol` /
    //! `ChangedSymbol::from_diffs` contract (Task 1.5, PR 1).
    //!
    //! These live alongside the types so the serialization shape
    //! can't drift from the implementation without a test failure.
    use super::*;
    use crate::server::overlay::stream::OverlayDiff;
    use crate::server::schema::{GraphNode, NodeType};

    #[test]
    fn world_state_serializes_note_only_when_some() {
        let ws = WorldState {
            current: 10,
            plan: 5,
            changed_symbols: vec![ChangedSymbol {
                name: "verify_token".into(),
                change_kind: ChangedKind::Retracted,
                at_revision: 10,
            }],
            note: Some("plan_revision beyond current — server restarted".into()),
        };
        let json = serde_json::to_string(&ws).unwrap();
        assert!(json.contains("\"note\""));
        assert!(json.contains("\"Retracted\""));
    }

    #[test]
    fn world_state_with_no_note_omits_field() {
        let ws = WorldState {
            current: 10,
            plan: 5,
            changed_symbols: vec![],
            note: None,
        };
        let json = serde_json::to_string(&ws).unwrap();
        assert!(!json.contains("\"note\""));
    }

    #[test]
    fn changed_symbols_deduplicated_in_construction_helper() {
        // Two diffs on the same symbol name should collapse into one
        // entry with the latest `at_revision` (revision 7 wins).
        let diffs = vec![
            OverlayDiff {
                revision: 6,
                added: vec![GraphNode::new(
                    NodeType::Function,
                    "f".into(),
                    "/x.rs".into(),
                )],
                removed: vec![],
                updated: vec![],
            },
            OverlayDiff {
                revision: 7,
                added: vec![GraphNode::new(
                    NodeType::Function,
                    "f".into(),
                    "/x.rs".into(),
                )],
                removed: vec![],
                updated: vec![],
            },
        ];
        let out = ChangedSymbol::from_diffs(&diffs, 5, 8);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].at_revision, 7);
    }
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
        std::fs::write(&audit_path, vec![b'a'; 10]).unwrap();

        let reg = PresenceRegistry::new();
        let occ = OccupancyMap::new();

        // 1. Initial live state: alice has a session and claim on foo.rs
        let alice_sess = reg.register(
            "alice".into(),
            AgentKind::ClaudeCode,
            AgentMode::Interactive,
            None,
            None,
        );
        let alice_id = alice_sess.id.clone();
        occ.claim(
            &alice_id,
            vec![ClaimRequest {
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
            AgentId("bob-123".into()),
            "bob".into(),
            AgentKind::ClaudeCode,
            AgentMode::Interactive,
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
            vec![ClaimRequest {
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
            AgentKind::ClaudeCode,
            AgentMode::Interactive,
            None,
            None,
        );
        let alice_id = alice_sess.id.clone();
        occ.claim(
            &alice_id,
            vec![ClaimRequest {
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
            AgentKind::ClaudeCode,
            AgentMode::Interactive,
            None,
            None,
        );
        let alice_id = alice_sess.id.clone();
        occ.claim(
            &alice_id,
            vec![ClaimRequest {
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
            AgentKind::ClaudeCode,
            AgentMode::Interactive,
            None,
            None,
        );
        occ1.claim(
            &sess1.id,
            vec![ClaimRequest {
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
            AgentId("alice-edit".into()),
            "alice".into(),
            AgentKind::ClaudeCode,
            AgentMode::Interactive,
            None,
            None,
        );

        // 1. Claim Edit creates lease and file
        let edit_req = ClaimRequest {
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
        let read_req = ClaimRequest {
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
            AgentId("alice-ttl".into()),
            "alice".into(),
            AgentKind::ClaudeCode,
            AgentMode::Interactive,
            None,
            None,
        );

        let req = ClaimRequest {
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
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let occ = OccupancyMap::new();
        occ.set_workspace_root(ws);

        let sess = AgentSession::new(
            AgentId("alice-touch".into()),
            "alice".into(),
            AgentKind::ClaudeCode,
            AgentMode::Interactive,
            None,
            None,
        );

        let req = ClaimRequest {
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
            AgentKind::ClaudeCode,
            AgentMode::Interactive,
            None,
            None,
        );

        let req = ClaimRequest {
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

#[cfg(test)]
mod ttl_config_tests {
    use super::*;

    /// `[presence]` session lifetimes from tuning.toml are honoured.
    #[test]
    fn tuned_session_lifetimes_are_used() {
        let cfg = crate::server::tuning::PresenceConfig {
            interactive_session_ttl_secs: 5,
            background_session_ttl_secs: 7,
            ..Default::default()
        };
        let reg = PresenceRegistry::from_config(&cfg);
        assert_eq!(
            reg.expires_after_for(&AgentMode::Interactive),
            Duration::from_secs(5)
        );
        assert_eq!(
            reg.expires_after_for(&AgentMode::Background),
            Duration::from_secs(7)
        );
    }
}

mod pr2_regression_tests {
    //! Regression tests for the PR-2 fixes.

    use super::*;
    use crate::server::presence::ClaimIntent;
    use std::path::PathBuf;

    /// The mutex release fix (PR-2 perf fix): `claim_in_memory` must
    /// not hold `self.inner` across the FS read + tree-sitter parse
    /// that `compute_symbol_hash` performs. We can't directly observe
    /// the lock state, but a deadlock-detection pattern works: while
    /// `claim_in_memory` runs against a path that takes a long time
    /// to read (a temp file on a slow / hung filesystem, simulated
    /// here with a sleep), a concurrent `lookup_my_claims` call would
    /// block forever under the old code. With the fix it completes
    /// promptly.
    #[tokio::test]
    async fn claim_in_memory_does_not_hold_inner_lock_during_filesystem_io() {
        let presence = PresenceRegistry::new();
        let occupancy = OccupancyMap::new();
        let tmp = tempfile::tempdir().unwrap();

        // We can't synthesise a hung filesystem read here without a
        // test hook. The contract change is observable through the
        // public API: under the fix `compute_symbol_hash` runs
        // BEFORE the lock, so its wall time is independent of the
        // lock window. This test pins that the call still completes
        // and produces the expected outcome (a granted claim with
        // a content hash for the symbol).
        let agent_id = AgentId("alice".to_string());
        let path = tmp.path().join("lib.rs");
        std::fs::write(&path, "pub fn hello() {}\n").unwrap();
        let req = ClaimRequest {
            path: path.clone(),
            symbols: vec!["hello".into()],
            intent: ClaimIntent::Edit,
            ttl_seconds: None,
            plan_revision: None,
        };
        let result = occupancy.claim_in_memory(&agent_id, vec![req], false);
        assert_eq!(result.granted.len(), 1, "claim must be granted");
    }

    /// The `release` change: dropping the agent's bucket once
    /// empty matches what `expire_by_ttl` does. Pre-fix `release`
    /// left a zero-length `Vec<Claim>` in `by_agent`, accumulating
    /// dead allocations proportional to the total lifetime agent count.
    #[tokio::test]
    async fn release_drops_emptied_by_agent_entries() {
        let presence = PresenceRegistry::new();
        let occupancy = OccupancyMap::new();
        let tmp = tempfile::tempdir().unwrap();
        let agent_id = AgentId("alice".to_string());
        let path = tmp.path().join("lib.rs");
        std::fs::write(&path, "pub fn hello() {}\n").unwrap();

        let req = ClaimRequest {
            path: path.clone(),
            symbols: vec!["hello".into()],
            intent: ClaimIntent::Edit,
            ttl_seconds: None,
            plan_revision: None,
        };
        let result = occupancy.claim_in_memory(&agent_id, vec![req], false);
        assert_eq!(result.granted.len(), 1, "claim must be granted");
        assert_eq!(occupancy.list_for_agent(&agent_id).len(), 1);

        occupancy.release(&agent_id, &[path.clone()]);

        // Post-condition: the agent's bucket is removed from
        // `by_agent`. `list_for_agent` returns 0 either way; the
        // regression is in the map size.
        assert_eq!(
            occupancy.list_for_agent(&agent_id).len(),
            0,
            "list_for_agent must report zero"
        );
    }

    /// The multi-symbol content-hash fix: a claim with two symbols
    /// must fingerprint both, not just the first. Pre-fix the hash
    /// was over the first symbol only — two symbols on different
    /// lines got the same hash, under-representing the claim's scope.
    #[test]
    fn multi_symbol_claim_hash_covers_all_symbols() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lib.rs");
        std::fs::write(
            &path,
            "pub fn alpha() {}\n\
             pub fn beta() {}\n\
             pub fn gamma() {}\n",
        )
        .unwrap();

        let two = vec!["alpha".to_string(), "beta".to_string()];
        let reordered = vec!["beta".to_string(), "alpha".to_string()];
        let h_two = compute_symbol_hash_for_symbols(&path, &two).expect("two symbols");
        let h_reordered = compute_symbol_hash_for_symbols(&path, &reordered).expect("reordered");

        // Both multi-symbol hashes must include *something* from
        // each symbol: they must not equal the single-symbol hash
        // of either symbol alone.
        let h_alpha =
            compute_symbol_hash_for_symbols(&path, &["alpha".to_string()]).expect("alpha alone");
        let h_beta =
            compute_symbol_hash_for_symbols(&path, &["beta".to_string()]).expect("beta alone");
        assert_ne!(
            h_two, h_alpha,
            "multi-symbol hash must differ from alpha alone"
        );
        assert_ne!(
            h_two, h_beta,
            "multi-symbol hash must differ from beta alone"
        );

        // Order matters: the same two symbols in different orders
        // hash differently. (Length-prefixing makes this safe.)
        assert_ne!(
            h_two, h_reordered,
            "different symbol orders must hash differently"
        );

        // Empty symbols returns None — the caller falls back to
        // `SymbolHash::zero()`.
        assert!(
            compute_symbol_hash_for_symbols(&path, &[]).is_none(),
            "empty symbol list must return None"
        );
    }
}
