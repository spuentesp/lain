//! Intent registry — per-agent goal + scope declarations.
//!
//! The intent layer sits *above* the claim layer. An agent declares a
//! goal and the symbols it intends to modify via `lain_intent`; the
//! pre-edit hook evaluator (in `server::evaluation`, PR 3) compares
//! the declared scope against peer intents and against the static
//! graph, returning GREEN / YELLOW / RED. This file is PR 1's scope:
//! the in-memory registry, the `Intent` type, and the persistence
//! shape. The MCP tool surface (`run_lain_intent`,
//! `run_list_active_intents`) lives in `src/server/mcp/intent_tools.rs`
//! and the hook ingestion endpoint is in PR 2.
//!
//! ## Why one intent per agent
//!
//! Each agent has at most one active intent at a time. Two intents
//! would force the agent (and the evaluator) to reason about which
//! one a given tool call belongs to; one intent per agent keeps the
//! model simple and matches how agents actually work — one task per
//! session. A second `declare` call replaces the first; the previous
//! intent is dropped without ceremony.
//!
//! ## Persistence
//!
//! `IntentRegistry` and `ActivityTracker` ride alongside the existing
//! presence + occupancy snapshot via `PersistedState`. Old state files
//! load with empty intent/activity maps via `#[serde(default)]` — no
//! migration. See `server::presence::save_pair` / `load_pair` for the
//! round-trip path.

use crate::server::presence::{unix_secs, AgentId};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, SystemTime};

/// Opaque identifier for an intent declaration. Currently a UUIDv4
/// string; the planner originally called for ULID, but the project
/// already pulls `uuid` for `AgentId`, so reusing the same generator
/// keeps the dep tree slim. The wire shape is just an opaque string
/// to callers — never parse it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IntentId(pub String);

impl IntentId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}

impl Default for IntentId {
    fn default() -> Self {
        Self::new()
    }
}

/// Lifecycle phase the agent reports. Stored verbatim — the evaluator
/// does not gate any tool on `status`. Other agents reading
/// `list_active_intents` use it to render the activity feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IntentStatus {
    /// Investigating the code (reads, greps, queries).
    Investigating,
    /// Planning the change; reads complete, edits not started.
    #[default]
    Planning,
    /// Actively editing in declared scope.
    Editing,
    /// Patches applied; reviewing the diff.
    Reviewing,
    /// Intent retired; entry is kept until the agent unregisters or
    /// the registry is pruned.
    Done,
}

impl IntentStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            IntentStatus::Investigating => "investigating",
            IntentStatus::Planning => "planning",
            IntentStatus::Editing => "editing",
            IntentStatus::Reviewing => "reviewing",
            IntentStatus::Done => "done",
        }
    }

    /// Parse from a wire string. Case-insensitive — the documented
    /// wire format is lowercase (`"editing"`), but a caller that
    /// sends `"Editing"` gets the same intent back rather than
    /// silently dropping the status. Anything that doesn't match
    /// any known spelling returns `Planning` (the default) so a
    /// malformed caller doesn't lose their declaration.
    pub fn parse(s: &str) -> Self {
        let lower = s.to_ascii_lowercase();
        match lower.as_str() {
            "investigating" => IntentStatus::Investigating,
            "editing" => IntentStatus::Editing,
            "reviewing" => IntentStatus::Reviewing,
            "done" => IntentStatus::Done,
            // "planning" and anything else defaults to Planning so a
            // missing or mistyped field never drops the intent.
            _ => IntentStatus::Planning,
        }
    }
}

/// One agent's declared intent. The agent calls `lain_intent` to
/// create or update this; the pre-edit hook evaluator consults
/// `scopes` (and the graph overlap) on every Edit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Intent {
    pub id: IntentId,
    pub agent_id: AgentId,
    /// Free-form goal text. Not parsed; rendered as-is in the
    /// activity feed and the evaluation reasons.
    pub goal: String,
    /// Symbol-level scopes the agent intends to modify. Stored as
    /// raw strings; the evaluator resolves them through the static
    /// graph. Path-form scopes (`src/auth.rs`) are also accepted and
    /// resolved against the graph's path index.
    pub scopes: Vec<String>,
    pub status: IntentStatus,
    #[serde(with = "unix_secs", default = "epoch_secs")]
    pub created_at: SystemTime,
    #[serde(with = "unix_secs", default = "epoch_secs")]
    pub updated_at: SystemTime,
}

fn epoch_secs() -> SystemTime {
    SystemTime::UNIX_EPOCH
}

