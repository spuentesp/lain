//! Intent declarations — the `lain_intent` surface, backed by
//! zoanthrope's **postboard**.
//!
//! The intent layer sits *above* the claim layer. An agent declares a
//! goal and the symbols it intends to modify via `lain_intent`; the
//! pre-edit hook evaluator (in `server::evaluation`) compares the
//! declared scope against peer intents and against the static graph,
//! returning GREEN / YELLOW / RED.
//!
//! Storage and arbitration moved to zoanthrope (`post_intention` /
//! `update_post` / `list_posts` / `remove_post` — its public postboard
//! of intentions). This file is the adapter: [`IntentBoard`] keeps the
//! `IntentRegistry` method names and the `Intent` wire shape lain's
//! clients already consume, and maps them onto posts.
//!
//! ## Field mapping (adapters, not rewrites)
//!
//! | lain `Intent` | zoanthrope post |
//! |---|---|
//! | `goal` | `title` |
//! | `status` | trailing `LAIN-INTENT …` note trailer (verbatim), mirrored coarsely into post `status` |
//! | `scopes` | note trailer (percent-encoded), path-shaped scopes also in `files` |
//! | `id` | `post_id` |
//!
//! The trailer carries the exact lain status (`investigating` /
//! `planning` / `editing` / `reviewing` / `done`) because the
//! postboard status vocabulary is coarser (`open`/`taken`/`done`/
//! `dropped`); without it a `reviewing` intent would round-trip as
//! `editing`. Posts with no trailer (written by non-lain agents) map
//! `open → planning`, `taken → editing`, `done|dropped → done`.
//!
//! One intent per agent is preserved: a second `declare` updates the
//! agent's existing post instead of opening a second one.
//!
//! ## Persistence
//!
//! None. zoanthrope persists the board in `.zoanthrope/state.json`.

use crate::server::coordination::CoordinationProxy;
use crate::server::presence::{unix_secs, AgentId};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Marker prefix of the adapter trailer stored in a post's `note`.
const TRAILER_PREFIX: &str = "LAIN-INTENT ";

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

