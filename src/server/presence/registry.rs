//! Presence registry: which agents are connected.
//!
//! `AgentSession`, `HeartbeatError`, and `PresenceRegistry` are the
//! in-memory state that tracks *who* is online. `PresenceRegistry` is
//! the same `Arc<Mutex<...>>` shape as `OccupancyMap` and is persisted
//! via the same `set_persist_callback` hook so a restart doesn't lose
//! the active-session list.
//!
//! Cross-references into the rest of `presence/`: `AgentId`, `AgentKind`,
//! `AgentMode` from `agent`; `ClaimIntent`, `Holder`, `OccupancyEntry`
//! from `claim` (the registry's `expire_stale` cross-cuts occupancy).

use std::sync::Arc;
use std::time::SystemTime;

use parking_lot::Mutex;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::agent::{new_agent_id, new_session_token, AgentId, AgentKind, AgentMode};
use super::OccupancyMap;

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
pub(crate) struct PresenceState {
    pub(crate) sessions: HashMap<AgentId, AgentSession>,
    pub(crate) by_token: HashMap<String, AgentId>,
    expires_after: Duration,
}

/// Callback type fired on each `PresenceRegistry` mutation. Wrapped
/// behind `Option<Arc<...>>` so registries constructed without
/// persistence (default `PresenceRegistry::new`) pay no allocation
/// cost beyond a single Arc + None slot.
pub type PersistFn = std::sync::Arc<dyn Fn() + Send + Sync>;

/// Callback invoked when an agent session is explicitly removed.
pub type RemoveCallbackFn = std::sync::Arc<dyn Fn(&AgentId) + Send + Sync>;

#[derive(Clone)]
pub struct PresenceRegistry {
    pub(crate) inner: std::sync::Arc<Mutex<PresenceState>>,
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
            let stale: Vec<AgentId> = s
                .sessions
                .iter()
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

