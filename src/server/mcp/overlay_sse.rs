//! Streaming response body for `/overlay/subscribe` (the SSE endpoint
//! the sidecar clients use to pull overlay diffs). Polls an mpsc
//! channel that is fed by a tokio task that pumps broadcast events
//! into JSON bytes. When the client disconnects (the channel is
//! closed), the body returns `None` and hyper finishes the response.

use hyper::body::{Bytes, Frame};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc;

pub struct OverlaySubscribeBody {
    rx: mpsc::UnboundedReceiver<std::io::Result<Bytes>>,
}

impl OverlaySubscribeBody {
    /// Construct a streaming body from a channel of `Bytes` chunks.
    /// Callers in `handler.rs` hand off the receiver side of an
    /// `UnsyncBoxBody`; the struct then drives the polling.
    pub fn new(rx: mpsc::UnboundedReceiver<std::io::Result<Bytes>>) -> Self {
        Self { rx }
    }
}

impl http_body::Body for OverlaySubscribeBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::io::Result<Frame<Self::Data>>>> {
        match Pin::new(&mut self.rx).poll_recv(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                Poll::Ready(Some(Ok(Frame::data(bytes))))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}
