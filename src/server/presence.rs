//! Presence types and the session registry for the multiplayer layer.
//!
//! The coordination core — file claims, exclusive leases, postboard —
//! lives in **zoanthrope** behind [`crate::server::coordination`].
//! What stays here is genuinely lain's:
//! - `PresenceRegistry`: the session auth table (who is connected,
//!   `session_token` verification, heartbeats) — the auth wrapper
//!   every multiplayer tool call passes through;
//! - the wire DTOs lain's clients already speak (`Claim`,
//!   `ConflictEntry`, `ClaimRequest`/`ClaimResult`, `OccupancyEntry`),
//!   now populated from zoanthrope responses;
//! - `WorldState` (graph-driven retract/changed-symbol detection) and
//!   the `PresenceEvent` SSE surface.
//!
//! `PresenceRegistry` is wrapped in `Arc<parking_lot::Mutex<...>>` so
//! the LainServer can clone it into the MCP dispatcher, the attribution
//! watcher, and the SSE endpoint without juggling lifetimes.

use std::path::PathBuf;
use std::time::SystemTime;

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

impl ClaimIntent {
    /// zoanthrope's wire spelling (`--intent read|edit`).
    pub fn as_str(&self) -> &'static str {
        match self {
            ClaimIntent::Read => "read",
            ClaimIntent::Edit => "edit",
        }
    }
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
        let activity_for_cb = std::sync::Arc::clone(&activity);
        let new_cb: PersistFn = std::sync::Arc::new(move || {
            let result = crate::server::presence::save_pair(
                &path_for_cb,
                &presence_for_cb,
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

/// Public, wire-safe view of an `AgentSession`. Mirrors every
/// non-credential field; `session_token` is excluded because any
/// subscriber that reads an `AgentJoined` frame can replay the token
/// and impersonate the holder through `heartbeat` / `claim_files` /
/// `release_files`. Constructed once at the SSE framing boundary via
/// `From<AgentSession>`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentJoinedPublic {
    pub id: AgentId,
    pub name: String,
    pub kind: AgentKind,
    pub mode: AgentMode,
    pub pid: Option<u32>,
    pub parent_session_id: Option<AgentId>,
    pub started_at: SystemTime,
    pub last_heartbeat: SystemTime,
}

impl From<AgentSession> for AgentJoinedPublic {
    fn from(s: AgentSession) -> Self {
        Self {
            id: s.id,
            name: s.name,
            kind: s.kind,
            mode: s.mode,
            pid: s.pid,
            parent_session_id: s.parent_session_id,
            started_at: s.started_at,
            last_heartbeat: s.last_heartbeat,
        }
    }
}

/// Public, wire-safe view of a `PresenceEvent`. The `AgentJoined`
/// variant wraps `AgentJoinedPublic` (no `session_token`); all other
/// variants are pass-throughs. Constructed once at the SSE framing
/// boundary via `From<PresenceEvent>` and consumed by
/// `sse::frame_for`'s `serde_json::to_string` call. `Deserialize`
/// is required because `EventsLog::replay_after` reads the same
/// shape back from `events.jsonl`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum PresenceEventPublic {
    AgentJoined(AgentJoinedPublic),
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

impl From<PresenceEvent> for PresenceEventPublic {
    fn from(e: PresenceEvent) -> Self {
        match e {
            PresenceEvent::AgentJoined(s) => Self::AgentJoined(AgentJoinedPublic::from(s)),
            PresenceEvent::AgentLeft(a) => Self::AgentLeft(a),
            PresenceEvent::HeartbeatExpired(a) => Self::HeartbeatExpired(a),
            PresenceEvent::ClaimGranted { agent_id, path } => Self::ClaimGranted { agent_id, path },
            PresenceEvent::ClaimReleased { agent_id, path } => {
                Self::ClaimReleased { agent_id, path }
            }
            PresenceEvent::ClaimRevoked {
                agent_id,
                path,
                reason,
            } => Self::ClaimRevoked {
                agent_id,
                path,
                reason,
            },
            PresenceEvent::ConflictDetected {
                agent_id,
                conflicts,
                severity,
            } => Self::ConflictDetected {
                agent_id,
                conflicts,
                severity,
            },
            PresenceEvent::EditLanded { event } => Self::EditLanded { event },
        }
    }
}


// ---------------------------------------------------------------------------
// Persistence: PresenceRegistry sessions + activity feed <-> JSON
// ---------------------------------------------------------------------------
//
// Claims and intents no longer persist here — zoanthrope owns that state
// (`<workspace>/.zoanthrope/state.json`, its own lock-file-serialized
// atomic writes). What remains is lain's session auth (the
// `session_token` table), the activity feed, and the audit-log offset
// bookkeeping. State files written by the old in-memory claims registry
// still load: serde ignores the retired `occupancy_*` / `intents`
// fields.
//
// Why free functions (not methods): the persistence layer is genuinely
// orthogonal to the in-memory data structure, and the persist hook
// closures must be `'static + Send + Sync` without capturing `LainServer`
// (that would create a ref cycle: server -> registry -> closure -> server).

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedState {
    /// `(agent_id_string, session)`.
    sessions: Vec<(String, AgentSession)>,
    /// Offset (in bytes) into `audit.jsonl` at which the next audit
    /// append should start on the next restart. Read out of the audit
    /// module on save and written back on load so crash-safe append
    /// continuation crosses process boundaries.
    #[serde(default)]
    audit_offset_bytes: u64,
    /// Unix-epoch seconds at which `audit.jsonl` was last reset
    /// because it was missing or corrupt on load.
    #[serde(default)]
    audit_reset_at_unix: Option<f64>,
    /// Per-agent observed tool-call activity. `(agent_id, activity)`.
    #[serde(default)]
    activities: Vec<(String, crate::server::activity::Activity)>,
}

/// Serialize the live session registry + activity feed to a JSON file
/// at `path`. Atomic: write `path.tmp`, then `rename` over `path`.
///
/// The `audit_offset_bytes` field is populated from the live
/// `audit.jsonl` (sibling of `path` under the same state directory) at
/// save time, so the value reflects "how much audit data was on disk at
/// the moment of this write".
pub fn save_pair(
    path: &Path,
    reg: &PresenceRegistry,
    activity: &crate::server::activity::ActivityTracker,
) -> Result<(), String> {
    let audit_dir: PathBuf = path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from(""));
    let audit_offset_bytes = crate::server::audit::current_offset_bytes(&audit_dir);

    let state = {
        let s = reg.inner.lock();
        PersistedState {
            sessions: s
                .sessions
                .iter()
                .map(|(k, v)| (k.0.clone(), v.clone()))
                .collect(),
            audit_offset_bytes,
            audit_reset_at_unix: None,
            activities: activity
                .snapshot()
                .into_iter()
                .map(|(id, a)| (id.0, a))
                .collect(),
        }
    };
    let json = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename into {}: {e}", path.display()))
}

/// Hydrate the live session registry + activity feed from `path`.
/// Idempotent: a missing file is a no-op and preserves live state.
///
/// Malformed JSON preserves live state and returns an error string — a
/// corrupt state file must not silently wipe the running session table.
///
/// Returns the (always empty) list of load-time coordination events:
/// claim-reclamation events died with the in-memory claims registry
/// (claims live in zoanthrope now, which has its own rejoin/orphan
/// policy). The `Vec` return is kept so the caller's event-forwarding
/// loop stays uniform.
pub fn load_pair(
    path: &Path,
    reg: &PresenceRegistry,
    activity: &crate::server::activity::ActivityTracker,
) -> Result<Vec<PresenceEvent>, String> {
    // Missing file is a no-op; any *other* I/O failure (the path
    // replaced by a directory, permissions revoked mid-run) fails
    // closed so the caller surfaces it instead of proceeding on a
    // stale view.
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("load {}: {e}", path.display())),
    };
    let state: PersistedState = match serde_json::from_str(&raw) {
        Ok(s) => s,
        Err(e) => return Err(format!("load {}: {e}", path.display())),
    };

    let mut s = reg.inner.lock();
    s.sessions.clear();
    s.by_token.clear();
    for (id_str, sess) in state.sessions {
        let id = AgentId(id_str);
        s.by_token.insert(sess.session_token.clone(), id.clone());
        s.sessions.insert(id, sess);
    }
    drop(s);

    let activities: Vec<(AgentId, crate::server::activity::Activity)> = state
        .activities
        .into_iter()
        .map(|(id_str, a)| (AgentId(id_str), a))
        .collect();
    activity.replace_all(activities);

    Ok(Vec::new())
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


