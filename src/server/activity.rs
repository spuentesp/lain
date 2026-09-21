//! Activity tracker — per-agent observation of tool calls.
//!
//! Hooks (PR 2's `POST /hook` endpoint) feed `ObservedTool` records
//! into this registry as the agent works. The activity feed surface
//! (`list_active_intents`) renders the ring buffer's last entry as
//! `last_tool`, dedupes `Read`/`Edit` targets into `observed_reads`,
//! and surfaces the most-recent target as `focus`.
//!
//! The tracker holds *observation*, not *declaration*. Agents do not
//! call this directly — the hook layer does, transparently to the
//! agent. Compare with `intent.rs`, which is the agent's explicit
//! declaration surface.
//!
//! The ring buffer caps at `MAX_RECENT_TOOLS` (100) entries per
//! agent. Older entries are dropped FIFO so the in-memory footprint
//! stays bounded for long-running sessions.

use crate::server::presence::{unix_secs, AgentId};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::SystemTime;

/// Cap on the per-agent ring buffer. 100 is enough to reconstruct
/// the agent's recent context for the activity feed without bloating
/// memory for long-running sessions. Roughly 5 minutes of activity
/// at typical agent tool-call rates.
const MAX_RECENT_TOOLS: usize = 100;

/// One observation recorded by a hook. The `target` is whatever the
/// agent-kind-specific hook extracted from the tool call — usually a
/// file path for `Read`/`Edit`/`Write`, a pattern for `Grep`, or a
/// command line for `Bash`. `None` for tools that have no obvious
/// target (e.g. agent-internal operations).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObservedTool {
    /// Tool name as the agent-kind reports it (e.g. `"Read"`,
    /// `"Grep"`, `"Bash"`, `"Edit"`, `"Write"`). Stored verbatim so
    /// downstream renderers can dispatch on the value without a
    /// translation layer.
    pub tool: String,
    /// Best-effort target extracted by the hook. `None` for tools
    /// that don't take a target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(with = "unix_secs", default = "unix_epoch")]
    pub at: SystemTime,
}

fn unix_epoch() -> SystemTime {
    SystemTime::UNIX_EPOCH
}

/// Per-agent live activity derived from the recent-tools ring buffer.
/// The `observed_reads`, `focus`, and `last_tool` fields are
/// computed by `ActivityTracker::summarize` — never stored directly —
/// so a single source of truth (`recent_tools`) drives every view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Activity {
    pub agent_id: AgentId,
    /// Bounded ring of the most recent tool calls. Capped at
    /// `MAX_RECENT_TOOLS`; the FIFO drop happens in
    /// `ActivityTracker::record`.
    #[serde(default)]
    pub recent_tools: VecDeque<ObservedTool>,
}

impl Activity {
    /// `last_tool` is just the most recent entry, or `None` when the
    /// agent has not yet been observed. `recent_tools` is FIFO-capped
    /// so this is `recent_tools.back()`.
    pub fn last_tool(&self) -> Option<&ObservedTool> {
        self.recent_tools.back()
    }

    /// Deduped, most-recent-first list of files this agent has read.
    /// Greps and bash commands are not counted as reads — those are
    /// exploratory, not the same "I'm about to edit this" signal.
    pub fn observed_reads(&self) -> Vec<String> {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut out: Vec<String> = Vec::new();
        for tool in self.recent_tools.iter().rev() {
            if !is_read_tool(&tool.tool) {
                continue;
            }
            if let Some(target) = tool.target.as_deref() {
                if seen.insert(target) {
                    out.push(target.to_string());
                }
            }
        }
        out
    }

    /// Most-recent observation's target — used as the "currently
    /// looking at" signal in the activity feed. Falls back to the
    /// most-recent read when the last observation was an Edit (which
    /// doesn't say what the agent is reading *now*).
    pub fn focus(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if let Some(last) = self.recent_tools.back() {
            if let Some(t) = &last.target {
                out.push(t.clone());
            }
        }
        // Add the most-recent read if it's distinct from the last
        // tool's target, so an Edit followed by an immediate Read
        // shows the read target as the focus.
        for tool in self.recent_tools.iter().rev() {
            if is_read_tool(&tool.tool) {
                if let Some(t) = &tool.target {
                    if !out.iter().any(|x| x == t) {
                        out.push(t.clone());
                    }
                }
                break;
            }
        }
        out
    }
}

fn is_read_tool(tool: &str) -> bool {
    // Matches `Read` (Claude Code / Cursor / Codex) and the
    // long-form variants an agent might surface. Case-insensitive
    // so `"read"` and `"READ"` collapse to the same bucket.
    let lower = tool.to_ascii_lowercase();
    matches!(lower.as_str(), "read" | "read_file" | "open_file")
}

