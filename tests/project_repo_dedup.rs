use lain::federation::loader::load_federation;
use lain::federation::repo_id::RepoId;

#[tokio::test]
async fn project_repo_is_idempotent() {
    // Build a throwaway workspace repo with a committed file so
    // `GitSensor::new` (called transitively by `load_federation` via
    // `RepoIndex::new`) can `libgit2::Repository::open` it. A bare
    // tempdir fails because there's no `.git` directory; a real
    // checkout of the host checkout would couple the test to a
    // specific machine layout and would not exist in CI.
    let repo_dir = tempfile::tempdir().unwrap();
    {
        let status = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(repo_dir.path())
            .status()
            .expect("git init");
        assert!(status.success(), "git init failed: {status}");
        std::fs::write(repo_dir.path().join("README.md"), "probe\n").unwrap();
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(repo_dir.path())
                .status()
                .expect("git failed");
            assert!(status.success(), "git {args:?} failed: {status}");
        };
        run(&["-c", "user.email=test@lain", "-c", "user.name=test", "add", "-A"]);
        run(&[
            "-c", "user.email=test@lain",
            "-c", "user.name=test",
            "commit", "-q", "-m", "fixture",
        ]);
    }

    // data_dir must be distinct from the workspace (otherwise the
    // federation's bincode files end up inside the seeded repo).
    let cfg_dir = tempfile::tempdir().unwrap();
    let cfg = cfg_dir.path().join("repos.yaml");
    std::fs::write(
        &cfg,
        format!(
            "data_dir: {}\nrepos:\n- id: probe\n  source: {{ type: workspace_dir, path: {} }}\n",
            cfg_dir.path().join("data").display(),
            repo_dir.path().display(),
        ),
    )
    .unwrap();
    let fed = load_federation(&cfg).await.unwrap();
    let id = RepoId::new("probe").unwrap();
    // First projection
    fed.project_repo(&id).await.unwrap();
    let first = fed.backend().edge_count();
    // Second projection — idempotent?
    fed.project_repo(&id).await.unwrap();
    let second = fed.backend().edge_count();
    println!("first={} second={}", first, second);
    assert_eq!(first, second, "second projection must not add edges");
}
