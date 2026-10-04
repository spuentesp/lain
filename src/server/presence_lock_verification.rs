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
