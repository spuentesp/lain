//! Bounded history of overlay diffs, keyed by monotonic `RevisionId`.
//!
//! The `RevisionLog` is the sidecar's view of "what changed since the last
//! time I asked". Each `OverlayDiff` is assigned a strictly-increasing revision
//! on enqueue; older revisions are evicted once the buffer is full so the
//! log's memory footprint stays bounded (`O(capacity)` diffs, not unbounded
//! history).
//!
//! Consumers typically pin a revision they have already observed and ask
//! `diffs_since` for the strictly-newer set on every tool call. `diffs_since`
//! is the only authoritative answer to "what is new?"; the underlying
//! `VolatileOverlay` only knows the latest merged state.
//!
//! This is Task 1.1 of the coordination staleness/audit design (PR 1).
//! Task 1.2 embeds a `RevisionLog` in `VolatileOverlay` and feeds it from the
//! overlay subscription path; nothing else outside this module reads it yet.
use crate::overlay::stream::OverlayDiff;
use std::collections::VecDeque;

/// Monotonic id assigned to each enqueued diff. Reuses the alias already
/// defined in `crate::overlay::stream` so callers and the broadcast bus
/// speak the same id space.
pub use crate::overlay::stream::RevisionId;

/// Outcome of `RevisionLog::diffs_since` when the requested revision falls
/// outside the retained window.
///
/// * `Ok` — the requested revision is within the buffered range; the returned
///   `Vec` may be empty (when `rev == current_revision`) or populated.
/// * `BeyondCurrent` — the caller asked for a revision newer than anything the
///   log has seen. This signals "the world is ahead of you" (e.g. a sidecar
///   reconnected mid-stream and is using a stale `current_revision`). The
///   caller should re-hydrate from a snapshot rather than trust the empty
///   answer.
/// * `TooOld` — the requested revision has already been evicted from the
///   ring buffer. Anything older than `floor_revision` cannot be replayed;
///   callers must fall back to a full snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupResult {
    /// Requested revision is within the retained window.
    Ok,
    /// Requested revision is newer than `current_revision`.
    BeyondCurrent,
    /// Requested revision is older than `floor_revision` and has been evicted.
    TooOld,
}

/// Fixed-capacity ring of `OverlayDiff`s with revision-keyed lookup.
///
/// Backed by a `VecDeque`; eviction happens on every enqueue once the buffer
/// is full (drop the oldest, append the newest). Allocating the storage up
/// front via `with_capacity` keeps the steady-state allocation pattern
/// predictable for long-lived sidecars.
#[derive(Debug)]
pub struct RevisionLog {
    diffs: VecDeque<OverlayDiff>,
    capacity: usize,
    /// Next revision to assign. Starts at 0 so the first enqueue produces id 1.
    next: RevisionId,
}

impl RevisionLog {
    /// Default-capacity log (256 diffs). Convenience for callers that do
    /// not care about the exact bound.
    pub fn new() -> Self {
        Self::with_capacity(256)
    }

    /// Build a log with a custom retention cap. Floor of 1 to avoid a
    /// zero-capacity log that would evict every diff it just received.
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            diffs: VecDeque::with_capacity(cap),
            capacity: cap.max(1),
            next: 0,
        }
    }

    /// Highest revision id assigned so far. Returns 0 when the log is empty.
    pub fn current_revision(&self) -> RevisionId {
        self.next
    }

    /// Oldest revision id still retained, or 0 when the log is empty.
    /// Revisions below this are evicted and unavailable via `diffs_since`.
    pub fn floor_revision(&self) -> RevisionId {
        self.diffs.front().map(|d| d.revision).unwrap_or(0)
    }

    /// Append a diff and assign it the next sequential revision.
    ///
    /// The caller-supplied `diff.revision` is ignored and overwritten with
    /// the freshly assigned id; the log is the single source of truth for
    /// revision numbering so subscribers and producers agree on the id
    /// space. Returns the assigned id so the caller can pin it.
    pub fn enqueue(&mut self, mut diff: OverlayDiff) -> RevisionId {
        self.next += 1;
        diff.revision = self.next;
        if self.diffs.len() == self.capacity {
            self.diffs.pop_front();
        }
        self.diffs.push_back(diff);
        self.next
    }

    /// Return all retained diffs strictly newer than `rev`.
    ///
    /// * `rev == current_revision` → `Ok(vec![])`.
    /// * `rev > current_revision` → `Err(BeyondCurrent)` (caller is behind
    ///   the bus; should re-hydrate from a snapshot).
    /// * `rev < floor_revision` → `Err(TooOld)` (history has been evicted).
    /// * Otherwise `Ok(Vec<OverlayDiff>)` with at least one entry.
    pub fn diffs_since(&self, rev: RevisionId) -> Result<Vec<OverlayDiff>, LookupResult> {
        if rev > self.next {
            return Err(LookupResult::BeyondCurrent);
        }
        if !self.diffs.is_empty() && rev < self.floor_revision() {
            return Err(LookupResult::TooOld);
        }
        Ok(self.diffs.iter().filter(|d| d.revision > rev).cloned().collect())
    }
}

impl Default for RevisionLog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{GraphNode, NodeType};

    fn fake_diff(rev: u64) -> OverlayDiff {
        OverlayDiff {
            revision: rev,
            added: vec![GraphNode::new(NodeType::Function, format!("f{rev}"), "/p.rs".into())],
            removed: vec![], updated: vec![],
        }
    }

    #[test]
    fn empty_log_returns_zero() {
        let log = RevisionLog::new();
        assert_eq!(log.current_revision(), 0);
        assert!(matches!(log.diffs_since(0), Ok(vec) if vec.is_empty()));
    }

    #[test]
    fn enqueue_assigns_sequential_revisions() {
        let mut log = RevisionLog::with_capacity(8);
        assert_eq!(log.enqueue(fake_diff(0)), 1); // caller-supplied revision is ignored
        assert_eq!(log.enqueue(fake_diff(99)), 2);
        assert_eq!(log.current_revision(), 2);
    }

    #[test]
    fn diffs_since_returns_only_strictly_newer() {
        let mut log = RevisionLog::with_capacity(8);
        log.enqueue(fake_diff(0)); // assigned 1
        log.enqueue(fake_diff(0)); // assigned 2
        log.enqueue(fake_diff(0)); // assigned 3
        let out = log.diffs_since(1).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].revision, 2);
        assert_eq!(out[1].revision, 3);
    }

    #[test]
    fn diffs_since_beyond_current_returns_beyond_current() {
        let mut log = RevisionLog::with_capacity(8);
        log.enqueue(fake_diff(0)); // → rev 1
        assert!(matches!(log.diffs_since(99), Err(LookupResult::BeyondCurrent)));
    }

    #[test]
    fn ring_evicts_too_old() {
        let mut log = RevisionLog::with_capacity(4);
        for _ in 0..10 { log.enqueue(fake_diff(0)); } // 10 enqueues, cap 4
        assert_eq!(log.current_revision(), 10);
        assert_eq!(log.floor_revision(), 7);
        assert!(matches!(log.diffs_since(5), Err(LookupResult::TooOld)));
        let ok = log.diffs_since(7).unwrap();
        // Strict `>` semantic, consistent with `diffs_since_returns_only_strictly_newer`:
        // floor is 7, current is 10, so only 8/9/10 are strictly newer than 7.
        // (The brief spec asserted 4, which would imply inclusive; that
        // contradicts the strict semantic established by test 3.)
        assert_eq!(ok.len(), 3);
    }
}
