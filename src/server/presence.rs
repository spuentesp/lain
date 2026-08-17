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

/// `SystemTime::default()` is not in `std`; provide a fixed UNIX_EPOCH
/// default for the timestamps that we deliberately leave out of the
/// persisted JSON (started_at, last_heartbeat, claimed_at). Hydrating
/// the registries from disk leaves these at UNIX_EPOCH; downstream code
/// that cares about real timestamps (the federation expiry loop) reloads
/// them via the live `heartbeat` flow, not via persistence.
fn epoch() -> SystemTime { SystemTime::UNIX_EPOCH }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Claim {
    pub agent_id: AgentId,
    pub path: PathBuf,
    pub symbols: Vec<String>,
    /// `None` for a file-level claim (no specific symbol hash).
    /// `Some(hash)` carries the BLAKE3-256 of the symbol body.
    /// Placeholder during PR 10: file-level -> `None`,
    /// symbol-level -> `Some(SymbolHash::zero())` until PR 11 wires
    /// tree-sitter bodies through.
    pub content_hash: Option<SymbolHash>,
    pub intent: ClaimIntent,
    #[serde(skip_serializing, default = "epoch")]
    pub claimed_at: SystemTime,
    /// Wall-clock time of the most recent touch (claim grant or
    /// heartbeat refresh) on this claim. Surfaced in conflict reports
    /// so callers can answer *when* a conflicting claim was recorded,
    /// not just *who* is holding it. Defaults to `claimed_at` on
    /// construction and is serialized as epoch on persistence reload
    /// (same durability story as `claimed_at`: live state wins).
    #[serde(skip_serializing, default = "epoch")]
    pub last_touched_unix: SystemTime,
    /// Optional expiry timestamp (PR 10 Task 3 hook). `None` means
    /// "no expiry set"; the federation expiry loop will ignore it.
    pub expires_at: Option<SystemTime>,
}

#[derive(Debug, Clone, serde::Serialize)]
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

