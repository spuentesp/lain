use super::LainServer;
use crate::schema::NodeType;
use std::path::Path;

fn commit(root: &Path) {
    let repo = git2::Repository::open(root).unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::now("test", "test@example.com").unwrap();
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "fixture",
        &tree,
        &parent.iter().collect::<Vec<_>>(),
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_lsp_refresh_preserves_identity_and_cleans_reverted_symbols() {
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    git2::Repository::init(root.path()).unwrap();
    let file = root.path().join("lib.rs");
    std::fs::write(&file, "pub fn existing() {}\n").unwrap();
    commit(root.path());
    let server = LainServer::new(root.path(), &data.path().join("graph.bin"), None).unwrap();
    server.lsp_pool.disable_rust_for_test().await;
    server.build_core_memory().await.unwrap();
    let original = server.graph.find_node_by_name("existing").unwrap();
    let mut diffs = crate::overlay::subscribe_channel();

    std::fs::write(
        &file,
        "pub fn existing() { let edited = 1; }\npub fn added() {}\n",
    )
    .unwrap();
    let (a, b) = tokio::join!(
        server.sync_volatile_overlay(),
        server.sync_volatile_overlay()
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(
        server.overlay.find_nodes_by_name("existing")[0].id,
        original.id
    );
    assert_eq!(server.overlay.find_nodes_by_name("added").len(), 1);

    // Replay the real broadcasts: replacing an ID must not delete its
    // replacement on a sidecar after a second refresh.
    let replica = crate::overlay::VolatileOverlay::new();
    while let Ok(diff) = diffs.try_recv() {
        for node in diff.added {
            replica.insert_node(node);
        }
        for id in diff.removed {
            replica.remove_node(&id);
        }
        for node in diff.updated {
            replica.insert_node(node);
        }
    }
    assert!(replica.get_node(&original.id).is_some());

    commit(root.path());
    server.sync_volatile_overlay().await.unwrap();
    assert_eq!(server.overlay.find_nodes_by_name("added").len(), 1);
    server.build_core_memory().await.unwrap();
    server.sync_volatile_overlay().await.unwrap();
    assert!(server.overlay.get_all_nodes().is_empty());

    std::fs::write(&file, "pub fn transient() {}\n").unwrap();
    server.sync_volatile_overlay().await.unwrap();
    assert_eq!(server.overlay.find_nodes_by_name("transient").len(), 1);
    std::fs::write(
        &file,
        "pub fn existing() { let edited = 1; }\npub fn added() {}\n",
    )
    .unwrap();
    server.sync_volatile_overlay().await.unwrap();
    assert!(server.overlay.get_all_nodes().is_empty());

    // One refresh accounts for the full change set, including more than
    // the old watcher's 20-path batch limit.
    for i in 0..25 {
        std::fs::write(
            root.path().join(format!("batch_{i}.rs")),
            format!("pub fn batch_{i}() {{}}\n"),
        )
        .unwrap();
    }
    server.sync_volatile_overlay().await.unwrap();
    assert_eq!(
        server
            .overlay
            .get_all_nodes()
            .iter()
            .filter(|n| n.name.starts_with("batch_"))
            .count(),
        25
    );

    let id = server.graph.find_node_by_name("existing").unwrap().id;
    drop(server);
    let reopened = LainServer::new(root.path(), &data.path().join("graph.bin"), None).unwrap();
    reopened.lsp_pool.disable_rust_for_test().await;
    std::fs::write(
        &file,
        "pub fn existing() { let edited = 2; }\npub fn added() {}\n",
    )
    .unwrap();
    reopened.sync_volatile_overlay().await.unwrap();
    assert_eq!(reopened.overlay.find_nodes_by_name("existing")[0].id, id);
    assert_eq!(
        reopened
            .graph
            .find_node_by_name("existing")
            .unwrap()
            .node_type,
        NodeType::Function
    );
}