type PersistFn = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct ActivityState {
    by_agent: HashMap<AgentId, Activity>,
}

#[derive(Clone)]
pub struct ActivityTracker {
    inner: Arc<Mutex<ActivityState>>,
    persist_cb: Arc<parking_lot::Mutex<Option<PersistFn>>>,
}

impl std::fmt::Debug for ActivityTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.inner.lock();
        f.debug_struct("ActivityTracker")
            .field("count", &s.by_agent.len())
            .finish()
    }
}

impl ActivityTracker {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ActivityState::default())),
            persist_cb: Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    pub fn set_persist_callback<F>(&self, cb: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        let mut slot = self.persist_cb.lock();
        *slot = Some(Arc::new(cb));
    }

    fn cloned_persist_cb(&self) -> Option<PersistFn> {
        self.persist_cb.lock().clone()
    }

    /// Record a tool observation. Creates the agent's `Activity`
    /// entry on first sight. The ring buffer is FIFO-capped at
    /// `MAX_RECENT_TOOLS` so a long-running session doesn't grow
    /// unbounded. Always fires the persist callback after the
    /// mutation; an observation with no callback installed is a
    /// no-op downstream.
    pub fn record(&self, agent_id: AgentId, tool: String, target: Option<String>) {
        let observed = ObservedTool {
            tool,
            target,
            at: SystemTime::now(),
        };
        {
            let mut s = self.inner.lock();
            let entry = s
                .by_agent
                .entry(agent_id.clone())
                .or_insert_with(|| Activity {
                    agent_id,
                    recent_tools: VecDeque::with_capacity(MAX_RECENT_TOOLS),
                });
            if entry.recent_tools.len() >= MAX_RECENT_TOOLS {
                entry.recent_tools.pop_front();
            }
            entry.recent_tools.push_back(observed);
        }
        if let Some(cb) = self.cloned_persist_cb() {
            cb();
        }
    }

    /// Snapshot one agent's activity. `None` when the agent has not
    /// been observed yet — distinct from an empty activity (which
    /// would still be `Some`).
    pub fn get(&self, agent_id: &AgentId) -> Option<Activity> {
        self.inner.lock().by_agent.get(agent_id).cloned()
    }

    /// Snapshot every tracked activity, in insertion order. Used by
    /// the persistence layer to flatten to a `Vec<(agent_id, Activity)>`
    /// for the JSON snapshot file.
    pub fn snapshot(&self) -> Vec<(AgentId, Activity)> {
        let s = self.inner.lock();
        s.by_agent
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Drop the agent's activity. Called from `unregister_agent` so
    /// the activity feed doesn't show ghost entries after a session
    /// ends.
    pub fn drop_agent(&self, agent_id: &AgentId) {
        let mut fired = false;
        {
            let mut s = self.inner.lock();
            if s.by_agent.remove(agent_id).is_some() {
                fired = true;
            }
        }
        if fired {
            if let Some(cb) = self.cloned_persist_cb() {
                cb();
            }
        }
    }

    /// Replace every tracked activity with `entries`. Used by the
    /// persistence loader — `presence::load_pair` flattens the
    /// JSON snapshot into `(agent_id, Activity)` pairs. The map
    /// already enforces one entry per agent, but if a stale state
    /// file holds duplicates, the later `insert` wins (the order
    /// isn't guaranteed across runs, so this is "last write wins"
    /// rather than "most recent wins"). No persist callback fires:
    /// hydration is a load, not a mutation the user needs to know
    /// about.
    pub fn replace_all(&self, entries: Vec<(AgentId, Activity)>) {
        let mut s = self.inner.lock();
        s.by_agent.clear();
        for (agent_id, a) in entries {
            // Defensive: the on-disk `Activity.agent_id` should match
            // the tuple key, but if a stale file got out of sync,
            // overwrite the entry's id with the map key so the two
            // agree.
            let mut a = a;
            a.agent_id = agent_id.clone();
            s.by_agent.insert(agent_id, a);
        }
    }
}

impl Default for ActivityTracker {
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
    fn record_creates_activity_on_first_call() {
        let t = ActivityTracker::new();
        t.record(agent("a"), "Read".into(), Some("src/auth.rs".into()));
        let got = t.get(&agent("a")).expect("activity must exist");
        assert_eq!(got.recent_tools.len(), 1);
        assert_eq!(got.recent_tools[0].tool, "Read");
        assert_eq!(got.recent_tools[0].target.as_deref(), Some("src/auth.rs"));
    }