#[derive(Debug, Clone, serde::Serialize)]
pub struct OccupancyEntry {
    pub path: PathBuf,
    pub agents: Vec<AgentId>,
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
    #[serde(skip_serializing, default = "epoch")]
    pub started_at: SystemTime,
    #[serde(skip_serializing, default = "epoch")]
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
    pub fn new() -> Self {
        Self::with_expiry(Duration::from_secs(60))
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

    pub fn expire_stale(&self) -> Vec<AgentId> {
        let now = SystemTime::now();
        let expires_after = self.inner.lock().expires_after;
        let stale: Vec<AgentId> = {
            let mut s = self.inner.lock();
            let stale: Vec<AgentId> = s.sessions.iter()
                .filter(|(_, sess)| now.duration_since(sess.last_heartbeat).unwrap_or_default() >= expires_after)
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

#[derive(Debug, Clone)]
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
}

#[derive(Debug, Clone)]
pub struct ClaimResult {
    pub granted: Vec<ClaimRequest>,
    pub conflicts: Vec<ConflictEntry>,
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
}

impl FileOccupancy {
    /// Intent that `agent` recorded for `sym` (or `__file_level__`).
    /// Returns `None` if the agent never claimed that scope — which is
    /// the same condition that drives the existing agent/symbol
    /// bookkeeping, so the two never disagree.
    fn intent_for(&self, agent: &AgentId, sym: &str) -> Option<ClaimIntent> {
        self.intents.get(sym).and_then(|m| m.get(agent)).cloned()
    }

    /// Last-touched timestamp for `agent` on `sym`. Mirrors
    /// `intent_for`. Falls back to `UNIX_EPOCH` when absent — callers
    /// turn this directly into a `ConflictEntry.last_seen_unix`
    /// via `Option::unwrap_or_default()`-style plumbing.
    fn last_touched_for(&self, agent: &AgentId, sym: &str) -> Option<SystemTime> {
        self.last_touched.get(sym).and_then(|m| m.get(agent)).copied()
    }

    /// Timestamp for `agent`'s file-level claim on this file. Only
    /// used when the incoming request is itself file-level and we
    /// want to surface when the other agent first recorded the
    /// whole-file claim. Agents with no file-level claim fall back
    /// to `UNIX_EPOCH`; the conflict entry's `last_seen_unix` field
    /// lets callers tell the difference between a real claim
    /// (`> UNIX_EPOCH`) and the no-claim fallback.
    fn last_touched_unix_for(&self, agent: &AgentId) -> SystemTime {
        self.last_touched_for(agent, "__file_level__")
            .unwrap_or(SystemTime::UNIX_EPOCH)
    }
}

#[derive(Debug, Default)]
struct OccupancyState {
    by_file: HashMap<PathBuf, FileOccupancy>,
    by_agent: HashMap<AgentId, Vec<Claim>>,
}

#[derive(Clone)]
pub struct OccupancyMap {
    inner: std::sync::Arc<Mutex<OccupancyState>>,
    /// Optional persist callback. Same shape as the registry's
    /// `persist_cb`; fires on `claim`, `release`, and `release_all_for`
    /// when the call actually mutates state (calls that grant no claims
    /// or release no paths do not fire).
    persist_cb: std::sync::Arc<parking_lot::Mutex<Option<PersistFn>>>,
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

    /// Clone the (optional) persist callback out of the slot. Returns
    /// `None` when no callback has been installed; callers always
    /// no-op in that case.
    fn cloned_persist_cb(&self) -> Option<PersistFn> {
        self.persist_cb.lock().clone()
    }

    pub fn claim(&self, agent_id: &AgentId, requests: Vec<ClaimRequest>) -> ClaimResult {
        let (granted, conflicts) = {
            let mut s = self.inner.lock();
            let mut granted = Vec::new();
            let mut conflicts = Vec::new();

            for req in requests {
                let entry = s.by_file.entry(req.path.clone()).or_default();
                let mut req_conflicts: Vec<ConflictEntry> = Vec::new();

                // Read claims never produce a conflict — wishlist
                // item #5. They still update the agent/symbol
                // bookkeeping below so the granting agent becomes
                // observable for occupancy listings.
                if req.intent == ClaimIntent::Edit {
                    // File-level collision: any other agent on this file
                    // (regardless of their intent) conflicts with a
                    // file-level Edit claim — we're going to rewrite the
                    // whole file. Symbol- and intent-aware filtering
                    // applies only to the symbol-level branch below.
                    if req.symbols.is_empty() {
                        for other in entry.agents.iter().filter(|a| *a != agent_id) {
                            req_conflicts.push(ConflictEntry {
                                agent_id: other.clone(),
                                path: req.path.clone(),
                                symbols: vec![],
                                intent: ClaimIntent::Edit,
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
                    entry.agents.insert(agent_id.clone());
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
                    // content hash; symbol-level claims get a placeholder
                    // hash for now — PR 11 will compute real bodies.
                    let content_hash = if req.symbols.is_empty() {
                        None
                    } else {
                        Some(SymbolHash::zero())
                    };
                    // Translate the request's optional TTL into an absolute
                    // expiry timestamp. `None` means "no expiry set" and the
                    // claim is only released explicitly or when the agent's
                    // session expires.
                    let expires_at = req.ttl_seconds
                        .map(|s| now + std::time::Duration::from_secs(s));
                    s.by_agent.entry(agent_id.clone()).or_default().push(Claim {
                        agent_id: agent_id.clone(),
                        path: req.path.clone(),
                        symbols: req.symbols.clone(),
                        content_hash,
                        intent: req.intent.clone(),
                        claimed_at: now,
                        last_touched_unix: now,
                        expires_at,
                    });
                    granted.push(req);
                } else {
                    conflicts.extend(req_conflicts);
                }
            }

            (granted, conflicts)
        };
        if !granted.is_empty() {
            if let Some(cb) = self.cloned_persist_cb() { cb(); }
        }
        ClaimResult { granted, conflicts }
    }

    pub fn release(&self, agent_id: &AgentId, paths: &[PathBuf]) -> Vec<PathBuf> {
        let released = {
            let mut s = self.inner.lock();
            let mut released = Vec::new();
            for path in paths {
                if let Some(entry) = s.by_file.get_mut(path) {
                    entry.agents.remove(agent_id);
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

    pub fn list_for_path(&self, path: &Path) -> Option<OccupancyEntry> {
        let s = self.inner.lock();
        s.by_file.get(path).map(|entry| {
            let mut symbols: Vec<SymbolOccupancy> = entry.symbols.iter()
                .filter(|(s, _)| s.as_str() != "__file_level__")
                .map(|(sym, agents)| SymbolOccupancy { symbol: sym.clone(), agents: agents.iter().cloned().collect() })
                .collect();
            symbols.sort_by(|a, b| a.symbol.cmp(&b.symbol));
            OccupancyEntry {
                path: path.to_path_buf(),
                agents: entry.agents.iter().cloned().collect(),
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
#[derive(Debug, Clone, serde::Serialize)]
pub enum PresenceEvent {
    AgentJoined(AgentSession),
    AgentLeft(AgentId),
    HeartbeatExpired(AgentId),
    ClaimGranted { agent_id: AgentId, path: PathBuf },
    ClaimReleased { agent_id: AgentId, path: PathBuf },
    ConflictDetected { agent_id: AgentId, conflicts: Vec<ConflictEntry> },
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
}

/// Serialize the in-memory presence registry + occupancy map to a JSON
/// file at `path`. The write is atomic: serialise to `path.tmp` first,
/// then `rename` over `path`. Returns a string error on any IO / JSON
/// failure; callers wrap as needed.
pub fn save_pair(
    path: &Path,
    reg: &PresenceRegistry,
    occ: &OccupancyMap,
) -> Result<(), String> {
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
        }
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create_dir_all({}): {e}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(&state)
        .map_err(|e| format!("serialize PersistedState: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json)
        .map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| format!("rename {} -> {}: {e}", tmp.display(), path.display()))?;
    Ok(())
}

/// Hydrate `reg` and `occ` from a JSON file previously written by
/// `save_pair`. When `path` does not exist this is a no-op (idempotent
/// loader: the registries stay as constructed).
///
/// On a successful read, prior contents of `reg` / `occ` are **not**
/// wiped before merge — callers should pass freshly-constructed
/// registries. Same string-error convention as `save_pair`.
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
    let state: PersistedState = serde_json::from_str(&json)
        .map_err(|e| format!("parse {}: {e}", path.display()))?;

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
