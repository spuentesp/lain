//! Integration tests for the filesystem-as-lock layer
//! (`lain::server::presence_lock`).
//!
//! Exercises [`try_lock`] / [`release_lock`] / [`refresh_lock`] against
//! a real tempdir; no lain-server involvement. The internal OccupancyMap
//! integration is covered by the existing `tests/presence.rs` suite —
//! adding `claim_with_session` there would just duplicate what the in-
//! memory layer already tests.

use lain::server::presence::{AgentId, AgentKind, ClaimIntent};
use lain::server::presence_lock::{release_lock, try_lock, ReleaseError};

fn make_agent(id: &str) -> AgentId {
    AgentId(format!("{id}-{}", std::process::id()).into())
}

/// Serializes the hooks-CLI tests so they don't race on the process-wide
/// `XDG_CONFIG_HOME` (and so the per-agent session file path stays
/// consistent between `claim` and `release`). Without this, parallel
/// `cargo test` runs let one test mutate `XDG_CONFIG_HOME` mid-way
/// through another, which breaks the
/// `zero_daemon_claim_and_release_work_without_a_server` happy path
/// ("no recorded nonce" from `release_filesystem` leaves the sentinel
/// in place, and the next agent's `claim` collides with it).
static HOOKS_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_hooks_xdg<F: FnOnce()>(xdg: &std::path::Path, body: F) {
    let prev = std::env::var("XDG_CONFIG_HOME").ok();
    std::env::set_var("XDG_CONFIG_HOME", xdg.to_string_lossy().to_string());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
    match prev {
        Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
        None => std::env::remove_var("XDG_CONFIG_HOME"),
    }
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

/// Acquire a fresh lock and confirm both that the helper returned
/// successfully *and* that the sentinel file landed on disk under
/// `<workspace>/.lain/locks/`. Releasing the lock must remove the file
/// (idempotently).
#[test]
fn try_lock_acquires_release_releases() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path();
    let path = ws.join("foo.rs");
    let agent = make_agent("alice");
    let lock = try_lock(ws, &path, &agent, AgentKind::ClaudeCode, ClaimIntent::Edit)
        .expect("lock");
    assert!(lock.path.exists());
    release_lock(&lock).unwrap();
    assert!(!lock.path.exists());
}

/// A second `try_lock` for the same path while the first is fresh
/// returns `LockConflict`, and the reported holder matches alice's
/// agent id. The in-memory layer is unaffected (this test uses no
/// `OccupancyMap`).
#[test]
fn try_lock_returns_conflict_on_duplicate() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path();
    let path = ws.join("foo.rs");
    let alice = make_agent("alice");
    let bob = make_agent("bob");
    let first = try_lock(ws, &path, &alice, AgentKind::ClaudeCode, ClaimIntent::Edit)
        .expect("first");
    let second = try_lock(ws, &path, &bob, AgentKind::ClaudeCode, ClaimIntent::Edit);
    assert!(second.is_err());
    let conflict = second.unwrap_err();
    assert_eq!(conflict.agent_id(), alice);
    release_lock(&first).unwrap();
}

/// When the existing lock is older than the TTL window (here simulated
/// by `set_file_mtime` to UNIX_EPOCH + 1s), a competing agent *can*
/// take it. The mtime check + stale-removal path is the only thing
/// that makes this layer safe against dead writers; this test is the
/// contract that says "stale means stealable."
#[test]
fn stale_lock_can_be_taken_after_mtime_window() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path();
    let path = ws.join("foo.rs");
    let alice = make_agent("alice");
    let bob = make_agent("bob");
    let first = try_lock(ws, &path, &alice, AgentKind::ClaudeCode, ClaimIntent::Edit)
        .expect("first");
    // Backdate the lock file's mtime to simulate a dead writer. Uses
    // `File::set_modified` (stable) rather than the nightly-only
    // `std::fs::set_file_mtime`.
    let past = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
    {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&first.path)
            .unwrap();
        f.set_modified(past).unwrap();
    }
    let second = try_lock(ws, &path, &bob, AgentKind::Kimi, ClaimIntent::Read)
        .expect("stale lock taken");
    release_lock(&second).unwrap();
}

