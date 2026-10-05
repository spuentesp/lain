//! Properties the `presence_lock` module documents, checked over generated
//! paths: sanitisation is injective and prefix-free (so `starts_with` scans
//! cannot confuse two paths), and every acquirable path yields a legal
//! single-component file name.
use super::*;
use proptest::prelude::*;

fn lock_file_name_len(p: &Path) -> usize {
    lock_path_for_with_nonce(Path::new("/w"), p, &uuid::Uuid::new_v4().to_string())
        .file_name()
        .unwrap()
        .len()
}

proptest! {
    /// Distinct (valid UTF-8) paths never share a sanitised name.
    #[test]
    fn sanitize_is_injective(a in "[ -~]{0,40}", b in "[ -~]{0,40}") {
        prop_assume!(a != b);
        prop_assert_ne!(sanitize(Path::new(&a)), sanitize(Path::new(&b)));
    }

    /// Same property in the hashed regime: long paths sharing a long prefix.
    #[test]
    fn sanitize_is_injective_for_long_paths(
        head in "[a-z/._-]{150,300}", t1 in "[a-z/._-]{1,20}", t2 in "[a-z/._-]{1,20}"
    ) {
        prop_assume!(t1 != t2);
        let (a, b) = (format!("{head}{t1}"), format!("{head}{t2}"));
        prop_assert_ne!(sanitize(Path::new(&a)), sanitize(Path::new(&b)));
    }

    /// The sanitised body never contains `.` or a path separator, so the
    /// `<san>.lock-` prefix of one path can never be a prefix of another's.
    #[test]
    fn sanitize_is_prefix_free(a in "\\PC{0,30}", b in "\\PC{0,30}") {
        prop_assume!(a != b);
        let (sa, sb) = (sanitize(Path::new(&a)), sanitize(Path::new(&b)));
        prop_assert!(!sa.contains(['.', '/', '\\']));
        let prefix_a = lock_filename_prefix(Path::new(&a));
        let file_b = format!("{sb}.lock-nonce");
        prop_assert!(!file_b.starts_with(&prefix_a));
    }

    /// Any path an agent can claim must produce a lock file name the
    /// filesystem accepts (NAME_MAX = 255 on ext4/APFS/NTFS).
    #[test]
    fn lock_file_name_fits_name_max(p in "[a-z/._-]{1,300}") {
        prop_assert!(lock_file_name_len(Path::new(&p)) <= 255,
            "lock file name for a {}-byte path is {} bytes", p.len(), lock_file_name_len(Path::new(&p)));
    }
}

/// End to end: a long, ordinary monorepo path must be lockable.
#[test]
fn long_path_can_be_locked() {
    let ws = tempfile::tempdir().unwrap();
    let long = "services/payments/internal/adapters/persistence/postgres/migrations/\
                20240101_create_ledger_entries_with_idempotency_keys_and_partitioning/\
                generated/ledger_entry_repository_impl.rs";
    let r = try_lock(
        ws.path(),
        Path::new(long),
        &AgentId("a".into()),
        AgentKind::Other("t".into()),
        ClaimIntent::Edit,
    );
    assert!(
        r.is_ok(),
        "uncontended lock on a {}-byte path failed: {:?}",
        long.len(),
        r.err()
    );
}

/// N threads race to lock one path, round after round. Exactly one may win
/// each round (`docs/formal/FsLeaseGuard.tla`, `MutualExclusion`).
#[test]
fn concurrent_acquirers_never_both_win() {
    use std::sync::{Arc, Barrier};
    const THREADS: usize = 8;
    for round in 0..60 {
        let ws = Arc::new(tempfile::tempdir().unwrap());
        let barrier = Arc::new(Barrier::new(THREADS));
        let handles: Vec<_> = (0..THREADS)
            .map(|i| {
                let (ws, barrier) = (Arc::clone(&ws), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    barrier.wait();
                    try_lock(
                        ws.path(),
                        Path::new("src/contended.rs"),
                        &AgentId(format!("agent-{i}")),
                        AgentKind::Other("t".into()),
                        ClaimIntent::Edit,
                    )
                    .is_ok()
                })
            })
            .collect();
        let winners = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|w| *w)
            .count();
        assert_eq!(
            winners, 1,
            "round {round}: {winners} acquirers hold the same lock"
        );
    }
}

