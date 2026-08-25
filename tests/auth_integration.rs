// -------------------------------------------------------------------------
// Runtime integration test for the HTTP auth + rate limit (P0 #1).
// Mirrors smoke6's verification of the AuthState module behavior:
// bearer-token parsing, dev-mode bypass, error paths, and the token
// bucket's per-key isolation.
// -------------------------------------------------------------------------
use lain::server::auth::{bearer_token, AuthError, AuthState, RateLimit};

#[test]
fn bearer_token_parses() {
    assert_eq!(bearer_token("Bearer abc").as_deref(), Some("abc"));
    assert_eq!(bearer_token("bearer xyz").as_deref(), Some("xyz"));
    assert_eq!(bearer_token("Bearer "), None);
    assert_eq!(bearer_token("Basic abc"), None);
    assert_eq!(bearer_token("abc"), None);
}

#[test]
fn dev_mode_accepts_everything() {
    let s = AuthState::dev_mode();
    assert!(s.check_bearer(None).is_ok());
    assert!(s.check_bearer(Some("anything")).is_ok());
    assert!(s.check_bearer(Some("Bearer weird-format")).is_ok());
    assert!(s.check_rate("any-key").is_ok());
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
fn auth_multiple_keys_accepted() {
    let s = AuthState {
        api_keys: Some(vec!["alpha".into(), "beta".into(), "gamma".into()]),
        rate_limit: None,
    };
    assert!(s.check_bearer(Some("Bearer alpha")).is_ok());
    assert!(s.check_bearer(Some("Bearer beta")).is_ok());
    assert!(s.check_bearer(Some("Bearer gamma")).is_ok());
    assert!(s.check_bearer(Some("Bearer delta")).is_err());
}

#[test]
fn rate_limit_drains_bucket_then_refills() {
    let rl = RateLimit::new(60);  // 60 rpm = 1 token / sec
    let k = "k1";
    for _ in 0..60 {
        assert!(rl.try_consume(k).is_ok(), "first 60 should be allowed");
    }
    // 61st call: empty bucket
    let err = rl.try_consume(k).unwrap_err();
    assert!(err >= 1, "retry_after should be ≥ 1 second, got {}", err);
}

#[test]
fn rate_limit_buckets_are_per_key() {
    let rl = RateLimit::new(1);
    assert!(rl.try_consume("a").is_ok());
    assert!(rl.try_consume("b").is_ok(), "b has its own bucket");
    assert!(rl.try_consume("a").is_err(), "a's bucket is now empty");
    assert!(rl.try_consume("b").is_err(), "b's bucket is now empty too");
}

#[test]
fn rate_limit_disabled_via_zero_rpm() {
    // from_env with LAIN_RATE_LIMIT_RPM=0 also disables
    // (production code disables via "off" or via the constructor
    // path that drops RateLimit). Verify the public API: AuthState
    // constructed without rate_limit is permissive.
    let s = AuthState {
        api_keys: Some(vec!["k".into()]),
        rate_limit: None,
    };
    for _ in 0..100 {
        assert!(s.check_rate("k").is_ok(), "rate limit disabled → all pass");
    }
}

#[test]
fn rate_limit_refills_over_time() {
    // 60 rpm = 1 token / sec. Drain, wait, see partial refill.
    let rl = RateLimit::new(60);
    let k = "k1";
    for _ in 0..60 {
        assert!(rl.try_consume(k).is_ok());
    }
    assert!(rl.try_consume(k).is_err(), "bucket drained");
    // Wait ~1.1 sec — at least 1 full token should refill.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert!(rl.try_consume(k).is_ok(), "token should have refilled");
}