/// `refresh_lock` must bump the sentinel file's mtime within the TTL
/// window so a competing `try_lock` doesn't think the holder is dead.
/// Read-back via `metadata().modified()` proves the kernel honored
/// `set_file_mtime` (cheap guard against filesystems that don't
/// preserve mtime).
#[test]
fn refresh_lock_keeps_lock_alive() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path();
    let path = ws.join("foo.rs");
    let agent = make_agent("alice");
    let lock = try_lock(ws, &path, &agent, AgentKind::ClaudeCode, ClaimIntent::Edit)
        .expect("lock");
    let mtime_before = std::fs::metadata(&lock.path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    lock.refresh_lock().unwrap();
    let mtime_after = std::fs::metadata(&lock.path).unwrap().modified().unwrap();
    assert!(mtime_after > mtime_before);
    release_lock(&lock).unwrap();
}

/// End-to-end exercise of the zero-daemon `claim`/`release` flow.
///
/// `lain::cli::hooks::claim` and `::release` probe the server at
/// `--url` first; when nothing's listening they fall through to the
/// filesystem lock layer. This test stands up no server and verifies
/// the two functions still grant, conflict, and release correctly —
/// the wishlist's #3 and #4 ("zero-daemon path" / "stateless claims").
#[test]
fn zero_daemon_claim_and_release_work_without_a_server() {
    use lain::cli::hooks::{claim, release};

    let _guard = HOOKS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let xdg = tempfile::tempdir().unwrap();
    with_hooks_xdg(xdg.path(), || {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path();
    let file_path = ws.join("foo.rs");
    std::fs::write(&file_path, "fn x() {}").unwrap();

    // Port 1 is reserved by IANA — nothing should be listening here.
    let dead_url = "http://127.0.0.1:1";

    // First agent claims — should succeed via the filesystem fallback.
    claim(
        dead_url,
        &[file_path.to_string_lossy().to_string()],
        "",
        "edit",
        "agent-a",
        "claude-code",
        "",
    )
    .expect("zero-daemon claim must succeed when no server is running");

    // Second agent claims the same path — must observe a conflict.
    let conflict = claim(
        dead_url,
        &[file_path.to_string_lossy().to_string()],
        "",
        "edit",
        "agent-b",
        "kimi",
        "",
    );
    assert!(
        conflict.is_err(),
        "second agent must see a filesystem conflict, got Ok"
    );

    // Release — idempotent. First call removes the sentinel; second is
    // a no-op (ENOENT-as-success). Both must succeed so a hook that
    // fires twice doesn't break the agent.
    release(
        dead_url,
        file_path.to_str().unwrap(),
        "",
        "agent-a",
        "claude-code",
        "",
    )
    .expect("first release must succeed");
    release(
        dead_url,
        file_path.to_str().unwrap(),
        "",
        "agent-a",
        "claude-code",
        "",
    )
    .expect("second release must be idempotent");

    // After release, the second agent can claim cleanly.
    claim(
        dead_url,
        &[file_path.to_string_lossy().to_string()],
        "",
        "edit",
        "agent-b",
        "kimi",
        "",
    )
    .expect("agent-b must be able to claim after agent-a released");
    })
}

