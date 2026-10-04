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
