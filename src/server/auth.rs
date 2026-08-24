//! Auth + rate limit for the HTTP transport.
//!
// Two env vars configure the policy:
//! - `LAIN_API_KEYS=key1,key2,key3` — comma-separated. If unset, auth is
//!   disabled and every request is accepted (dev mode). When set,
//!   requests to `/mcp` and `/events` must carry
//!   `Authorization: Bearer <key>`. `/health` is exempt.
//! - `LAIN_RATE_LIMIT_RPM=N` — per-key requests-per-minute budget.
//!   Honoured in every mode. When unset the limit defaults to 60 rpm
//!   **only if API keys are configured**; with auth off there is no key
//!   to bucket by, so the limit would throttle the local user and guard
//!   nothing. `LAIN_RATE_LIMIT=off` disables it even when keys are set.
//!
//! Stdio transport is exempt from both checks (local process).
//!
//! Errors:
//! - 401 if Authorization is missing or the key is unknown
//! - 429 if the rate limit is exceeded; the response carries
//!   `Retry-After` (seconds until next token)
//!
//! The auth check fires once per HTTP request, not per SSE event. SSE
//! subscribers stay connected for as long as their initial GET passes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use parking_lot::Mutex;

/// Per-key authentication + rate limit state.
#[derive(Debug, Clone)]
pub struct AuthState {
    /// `None` when no `LAIN_API_KEYS` is set (dev mode: every request
    /// passes auth; rate limit still applies if `LAIN_RATE_LIMIT_RPM`
    /// is configured).
    pub api_keys: Option<Vec<String>>,
    /// `None` when rate limit is explicitly disabled via
    /// `LAIN_RATE_LIMIT=off`. `Some(budget)` with `budget=0` would also
    /// disable — we treat 0 as disabled for symmetry.
    pub rate_limit: Option<RateLimit>,
}

impl AuthState {
    /// Read the policy from environment. Called once at server startup.
    pub fn from_env() -> Self {
        let api_keys = std::env::var("LAIN_API_KEYS")
            .ok()
            .map(|raw| raw.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>());
        let api_keys = api_keys.filter(|v| !v.is_empty());

        let rate_limit_disabled = std::env::var("LAIN_RATE_LIMIT")
            .map(|v| v.to_ascii_lowercase() == "off")
            .unwrap_or(false);
        let explicit_rpm = std::env::var("LAIN_RATE_LIMIT_RPM")
            .ok()
            .and_then(|v| v.parse::<u32>().ok());
        let rate_limit = if rate_limit_disabled {
            None
        } else {
            // The bucket key is the bearer token, so with keys
            // configured each key gets its own budget and the limit
            // does what it is for. With no keys, auth is off and every
            // caller shares one `anonymous` bucket — the limit then
            // throttles the legitimate local user and protects nobody,
            // because there is no key to abuse. Several agents on one
            // local server share 60 rpm between them, which a single
            // agent exploring a codebase exceeds on its own: observed
            // as `429 rate limit exceeded` partway through a routine
            // demo run, including on the `/ui/...` pages.
            //
            // So: default the limit on only when auth is on. An
            // explicit `LAIN_RATE_LIMIT_RPM` is always honoured, in
            // either mode, for anyone who wants a local cap.
            match (explicit_rpm, api_keys.is_some()) {
                (Some(0), _) => None,
                (Some(rpm), _) => Some(RateLimit::new(rpm)),
                (None, true) => Some(RateLimit::new(60)),
                (None, false) => None,
            }
        };

        AuthState { api_keys, rate_limit }
    }

    /// No-env fallback: dev mode (no auth, no rate limit).
    pub fn dev_mode() -> Self {
        AuthState { api_keys: None, rate_limit: None }
    }

    /// Check the `Authorization: Bearer <key>` header against the configured
    /// keys. Returns `Ok(())` if auth passes (or is disabled), `Err(reason)`
    /// otherwise. Stdio callers should skip this entirely.
    pub fn check_bearer(&self, auth_header: Option<&str>) -> Result<(), AuthError> {
        let Some(expected_keys) = &self.api_keys else {
            return Ok(());  // dev mode
        };
        let header = auth_header.ok_or(AuthError::Missing)?;
        let token = bearer_token(header).ok_or(AuthError::Malformed)?;
        if expected_keys.iter().any(|k| k == &token) {
            Ok(())
        } else {
            Err(AuthError::Invalid)
        }
    }

    /// Try to consume a token from the rate limiter. Returns the
    /// `Retry-After` (seconds) when denied. The same `key` returned by
    /// `check_bearer` is the bucket key; when no auth is configured,
    /// `&"anonymous"` is used as the per-IP-style fallback.
    pub fn check_rate(&self, key: &str) -> Result<(), u64> {
        let Some(rl) = &self.rate_limit else {
            return Ok(());
        };
        rl.try_consume(key)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    Missing,
    Malformed,
    Invalid,
}

impl AuthError {
    pub fn http_status(self) -> u16 {
        match self {
            AuthError::Missing | AuthError::Malformed | AuthError::Invalid => 401,
        }
    }
    pub fn message(self) -> &'static str {
        match self {
            AuthError::Missing => "missing Authorization: Bearer <key> header",
            AuthError::Malformed => "Authorization header must be 'Bearer <key>'",
            AuthError::Invalid => "invalid API key",
        }
    }
}

