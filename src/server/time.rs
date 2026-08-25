use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Seconds since the epoch, as `i64`. Pre-epoch collapses to 0
/// rather than underflowing (saturating `duration_since`).
pub fn unix_secs(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Seconds since the epoch, as `u64`. Pre-epoch collapses to 0.
pub fn unix_secs_u64(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Seconds since the epoch with millisecond precision, as `f64`.
/// Matches `AuditEvent::ts_unix: f64` so persistence and reads stay
/// in the same unit (loaders that parsed sub-second audit events
/// would break if this collapsed to integer seconds).
pub fn now_unix_f64() -> f64 {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    dur.as_secs() as f64 + dur.subsec_millis() as f64 / 1_000.0
}

/// Convenience wrapper for the common "now" case.
pub fn now_unix() -> i64 { unix_secs(SystemTime::now()) }

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn unix_secs_returns_seconds_since_epoch() {
        let t = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(unix_secs(t), 1_700_000_000);
    }

    #[test]
    fn unix_secs_collapses_pre_epoch_to_zero() {
        let t = UNIX_EPOCH - Duration::from_secs(10);
        assert_eq!(unix_secs(t), 0);
    }

    #[test]
    fn unix_secs_u64_returns_seconds() {
        let t = UNIX_EPOCH + Duration::from_secs(42);
        assert_eq!(unix_secs_u64(t), 42);
    }

    #[test]
    fn now_unix_is_close_to_wall_clock() {
        let before = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        let n = now_unix();
        let after = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        assert!(n >= before && n <= after, "n={n}, before={before}, after={after}");
    }
}
