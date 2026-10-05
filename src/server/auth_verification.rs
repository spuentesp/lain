//! Token-bucket properties, with an injected clock.
//!
//! * rate bound   — over any run, allowed requests per key never exceed
//!                  `capacity + rpm * elapsed / 60` (+1 for the in-flight token).
//! * hint honesty — after `Err(retry_after)`, waiting that long makes the
//!                  next request succeed.
//! * boundedness  — the bucket map never exceeds `RATE_LIMIT_MAX_BUCKETS`.
use super::*;
use proptest::prelude::*;
use std::time::Duration;

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]

    #[test]
    fn allowed_requests_respect_the_rate_bound(
        rpm in 1u32..240,
        steps in prop::collection::vec((0usize..3, 0u64..4000), 1..120), // (key, advance_ms)
    ) {
        let rl = RateLimit::new(rpm);
        let t0 = Instant::now();
        let mut now = t0;
        let mut allowed = [0u64; 3];
        for (k, advance_ms) in steps {
            now += Duration::from_millis(advance_ms);
            if rl.try_consume_at(&format!("k{k}"), now).is_ok() {
                allowed[k] += 1;
            }
        }
        let elapsed = now.duration_since(t0).as_secs_f64();
        let bound = rpm as f64 + rpm as f64 * elapsed / 60.0 + 1.0;
        for a in allowed {
            prop_assert!(a as f64 <= bound, "allowed {a} > bound {bound} (rpm {rpm}, {elapsed}s)");
        }
    }

    #[test]
    fn retry_after_is_honest_and_at_least_one_second(
        rpm in 1u32..240,
        drain in 0usize..400,
    ) {
        let rl = RateLimit::new(rpm);
        let mut now = Instant::now();
        let mut hint = None;
        for _ in 0..drain.max(rpm as usize + 1) {
            if let Err(s) = rl.try_consume_at("k", now) {
                hint = Some(s);
                break;
            }
        }
        let s = hint.expect("a bucket must run dry within capacity+1 requests");
        prop_assert!(s >= 1);
        now += Duration::from_secs(s);
        prop_assert!(rl.try_consume_at("k", now).is_ok(), "waited {s}s as told, still denied");
    }
}

#[test]
fn bucket_map_never_exceeds_its_cap_and_keeps_recent_keys() {
    let rl = RateLimit::new(1);
    let t0 = Instant::now();
    for i in 0..(RATE_LIMIT_MAX_BUCKETS * 2) {
        let _ = rl.try_consume_at(&format!("key-{i}"), t0 + Duration::from_millis(i as u64));
        assert!(rl.inner.lock().len() <= RATE_LIMIT_MAX_BUCKETS);
    }
    let guard = rl.inner.lock();
    let newest = format!("key-{}", RATE_LIMIT_MAX_BUCKETS * 2 - 1);
    assert!(
        guard.contains_key(&newest),
        "the newest key must survive eviction"
    );
    assert!(
        !guard.contains_key("key-0"),
        "the oldest key is the one evicted"
    );
}

/// `Retry-After` is tight, not just sufficient: with a partly refilled bucket the
/// hint is the time to reach one token, and waiting a second less is not enough.
/// (Sign-flipping `1.0 - tokens` into `1.0 + tokens` only ever over-waits, so a
/// "waited as told, was allowed" check cannot see it.)
#[test]
fn retry_after_is_the_time_to_one_token_with_a_partial_bucket() {
    let rl = RateLimit::new(6); // refill 0.1 token/s, capacity 6
    let t0 = Instant::now();
    for _ in 0..6 {
        rl.try_consume_at("k", t0).unwrap();
    }
    // 5 s later: 0.5 token. Needs 0.5 more = 5 s.
    let t = t0 + Duration::from_secs(5);
    assert_eq!(rl.try_consume_at("k", t), Err(5));
    // Waiting 4 more seconds is not enough; 5 is.
    assert!(rl.try_consume_at("k", t + Duration::from_secs(4)).is_err());
    assert!(rl.try_consume_at("k", t + Duration::from_secs(5)).is_ok());
}
