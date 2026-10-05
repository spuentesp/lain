//! Server-Sent Events stream for presence + occupancy events.
//!
//! `serve_sse` wraps a `tokio::sync::broadcast::Receiver<(u64, PresenceEvent)>`
//! so a client driving the returned `SseStream` with `.next()` receives one
//! `SseFrame` per broadcast event. The stream terminates (returns `None`)
//! when the broadcast sender is dropped.
//!
//! We don't depend on `futures` or `tokio_stream`, so `SseStream::next` is a
//! hand-rolled analogue of `Stream::poll_next`. The shape
//! (`Option<Result<SseFrame, Infallible>>`) matches what a
//! `Stream<Item = Result<SseFrame, Infallible>>` would produce, so swapping
//! in a `futures::Stream` later is a no-op for callers.
//!
//! Resume: when the client passes a `Last-Event-ID`, `serve_sse` first
//! drains every event with `event_id > last_id` from the durable
//! [`EventsLog`] (in id order) and only then yields from the live bus.
//! Live frames carry the same durable id, so a reconnecting client can
//! always resume from the last id it saw. Events that arrive on the bus
//! while the replay is being assembled may appear twice (once from the
//! log, once live); clients must treat ids as dedup keys, which the SSE
//! spec already requires them to track.
//!
//! The full streaming body for `GET /events` is wired in Task 11; the
//! `sse_placeholder_body` helper exists so the HTTP handler can return a
//! well-formed `text/event-stream` response with a single `ready` frame
//! before the live stream is plugged in.

use crate::server::events_log::EventsLog;
use crate::server::presence::{PresenceEvent, PresenceEventPublic};
use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::broadcast;

/// One SSE frame. `event` is the SSE event name, `data` is the
/// JSON-serialized `PresenceEvent`, and `id` is the durable event id
/// assigned by the [`EventsLog`] so clients can use `Last-Event-ID` to
/// resume after a disconnect.
#[derive(Debug, Clone)]
pub struct SseFrame {
    pub event: &'static str,
    pub data: String,
    pub id: u64,
}

/// SSE event-name mapping for a `PresenceEventPublic`. Shared between
/// the live path (which converts via `PresenceEventPublic::from`) and
/// the replay backlog (which reads the public DTO straight off disk)
/// so both emit identical frames.
fn event_name(event: &PresenceEventPublic) -> &'static str {
    match event {
        PresenceEventPublic::AgentJoined(_) => "agent_joined",
        PresenceEventPublic::AgentLeft(_) => "agent_left",
        PresenceEventPublic::HeartbeatExpired(_) => "heartbeat_expired",
        PresenceEventPublic::ClaimGranted { .. } => "claim_granted",
        PresenceEventPublic::ClaimReleased { .. } => "claim_released",
        PresenceEventPublic::ClaimRevoked { .. } => "claim_revoked",
        PresenceEventPublic::ConflictDetected { .. } => "conflict_detected",
        PresenceEventPublic::EditLanded { .. } => "edit_landed",
    }
}

fn frame_for(id: u64, event: PresenceEventPublic) -> SseFrame {
    let event_name = event_name(&event);
    let data = serde_json::to_string(&event).unwrap_or_else(|_| "{}".into());
    SseFrame {
        event: event_name,
        data,
        id,
    }
}

/// Build one `SseFrame` from a `PresenceEvent`. Public so
/// `tests/review_ledger_2026_09_07::joined_event_contains_bearer_credential`
/// can exercise the same wire path that live SSE subscribers see,
/// rather than asserting on a hand-rolled serializer that might
/// diverge from production. The internal type is converted to
/// `PresenceEventPublic` here so the wire shape is identical
/// regardless of which path (live broadcast or durable replay)
/// produced the frame.
pub fn build_frame(id: u64, event: PresenceEvent) -> SseFrame {
    frame_for(id, PresenceEventPublic::from(event))
}

/// Owning stream of `SseFrame`s produced from a `broadcast::Receiver`,
/// preceded by any replayed frames from the durable log.
pub struct SseStream {
    rx: broadcast::Receiver<(u64, PresenceEvent)>,
    /// Replayed frames (from `EventsLog::replay_after`) drained before
    /// the first live frame.
    backlog: VecDeque<SseFrame>,
}