/// Callback type fired on every mutation that should be persisted.
/// Same shape as `PresenceRegistry::PersistFn`; stored as an
/// `Option<Arc<...>>` so a registry constructed without persistence
/// pays only an `Arc` + `None` slot.
type PersistFn = std::sync::Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct IntentState {
    /// Intent id → Intent. One entry per active declaration.
    by_id: HashMap<IntentId, Intent>,
    /// Agent → intent id. Used for the "one intent per agent" rule.
    by_agent: HashMap<AgentId, IntentId>,
}

#[derive(Clone)]
pub struct IntentRegistry {
    inner: std::sync::Arc<Mutex<IntentState>>,
    persist_cb: std::sync::Arc<parking_lot::Mutex<Option<PersistFn>>>,
}

impl std::fmt::Debug for IntentRegistry {
    // Manual Debug: `dyn Fn() + Send + Sync` doesn't implement Debug.
    // Surface a counter so `{:?}` is still useful in logs / tests.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.inner.lock();
        f.debug_struct("IntentRegistry")
            .field("count", &s.by_id.len())
            .finish()
    }
}

impl IntentRegistry {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(Mutex::new(IntentState::default())),
            persist_cb: std::sync::Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    pub fn set_persist_callback<F>(&self, cb: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        let mut slot = self.persist_cb.lock();
        *slot = Some(std::sync::Arc::new(cb));
    }

    fn cloned_persist_cb(&self) -> Option<PersistFn> {
        self.persist_cb.lock().clone()
    }

    /// Declare (or replace) the agent's current intent. A second
    /// call for the same `agent_id` retires the previous intent: its
    /// `id` is dropped from `by_id`. The replacement path bumps
    /// `updated_at`; a brand-new declaration also sets `created_at`.
    /// Returns the `IntentId` of the (now-current) entry.
    pub fn declare(
        &self,
        agent_id: AgentId,
        goal: String,
        scopes: Vec<String>,
        status: IntentStatus,
    ) -> IntentId {
        let id = IntentId::new();
        let now = SystemTime::now();
        let intent = Intent {
            id: id.clone(),
            agent_id: agent_id.clone(),
            goal,
            scopes,
            status,
            created_at: now,
            updated_at: now,
        };
        {
            let mut s = self.inner.lock();
            // Retire the agent's previous intent, if any.
            if let Some(prev_id) = s.by_agent.get(&agent_id).cloned() {
                s.by_id.remove(&prev_id);
            }
            s.by_id.insert(id.clone(), intent);
            s.by_agent.insert(agent_id, id.clone());
        }
        if let Some(cb) = self.cloned_persist_cb() {
            cb();
        }
        id
    }

    /// Apply a partial update to an existing intent. Fields set to
    /// `None` are left untouched. Returns the updated `Intent` on
    /// success; `Err` with a stable message on `UnknownIntent` or
    /// `WrongAgent`. The auth check (token resolves to the agent)
    /// is the caller's responsibility — the registry trusts the
    /// `agent_id` it is given.
    pub fn update(
        &self,
        agent_id: AgentId,
        intent_id: IntentId,
        add_scopes: Vec<String>,
        remove_scopes: Vec<String>,
        goal: Option<String>,
        status: Option<IntentStatus>,
    ) -> Result<Intent, String> {
        let mut s = self.inner.lock();
        let intent = s.by_id.get_mut(&intent_id).ok_or("unknown_intent")?;
        if intent.agent_id != agent_id {
            return Err("wrong_agent".into());
        }
        let remove_set: std::collections::HashSet<&str> =
            remove_scopes.iter().map(String::as_str).collect();
        let mut scopes: Vec<String> = intent
            .scopes
            .iter()
            .filter(|s| !remove_set.contains(s.as_str()))
            .cloned()
            .collect();
        for new in add_scopes {
            if !scopes.contains(&new) {
                scopes.push(new);
            }
        }
        intent.scopes = scopes;
        if let Some(g) = goal {
            intent.goal = g;
        }
        if let Some(st) = status {
            intent.status = st;
        }
        intent.updated_at = SystemTime::now();
        let snapshot = intent.clone();
        drop(s);
        if let Some(cb) = self.cloned_persist_cb() {
            cb();
        }
        Ok(snapshot)
    }

    /// Look up the agent's current intent. `None` when the agent has
    /// never declared (or the previous intent was retired via a
    /// second declare).
    pub fn get(&self, agent_id: &AgentId) -> Option<Intent> {
        let s = self.inner.lock();
        let id = s.by_agent.get(agent_id)?;
        s.by_id.get(id).cloned()
    }

    /// Look up an intent by its id. Used by the MCP layer when an
    /// agent wants to update a specific intent.
    pub fn get_by_id(&self, intent_id: &IntentId) -> Option<Intent> {
        self.inner.lock().by_id.get(intent_id).cloned()
    }

