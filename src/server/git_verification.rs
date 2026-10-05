//! `GitSensor` is shared across threads through `Arc<AnyGitSensor>`. Its
//! `Sync` impl is now derived (the handle sits behind a mutex), not asserted
//! with `unsafe`; these tests pin that.
use super::*;

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn git_sensor_is_send_and_sync_without_unsafe() {
    assert_send_sync::<GitSensor>();
}

/// Many threads drive one shared handle at once. Before the handle was
/// serialised this was an unsynchronised use of a `git_repository`.
#[test]
fn concurrent_use_of_one_shared_sensor_is_consistent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(root.join("b.rs"), "fn b() {}\n").unwrap();
    let repo = git2::Repository::init(root).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("a.rs")).unwrap();
    index.add_path(Path::new("b.rs")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("t", "t@example.com").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();
    std::fs::write(root.join("c.rs"), "fn c() {}\n").unwrap(); // untracked

    let sensor = Arc::new(GitSensor::new(root).unwrap());
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let s = Arc::clone(&sensor);
            let root = root.to_path_buf();
            std::thread::spawn(move || {
                for _ in 0..50 {
                    assert!(s.is_valid());
                    assert_eq!(s.get_all_tracked_files().unwrap().len(), 2);
                    assert_eq!(s.get_uncommitted_changes().unwrap().len(), 1);
                    assert!(!s.is_ignored(&root.join("a.rs")).unwrap());
                }
            })
        })
        .collect();
    for w in workers {
        w.join().unwrap();
    }
}

// ---- differential test against the real `git` CLI ------------------------------------

mod differential {
    use super::*;
    use proptest::prelude::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "core.autocrlf=false",
            ])
            .args(args)
            .current_dir(root)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    const NAMES: [&str; 4] = ["a.rs", "b.rs", "dir/c.rs", "dir/sub/d.rs"];

    #[derive(Clone, Debug)]
    enum Op {
        Write(usize, u8),
        Delete(usize),
        StageAll,
        StageOne(usize),
        Commit,
        Unstage(usize),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => (0..NAMES.len(), 0u8..3).prop_map(|(i, c)| Op::Write(i, c)),
            2 => (0..NAMES.len()).prop_map(Op::Delete),
            1 => Just(Op::StageAll),
            2 => (0..NAMES.len()).prop_map(Op::StageOne),
            2 => Just(Op::Commit),
            1 => (0..NAMES.len()).prop_map(Op::Unstage),
        ]
    }

    fn apply(root: &Path, op: &Op) {
        match op {
            Op::Write(i, c) => {
                let p = root.join(NAMES[*i]);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, format!("fn f{c}() {{}}\n")).unwrap();
            }
            Op::Delete(i) => {
                let _ = std::fs::remove_file(root.join(NAMES[*i]));
            }
            Op::StageAll => {
                git(root, &["add", "-A"]);
            }
            Op::StageOne(i) => {
                // `git add -A -- path` also stages a deletion; it errors for a
                // path that neither exists nor is tracked, which is fine here.
                let _ = Command::new("git")
                    .args(["add", "-A", "--", NAMES[*i]])
                    .current_dir(root)
                    .output();
            }
            Op::Unstage(i) => {
                let _ = Command::new("git")
                    .args(["reset", "-q", "--", NAMES[*i]])
                    .current_dir(root)
                    .output();
            }
            Op::Commit => {
                let _ = Command::new("git")
                    .args([
                        "-c",
                        "user.name=t",
                        "-c",
                        "user.email=t@t",
                        "commit",
                        "-qm",
                        "x",
                    ])
                    .current_dir(root)
                    .output();
            }
        }
    }

    /// What the sensor must report for a path, from git's own plumbing:
    /// `(staged, deleted)`. git prints two lines for a path that is both a
    /// staged deletion and a new untracked file; they merge into one entry.
    fn expected(root: &Path) -> BTreeMap<String, (bool, bool)> {
        let mut out: BTreeMap<String, (bool, bool)> = BTreeMap::new();
        for line in git(
            root,
            &[
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
                "--no-renames",
            ],
        )
        .lines()
        {
            let (xy, path) = (&line[..2], line[3..].to_string());
            if xy == "!!" {
                continue;
            }
            let staged = !matches!(xy.as_bytes()[0], b' ' | b'?');
            let deleted = !root.join(&path).exists();
            let e = out.entry(path).or_insert((false, deleted));
            e.0 |= staged;
        }
        out
    }

    fn actual(sensor: &GitSensor, root: &Path) -> BTreeMap<String, (bool, ChangeType)> {
        sensor
            .get_uncommitted_changes()
            .unwrap()
            .into_iter()
            .map(|c| {
                let rel = c
                    .path
                    .strip_prefix(root)
                    .unwrap_or(&c.path)
                    .to_string_lossy()
                    .replace('\\', "/");
                (rel, (c.staged, c.change_type))
            })
            .collect()
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 40, max_shrink_iters: 200, ..ProptestConfig::default() })]

        #[test]
        fn sensor_agrees_with_git_after_every_operation(ops in prop::collection::vec(op(), 1..14)) {
            let dir = tempfile::tempdir().unwrap();
            let root = dunce::canonicalize(dir.path()).unwrap();
            git(&root, &["init", "-q"]);
            std::fs::write(root.join("seed.rs"), "fn seed() {}\n").unwrap();
            git(&root, &["add", "-A"]);
            git(&root, &["commit", "-qm", "seed"]);
            let sensor = GitSensor::new(&root).unwrap();

            for (n, o) in ops.iter().enumerate() {
                apply(&root, o);
                let want = expected(&root);
                let got = actual(&sensor, &root);
                let got_fn: BTreeMap<_, _> = got
                    .iter()
                    .map(|(p, (st, t))| (p.clone(), (*st, matches!(t, ChangeType::Deleted))))
                    .collect();
                prop_assert_eq!(&got_fn, &want, "after op #{} {:?} (ops so far: {:?})", n, o, &ops[..=n]);

                // `Added` is only a label, but it must not contradict git: the
                // path is absent from HEAD, or HEAD's copy is staged for deletion.
                let head_files: BTreeSet<String> = git(&root, &["ls-tree", "-r", "--name-only", "HEAD"])
                    .lines()
                    .map(str::to_string)
                    .collect();
                let staged_deleted: BTreeSet<String> = git(&root, &["diff", "--cached", "--name-only", "--diff-filter=D"])
                    .lines()
                    .map(str::to_string)
                    .collect();
                for (path, (_, t)) in &got {
                    if matches!(t, ChangeType::Added) {
                        prop_assert!(
                            !head_files.contains(path) || staged_deleted.contains(path),
                            "{} labelled Added but it is in HEAD and not staged for deletion", path
                        );
                    }
                }

                // Contract: tracked files that still exist on disk (a file that
                // is in the index but deleted from the tree cannot be read).
                let tracked: BTreeSet<String> = git(&root, &["ls-files"])
                    .lines()
                    .filter(|p| root.join(p).is_file())
                    .map(str::to_string)
                    .collect();
                let seen: BTreeSet<String> = sensor
                    .get_all_tracked_files()
                    .unwrap()
                    .into_iter()
                    .map(|p| {
                        p.strip_prefix(&root)
                            .unwrap_or(&p)
                            .to_string_lossy()
                            .replace('\\', "/")
                    })
                    .collect();
                prop_assert_eq!(seen, tracked, "tracked files differ after {:?}", o);

                let head = git(&root, &["rev-parse", "HEAD"]).trim().to_string();
                prop_assert_eq!(sensor.get_latest_commit().unwrap(), head);
            }
        }
    }
}
