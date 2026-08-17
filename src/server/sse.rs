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