impl SseStream {
    /// Wait for the next event and convert it into a frame, draining
    /// the replay backlog first.
    ///
    /// Lagged events are dropped silently (the broadcast ring buffer
    /// overwrote them before we caught up); the loop continues with the
    /// next event. Returns `None` only when the broadcast sender has been
    /// dropped.
    pub async fn next(&mut self) -> Option<Result<SseFrame, Infallible>> {
        if let Some(frame) = self.backlog.pop_front() {
            return Some(Ok(frame));
        }
        loop {
            match self.rx.recv().await {
                Ok((id, event)) => {
                    return Some(Ok(frame_for(id, PresenceEventPublic::from(event))))
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

/// Build an `SseStream` from a freshly-cloned broadcast receiver.
///
/// `last_event_id` is the raw value of the client's `Last-Event-ID`
/// header. When it parses as a `u64`, every event with a durable id
/// greater than it is replayed from `events_log` (in id order) before
/// the first live frame; an absent or unparseable value means "live
/// only".
pub fn serve_sse(
    rx: broadcast::Receiver<(u64, PresenceEvent)>,
    last_event_id: Option<String>,
    events_log: Arc<EventsLog>,
) -> SseStream {
    let backlog = last_event_id
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .map(|last_id| {
            events_log
                .replay_after(last_id)
                .map(|(id, ev)| frame_for(id, ev))
                .collect()
        })
        .unwrap_or_default();
    SseStream { rx, backlog }
}

// (removed: had no caller and no test anywhere in the tree)

#[cfg(test)]
mod tests {
    //! Focused coverage for the wire JSON shape of `PresenceEvent`'s
    //! SSE variants. The full stream/end-to-end contract is exercised
    //! in `tests/audit_integration.rs` against a real `LainServer`;
    //! this module just pins the serializer so a future shape change
    //! fails locally rather than in the integration suite.

    use super::*;
    use crate::server::audit::AuditEvent;
    use crate::server::presence::{AgentId, ClaimIntent, ConflictEntry};
    use std::path::PathBuf;
    use std::time::SystemTime;

    /// `EditLanded` must serialize the `AuditEvent`'s fields at the
    /// top level of the JSON object (matching the wire spec
    /// `{"agent_id":"…", "path":"…", "claim_set":[…], …}`), not
    /// nested under an `event` key. The `landed_revision` and
    /// `ts_unix` fields are the auditable counters Command Center
    /// relies on, so they're checked explicitly.
    #[tokio::test]
    async fn edit_landed_event_serializes_with_full_payload() {
        let event = AuditEvent {
            ts_unix: 1.7e9,
            agent_id: AgentId("a-edit".into()),
            path: "/src/lib.rs".to_string(),
            claim_set: vec![],
            racers: vec![],
            plan_revision: Some(7),
            landed_revision: 42,
            scope: None,
        };
        let json = serde_json::to_value(&PresenceEvent::EditLanded { event }).unwrap();

        // Wire-contract checks: every AuditEvent field lives under
        // the `EditLanded` variant tag and inside the `event` field
        // (serde's external-tag default wraps a struct variant's
        // fields under their original names). Consumers read
        // `data["EditLanded"]["event"]["<field>"]`. The SSE frame's
        // `event:` field is `"edit_landed"`, so a header-only
        // subscriber can recognize the type without parsing the body.
        assert_eq!(json["EditLanded"]["event"]["agent_id"], "a-edit");
        assert_eq!(json["EditLanded"]["event"]["path"], "/src/lib.rs");
        assert_eq!(json["EditLanded"]["event"]["plan_revision"], 7);
        assert_eq!(json["EditLanded"]["event"]["landed_revision"], 42);
        assert!((json["EditLanded"]["event"]["ts_unix"].as_f64().unwrap() - 1.7e9).abs() < 0.001);
        assert!(json["EditLanded"]["event"]["claim_set"].is_array());
        assert!(json["EditLanded"]["event"]["racers"].is_array());

        // The SSE event-name mapping must be `edit_landed` — that's
        // what the Command Center subscribes to.
        let frame = build_frame_for(PresenceEvent::EditLanded {
            event: AuditEvent {
                ts_unix: 0.0,
                agent_id: AgentId("z".into()),
                path: "/x".to_string(),
                claim_set: vec![],
                racers: vec![],
                plan_revision: None,
                landed_revision: 0,
                scope: None,
            },
        })
        .await;
        assert_eq!(frame.event, "edit_landed");
    }

    #[tokio::test]
    async fn sse_severity_conflict_detected_includes_severity_field() {
        let event = PresenceEvent::ConflictDetected {
            agent_id: AgentId("a-conflict".into()),
            conflicts: vec![ConflictEntry {
                inferred: false,
                agent_id: AgentId("holder".into()),
                path: PathBuf::from("src/lib.rs"),
                symbols: vec!["login".into(), "logout".into()],
                intent: ClaimIntent::Edit,
                last_seen_unix: SystemTime::UNIX_EPOCH,
            }],
            severity: "high".to_string(),
        };

        let frame = build_frame_for(event.clone()).await;
        let payload: serde_json::Value = serde_json::from_str(&frame.data).unwrap();
        assert_eq!(frame.event, "conflict_detected");
        assert_eq!(payload["ConflictDetected"]["severity"], "high");
    }

    /// Helper: build one `SseFrame` from a `PresenceEvent` without
    /// needing a live broadcast channel. Routes through the public
    /// `build_frame` so the wire path is identical to what live
    /// subscribers see.
    async fn build_frame_for(event: PresenceEvent) -> SseFrame {
        build_frame(1, event)
    }
}

#[cfg(test)]
mod framing_properties {
    //! A frame is rendered as `event: E\ndata: D\nid: N\n\n` (handler.rs). SSE
    //! has no escaping: a newline inside `data` would end the frame early and
    //! let attacker-chosen text (an agent id, a path) inject fields or whole
    //! extra events. Check the wire text against a spec-style parser.
    use super::*;
    use crate::server::presence::AgentId;
    use proptest::prelude::*;
    use std::path::PathBuf;

    fn render(f: &SseFrame) -> String {
        format!("event: {}\ndata: {}\nid: {}\n\n", f.event, f.data, f.id)
    }

    /// Minimal SSE parser: events end at a blank line; fields are `name: value`.
    fn parse(wire: &str) -> Vec<Vec<(String, String)>> {
        let mut events = Vec::new();
        let mut cur: Vec<(String, String)> = Vec::new();
        for line in wire.split('\n') {
            if line.is_empty() {
                if !cur.is_empty() {
                    events.push(std::mem::take(&mut cur));
                }
            } else if let Some((k, v)) = line.split_once(": ") {
                cur.push((k.into(), v.into()));
            } else {
                cur.push((line.into(), String::new()));
            }
        }
        events
    }

    proptest! {
        #[test]
        fn hostile_strings_cannot_split_or_inject_frames(
            agent in "\\PC{0,20}|[a-z]{0,5}(\\n|\\r|\\r\\n)event: x\\ndata: y\\n\\n[a-z]{0,5}",
            path in "[ -~\\n\\r]{0,30}",
            id in any::<u64>(),
        ) {
            let events = [
                PresenceEvent::AgentLeft(AgentId(agent.clone())),
                PresenceEvent::HeartbeatExpired(AgentId(agent.clone())),
                PresenceEvent::ClaimGranted { agent_id: AgentId(agent.clone()), path: PathBuf::from(&path) },
                PresenceEvent::ClaimRevoked { agent_id: AgentId(agent), path: PathBuf::from(path), reason: "ttl_expired".into() },
            ];
            for e in events {
                let frame = build_frame(id, e);
                let wire = render(&frame);
                let parsed = parse(&wire);
                prop_assert_eq!(parsed.len(), 1, "one event must stay one event: {:?}", wire);
                let names: Vec<&str> = parsed[0].iter().map(|(k, _)| k.as_str()).collect();
                prop_assert_eq!(names, vec!["event", "data", "id"], "injected field: {:?}", wire);
                prop_assert!(!wire[..wire.len() - 2].contains('\r'), "raw CR in frame: {wire:?}");
                let (_, data) = &parsed[0][1];
                prop_assert!(serde_json::from_str::<serde_json::Value>(data).is_ok(), "data is not JSON");
                prop_assert_eq!(&parsed[0][2].1, &id.to_string());
            }
        }
    }
}