    /// Snapshot of every active intent, ordered by `updated_at`
    /// descending — most recently changed first. The activity feed
    /// surface (`list_active_intents`) renders this verbatim.
    pub fn list_active(&self) -> Vec<Intent> {
        let s = self.inner.lock();
        let mut out: Vec<Intent> = s.by_id.values().cloned().collect();
        out.sort_by_key(|i| std::cmp::Reverse(i.updated_at));
        out
    }

    /// Flat `(agent_id, intent)` snapshot used by `presence::save_pair`
    /// to flatten the registry into the JSON state file. Unlike
    /// `list_active`, this preserves the by-agent lookup so the loader
    /// can rebuild the inverse map on hydration. The on-disk shape
    /// is a flat list, not a HashMap, because serde_json's HashMap
    /// representation is non-deterministic across runs — tuples keep
    /// the file stable to hand-inspection.
    pub fn snapshot(&self) -> Vec<(AgentId, Intent)> {
        let s = self.inner.lock();
        s.by_agent
            .iter()
            .filter_map(|(agent_id, intent_id)| {
                s.by_id
                    .get(intent_id)
                    .map(|i| (agent_id.clone(), i.clone()))
            })
            .collect()
    }

    /// Drop the agent's current intent. Called on `unregister_agent`
    /// so the activity feed doesn't show a ghost intent after the
    /// agent's session ends.
    pub fn retire(&self, agent_id: &AgentId) {
        let mut s = self.inner.lock();
        if let Some(prev_id) = s.by_agent.remove(agent_id) {
            s.by_id.remove(&prev_id);
        }
        drop(s);
        if let Some(cb) = self.cloned_persist_cb() {
            cb();
        }
    }

    /// Drop every intent whose `updated_at` is older than `older_than`.
    /// Used by the expiry loop to retire stale intents from background
    /// (cron / CI) agents that crashed without `unregister_agent`.
    /// Active agents are unaffected; this only catches `Done` or
    /// never-updated stale entries.
    pub fn prune_older_than(&self, older_than: Duration) {
        let now = SystemTime::now();
        let mut dropped = false;
        {
            let mut s = self.inner.lock();
            let stale_ids: Vec<IntentId> = s
                .by_id
                .iter()
                .filter_map(|(id, intent)| {
                    now.duration_since(intent.updated_at)
                        .ok()
                        .filter(|age| *age >= older_than)
                        .map(|_| id.clone())
                })
                .collect();
            for id in &stale_ids {
                if let Some(intent) = s.by_id.remove(id) {
                    s.by_agent.remove(&intent.agent_id);
                    dropped = true;
                }
            }
        }
        if dropped {
            if let Some(cb) = self.cloned_persist_cb() {
                cb();
            }
        }
    }

    /// Replace every intent in the registry with `entries`. Used by
    /// the persistence loader — `presence::load_pair` builds a flat
    /// list from the JSON snapshot. Duplicate entries for the same
    /// agent are reconciled by keeping the most recent
    /// `updated_at`; this defends against a stale state file that
    /// somehow accumulated two intents for one agent (it shouldn't,
    /// but the loader is the only place that runs without the
    /// "one intent per agent" invariant enforced). No persist
    /// callback fires: hydration is a load operation, not a
    /// mutation the user needs to know about.
    pub fn replace_all(&self, entries: Vec<Intent>) {
        // Pick the most-recently-updated intent per agent before
        // mutating the registry, so the rest of the function holds
        // a single lock.
        let mut by_agent: HashMap<AgentId, Intent> = HashMap::new();
        for i in entries {
            match by_agent.get(&i.agent_id) {
                Some(existing) if existing.updated_at >= i.updated_at => {}
                _ => {
                    by_agent.insert(i.agent_id.clone(), i);
                }
            }
        }
        let mut s = self.inner.lock();
        s.by_id.clear();
        s.by_agent.clear();
        for (agent_id, i) in by_agent {
            s.by_agent.insert(agent_id, i.id.clone());
            s.by_id.insert(i.id.clone(), i);
        }
    }
}

