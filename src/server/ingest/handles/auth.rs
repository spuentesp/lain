//! Auth — per-key auth + rate limit state.
//!
//! Extracted from `LainServer` in PR 3.7a. `LainServer` will hold an
//! `Arc<AuthHandle>` in PR 3.7b.
//!
//! No public methods live on `LainServer` for `auth` today; the slot
//! exists for the HTTP request handler (which currently clones the
//! `Arc` straight from the constructor). The handle reserves the
//! shape so future per-request helpers (e.g. `auth_for_request`) have
//! a home.

use crate::server::auth::AuthState;
use std::sync::Arc;

/// Per-key auth + rate limit (P0 #1). Populated from `LAIN_API_KEYS`
/// and `LAIN_RATE_LIMIT_RPM` env vars at server startup.
pub struct AuthHandle {
    pub(crate) auth: Arc<AuthState>,
}

impl AuthHandle {
    pub fn new(auth: Arc<AuthState>) -> Self {
        Self { auth }
    }

    /// Borrowed handle to the auth + rate limit state.
    pub fn auth(&self) -> &Arc<AuthState> {
        &self.auth
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::auth::AuthState;

    #[test]
    fn exposes_the_state_it_was_built_with() {
        let state = Arc::new(AuthState::from_env());
        let handle = AuthHandle::new(Arc::clone(&state));
        assert!(Arc::ptr_eq(handle.auth(), &state));
    }
}