fn guard_files(ws: &Path) -> Vec<String> {
    std::fs::read_dir(ws.join(".lain/locks"))
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".guard") || n.contains(".guard-stale"))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn acquire_leaves_no_guard_file_behind() {
    let ws = tempfile::tempdir().unwrap();
    let r = try_lock(
        ws.path(),
        Path::new("a.rs"),
        &AgentId("a".into()),
        AgentKind::Other("t".into()),
        ClaimIntent::Edit,
    );
    assert!(r.is_ok());
    assert!(
        guard_files(ws.path()).is_empty(),
        "{:?}",
        guard_files(ws.path())
    );
    // A losing acquirer must clean up too.
    let r2 = try_lock(
        ws.path(),
        Path::new("a.rs"),
        &AgentId("b".into()),
        AgentKind::Other("t".into()),
        ClaimIntent::Edit,
    );
    assert!(r2.is_err());
    assert!(guard_files(ws.path()).is_empty());
}

/// A guard left behind by a crashed acquirer must not wedge the path forever.
#[test]
fn stale_guard_from_a_crashed_acquirer_is_taken_over() {
    let ws = tempfile::tempdir().unwrap();
    let dir = ws.path().join(".lain/locks");
    std::fs::create_dir_all(&dir).unwrap();
    let guard = dir.join(format!("{}.guard", sanitize(Path::new("a.rs"))));
    let f = std::fs::File::create(&guard).unwrap();
    f.set_modified(SystemTime::now() - GUARD_TTL - Duration::from_secs(1))
        .unwrap();
    drop(f);

    let r = try_lock(
        ws.path(),
        Path::new("a.rs"),
        &AgentId("a".into()),
        AgentKind::Other("t".into()),
        ClaimIntent::Edit,
    );
    assert!(r.is_ok(), "stale guard wedged the lock: {:?}", r.err());
    assert!(guard_files(ws.path()).is_empty());
}

// ---- found by mutation testing -------------------------------------------------

fn lock_dir(ws: &Path) -> PathBuf {
    let d = ws.join(".lain").join("locks");
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn sanitize_hashes_only_past_the_limit() {
    // Exactly MAX_SANITIZED_LEN encoded bytes: still the plain, readable form.
    let at_limit = "a".repeat(MAX_SANITIZED_LEN);
    assert_eq!(sanitize(Path::new(&at_limit)), at_limit);
    // One byte more: hashed and bounded.
    let over = "a".repeat(MAX_SANITIZED_LEN + 1);
    let s = sanitize(Path::new(&over));
    assert!(s.contains('~'), "over-limit names must be hashed: {s}");
    assert!(s.len() <= MAX_SANITIZED_LEN);
}

#[test]
fn lock_paths_are_exactly_where_the_docs_say() {
    let ws = Path::new("/w");
    assert_eq!(
        lock_path_for(ws, Path::new("src/a.rs")),
        PathBuf::from("/w/.lain/locks/src%2Fa%2Ers.lock")
    );
    assert_eq!(
        lock_path_for_with_nonce(ws, Path::new("src/a.rs"), "N"),
        PathBuf::from("/w/.lain/locks/src%2Fa%2Ers.lock-N")
    );
}

#[test]
fn canonical_lock_key_agrees_for_every_spelling_of_one_file() {
    // Platform-absolute paths: `/work/space` is not absolute on Windows.
    let base = std::env::temp_dir();
    let ws = base.join("work").join("space");
    let rel = canonical_lock_key(&ws, Path::new("src/a.rs"));
    assert_eq!(rel, "src/a.rs");
    assert_eq!(canonical_lock_key(&ws, &ws.join("src").join("a.rs")), rel);
    assert_eq!(canonical_lock_key(&ws, Path::new("./src/../src/a.rs")), rel);
    // Outside the workspace stays absolute and distinct.
    let outside = base.join("elsewhere").join("a.rs");
    assert_eq!(
        canonical_lock_key(&ws, &outside),
        crate::server::path_util::posix_string(&outside)
    );
    assert_ne!(canonical_lock_key(&ws, Path::new("src/b.rs")), rel);
}

#[test]
fn release_error_display_names_the_path_and_both_nonces() {
    let e = ReleaseError::NotOwner {
        path: PathBuf::from("/w/x.lock-1"),
        expected: "mine".into(),
        found: "theirs".into(),
    };
    let text = e.to_string();
    assert!(
        text.contains("/w/x.lock-1") && text.contains("mine") && text.contains("theirs"),
        "{text}"
    );
    let io = ReleaseError::Io(std::io::Error::other("disk on fire")).to_string();
    assert!(io.contains("disk on fire"), "{io}");
}

#[test]
fn a_read_intent_holder_is_reported_as_read() {
    let ws = tempfile::tempdir().unwrap();
    let first = try_lock(
        ws.path(),
        Path::new("a.rs"),
        &AgentId("reader".into()),
        AgentKind::Other("t".into()),
        ClaimIntent::Read,
    )
    .unwrap();
    let conflict = try_lock(
        ws.path(),
        Path::new("a.rs"),
        &AgentId("other".into()),
        AgentKind::Other("t".into()),
        ClaimIntent::Edit,
    )
    .err()
    .expect("the file is held");
    assert_eq!(conflict.agent_id().0, "reader");
    assert_eq!(conflict.intent(), ClaimIntent::Read);
    drop(first);
}

#[test]
fn current_holder_reports_live_locks_only() {
    let ws = tempfile::tempdir().unwrap();
    let dir = lock_dir(ws.path());
    assert!(
        current_holder(ws.path(), Path::new("a.rs")).is_none(),
        "no lock file"
    );

    let live = dir.join(format!("{}.lock-live", sanitize(Path::new("a.rs"))));
    std::fs::write(
        &live,
        r#"{"agent_id":"holder","kind":"other","intent":"edit","nonce":"live"}"#,
    )
    .unwrap();
    let h = current_holder(ws.path(), Path::new("a.rs")).expect("a live lock is a holder");
    assert_eq!(h.agent_id().0, "holder");
    // A lock for a different path is not this path's holder.
    assert!(current_holder(ws.path(), Path::new("b.rs")).is_none());

    // Expired: no longer a holder.
    let f = std::fs::OpenOptions::new().write(true).open(&live).unwrap();
    f.set_modified(SystemTime::now() - LOCK_TTL - Duration::from_secs(5))
        .unwrap();
    drop(f);
    assert!(
        current_holder(ws.path(), Path::new("a.rs")).is_none(),
        "an expired lock holds nothing"
    );
}

/// A guard that cannot be created for any reason other than "it exists" must
/// not be mistaken for contention: proceed unguarded, immediately.
#[test]
fn an_uncreatable_guard_does_not_block_the_acquire() {
    let ws = tempfile::tempdir().unwrap();
    let missing_dir = ws.path().join("no-such-dir");
    let started = std::time::Instant::now();
    let g = AcquireGuard::take(&missing_dir, Path::new("a.rs"));
    assert!(
        g.is_ok(),
        "an unwritable guard location must degrade to unguarded"
    );
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "took {:?}",
        started.elapsed()
    );
}

