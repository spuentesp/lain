//! Respawn-budget accounting for the git sidecar
//! (`docs/formal/` has no spec for this; the property is stated here).
//!
//! The budget exists to stop a crash-looping sidecar from being respawned
//! without limit. It must therefore count *attempts*, not just successes: a
//! binary that dies on startup never reaches the success path, so counting
//! only successful respawns left it unbounded — every git call paid two
//! spawn-and-timeout cycles forever.
#![cfg(unix)]
use super::*;

fn broken_inner(dir: &Path) -> SidecarInner {
    SidecarInner {
        workspace: dir.to_path_buf(),
        child: None,
        stream: None,
        socket_path: generate_socket_path(),
        respawn_history: VecDeque::new(),
        consecutive_failures: 0,
        last_call_duration: Duration::ZERO,
        call_timeout: Duration::from_secs(1),
        // Exits immediately: the spawn "succeeds", the handshake never can.
        custom_bin_path: Some(PathBuf::from("/bin/false")),
        is_initial_boot: false,
    }
}

#[test]
fn a_binary_that_dies_on_startup_exhausts_the_respawn_budget() {
    if !Path::new("/bin/false").exists() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let mut inner = broken_inner(tmp.path());

    let mut budget_hit_after = None;
    for attempt in 1..=(MAX_RESPAWNS_PER_WINDOW + 3) {
        let err = inner
            .ensure_connected()
            .expect_err("/bin/false can never connect");
        if err.to_string().contains("respawn budget exceeded") {
            budget_hit_after = Some(attempt);
            break;
        }
    }
    let n = budget_hit_after.expect("failed spawns never counted against the respawn budget");
    assert!(
        n <= MAX_RESPAWNS_PER_WINDOW + 1,
        "budget tripped only after {n} attempts (limit {MAX_RESPAWNS_PER_WINDOW})"
    );

    // Once exhausted, further calls must fail fast rather than spawn again.
    let started = Instant::now();
    for _ in 0..20 {
        assert!(inner.ensure_connected().is_err());
    }
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "calls past the budget are still spawning children ({:?})",
        started.elapsed()
    );
}