/// Codex contract `expired_holder_cannot_release_replacement_holder`.
///
/// Alice acquires a lock; her TTL elapses; Bob re-acquires the same
/// path; Alice's stale `FileLock` must NOT be able to remove Bob's
/// sentinel. The pre-fix `release_lock` blindly `remove_file`'d the
/// path, so a long-lived process that held a `FileLock` across its
/// own TTL could stomp on whoever took the claim after it. The
/// nonce-in-filename design closes that hole structurally: Alice's
/// release only touches Alice's specific nonce-bearing file. Bob's
/// lock lives at a different filename, so Alice's stale release
/// cannot clobber it regardless of whether her own file still
/// exists.
#[test]
fn expired_holder_cannot_release_replacement_holder() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path();
    let path = ws.join("foo.rs");
    let alice = make_agent("alice");
    let bob = make_agent("bob");

    let alice_lock = try_lock(ws, &path, &alice, AgentKind::ClaudeCode, ClaimIntent::Edit)
        .expect("alice acquires");
    assert!(alice_lock.path.exists());

    // Backdate the sentinel so Bob's `try_lock` sees it as stale and
    // takes over. The new nonce-in-filename design means Bob's stale
    // takeover also removes Alice's specific nonce-bearing file.
    let past = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
    {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&alice_lock.path)
            .unwrap();
        f.set_modified(past).unwrap();
    }
    let bob_lock = try_lock(ws, &path, &bob, AgentKind::Kimi, ClaimIntent::Edit)
        .expect("bob takes over a stale lock");
    assert_ne!(
        alice_lock.nonce, bob_lock.nonce,
        "every acquire mints a fresh nonce; the two holders' nonces must differ"
    );
    assert_ne!(
        alice_lock.path, bob_lock.path,
        "every acquire writes a distinct nonce-bearing filename"
    );

    // Alice's stale `FileLock` cannot release Bob's sentinel because
    // Alice's release only touches Alice's specific nonce-bearing
    // path — Bob's file lives at a different filename. After Bob's
    // stale-takeover removed Alice's file, Alice's release correctly
    // returns NotOwner (her sentinel is gone, so she no longer owns
    // the lock).
    let alice_release = release_lock(&alice_lock);
    match alice_release {
        Err(ReleaseError::NotOwner { .. }) => {}
        other => panic!(
            "expected NotOwner for alice's stale release (her sentinel was removed by bob's stale-takeover), got {other:?}"
        ),
    }

    // Bob's lock must still exist on disk — alice's release cannot
    // have clobbered it because it lives at a different path.
    assert!(
        bob_lock.path.exists(),
        "bob's sentinel must survive alice's stale release attempt"
    );

    // Bob's release with his own nonce succeeds.
    release_lock(&bob_lock).expect("bob's release succeeds");
    assert!(!bob_lock.path.exists());
}

/// Codex contract `distinct_paths_have_distinct_claim_files`.
///
/// The pre-fix `sanitize` collapsed `src/a.b` and `src/a_b` onto the
/// same lock filename (`src__a_b`), so two semantically different
/// paths shared one sentinel. The new scheme percent-encodes every
/// non-`[A-Za-z0-9_-]` byte, which preserves distinctness by
/// construction: distinct byte sequences → distinct encoded
/// sequences. The two lock files live independently, and each
/// release only removes its own sentinel.
#[test]
fn distinct_paths_have_distinct_claim_files() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path();
    // Use workspace-local paths so the workspace walk in
    // `lock_path_for` lands inside the tempdir; the parent dir is
    // what `sanitize` encodes, so pick strings that exercise the
    // exact collision from the brief.
    let dotted = ws.join("a.b");
    let under = ws.join("a_b");
    let agent = make_agent("alice");

    let lock_dotted =
        try_lock(ws, &dotted, &agent, AgentKind::ClaudeCode, ClaimIntent::Edit)
            .expect("dotted path claim");
    let lock_under =
        try_lock(ws, &under, &agent, AgentKind::ClaudeCode, ClaimIntent::Edit)
            .expect("underscore path claim");

    assert_ne!(
        lock_dotted.path, lock_under.path,
        "{} and {} must map to distinct sentinel files; got {:?} and {:?}",
        dotted.display(),
        under.display(),
        lock_dotted.path,
        lock_under.path
    );
    assert!(lock_dotted.path.exists());
    assert!(lock_under.path.exists());

    // Releases are independent: removing one sentinel must not touch
    // the other. The pre-fix code shared one file, so the first
    // release would have taken the second claim with it.
    release_lock(&lock_dotted).expect("dotted release");
    assert!(!lock_dotted.path.exists());
    assert!(
        lock_under.path.exists(),
        "underscore sentinel must survive the dotted release"
    );
    release_lock(&lock_under).expect("underscore release");
    assert!(!lock_under.path.exists());
}