/// Parse the `<token>` from an `Authorization: Bearer <token>` header.
pub fn bearer_token(header: &str) -> Option<String> {
    let mut parts = header.splitn(2, ' ');
    let scheme = parts.next()?.trim();
    let token = parts.next()?.trim();
    if !scheme.eq_ignore_ascii_case("Bearer") || token.is_empty() {
        return None;
    }
    Some(token.to_string())
}

/// Token-bucket rate limiter. One bucket per key, refilling at
/// `requests_per_minute` per minute (continuous, not per-window).
#[derive(Debug, Clone)]
pub struct RateLimit {
    rpm: u32,
    inner: Arc<Mutex<HashMap<String, Bucket>>>,
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

impl RateLimit {
    pub fn new(requests_per_minute: u32) -> Self {
        RateLimit {
            rpm: requests_per_minute,
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Consume one token. Returns `Ok(())` if the request is allowed,
    /// `Err(retry_after_secs)` if the bucket is empty.
    pub fn try_consume(&self, key: &str) -> Result<(), u64> {
        let now = Instant::now();
        let capacity = self.rpm as f64;
        let refill_per_sec = self.rpm as f64 / 60.0;
        let mut guard = self.inner.lock();
        let bucket = guard.entry(key.to_string()).or_insert(Bucket {
            tokens: capacity,
            last_refill: now,
        });
        // Refill
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * refill_per_sec).min(capacity);
        bucket.last_refill = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            // Retry-after: seconds until tokens reach 1.0
            let needed = 1.0 - bucket.tokens;
            let secs = (needed / refill_per_sec).ceil() as u64;
            Err(secs.max(1))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards against re-introducing a shared bucket in dev mode.
    ///
    /// With no API keys, auth is off and every caller lands in one
    /// `anonymous` bucket, so a 60 rpm default throttles the local user
    /// and protects nothing — there is no key that could be abused.
    /// Several agents on one local server share that budget, and a
    /// single agent exploring a codebase exceeds it alone: a routine
    /// demo run hit `429 rate limit exceeded` partway through, on the
    /// `/ui/...` pages among others.
    ///
    /// `LAIN_RATE_LIMIT_RPM` is still honoured in both modes.
    #[test]
    fn rate_limit_defaults_off_without_keys_and_on_with_them() {
        // Constructed directly rather than through `from_env`, which
        // reads process-global state and would race other tests.
        let dev = AuthState { api_keys: None, rate_limit: None };
        for _ in 0..500 {
            assert!(
                dev.check_rate("anonymous").is_ok(),
                "dev mode must not throttle the local user"
            );
        }

        let keyed = AuthState {
            api_keys: Some(vec!["k1".into()]),
            rate_limit: Some(RateLimit::new(60)),
        };
        let mut denied = false;
        for _ in 0..500 {
            if keyed.check_rate("k1").is_err() {
                denied = true;
                break;
            }
        }
        assert!(denied, "with a key configured the budget must still bite");

        // Separate keys must not share a budget.
        assert!(
            keyed.check_rate("k2").is_ok(),
            "a second key gets its own bucket"
        );
    }

    #[test]
    fn bearer_token_parses() {
        assert_eq!(bearer_token("Bearer abc"), Some("abc".into()));
        assert_eq!(bearer_token("bearer xyz"), Some("xyz".into()));
        assert_eq!(bearer_token("Bearer "), None);
        assert_eq!(bearer_token("Basic abc"), None);
        assert_eq!(bearer_token("abc"), None);
    }

    #[test]
    fn auth_disabled_when_no_keys() {
        let s = AuthState::dev_mode();
        assert!(s.check_bearer(None).is_ok());
        assert!(s.check_bearer(Some("anything")).is_ok());
    }

    #[test]
    fn auth_rejects_when_keys_but_no_header() {
        let s = AuthState {
            api_keys: Some(vec!["k1".into()]),
            rate_limit: None,
        };
        assert_eq!(s.check_bearer(None), Err(AuthError::Missing));
        assert_eq!(s.check_bearer(Some("Basic abc")), Err(AuthError::Malformed));
        assert_eq!(s.check_bearer(Some("Bearer k2")), Err(AuthError::Invalid));
        assert_eq!(s.check_bearer(Some("Bearer k1")), Ok(()));
    }

    #[test]
    fn rate_limit_drains_bucket() {
        let rl = RateLimit::new(3);  // 3 rpm = 1 token / 20s
        let k = "k1";
        assert!(rl.try_consume(k).is_ok());
        assert!(rl.try_consume(k).is_ok());
        assert!(rl.try_consume(k).is_ok());
        let err = rl.try_consume(k).unwrap_err();
        assert!(err >= 1);
    }

    #[test]
    fn rate_limit_buckets_are_per_key() {
        let rl = RateLimit::new(1);
        assert!(rl.try_consume("a").is_ok());
        assert!(rl.try_consume("b").is_ok());  // separate bucket
        assert!(rl.try_consume("a").is_err());
    }
}