/// Lifecycle phase the agent reports. Stored verbatim in the note
/// trailer — the evaluator does not gate any tool on `status`. Other
/// agents reading `list_active_intents` use it to render the activity
/// feed.
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
    /// the board post is removed.
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

    /// Coarse zoanthrope post status for this phase. The postboard
    /// vocabulary is `open|taken|done|dropped`; the exact phase
    /// survives in the note trailer.
    pub fn post_status(self) -> &'static str {
        match self {
            IntentStatus::Investigating | IntentStatus::Planning => "open",
            IntentStatus::Editing | IntentStatus::Reviewing => "taken",
            IntentStatus::Done => "done",
        }
    }

    /// Inverse of [`Self::post_status`] for posts without a trailer.
    pub fn from_post_status(s: &str) -> Self {
        match s {
            "open" => IntentStatus::Planning,
            "taken" => IntentStatus::Editing,
            // "done" and "dropped" are both terminal for lain's
            // vocabulary.
            _ => IntentStatus::Done,
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

// ── note trailer encode / decode ─────────────────────────────────────

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(b as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Pack lain's exact status + scopes into a post note, keeping the
/// human-readable goal text as the note body.
pub fn encode_note(note_text: &str, status: IntentStatus, scopes: &[String]) -> String {
    let scopes_enc: Vec<String> = scopes.iter().map(|s| percent_encode(s)).collect();
    let trailer = format!(
        "{TRAILER_PREFIX}status={} scopes={}",
        status.as_str(),
        scopes_enc.join(",")
    );
    if note_text.is_empty() {
        trailer
    } else {
        format!("{note_text}\n{trailer}")
    }
}

/// Recover `(note_text, status, scopes)` from a post note. Posts
/// without a trailer (non-lain authors) yield `None` status and empty
/// scopes.
pub fn decode_note(note: &str) -> (String, Option<IntentStatus>, Vec<String>) {
    let Some(idx) = note.rfind(TRAILER_PREFIX) else {
        return (note.to_string(), None, Vec::new());
    };
    let body = note[..idx].trim_end_matches('\n').to_string();
    let trailer = &note[idx + TRAILER_PREFIX.len()..];
    let mut status = None;
    let mut scopes = Vec::new();
    for part in trailer.split_whitespace() {
        if let Some(v) = part.strip_prefix("status=") {
            status = Some(IntentStatus::parse(v));
        } else if let Some(v) = part.strip_prefix("scopes=") {
            scopes = v
                .split(',')
                .filter(|s| !s.is_empty())
                .map(percent_decode)
                .collect();
        }
    }
    (body, status, scopes)
}

/// Path-shaped scopes (the ones zoanthrope's `files` field can carry
/// without being mangled by its path canonicalisation). Symbol scopes
/// live only in the trailer.
fn path_scopes(scopes: &[String]) -> Vec<String> {
    scopes
        .iter()
        .filter(|s| s.contains('/') || s.contains('\\'))
        .cloned()
        .collect()
}

fn post_to_intent(post: &serde_json::Value) -> Option<Intent> {
    let post_id = post.get("post_id")?.as_str()?.to_string();
    let agent = post.get("agent_id")?.as_str()?.to_string();
    let title = post
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    let note = post.get("note").and_then(|t| t.as_str()).unwrap_or("");
    let (body, trailer_status, scopes) = decode_note(note);
    let goal = if body.is_empty() { title } else { body };
    let status = trailer_status.unwrap_or_else(|| {
        IntentStatus::from_post_status(
            post.get("status")
                .and_then(|s| s.as_str())
                .unwrap_or("open"),
        )
    });
    let at = |key: &str| {
        post.get(key)
            .and_then(serde_json::Value::as_f64)
            .map(|s| SystemTime::UNIX_EPOCH + Duration::from_secs_f64(s.max(0.0)))
            .unwrap_or(SystemTime::UNIX_EPOCH)
    };
    Some(Intent {
        id: IntentId(post_id),
        agent_id: AgentId(agent),
        goal,
        scopes,
        status,
        created_at: at("created_at"),
        updated_at: at("updated_at"),
    })
}

/// Postboard adapter — the `IntentRegistry` method surface, delegated
/// to zoanthrope. Degraded (no binary): declarations return a locally
/// minted [`Intent`] and are logged, never block; listings are empty.
#[derive(Clone)]
pub struct IntentBoard {
    proxy: Arc<CoordinationProxy>,
}

impl std::fmt::Debug for IntentBoard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IntentBoard").finish_non_exhaustive()
    }
}

impl IntentBoard {
    pub fn new(proxy: Arc<CoordinationProxy>) -> Self {
        Self { proxy }
    }

    /// Declare (or replace) the agent's intent. zoanthrope mints the
    /// post id; a degraded proxy returns a locally minted intent that
    /// exists only in the response.
    pub fn declare(
        &self,
        agent_id: AgentId,
        goal: String,
        scopes: Vec<String>,
        status: IntentStatus,
    ) -> Result<Intent, String> {
        let note = encode_note("", status, &scopes);
        // One intent per agent: reuse the agent's existing post when
        // there is one (a second declare replaces the first).
        if let Some(existing) = self.get(&agent_id) {
            return self.update(
                agent_id,
                existing.id,
                Vec::new(),
                Vec::new(),
                Some(goal),
                Some(status),
                Some(scopes),
            );
        }
        match self
            .proxy
            .post_intention(agent_id.as_str(), &goal, &note, &path_scopes(&scopes))
        {
            Ok(body) => {
                let post = body.get("post").cloned().unwrap_or(body.clone());
                post_to_intent(&post).ok_or_else(|| "post_intention: bad post body".to_string())
            }
            Err(_e) if self.proxy.is_degraded() => {
                Ok(self.degraded_intent(&agent_id, goal, scopes, status))
            }
            Err(e) => {
                tracing::warn!("post_intention failed, returning local intent: {e}");
                Ok(self.degraded_intent(&agent_id, goal, scopes, status))
            }
        }
    }

    fn degraded_intent(
        &self,
        agent_id: &AgentId,
        goal: String,
        scopes: Vec<String>,
        status: IntentStatus,
    ) -> Intent {
        Intent {
            id: IntentId::new(),
            agent_id: agent_id.clone(),
            goal,
            scopes,
            status,
            created_at: SystemTime::now(),
            updated_at: SystemTime::now(),
        }
    }

    /// Partial update of an existing intent. `None` fields leave the
    /// stored value alone. `add_scopes` appends without duplicates;
    /// `remove_scopes` filters by exact match.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &self,
        agent_id: AgentId,
        intent_id: IntentId,
        add_scopes: Vec<String>,
        remove_scopes: Vec<String>,
        goal: Option<String>,
        status: Option<IntentStatus>,
        scopes: Option<Vec<String>>,
    ) -> Result<Intent, String> {
        let current = self.get(&agent_id).unwrap_or(Intent {
            id: intent_id.clone(),
            agent_id: agent_id.clone(),
            goal: String::new(),
            scopes: Vec::new(),
            status: IntentStatus::Planning,
            created_at: SystemTime::now(),
            updated_at: SystemTime::now(),
        });
        let mut new_scopes = scopes.unwrap_or(current.scopes.clone());
        for s in add_scopes {
            if !new_scopes.contains(&s) {
                new_scopes.push(s);
            }
        }
        new_scopes.retain(|s| !remove_scopes.contains(s));
        let new_goal = goal.unwrap_or(current.goal.clone());
        let new_status = status.unwrap_or(current.status);
        let note = encode_note("", new_status, &new_scopes);
        let body = self
            .proxy
            .update_post(
                agent_id.as_str(),
                intent_id.as_str(),
                Some(new_status.post_status()),
                Some(&new_goal),
                Some(&note),
                Some(&path_scopes(&new_scopes)),
            )
            .map_err(|e| e.to_string())?;
        let post = body.get("post").cloned().unwrap_or(body);
        post_to_intent(&post).ok_or_else(|| "update_post: bad post body".to_string())
    }

    /// The agent's current intent, if any (newest post for the agent).
    pub fn get(&self, agent_id: &AgentId) -> Option<Intent> {
        let body = self.proxy.list_posts(Some(agent_id.as_str()), None).ok()?;
        let posts = body.get("posts")?.as_array()?;
        posts.iter().filter_map(post_to_intent).next()
    }

    pub fn get_by_id(&self, intent_id: &IntentId) -> Option<Intent> {
        let body = self.proxy.list_posts(None, None).ok()?;
        let posts = body.get("posts")?.as_array()?;
        posts
            .iter()
            .filter_map(post_to_intent)
            .find(|i| i.id == *intent_id)
    }

    /// Active intents — posts still open or taken on the board.
    pub fn list_active(&self) -> Vec<Intent> {
        self.snapshot()
            .into_iter()
            .map(|(_, i)| i)
            .filter(|i| !matches!(i.status, IntentStatus::Done))
            .collect()
    }

    /// Every intent on the board, newest-updated first (zoanthrope's
    /// ordering).
    pub fn snapshot(&self) -> Vec<(AgentId, Intent)> {
        let Ok(body) = self.proxy.list_posts(None, None) else {
            return Vec::new();
        };
        body.get("posts")
            .and_then(serde_json::Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(post_to_intent)
                    .map(|i| (i.agent_id.clone(), i))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Retire the agent's intent on unregister: drop its posts.
    pub fn retire(&self, agent_id: &AgentId) {
        if let Ok(body) = self.proxy.list_posts(Some(agent_id.as_str()), None) {
            if let Some(posts) = body.get("posts").and_then(serde_json::Value::as_array) {
                for post in posts {
                    if let Some(pid) = post.get("post_id").and_then(|p| p.as_str()) {
                        let _ = self.proxy.remove_post(agent_id.as_str(), pid);
                    }
                }
            }
        }
    }

    /// No-op: board hygiene is zoanthrope's (presence TTL + its own
    /// state file). Kept so expiry call sites stay uniform.
    pub fn prune_older_than(&self, _older_than: Duration) {}

    /// No-op: the board is not persisted by lain. Kept so
    /// construction call sites stay uniform.
    pub fn replace_all(&self, _intents: Vec<Intent>) {}

    /// No-op: zoanthrope persists the board. Kept so construction
    /// call sites stay uniform.
    pub fn set_persist_callback<F>(&self, _cb: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_round_trips_status_and_scopes() {
        let scopes = vec!["auth::validate_token".to_string(), "src/a.rs".to_string()];
        let note = encode_note("fix the parser", IntentStatus::Reviewing, &scopes);
        let (body, status, decoded) = decode_note(&note);
        assert_eq!(body, "fix the parser");
        assert_eq!(status, Some(IntentStatus::Reviewing));
        assert_eq!(decoded, scopes);
    }

    #[test]
    fn note_handles_commas_and_spaces_in_scopes() {
        let scopes = vec!["GET /api/orders/{}".to_string(), "a, b".to_string()];
        let note = encode_note("", IntentStatus::Editing, &scopes);
        let (body, status, decoded) = decode_note(&note);
        assert_eq!(body, "");
        assert_eq!(status, Some(IntentStatus::Editing));
        assert_eq!(decoded, scopes);
    }

    #[test]
    fn foreign_posts_without_trailer_map_coarsely() {
        let (body, status, scopes) = decode_note("a human wrote this note");
        assert_eq!(body, "a human wrote this note");
        assert_eq!(status, None, "no trailer → caller maps post status");
        assert!(scopes.is_empty());
        assert_eq!(
            IntentStatus::from_post_status("open"),
            IntentStatus::Planning
        );
        assert_eq!(
            IntentStatus::from_post_status("taken"),
            IntentStatus::Editing
        );
        assert_eq!(
            IntentStatus::from_post_status("dropped"),
            IntentStatus::Done
        );
    }

    #[test]
    fn post_status_vocabulary_covers_all_lain_phases() {
        for s in [
            IntentStatus::Investigating,
            IntentStatus::Planning,
            IntentStatus::Editing,
            IntentStatus::Reviewing,
            IntentStatus::Done,
        ] {
            let post = s.post_status();
            assert!(matches!(post, "open" | "taken" | "done"));
        }
        // Round-trip: the trailer carries the exact phase, so the
        // coarse mirror never has to reconstruct it.
        for s in [
            IntentStatus::Investigating,
            IntentStatus::Planning,
            IntentStatus::Editing,
            IntentStatus::Reviewing,
            IntentStatus::Done,
        ] {
            let note = encode_note("", s, &[]);
            let (_, back, _) = decode_note(&note);
            assert_eq!(back, Some(s), "phase must round-trip via trailer");
        }
    }
}