/// Codex H3: the old `release_lock` checked the nonce with a separate
/// `read_to_string` and then called `std::fs::remove_file`, leaving a
/// window in which a replacement holder could `try_lock` over the
/// stale sentinel. The old owner's release would then `remove_file`
/// the replacement's brand-new lock. The atomic compare-and-delete
/// renames the sentinel into a private tempfile BEFORE the nonce
/// check, so the original lock path is not visible to other agents
/// during the check. Loop the scenario to make the test sensitive to
/// any regression that re-opens the race window.
#[test]
fn stale_release_preserves_replacement_holder_under_repeat() {
    for iter in 0..64 {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        let path = ws.join("foo.rs");
        let alice = make_agent(&format!("alice-{iter}"));
        let bob = make_agent(&format!("bob-{iter}"));

        let alice_lock =
            try_lock(ws, &path, &alice, AgentKind::ClaudeCode, ClaimIntent::Edit)
                .expect("alice acquires");
        let past = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
        {
            let f = std::fs::OpenOptions::new()
                .write(true)
                .open(&alice_lock.path)
                .unwrap();
            f.set_modified(past).unwrap();
        }
        let bob_lock = try_lock(ws, &path, &bob, AgentKind::Kimi, ClaimIntent::Edit)
            .expect("bob takes over a stale lock");
        assert_ne!(
            alice_lock.nonce, bob_lock.nonce,
            "iter {iter}: every acquire mints a fresh nonce"
        );
        match release_lock(&alice_lock) {
            Err(ReleaseError::NotOwner { .. }) => {}
            other => panic!(
                "iter {iter}: expected ReleaseError::NotOwner for alice's stale release, got {other:?}"
            ),
        }
        assert!(
            bob_lock.path.exists(),
            "iter {iter}: bob's sentinel must survive alice's stale release"
        );

        release_lock(&bob_lock).expect("bob's release succeeds");
        assert!(!bob_lock.path.exists(), "iter {iter}: bob's release clears the sentinel");
    }
}

/// Codex H3, concurrent form: thread A releases while thread B is
/// trying to acquire / re-acquire. The atomic compare-and-delete
/// closes the race window by construction (the lock file is not visible
/// at its original path during the check), so across many iterations
/// B's lock file — once it lands — must never be deleted by A's
/// release.
#[test]
fn stale_release_does_not_clobber_concurrent_acquire() {
    use std::sync::{Arc, Barrier};

    for iter in 0..32 {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        let path = ws.join("foo.rs");
        let alice = make_agent(&format!("alice-{iter}"));
        let bob = make_agent(&format!("bob-{iter}"));

        let alice_lock =
            try_lock(ws, &path, &alice, AgentKind::ClaudeCode, ClaimIntent::Edit)
                .expect("alice acquires");
        let past = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
        {
            let f = std::fs::OpenOptions::new()
                .write(true)
                .open(&alice_lock.path)
                .unwrap();
            f.set_modified(past).unwrap();
        }

        let barrier = Arc::new(Barrier::new(2));
        let ws_b = ws.to_path_buf();
        let path_b = path.clone();
        let alice_handle = {
            let alice_lock = alice_lock.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                let _ = release_lock(&alice_lock);
            })
        };
        let bob_handle = {
            let bob = bob.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                try_lock(
                    &ws_b,
                    &path_b,
                    &bob,
                    AgentKind::Kimi,
                    ClaimIntent::Edit,
                )
            })
        };
        let _ = alice_handle.join();
        let bob_result = bob_handle.join().expect("bob panics");

        // If bob acquired, his lock must still exist on disk — alice's
        // release did not get a chance to clobber it mid-flight. If
        // alice's release landed first (the file at the lock path was
        // already gone), bob's `try_lock` returns a conflict and there
        // is nothing on disk to assert against.
        if let Ok(bob_lock) = bob_result {
            assert!(
                bob_lock.path.exists(),
                "iter {iter}: bob's lock must survive a concurrent stale release"
            );
            release_lock(&bob_lock).expect("bob releases");
        }
    }
}