/// A fresh guard held by someone else: wait, but only up to GUARD_WAIT.
#[test]
fn a_contended_guard_is_waited_on_for_a_bounded_time() {
    let ws = tempfile::tempdir().unwrap();
    let dir = lock_dir(ws.path());
    std::fs::File::create(dir.join(format!("{}.guard", sanitize(Path::new("a.rs"))))).unwrap();
    let started = std::time::Instant::now();
    let r = AcquireGuard::take(&dir, Path::new("a.rs"));
    let waited = started.elapsed();
    assert!(r.is_err(), "a live guard must not be taken");
    assert!(
        waited >= GUARD_WAIT - Duration::from_millis(100),
        "gave up after only {waited:?}"
    );
    assert!(
        waited < GUARD_WAIT + Duration::from_secs(2),
        "waited {waited:?}"
    );
}

/// When nothing under `.lain/` can be created (it is a file), acquiring must
/// fail in bounded time, not retry forever.
#[test]
fn an_unusable_lock_directory_fails_in_bounded_time() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join(".lain"), b"not a directory").unwrap();
    let started = std::time::Instant::now();
    let r = try_lock(
        ws.path(),
        Path::new("a.rs"),
        &AgentId("a".into()),
        AgentKind::Other("t".into()),
        ClaimIntent::Edit,
    );
    assert!(r.is_err());
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "took {:?}",
        started.elapsed()
    );
}

/// An expired lock held by someone else is cleaned up and the acquire
/// succeeds; a live one still conflicts. (Pins the `age > ttl` decision in
/// the scan: weakening it to `==` left expired locks blocking forever.)
#[test]
fn an_expired_foreign_lock_is_taken_over_but_a_live_one_conflicts() {
    let ws = tempfile::tempdir().unwrap();
    let dir = lock_dir(ws.path());
    let p = Path::new("a.rs");
    let theirs = dir.join(format!("{}.lock-theirs", sanitize(p)));
    std::fs::write(
        &theirs,
        r#"{"agent_id":"other","kind":"other","intent":"edit","nonce":"theirs"}"#,
    )
    .unwrap();
    let me = AgentId("me".into());
    let live = try_lock(
        ws.path(),
        p,
        &me,
        AgentKind::Other("t".into()),
        ClaimIntent::Edit,
    );
    assert!(live.is_err(), "a live foreign lock must conflict");

    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(&theirs)
        .unwrap();
    f.set_modified(SystemTime::now() - LOCK_TTL - Duration::from_secs(5))
        .unwrap();
    drop(f);
    let got = try_lock(
        ws.path(),
        p,
        &me,
        AgentKind::Other("t".into()),
        ClaimIntent::Edit,
    );
    assert!(
        got.is_ok(),
        "an expired foreign lock must not block: {:?}",
        got.err()
    );
    assert!(!theirs.exists(), "the expired lock file was removed");
}