    #[test]
    fn record_appends_to_recent_tools() {
        let t = ActivityTracker::new();
        for i in 0..5 {
            t.record(agent("a"), "Read".into(), Some(format!("src/file{i}.rs")));
        }
        let got = t.get(&agent("a")).unwrap();
        assert_eq!(got.recent_tools.len(), 5);
        assert_eq!(
            got.recent_tools.back().unwrap().target.as_deref(),
            Some("src/file4.rs"),
            "most recent observation must be the last"
        );
    }

    #[test]
    fn ring_buffer_caps_at_max_recent_tools() {
        let t = ActivityTracker::new();
        // Push 5 + MAX_RECENT_TOOLS more entries; only the most
        // recent MAX_RECENT_TOOLS survive.
        for i in 0..(MAX_RECENT_TOOLS + 5) {
            t.record(agent("a"), "Read".into(), Some(format!("src/file{i}.rs")));
        }
        let got = t.get(&agent("a")).unwrap();
        assert_eq!(got.recent_tools.len(), MAX_RECENT_TOOLS);
        // The first 5 entries were dropped, so the oldest surviving
        // entry is the 6th observation.
        assert_eq!(
            got.recent_tools.front().unwrap().target.as_deref(),
            Some("src/file5.rs")
        );
        assert_eq!(
            got.recent_tools.back().unwrap().target.as_deref(),
            Some(format!("src/file{}.rs", MAX_RECENT_TOOLS + 4).as_str())
        );
    }

    #[test]
    fn observed_reads_dedupes_and_orders_most_recent_first() {
        let t = ActivityTracker::new();
        t.record(agent("a"), "Read".into(), Some("src/auth.rs".into()));
        t.record(agent("a"), "Read".into(), Some("src/token.rs".into()));
        t.record(agent("a"), "Edit".into(), Some("src/auth.rs".into()));
        t.record(agent("a"), "Read".into(), Some("src/auth.rs".into()));
        let got = t.get(&agent("a")).unwrap();
        let reads = got.observed_reads();
        assert_eq!(
            reads,
            vec!["src/auth.rs".to_string(), "src/token.rs".to_string()],
            "most recent first, deduped"
        );
    }

    #[test]
    fn observed_reads_excludes_non_read_tools() {
        let t = ActivityTracker::new();
        t.record(agent("a"), "Read".into(), Some("src/a.rs".into()));
        t.record(agent("a"), "Bash".into(), Some("cargo test".into()));
        t.record(agent("a"), "Grep".into(), Some("validate_token".into()));
        let got = t.get(&agent("a")).unwrap();
        assert_eq!(got.observed_reads(), vec!["src/a.rs".to_string()]);
    }

    #[test]
    fn focus_returns_last_tool_target_and_most_recent_read() {
        let t = ActivityTracker::new();
        t.record(agent("a"), "Read".into(), Some("src/auth.rs".into()));
        t.record(agent("a"), "Bash".into(), Some("cargo test".into()));
        let got = t.get(&agent("a")).unwrap();
        let focus = got.focus();
        assert_eq!(
            focus,
            vec!["cargo test".to_string(), "src/auth.rs".to_string()],
            "focus shows last tool + last distinct read"
        );
    }

    #[test]
    fn drop_agent_removes_entry() {
        let t = ActivityTracker::new();
        t.record(agent("a"), "Read".into(), Some("src/a.rs".into()));
        t.record(agent("b"), "Read".into(), Some("src/b.rs".into()));
        t.drop_agent(&agent("a"));
        assert!(t.get(&agent("a")).is_none());
        assert!(
            t.get(&agent("b")).is_some(),
            "dropping one agent must not touch another"
        );
    }

    #[test]
    fn snapshot_returns_every_tracked_agent() {
        let t = ActivityTracker::new();
        t.record(agent("a"), "Read".into(), Some("src/a.rs".into()));
        t.record(agent("b"), "Read".into(), Some("src/b.rs".into()));
        let snap = t.snapshot();
        assert_eq!(snap.len(), 2);
    }

    #[test]
    fn persist_callback_fires_on_record_and_drop() {
        let t = ActivityTracker::new();
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter_cb = Arc::clone(&counter);
        t.set_persist_callback(move || {
            counter_cb.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        t.record(agent("a"), "Read".into(), Some("src/a.rs".into()));
        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 1);
        t.drop_agent(&agent("a"));
        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    #[test]
    fn is_read_tool_accepts_common_spelling_variants() {
        assert!(is_read_tool("Read"));
        assert!(is_read_tool("read"));
        assert!(is_read_tool("READ"));
        assert!(is_read_tool("read_file"));
        assert!(is_read_tool("Read_File"));
        assert!(!is_read_tool("Edit"));
        assert!(!is_read_tool("Grep"));
        assert!(!is_read_tool("Bash"));
    }
}