/// Codex M1: the previous hook release flow removed and persisted the
/// nonce from the session file BEFORE attempting `release_lock_for_path`.
/// An I/O failure therefore left no credential for a safe retry. The
/// fix reads the nonce first, attempts release, and only removes the
/// nonce on success. Verifies that an I/O failure preserves the nonce
/// so a subsequent successful retry can authenticate, and that a
/// successful first call still removes the nonce (no dangling
/// credentials left behind on the happy path).
#[test]
fn hook_release_preserves_nonce_on_failure_for_retry() {
    use lain::cli::hooks::{lock, unlock};

    let _guard = HOOKS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let xdg = tempfile::tempdir().unwrap();
    with_hooks_xdg(xdg.path(), || {
    let ws = tempfile::tempdir().unwrap();
    let ws_str = ws.path().to_string_lossy().to_string();
    let file_path = ws.path().join("foo.rs");
    std::fs::write(&file_path, "fn x() {}").unwrap();
    let path_str = file_path.to_string_lossy().to_string();
    let agent_name = "retry-agent";

    // Acquire: writes a lock sentinel and records the nonce in the
    // hooks session file under `XDG_CONFIG_HOME/lain/hooks/<agent>.session`.
    lock(&ws_str, &path_str, agent_name, "claude-code", "edit")
        .expect("lock must succeed");
    let lock_path = lain::server::presence_lock::lock_path_for(ws.path(), &file_path);
    let session_path =
        lain::config::hooks_dir().join(format!("{agent_name}.session"));

    let nonce_before = read_nonce_from_session(&session_path, &lock_path);
    assert!(
        nonce_before.is_some(),
        "lock must record the nonce in the hooks session"
    );

    // The actual sentinel on disk lives at <canonical>.lock-<nonce_hex>,
    // not at the canonical path used as the session-file key.
    let nonce_value = nonce_before.as_ref().expect("nonce present");
    let actual_lock_path = lain::server::presence_lock::lock_path_for_with_nonce(
        ws.path(),
        &file_path,
        &nonce_value,
    );

    // Force a release I/O failure by replacing the lock sentinel with
    // a directory of the same name. `rename(L, T)` fails with `IsADirectory`
    // because POSIX rename rejects source-dir → dest-file when the
    // destination is on a non-empty filesystem, and the new
    // compare-and-delete code surfaces that as `ReleaseError::Io`.
    std::fs::remove_file(&actual_lock_path).unwrap();
    std::fs::create_dir(&actual_lock_path).unwrap();

    let first_attempt = unlock(&ws_str, &path_str, agent_name);
    assert!(
        first_attempt.is_err(),
        "unlock must fail when the lock path is a directory, got {first_attempt:?}"
    );

    let nonce_after_fail = read_nonce_from_session(&session_path, &lock_path);
    assert_eq!(
        nonce_after_fail, nonce_before,
        "nonce must survive a failed release so the caller can retry"
    );

    // Restore the lock sentinel as a regular file carrying the same
    // nonce. Now the second `unlock` call can authenticate and finish
    // the release.
    std::fs::remove_dir(&actual_lock_path).unwrap();
    let body = serde_json::json!({
        "agent_id": "retry-agent",
        "kind": "claude-code",
        "intent": "edit",
        "nonce": nonce_value,
        "claimed_at": 0,
    });
    std::fs::write(&actual_lock_path, serde_json::to_string(&body).unwrap()).unwrap();

    let second_attempt = unlock(&ws_str, &path_str, agent_name);
    assert!(
        second_attempt.is_ok(),
        "retry must succeed once the lock is recoverable, got {second_attempt:?}"
    );
    assert!(
        !actual_lock_path.exists(),
        "lock sentinel must be gone after a successful retry"
    );

    let nonce_after_success = read_nonce_from_session(&session_path, &lock_path);
    assert!(
        nonce_after_success.is_none(),
        "nonce must be cleared after a successful release (no dangling credential)"
    );
    })
}

/// Read the recorded nonce for `lock_path` out of the serialized
/// hooks session JSON. Avoids depending on the internal
/// `HookSession` shape from this integration test — the wire format
/// is the contract the on-disk file must satisfy.
fn read_nonce_from_session(
    session_path: &std::path::Path,
    lock_path: &std::path::Path,
) -> Option<String> {
    let raw = std::fs::read_to_string(session_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("lock_nonces")?
        .get(lock_path.to_string_lossy().as_ref())?
        .as_str()
        .map(|s| s.to_string())
}
