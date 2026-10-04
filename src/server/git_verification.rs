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
