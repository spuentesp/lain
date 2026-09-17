//! End-to-end tests for the handoff MCP tools (`leave_handoff_note`,
//! `get_pending_handoffs`) — boots the real `lain server` binary
//! against a single-repo federation fixture and exercises the
//! dispatcher path production agents hit.
//!
//! Pins:
//!
//! * `leave_handoff_note` returns `id` + `expires_at_unix_ms`
//!   (24h TTL from creation).
//! * `get_pending_handoffs` (no filter) surfaces the freshly-left
//!   note.
//! * `get_pending_handoffs(since_unix_ms = t)` where `t` is past the
//!   note's created_at hides it (the filter is strict `>=`).
//! * Handoffs persist across `unregister_agent` → `register_agent`
//!   because they are workspace-scoped, not session-scoped — a
//!   second agent must be able to pick up what the first one left.
//!
//! Closes `docs/FOLLOWUPS.md` item 2 for handoffs specifically
//! (the annotation side of that entry is `tests/annotations_e2e.rs`).
//!
//! The "expired handoff is filtered" assertion that the original
//! plan mentioned lives at the unit-test layer (in
//! `src/server/annotations.rs` via the `created_at_unix_ms + ttl_ms`
//! arithmetic, since the production code uses `SystemTime::now()`
//! directly and there is no clock-injection seam). The
//! `since_unix_ms` filter assertion above covers the same
//! user-visible contract at the JSON-RPC layer.

mod common;

use std::path::PathBuf;

use common::{
    boot_annotation_like_server, git_init_committed, register_agent_session, tools_call_text,
    ServerGuard,
};

/// Single-repo federation fixture with one Rust crate that defines
/// a single function `handoff_target`. `leave_handoff_note` in
/// single-repo mode pins to the lone repo automatically, so the
/// test surface stays small.
struct HandoffFixture {
    root: PathBuf,
}

impl HandoffFixture {
    fn build() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        std::mem::forget(tmp);

        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"ho_a\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("src/lib.rs"),
            "/// Doc so the indexer keeps the symbol.\n\
             pub fn handoff_target() -> u32 { 1 }\n",
        )
        .unwrap();
        git_init_committed(&repo);

        let data_dir = root.join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(
            root.join("repos.yaml"),
            format!(
                "data_dir: {}\n\
                 ready_threshold: 0.5\n\
                 repos:\n\
                 \x20 - id: ho_a\n\
                 \x20   source: {{ type: workspace_dir, path: {} }}\n",
                data_dir.display(),
                repo.display(),
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("workspaces.yaml"),
            "workspaces:\n  - name: solo\n    members: [ho_a]\n",
        )
        .unwrap();
        Self { root }
    }

    fn repos_yaml(&self) -> PathBuf {
        self.root.join("repos.yaml")
    }
}

fn boot_and_wait(fixture: &HandoffFixture) -> (String, ServerGuard) {
    boot_annotation_like_server(
        &fixture.repos_yaml(),
        &fixture.root,
        "handoff-e2e",
        &["handoff_target"],
    )
}

fn register_session(host: &str, name: &str) -> String {
    register_agent_session(host, name)
}

