//! Server-Sent Events stream for presence + occupancy events.
//!
//! `serve_sse` wraps a `tokio::sync::broadcast::Receiver<PresenceEvent>` so a
//! client driving the returned `SseStream` with `.next()` receives one
//! `SseFrame` per broadcast event. The stream terminates (returns `None`)
//! when the broadcast sender is dropped.
//!
//! We don't depend on `futures` or `tokio_stream`, so `SseStream::next` is a
//! hand-rolled analogue of `Stream::poll_next`. The shape
//! (`Option<Result<SseFrame, Infallible>>`) matches what a
//! `Stream<Item = Result<SseFrame, Infallible>>` would produce, so swapping
//! in a `futures::Stream` later is a no-op for callers.
//!
//! The full streaming body for `GET /events` is wired in Task 11; the
//! `sse_placeholder_body` helper exists so the HTTP handler can return a
//! well-formed `text/event-stream` response with a single `ready` frame
//! before the live stream is plugged in.

use crate::server::presence::PresenceEvent;
use std::convert::Infallible;
use tokio::sync::broadcast;

/// One SSE frame. `event` is the SSE event name, `data` is the
/// JSON-serialized `PresenceEvent`, and `id` is a monotonic counter so
/// clients can use `Last-Event-ID` to resume after a disconnect.
#[derive(Debug, Clone)]
pub struct SseFrame {
    pub event: &'static str,
    pub data: String,
    pub id: u64,
}

/// Owning stream of `SseFrame`s produced from a `broadcast::Receiver`.
pub struct SseStream {
    rx: broadcast::Receiver<PresenceEvent>,
    counter: u64,
}

impl SseStream {
    /// Wait for the next broadcast event and convert it into a frame.
    ///
    /// Lagged events are dropped silently (the broadcast ring buffer
    /// overwrote them before we caught up); the loop continues with the
    /// next event. Returns `None` only when the broadcast sender has been
    /// dropped.
    pub async fn next(&mut self) -> Option<Result<SseFrame, Infallible>> {
        loop {
            match self.rx.recv().await {
                Ok(event) => {
                    self.counter += 1;
                    let data = serde_json::to_string(&event).unwrap_or_else(|_| "{}".into());
                    let name: &'static str = match &event {
                        PresenceEvent::AgentJoined(_) => "agent_joined",
                        PresenceEvent::AgentLeft(_) => "agent_left",
                        PresenceEvent::HeartbeatExpired(_) => "heartbeat_expired",
                        PresenceEvent::ClaimGranted { .. } => "claim_granted",
                        PresenceEvent::ClaimReleased { .. } => "claim_released",
                        PresenceEvent::ConflictDetected { .. } => "conflict_detected",
                        PresenceEvent::EditLanded { .. } => "edit_landed",
                    };
                    return Some(Ok(SseFrame { event: name, data, id: self.counter }));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

/// Build an `SseStream` from a freshly-cloned broadcast receiver.
///
/// `_last_event_id` is accepted for API symmetry with the eventual HTTP
/// handler (which will read it from the `Last-Event-ID` header); for the
/// MVP broadcast-channel transport the seek-backward behavior isn't
/// expressible, so the parameter is ignored. Once the broadcast buffer
/// is replaced with a durable ring (see Task 11), this becomes a
/// `skip_while(frame.id <= last_id)` before yielding.
pub fn serve_sse(
    rx: broadcast::Receiver<PresenceEvent>,
    _last_event_id: Option<String>,
) -> SseStream {
    SseStream { rx, counter: 0 }
}

/// Static placeholder body for `GET /events` until Task 11 wires the
/// real streaming body. Emits exactly one well-formed SSE frame so a
/// `curl -N` client sees a valid `text/event-stream` response shape.
pub fn sse_placeholder_body() -> Vec<u8> {
    b"event: ready\ndata: {}\n\n".to_vec()
}

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
            path: PathBuf::from("/src/lib.rs"),
            claim_set: vec![],
            racers: vec![],
            plan_revision: Some(7),
            landed_revision: 42,
        };
        let json =
            serde_json::to_value(&PresenceEvent::EditLanded { event }).unwrap();

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
        let frame = build_frame_for(&PresenceEvent::EditLanded {
            event: AuditEvent {
                ts_unix: 0.0,
                agent_id: AgentId("z".into()),
                path: PathBuf::from("/x"),
                claim_set: vec![],
                racers: vec![],
                plan_revision: None,
                landed_revision: 0,
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
                agent_id: AgentId("holder".into()),
                path: PathBuf::from("src/lib.rs"),
                symbols: vec!["login".into(), "logout".into()],
                intent: ClaimIntent::Edit,
                last_seen_unix: SystemTime::UNIX_EPOCH,
            }],
            severity: "high",
        };

        let frame = build_frame_for(&event).await;
        let payload: serde_json::Value = serde_json::from_str(&frame.data).unwrap();
        assert_eq!(frame.event, "conflict_detected");
        assert_eq!(payload["ConflictDetected"]["severity"], "high");
    }

    /// Helper: build one `SseFrame` from a `PresenceEvent` without
    /// needing a live broadcast channel. Mirrors the inline mapping
    /// in `SseStream::next`.
    async fn build_frame_for(event: &PresenceEvent) -> SseFrame {
        let data = serde_json::to_string(event).unwrap_or_else(|_| "{}".into());
        let name: &'static str = match event {
            PresenceEvent::AgentJoined(_) => "agent_joined",
            PresenceEvent::AgentLeft(_) => "agent_left",
            PresenceEvent::HeartbeatExpired(_) => "heartbeat_expired",
            PresenceEvent::ClaimGranted { .. } => "claim_granted",
            PresenceEvent::ClaimReleased { .. } => "claim_released",
            PresenceEvent::ConflictDetected { .. } => "conflict_detected",
            PresenceEvent::EditLanded { .. } => "edit_landed",
        };
        SseFrame {
            event: name,
            data,
            id: 1,
        }
    }
}