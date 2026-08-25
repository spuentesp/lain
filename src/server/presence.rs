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


use crate::server::revision_log::RevisionId;

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct AgentId(pub String);

impl AgentId {
    pub fn as_str(&self) -> &str { &self.0 }
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
mod unix_secs {
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
fn epoch_secs() -> SystemTime { SystemTime::UNIX_EPOCH }

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
}

/// Callback type fired on each `PresenceRegistry` mutation. Wrapped
/// behind `Option<Arc<...>>` so registries constructed without
/// persistence (default `PresenceRegistry::new`) pay no allocation
/// cost beyond a single Arc + None slot.
type PersistFn = std::sync::Arc<dyn Fn() + Send + Sync>;

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
        let cfg = crate::server::tuning::PresenceConfig::default();
        Self::with_expiry(Duration::from_secs(cfg.interactive_session_ttl_secs))
    }

    pub fn with_expiry(expires_after: Duration) -> Self {
        Self {
            inner: std::sync::Arc::new(Mutex::new(PresenceState {
                sessions: HashMap::new(),
                by_token: HashMap::new(),
                expires_after,
            })),
            persist_cb: std::sync::Arc::new(parking_lot::Mutex::new(None)),
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

    /// Clone the (optional) persist callback out of the slot. Returns
    /// `None` when no callback has been installed; callers always
    /// no-op in that case.
    fn cloned_persist_cb(&self) -> Option<PersistFn> {
        self.persist_cb.lock().clone()
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
            s.by_token.insert(session.session_token.clone(), session.id.clone());
            s.sessions.insert(session.id.clone(), session.clone());
        }
        if let Some(cb) = self.cloned_persist_cb() { cb(); }
        session
    }

    pub fn heartbeat(&self, agent_id: &AgentId, session_token: &str) -> Result<(), HeartbeatError> {
        let mut s = self.inner.lock();
        let session = s.sessions.get_mut(agent_id).ok_or(HeartbeatError::UnknownAgent)?;
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
            AgentMode::Background => Duration::from_secs(
                crate::server::tuning::PresenceConfig::default().background_session_ttl_secs,
            ),
            AgentMode::Interactive => self.expires_after(),
        }
    }

    pub fn expire_stale(&self) -> Vec<AgentId> {
        let now = SystemTime::now();
        let expires_after = self.inner.lock().expires_after;
        let stale: Vec<AgentId> = {
            let mut s = self.inner.lock();
            let stale: Vec<AgentId> = s.sessions.iter()
                .filter(|(_, sess)| {
                    let ttl = match sess.mode {
                        AgentMode::Background => Duration::from_secs(
                            crate::server::tuning::PresenceConfig::default()
                                .background_session_ttl_secs,
                        ),
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
            if let Some(cb) = self.cloned_persist_cb() { cb(); }
        }
        stale
    }

    pub fn list_active(&self, include_background: bool) -> Vec<AgentSession> {
        let s = self.inner.lock();
        s.sessions.values()
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
            if let Some(cb) = self.cloned_persist_cb() { cb(); }
        }
        removed
    }

    pub fn by_token(&self, token: &str) -> Option<AgentSession> {
        let s = self.inner.lock();
        s.by_token.get(token).and_then(|id| s.sessions.get(id).cloned())
    }
}

impl Default for PresenceRegistry {
    fn default() -> Self { Self::new() }
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
        self.last_touched.get(sym).and_then(|m| m.get(agent)).copied()
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

/// Lexically normalize a path: resolve `.` and `..` components without
/// touching the filesystem. `/a/../b` becomes `/b`, `./a/b` becomes
/// `a/b`, and `/..` stays `/`. A leading `..` on a relative path is
/// kept — there is nothing to pop it against.
///
/// Lexical on purpose: a claim may name a file the agent is about to
/// *create*, so `fs::canonicalize` would fail on exactly the paths that
/// matter most.
fn lexical_normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                // `/..` is `/`; a prefix behaves the same way.
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                _ => out.push(".."),
            },
            other => out.push(other.as_os_str()),
        }
    }
    out
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
fn canonical_claim_path(roots: &[PathBuf], path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        lexical_normalize(path)
    } else {
        let anchored = roots
            .iter()
            .map(|root| lexical_normalize(&root.join(path)))
            .find(|candidate| candidate.exists());
        match anchored.or_else(|| roots.first().map(|root| lexical_normalize(&root.join(path)))) {
            Some(p) => p,
            None => return lexical_normalize(path),
        }
    };

    match roots.first() {
        Some(primary) => match absolute.strip_prefix(primary) {
            Ok(rel) => rel.to_path_buf(),
            Err(_) => absolute,
        },
        None => absolute,
    }
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
        }
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

    /// Set the workspace root so `claim` can write the
    /// filesystem-as-lock side-effect under
    /// `<workspace>/.lain/locks/<file>.json`. Called once per
    /// `LainServer` constructor, mirroring `set_persist_callback`.
    /// When unset (e.g. unit tests, federation paths without a
    /// workspace anchor), `claim` skips the filesystem write entirely.
    pub fn set_workspace_root(&self, workspace_root: &Path) {
        let mut slot = self.workspace_root.lock();
        *slot = Some(workspace_root.to_path_buf());
        drop(slot);
        // The workspace is also the first anchor for relative claim
        // paths. Kept at the front so it wins over repo roots added
        // later by `add_claim_roots`.
        let mut roots = self.claim_roots.lock();
        let root = lexical_normalize(workspace_root);
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
                                inferred: entry.inferred.contains(&other),
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
                                            inferred: entry.inferred.contains(&other),
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
                            for other in file_level_agents.iter().filter(|a| *a != agent_id).cloned().collect::<Vec<_>>() {
                                if entry.intent_for(&other, "__file_level__") == Some(ClaimIntent::Edit) {
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
                        entry.symbols.entry("__file_level__".into()).or_default().insert(agent_id.clone());
                        entry.intents.entry("__file_level__".into()).or_default().insert(agent_id.clone(), req.intent.clone());
                        entry.last_touched.entry("__file_level__".into()).or_default().insert(agent_id.clone(), now);
                    } else {
                        for sym in &req.symbols {
                            entry.symbols.entry(sym.clone()).or_default().insert(agent_id.clone());
                            entry.intents.entry(sym.clone()).or_default().insert(agent_id.clone(), req.intent.clone());
                            entry.last_touched.entry(sym.clone()).or_default().insert(agent_id.clone(), now);
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
                        compute_symbol_hash(&req.path, sym)
                            .or_else(|| Some(SymbolHash::zero()))
                    };
                    // Translate the request's optional TTL into an absolute
                    // expiry timestamp. `None` means "no expiry set" and the
                    // claim is only released explicitly or when the agent's
                    // session expires.
                    let expires_at = req.ttl_seconds
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
            if let Some(cb) = self.cloned_persist_cb() { cb(); }
        }
        ClaimResult { granted, conflicts, advisories, world_state: None }
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
        let mut s = self.inner.lock();
        for entry in s.by_file.values_mut() {
            for per_agent in entry.last_touched.values_mut() {
                if per_agent.contains_key(agent_id) {
                    per_agent.insert(agent_id.clone(), now);
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
                    let syms_to_remove: Vec<String> = entry.symbols.iter()
                        .filter(|(_, agents)| agents.contains(agent_id))
                        .map(|(s, _)| s.clone())
                        .collect();
                    for s in syms_to_remove {
                        if let Some(set) = entry.symbols.get_mut(&s) {
                            set.remove(agent_id);
                            if set.is_empty() { entry.symbols.remove(&s); }
                        }
                        // Mirror the same key into the parallel
                        // intent / timestamp tracks so they don't
                        // outlive a now-empty symbol set. Without
                        // this, `intents_for(other, sym)` could
                        // return a stale intent for a scope the
                        // agent no longer holds.
                        if let Some(m) = entry.intents.get_mut(&s) {
                            m.remove(agent_id);
                            if m.is_empty() { entry.intents.remove(&s); }
                        }
                        if let Some(m) = entry.last_touched.get_mut(&s) {
                            m.remove(agent_id);
                            if m.is_empty() { entry.last_touched.remove(&s); }
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
        if !released.is_empty() {
            if let Some(cb) = self.cloned_persist_cb() { cb(); }
        }
        released
    }

    pub fn release_all_for(&self, agent_id: &AgentId) -> Vec<PathBuf> {
        let paths: Vec<PathBuf> = {
            let s = self.inner.lock();
            s.by_agent.get(agent_id).map(|cs| cs.iter().map(|c| c.path.clone()).collect()).unwrap_or_default()
        };
        let released = self.release(agent_id, &paths);
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
                            if set.is_empty() { entry.symbols.remove(sym); }
                        }
                        // Same shadow cleanup as in `release`: the
                        // intent / timestamp tracks must agree with
                        // `symbols` or risk leaving stale (agent,
                        // scope) pairs reachable to
                        // `intent_for` / `last_touched_for`.
                        if let Some(m) = entry.intents.get_mut(sym) {
                            m.remove(agent_id);
                            if m.is_empty() { entry.intents.remove(sym); }
                        }
                        if let Some(m) = entry.last_touched.get_mut(sym) {
                            m.remove(agent_id);
                            if m.is_empty() { entry.last_touched.remove(sym); }
                        }
                    }
                    if entry.agents.is_empty() && entry.symbols.is_empty() {
                        s.by_file.remove(path);
                    }
                }
                if let Some(claims) = s.by_agent.get_mut(agent_id) {
                    claims.retain(|c| !(c.path == *path && c.expires_at.map(|e| e <= now).unwrap_or(false)));
                    if claims.is_empty() {
                        s.by_agent.remove(agent_id);
                    }
                }
                released.push((agent_id.clone(), path.clone()));
            }

            released
        };
        if !released.is_empty() {
            if let Some(cb) = self.cloned_persist_cb() { cb(); }
        }
        released
    }

    /// Best-effort write of `<workspace>/.lain/locks/<file>.json` for
    /// each path that was just granted. Called by
    /// `claim_with_session` after the in-memory bookkeeping settles.
    /// No-op when no workspace root is configured (unit tests,
    /// federation paths).
    ///
    /// The in-memory state is *not* rolled back if `try_lock` reports
    /// a conflict or an I/O error — both are logged via `tracing::warn`
    /// and the claim stands. Operators reading the lock file directly
    /// see the holder; readers going through `lain` see the in-memory
    /// state. PR 17 does not track `FileLock` handles for release on
    /// `release()` — stale locks age out via the 5s TTL on the next
    /// `try_lock` from another agent (see `presence_lock::LOCK_TTL`).
    fn write_lock_files(&self, session: &AgentSession, granted: &[ClaimRequest]) {
        let Some(workspace) = self.workspace_root_snapshot() else {
            return;
        };
        for req in granted {
            match crate::server::presence_lock::try_lock(
                &workspace,
                &req.path,
                &session.id,
                session.kind.clone(),
                req.intent.clone(),
            ) {
                Ok(_lock) => {}
                Err(conflict) => {
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
            let mut symbols: Vec<SymbolOccupancy> = entry.symbols.iter()
                .filter(|(s, _)| s.as_str() != "__file_level__")
                .map(|(sym, agents)| SymbolOccupancy { symbol: sym.clone(), agents: agents.iter().cloned().collect() })
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
    fn default() -> Self { Self::new() }
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
    ClaimGranted { agent_id: AgentId, path: PathBuf },
    ClaimReleased { agent_id: AgentId, path: PathBuf },
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
#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedState {
    /// `(agent_id_string, session)`.
    sessions: Vec<(String, AgentSession)>,
    /// `(path, file_level_agents, [(symbol, agents)])`. The
    /// `__file_level__` sentinel that lives in the in-memory symbol
    /// map is filtered out before serialization; the file-level agents
    /// list is derived directly from `FileOccupancy::agents`.
    occupancy_by_file: Vec<(PathBuf, Vec<String>, Vec<(String, Vec<String>)>)>,
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
        PersistedState {
            sessions: s.sessions.iter()
                .map(|(k, v)| (k.0.clone(), v.clone()))
                .collect(),
            occupancy_by_file: o.by_file.iter().map(|(p, fo)| {
                let agents: Vec<String> = fo.agents.iter().map(|a| a.0.clone()).collect();
                let symbols: Vec<(String, Vec<String>)> = fo.symbols.iter()
                    .filter(|(sym, _)| sym.as_str() != "__file_level__")
                    .map(|(sym, agents)| (sym.clone(), agents.iter().map(|a| a.0.clone()).collect()))
                    .collect();
                (p.clone(), agents, symbols)
            }).collect(),
            occupancy_by_agent: o.by_agent.iter()
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
        }
    };
    let json = serde_json::to_string_pretty(&state)
        .map_err(|e| format!("serialize PersistedState: {e}"))?;
    crate::cli::io::write_file_atomic(path, json.as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

/// Hydrate `reg` and `occ` from a JSON file previously written by
/// `save_pair`. When `path` does not exist this is a no-op (idempotent
/// loader: the registries stay as constructed).
///
/// On a successful read, prior contents of `reg` / `occ` are **not**
/// wiped before merge — callers should pass freshly-constructed
/// registries. Same string-error convention as `save_pair`.
///
/// Task 2.6: after a successful parse, if the live `audit.jsonl` is
/// missing or unreadable in the state directory (`path.parent()`),
/// the loader rewrites the state file with `audit_offset_bytes = 0`
/// and `audit_reset_at_unix = Some(now)`. The spec calls for a WARN
/// here; we surface it through `tracing::warn!` so operators see it
/// in the server log. The next `save_pair` then persists the reset
/// timestamp out to the world; subsequent restarts see the marker
/// and don't re-warn.
pub fn load_pair(
    path: &Path,
    reg: &PresenceRegistry,
    occ: &OccupancyMap,
) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let json = std::fs::read_to_string(path)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut state: PersistedState = serde_json::from_str(&json)
        .map_err(|e| format!("parse {}: {e}", path.display()))?;

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

    let mut s = reg.inner.lock();
    let mut o = occ.inner.lock();
    for (k, sess) in state.sessions {
        s.sessions.insert(AgentId(k.clone()), sess.clone());
        s.by_token.insert(sess.session_token, AgentId(k));
    }
    for (path_str, agents, symbols) in state.occupancy_by_file {
        let pb = PathBuf::from(path_str);
        let entry = o.by_file.entry(pb).or_default();
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
    for (k, claims) in state.occupancy_by_agent {
        o.by_agent.insert(AgentId(k), claims);
    }

    Ok(())
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
    let defs = crate::server::treesitter::extract_definitions(path, &src);
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
                added: vec![GraphNode::new(NodeType::Function, "f".into(), "/x.rs".into())],
                removed: vec![],
                updated: vec![],
            },
            OverlayDiff {
                revision: 7,
                added: vec![GraphNode::new(NodeType::Function, "f".into(), "/x.rs".into())],
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
        let state: PersistedState = serde_json::from_str(json)
            .expect("PersistedState should accept audit fields");
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
        save_pair(&path, &reg, &occ).expect("save_pair");
        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("\"audit_offset_bytes\""), "save_pair must emit audit_offset_bytes; got:\n{written}");
        assert!(written.contains("\"audit_reset_at_unix\""), "save_pair must emit audit_reset_at_unix; got:\n{written}");

        // Round-trip back through `load_pair` -> PersistedState with no
        // parse error, then double-check we read what we wrote.
        load_pair(&path, &reg, &occ).expect("load_pair");
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
        save_pair(&state_path, &reg, &occ).expect("save_pair");

        let written = fs::read_to_string(&state_path).unwrap();
        let parsed: PersistedState = serde_json::from_str(&written)
            .expect("state file must round-trip after save");
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
        assert!(!dir.path().join(crate::server::audit::AUDIT_LOG_FILENAME).exists());

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
        load_pair(&state_path, &reg, &occ).expect("load_pair");

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
        load_pair(&state_path, &reg, &occ).expect("load_pair");

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
}