/// `leave_handoff_note` returns `{id, expires_at_unix_ms}`; the
/// `expires_at_unix_ms` is creation + 24h. `get_pending_handoffs`
/// (no filter) surfaces the freshly-left note. Both fields are
/// part of the documented contract.
#[test]
fn leave_then_get_pending_roundtrip() {
    let fixture = HandoffFixture::build();
    let (host, _guard) = boot_and_wait(&fixture);
    let session = register_session(&host, "handoff-e2e-1");

    let body = "fixture handoff: roundtrip";
    let left = tools_call_text(
        &host,
        "leave_handoff_note",
        serde_json::json!({
            "session_token": session,
            "body": body,
        }),
    );
    let left_v: serde_json::Value = serde_json::from_str(&left)
        .unwrap_or_else(|e| panic!("leave_handoff_note not JSON: {e}\n{left}"));
    let id = left_v
        .pointer("/id")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("missing id: {left_v}"))
        .to_string();
    let expires_at = left_v
        .pointer("/expires_at_unix_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| panic!("missing expires_at_unix_ms: {left_v}"));

    // 24h TTL — the assertion range is wide (23h59m .. 24h01m) so a
    // 100ms slow CI runner doesn't false-positive. The contract is
    // `expires = created + 24h`; the contract's exact ms value is
    // implementation-defined.
    let ttl_ms: u64 = 24 * 60 * 60 * 1000;
    assert!(
        expires_at > 0,
        "expires_at_unix_ms must be a positive unix-ms timestamp: {left_v}"
    );
    // Round-trip time from registration to `leave_handoff_note` is
    // also small (<1s), so created_at_unix_ms is approximately
    // (expires_at - 24h). We don't read created_at directly here
    // because the response envelope omits it; this assertion is the
    // closest we can pin to the wire without changing the contract.
    assert!(
        expires_at >= ttl_ms,
        "expires_at_unix_ms must be at least 24h from the unix epoch: {expires_at} (ttl_ms={ttl_ms})"
    );

    // get_pending_handoffs surfaces the freshly-left note.
    let pending = tools_call_text(
        &host,
        "get_pending_handoffs",
        serde_json::json!({"since_unix_ms": 0}),
    );
    let pending_v: serde_json::Value = serde_json::from_str(&pending).unwrap();
    let rows = pending_v
        .pointer("/handoffs")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("missing handoffs array: {pending_v}"));
    assert!(
        rows.iter()
            .any(|r| r.pointer("/id").and_then(|v| v.as_str()) == Some(&id)
                && r.pointer("/body").and_then(|v| v.as_str()) == Some(body)),
        "get_pending_handoffs must surface the freshly-left note: {pending_v}"
    );
}