impl Default for IntentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(s: &str) -> AgentId {
        AgentId(s.to_string())
    }

    #[test]
    fn declare_creates_intent_and_returns_id() {
        let r = IntentRegistry::new();
        let id = r.declare(
            agent("a"),
            "add refresh-token validation".into(),
            vec!["auth::validate_token".into()],
            IntentStatus::Editing,
        );
        let got = r.get(&agent("a")).expect("intent must exist");
        assert_eq!(got.id, id);
        assert_eq!(got.goal, "add refresh-token validation");
        assert_eq!(got.scopes, vec!["auth::validate_token".to_string()]);
        assert_eq!(got.status, IntentStatus::Editing);
        assert_eq!(got.created_at, got.updated_at);
    }

    #[test]
    fn second_declare_replaces_first() {
        let r = IntentRegistry::new();
        let id1 = r.declare(agent("a"), "first".into(), vec![], IntentStatus::Planning);
        // A second declare retires id1 — the registry holds exactly
        // one intent per agent at any time.
        let id2 = r.declare(
            agent("a"),
            "second".into(),
            vec!["x".into()],
            IntentStatus::Editing,
        );
        assert_ne!(id1, id2);
        assert!(r.get_by_id(&id1).is_none(), "first intent must be retired");
        let current = r.get(&agent("a")).unwrap();
        assert_eq!(current.id, id2);
        assert_eq!(current.goal, "second");
    }

    #[test]
    fn update_applies_partial_changes_and_bumps_updated_at() {
        let r = IntentRegistry::new();
        let id = r.declare(
            agent("a"),
            "initial".into(),
            vec!["a::x".into(), "a::y".into()],
            IntentStatus::Planning,
        );
        let before = r.get(&agent("a")).unwrap().updated_at;
        std::thread::sleep(std::time::Duration::from_millis(5));
        let updated = r
            .update(
                agent("a"),
                id.clone(),
                vec!["a::z".into()],
                vec!["a::x".into()],
                Some("refined".into()),
                Some(IntentStatus::Editing),
            )
            .expect("update must succeed for the owning agent");
        assert_eq!(updated.goal, "refined");
        assert_eq!(updated.status, IntentStatus::Editing);
        assert_eq!(
            updated.scopes,
            vec!["a::y".to_string(), "a::z".to_string()],
            "removed scope gone, added scope appended without duplicates"
        );
        assert!(
            updated.updated_at > before,
            "updated_at must advance: before={:?} after={:?}",
            before,
            updated.updated_at
        );
    }

    #[test]
    fn update_rejects_unknown_intent() {
        let r = IntentRegistry::new();
        r.declare(agent("a"), "g".into(), vec![], IntentStatus::Planning);
        let err = r
            .update(
                agent("a"),
                IntentId("not-a-real-id".into()),
                vec![],
                vec![],
                None,
                None,
            )
            .unwrap_err();
        assert_eq!(err, "unknown_intent");
    }

    #[test]
    fn update_rejects_wrong_agent() {
        let r = IntentRegistry::new();
        let id = r.declare(agent("a"), "g".into(), vec![], IntentStatus::Planning);
        // Bob cannot mutate Alice's intent.
        let err = r
            .update(agent("b"), id, vec![], vec![], Some("hijack".into()), None)
            .unwrap_err();
        assert_eq!(err, "wrong_agent");
    }

    #[test]
    fn retire_removes_intent_and_keeps_other_agents_intact() {
        let r = IntentRegistry::new();
        r.declare(agent("a"), "a-goal".into(), vec![], IntentStatus::Editing);
        r.declare(agent("b"), "b-goal".into(), vec![], IntentStatus::Planning);
        r.retire(&agent("a"));
        assert!(r.get(&agent("a")).is_none());
        assert!(
            r.get(&agent("b")).is_some(),
            "retiring one agent must not touch another"
        );
    }

    #[test]
    fn list_active_orders_by_most_recently_updated() {
        let r = IntentRegistry::new();
        r.declare(agent("a"), "a".into(), vec![], IntentStatus::Planning);
        std::thread::sleep(std::time::Duration::from_millis(5));
        r.declare(agent("b"), "b".into(), vec![], IntentStatus::Planning);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let id_c = r.declare(agent("c"), "c".into(), vec![], IntentStatus::Planning);
        let active = r.list_active();
        assert_eq!(active.len(), 3);
        assert_eq!(
            active[0].id, id_c,
            "most recent declaration must sort first"
        );
    }

    #[test]
    fn persist_callback_fires_on_declare_and_update_and_retire() {
        let r = IntentRegistry::new();
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter_cb = std::sync::Arc::clone(&counter);
        r.set_persist_callback(move || {
            counter_cb.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });

        let id = r.declare(agent("a"), "g".into(), vec![], IntentStatus::Planning);
        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 1);
        r.update(agent("a"), id, vec!["x".into()], vec![], None, None)
            .unwrap();
        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 2);
        r.retire(&agent("a"));
        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 3);
    }

    #[test]
    fn parse_status_round_trips() {
        for (s, expected) in [
            ("investigating", IntentStatus::Investigating),
            ("planning", IntentStatus::Planning),
            ("editing", IntentStatus::Editing),
            ("reviewing", IntentStatus::Reviewing),
            ("done", IntentStatus::Done),
        ] {
            assert_eq!(IntentStatus::parse(s), expected);
            assert_eq!(expected.as_str(), s);
        }
        // Unknown strings fall back to Planning rather than erroring
        // — a mistyped status from the agent must never drop the
        // entire declaration.
        assert_eq!(IntentStatus::parse("frobnicate"), IntentStatus::Planning);
    }
}
