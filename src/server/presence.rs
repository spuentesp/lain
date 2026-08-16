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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimIntent {
    Read,
    Edit,
}

#[derive(Debug, Clone)]
pub struct Claim {
    pub agent_id: AgentId,
    pub path: PathBuf,
    pub symbols: Vec<String>,
    pub intent: ClaimIntent,
    pub claimed_at: SystemTime,
}

#[derive(Debug, Clone)]
pub struct ConflictEntry {
    pub agent_id: AgentId,
    pub name: String,
    pub path: PathBuf,
    pub symbols: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SymbolOccupancy {
    pub symbol: String,
    pub agents: Vec<AgentId>,
}

#[derive(Debug, Clone)]
pub struct OccupancyEntry {
    pub path: PathBuf,
    pub agents: Vec<AgentId>,
    pub symbols: Vec<SymbolOccupancy>,
}

#[derive(Debug, Clone)]
pub struct AgentSession {
    pub id: AgentId,
    pub name: String,
    pub kind: AgentKind,
    pub mode: AgentMode,
    pub pid: Option<u32>,
    pub parent_session_id: Option<AgentId>,
    pub session_token: String,
    pub started_at: SystemTime,
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

#[derive(Debug, Clone)]
pub struct PresenceRegistry {
    inner: std::sync::Arc<Mutex<PresenceState>>,
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
        }
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
        let mut s = self.inner.lock();
        s.by_token.insert(session.session_token.clone(), session.id.clone());
        s.sessions.insert(session.id.clone(), session.clone());
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
        let mut s = self.inner.lock();
        let removed = s.sessions.remove(id);
        if let Some(ref sess) = removed {
            s.by_token.remove(&sess.session_token);
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
}

#[derive(Debug, Default)]
struct OccupancyState {
    by_file: HashMap<PathBuf, FileOccupancy>,
    by_agent: HashMap<AgentId, Vec<Claim>>,
}

#[derive(Debug, Clone)]
pub struct OccupancyMap {
    inner: std::sync::Arc<Mutex<OccupancyState>>,
}

impl OccupancyMap {
    pub fn new() -> Self {
        Self { inner: std::sync::Arc::new(Mutex::new(OccupancyState::default())) }
    }

    pub fn claim(&self, agent_id: &AgentId, requests: Vec<ClaimRequest>) -> ClaimResult {
        let mut s = self.inner.lock();
        let mut granted = Vec::new();
        let mut conflicts = Vec::new();

        for req in requests {
            let entry = s.by_file.entry(req.path.clone()).or_default();
            let mut req_conflicts: Vec<ConflictEntry> = Vec::new();

            // File-level collision: any other agent on this file conflicts with a
            // file-level claim (empty symbols).
            if req.symbols.is_empty() {
                for other in entry.agents.iter().filter(|a| *a != agent_id) {
                    req_conflicts.push(ConflictEntry {
                        agent_id: other.clone(),
                        name: "<unknown>".into(),
                        path: req.path.clone(),
                        symbols: vec![],
                    });
                }
            } else {
                // Symbol-level collision: for each requested symbol, any other
                // agent on that symbol conflicts.
                for sym in &req.symbols {
                    if let Some(others) = entry.symbols.get(sym) {
                        for other in others.iter().filter(|a| *a != agent_id) {
                            req_conflicts.push(ConflictEntry {
                                agent_id: other.clone(),
                                name: "<unknown>".into(),
                                path: req.path.clone(),
                                symbols: vec![sym.clone()],
                            });
                        }
                    }
                }
                // Also conflict if ANYONE has a file-level (empty symbols) claim on this file.
                for other in entry.agents.iter().filter(|a| entry.symbols.get("__file_level__").map(|s| s.contains(a)).unwrap_or(false) || entry.symbols.is_empty() && false).filter(|a| *a != agent_id) {
                    req_conflicts.push(ConflictEntry {
                        agent_id: other.clone(),
                        name: "<unknown>".into(),
                        path: req.path.clone(),
                        symbols: vec![],
                    });
                }
            }

            if req_conflicts.is_empty() {
                // Apply: add agent to file; add to symbol sets.
                entry.agents.insert(agent_id.clone());
                for sym in &req.symbols {
                    entry.symbols.entry(sym.clone()).or_default().insert(agent_id.clone());
                }
                if req.symbols.is_empty() {
                    entry.symbols.entry("__file_level__".into()).or_default().insert(agent_id.clone());
                }
                let now = SystemTime::now();
                s.by_agent.entry(agent_id.clone()).or_default().push(Claim {
                    agent_id: agent_id.clone(),
                    path: req.path.clone(),
                    symbols: req.symbols.clone(),
                    intent: req.intent.clone(),
                    claimed_at: now,
                });
                granted.push(req);
            } else {
                conflicts.extend(req_conflicts);
            }
        }

        ClaimResult { granted, conflicts }
    }

    pub fn release(&self, agent_id: &AgentId, paths: &[PathBuf]) -> Vec<PathBuf> {
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
    }

    pub fn release_all_for(&self, agent_id: &AgentId) -> Vec<PathBuf> {
        let s = self.inner.lock();
        let paths: Vec<PathBuf> = s.by_agent.get(agent_id).map(|cs| cs.iter().map(|c| c.path.clone()).collect()).unwrap_or_default();
        drop(s);
        self.release(agent_id, &paths)
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
        let s = self.inner.lock();
        s.by_file.keys().filter_map(|p| self.list_for_path(p)).collect()
    }

    pub fn list_for_agent(&self, agent_id: &AgentId) -> Vec<Claim> {
        let s = self.inner.lock();
        s.by_agent.get(agent_id).cloned().unwrap_or_default()
    }
}

impl Default for OccupancyMap {
    fn default() -> Self { Self::new() }
}