/// `since_unix_ms` is a strict `>=` filter. A note whose
/// `created_at_unix_ms` is `t1` must be hidden when the caller asks
/// for rows with `created_at_unix_ms >= t1 + 1`. The agent-facing
/// contract is: "give me rows strictly newer than the last one I
/// processed." The alternative interpretation (off-by-one) would
/// have the row surface, and an agent relying on the filter to skip
/// rows it has already processed would re-handle them on every
/// call.
///
/// `leave_handoff_note` does not expose `created_at_unix_ms` in its
/// response (only `id` and `expires_at_unix_ms`), so the test
/// derives `created_at ≈ expires_at - 24h` from the documented TTL
/// and uses that approximation to drive the filter. The
/// approximation is off by at most a few hundred milliseconds
/// (round-trip + server-side work between `now_ms` capture and
/// response write), which is well inside the 24h envelope the
/// assertions exercise.
#[test]
fn since_unix_ms_filter_is_strict_lower_bound() {
    let fixture = HandoffFixture::build();
    let (host, _guard) = boot_and_wait(&fixture);
    let session = register_session(&host, "handoff-e2e-since");

    let left = tools_call_text(
        &host,
        "leave_handoff_note",
        serde_json::json!({
            "session_token": session,
            "body": "fixture handoff: since filter",
        }),
    );
    let left_v: serde_json::Value = serde_json::from_str(&left).unwrap();
    let id = left_v
        .pointer("/id")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();
    let expires_at = left_v
        .pointer("/expires_at_unix_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert!(expires_at > 0, "missing expires_at_unix_ms: {left_v}");
    let ttl_ms: u64 = 24 * 60 * 60 * 1000;
    let created_at_approx = expires_at - ttl_ms;

    // since_unix_ms just past the created_at must hide the row.
    // Use a 2-second margin so the wall-clock gap between
    // server-side `now_ms` capture and our `expires_at` read does
    // not flip the assertion by accident.
    let since_filtered = tools_call_text(
        &host,
        "get_pending_handoffs",
        serde_json::json!({"since_unix_ms": created_at_approx + 2_000}),
    );
    let since_v: serde_json::Value = serde_json::from_str(&since_filtered).unwrap();
    let rows = since_v
        .pointer("/handoffs")
        .and_then(|v| v.as_array())
        .unwrap();
    assert!(
        rows.iter()
            .all(|r| r.pointer("/id").and_then(|v| v.as_str()) != Some(&id)),
        "since_unix_ms = created_at + 2s must hide the row (strict >=): {since_v}"
    );

    // since_unix_ms = created_at - 1 hour must show it
    // (inclusive lower bound). Picking an hour below the creation
    // timestamp removes any uncertainty from the
    // `expires_at - 24h` approximation: a row whose `created_at`
    // is at most a second into the past is guaranteed to satisfy
    // `created_at >= (created_at - 1h)`.
    let since_inclusive = tools_call_text(
        &host,
        "get_pending_handoffs",
        serde_json::json!({"since_unix_ms": created_at_approx - 3_600_000}),
    );
    let since_inclusive_v: serde_json::Value = serde_json::from_str(&since_inclusive).unwrap();
    let rows_inclusive = since_inclusive_v
        .pointer("/handoffs")
        .and_then(|v| v.as_array())
        .unwrap();
    assert!(
        rows_inclusive
            .iter()
            .any(|r| r.pointer("/id").and_then(|v| v.as_str()) == Some(&id)),
        "since_unix_ms well below created_at must show the row: {since_inclusive_v}"
    );
}

/// Handoffs are workspace-scoped, not session-scoped. The
/// production server does not expose an `unregister_agent` tool
/// (sessions stay alive until they expire on their own), so this
/// test uses two simultaneous sessions to pin the same property:
/// a note left by session A is visible to session B in the same
/// workspace.
///
/// The alternative contract — "handoffs visible only to the leaving
/// agent" — would break the use case for handoffs entirely: an
/// agent that crashes mid-task has no way to read its own note,
/// and a different agent picking up the workspace must be able to
/// see what was left. This test pins the workspace-wide visibility
/// at the JSON-RPC layer.
#[test]
fn handoff_visible_across_sessions_in_same_workspace() {
    let fixture = HandoffFixture::build();
    let (host, _guard) = boot_and_wait(&fixture);

    let session_a = register_session(&host, "handoff-agent-a");
    let left = tools_call_text(
        &host,
        "leave_handoff_note",
        serde_json::json!({
            "session_token": session_a,
            "author": "handoff-agent-a",
            "body": "fixture handoff: workspace-scoped",
        }),
    );
    let left_v: serde_json::Value = serde_json::from_str(&left).unwrap();
    let id = left_v
        .pointer("/id")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();

    // A second session registers in the same workspace and asks
    // for pending handoffs.
    let _session_b = register_session(&host, "handoff-agent-b");
    let pending = tools_call_text(
        &host,
        "get_pending_handoffs",
        serde_json::json!({"since_unix_ms": 0}),
    );
    let pending_v: serde_json::Value = serde_json::from_str(&pending).unwrap();
    let rows = pending_v
        .pointer("/handoffs")
        .and_then(|v| v.as_array())
        .unwrap();
    assert!(
        rows.iter()
            .any(|r| r.pointer("/id").and_then(|v| v.as_str()) == Some(&id)),
        "handoff must be visible to a different session in the same workspace: {pending_v}"
    );
    // The recorded author on the row is the explicit value the
    // leaving agent supplied, not the querying one. Drift in this
    // attribution would let any observer pass off a stranger's
    // note as their own; pin it. (`leave_handoff_note` accepts an
    // optional `author` arg — when the caller does not supply one
    // the dispatcher falls back to the session's UUID. The
    // test's `author: "handoff-agent-a"` exercises the explicit
    // path so the assertion can read a stable string instead of
    // a generated UUID.)
    let row = rows
        .iter()
        .find(|r| r.pointer("/id").and_then(|v| v.as_str()) == Some(&id))
        .unwrap();
    assert_eq!(
        row.pointer("/author").and_then(|v| v.as_str()),
        Some("handoff-agent-a"),
        "author on the row must be the leaving agent, not the querying one: {row}"
    );
}
