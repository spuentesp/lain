//! Lifecycle — server start time and uptime.
//!
//! Extracted from `LainServer` in PR 3.7a. `LainServer` will hold an
//! `Arc<LifecycleInfo>` in PR 3.7b and forward `started_at()` through.

use std::time::SystemTime;

/// Process start time, captured at construction. Used by
/// `get_server_status` to report uptime.
pub struct LifecycleInfo {
    pub(crate) started_at: SystemTime,
}

impl LifecycleInfo {
    pub fn new(started_at: SystemTime) -> Self {
        Self { started_at }
    }

    /// Process start time, captured at construction. Used by
    /// `get_server_status` to report uptime.
    pub fn started_at(&self) -> SystemTime {
        self.started_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_the_started_at_passed_in() {
        let now = SystemTime::now();
        let handle = LifecycleInfo::new(now);
        assert_eq!(handle.started_at(), now);
    }
}
