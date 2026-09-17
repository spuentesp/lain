//! Cooperative offthread helper for sync work the indexing pipeline
//! would otherwise run on the Tokio worker pool.
//!
//! AGENT_UX_ROADMAP.md Milestone 4 design §"Index execution and
//! consistency": "Route synchronous Git, parser, model, and
//! filesystem work through `spawn_blocking`; keep async LSP I/O on
//! Tokio tasks." The pre-fix design let a single stuck
//! `LspMultiplexer` round-trip (or a slow `git2` revwalk, or a
//! `tree-sitter` parse of a very large file) block the Tokio worker
//! holding the LSP `AsyncMutex` — every other file currently being
//! scanned would wait behind it.
//!
//! `offthread` is the one helper the migration routes through. The
//! closure runs on the blocking-thread pool; cancelling the
//! supplied `CancellationToken` aborts the blocking task and
//! resolves the future with `LainError::Cancelled`. The closure
//! itself is still responsible for any internal cancellation
//! checks (e.g. observing the token between batches of a long
//! computation); `offthread` only guarantees the closure won't
//! outlive the token.
//!
//! `Send + 'static` is required because the closure moves into
//! `spawn_blocking`. Any `Arc<Mutex<…>>` the closure needs must be
//! cloned before being moved in; the `parking_lot::Mutex` guard is
//! `!Send`, so the lock must be acquired *inside* the closure —
//! never held across the await boundary on the caller side.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::error::LainError;

/// Run `f` on Tokio's blocking-thread pool. If `cancel` is observed
/// before `f` completes, the spawned task is aborted and the future
/// resolves to `Err(LainError::Cancelled)`.
///
/// The closure runs to completion or is aborted; it cannot return a
/// partial value. A caller that needs partial progress must check
/// the token inside the closure and return early.
pub(crate) fn offthread<F, R>(cancel: CancellationToken, f: F) -> Offthread<R>
where
    F: FnOnce() -> Result<R, LainError> + Send + 'static,
    R: Send + 'static,
{
    let cancel_for_fut = cancel.clone();
    let cancel_fut = cancel_for_fut.cancelled_owned();
    let join = tokio::task::spawn_blocking(f);
    Offthread {
        cancel,
        cancel_fut: Some(Box::pin(cancel_fut)),
        join: Some(join),
    }
}

/// Future returned by [`offthread`]. Polls the underlying
/// `JoinHandle` and the cancel token concurrently.
pub(crate) struct Offthread<R> {
    cancel: CancellationToken,
    /// Pinned `cancelled()` future. We store it so we can poll it
    /// inside our own `poll` (registering the waker that wakes us
    /// when cancellation lands) without needing an async context.
    cancel_fut: Option<Pin<Box<tokio_util::sync::WaitForCancellationFutureOwned>>>,
    join: Option<JoinHandle<Result<R, LainError>>>,
}

impl<R> Future for Offthread<R> {
    type Output = Result<R, LainError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        // Cheap fast-path: token already cancelled before we even
        // start polling. Avoids one `JoinHandle::poll` round-trip.
        if this.cancel.is_cancelled() && this.join.is_some() {
            if let Some(join) = this.join.take() {
                join.abort();
            }
            return Poll::Ready(Err(LainError::Cancelled));
        }

        // Poll the cancellation future so its waker registers with
        // our `Context`. When the token fires, the next poll returns
        // `Poll::Ready(Err(LainError::Cancelled))` from the
        // cancellation branch below and we abort the join handle.
        if let Some(cancel_fut) = this.cancel_fut.as_mut() {
            if Pin::new(cancel_fut).poll(cx).is_ready() {
                if let Some(join) = this.join.take() {
                    join.abort();
                }
                this.cancel_fut = None;
                return Poll::Ready(Err(LainError::Cancelled));
            }
        }

        let join = match this.join.as_mut() {
            Some(j) => j,
            None => {
                // We already returned Cancelled; poll should not be
                // called again. Returning Ready(Cancelled) is the
                // contractually correct "stuck future" answer.
                return Poll::Ready(Err(LainError::Cancelled));
            }
        };

        match Pin::new(join).poll(cx) {
            Poll::Ready(Ok(result)) => Poll::Ready(result),
            Poll::Ready(Err(join_error)) => {
                // Tokio reports JoinError for both panic and
                // cancellation. We abort the task ourselves when the
                // token fires, so map every JoinError to Cancelled —
                // the cancellation path is the only one that should
                // produce this state in production.
                if this.cancel.is_cancelled() {
                    Poll::Ready(Err(LainError::Cancelled))
                } else {
                    Poll::Ready(Err(LainError::Other(format!(
                        "blocking task failed: {join_error}"
                    ))))
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(flavor = "current_thread")]
    async fn offthread_returns_the_closures_result() {
        let cancel = CancellationToken::new();
        let result: Result<i32, LainError> = offthread(cancel, || Ok(42)).await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn offthread_propagates_closure_errors() {
        let cancel = CancellationToken::new();
        let result: Result<i32, LainError> =
            offthread(cancel, || Err(LainError::Other("boom".into()))).await;
        assert!(matches!(result, Err(LainError::Other(_))));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn offthread_returns_cancelled_when_token_is_already_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let started = std::time::Instant::now();
        let result: Result<i32, LainError> = offthread(cancel, || Ok(1)).await;
        assert!(matches!(result, Err(LainError::Cancelled)));
        // The fast-path should not have waited for the blocking
        // thread pool at all.
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "pre-cancelled token must short-circuit; took {:?}",
            started.elapsed()
        );
    }

    /// AGENT_UX_ROADMAP.md M4 follow-up: cancelling the token
    /// while the blocking closure is mid-flight must abort the
    /// spawn_blocking task and resolve `Cancelled` within budget.
    /// This exercises the polling path inside `Future::poll` —
    /// not the fast-path — so it depends on the `cancel_fut`
    /// waker being registered with the caller's `Context`.
    #[tokio::test(flavor = "current_thread")]
    async fn offthread_aborts_in_flight_work_when_token_fires() {
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        // Fire cancellation in 50 ms; the closure sleeps for 5 s.
        let _ = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel_clone.cancel();
        });
        let started = std::time::Instant::now();
        let result: Result<i32, LainError> = offthread(cancel, || {
            std::thread::sleep(Duration::from_secs(5));
            Ok(99)
        })
        .await;
        assert!(
            matches!(result, Err(LainError::Cancelled)),
            "expected Cancelled; got {result:?}"
        );
        // Cancel lands ~50 ms in; offthread should resolve well
        // under the 5 s sleep. Allow generous slack for slow CI.
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "cancellation did not bound the work; took {:?}",
            started.elapsed()
        );
    }
}
