pub mod config;
pub mod federated_index;
pub mod graph_backend;
pub mod health;
pub mod loader;
pub mod manifest;
pub mod matching;
pub mod repo_id;
pub mod repo_index;
pub mod repo_source;

#[cfg(any(test, feature = "test-utils"))]
use crate::error::LainError;
#[cfg(any(test, feature = "test-utils"))]
use crate::federation::federated_index::FederatedIndex;
#[cfg(any(test, feature = "test-utils"))]
use crate::federation::graph_backend::PetgraphBackend;
#[cfg(any(test, feature = "test-utils"))]
use crate::schema::{EdgeType, GraphEdge, NodeType};
#[cfg(any(test, feature = "test-utils"))]
use std::path::Path;
#[cfg(any(test, feature = "test-utils"))]
use std::sync::Arc;

/// Build a synthetic `FederatedIndex` for benchmarks and large-fixture tests
/// without paying the cost of real indexing (`RepoIndex::index` runs the full
/// tree-sitter + LSP + git pipeline, which is overkill for a perf test that
/// only exercises the federation-level APIs).
///
/// The fixture is `num_repos` × `nodes_per_repo` nodes = `num_repos *
/// nodes_per_repo` total nodes, organized as a chain inside each repo:
/// node `r{i}f{j}` has an outgoing `Calls` edge to `r{i}f{j-1}` for `j > 0`.
/// Names are uniquified per repo (`r{i}f{j}`) so that `resolve_symbol` is
/// unambiguous in a multi-repo federation — picking `"r0f0"` resolves to
/// exactly one node in `repo0`.
///
/// Writes go through `GraphDatabase::insert_nodes_batch` /
/// `insert_edges_batch` on the underlying backend. `add_repo` is
/// intentionally bypassed: it constructs a `RepoIndex` (which opens a
/// `GitSensor` and a `notify` watcher), neither of which the perf test
/// exercises, and would force the caller to set up `num_repos` real git
/// directories. The per-write path (`upsert_node_global` / `upsert_edge`)
/// is also avoided because it `save_to_disk_sync`s after every write —
/// 50K fsyncs for a 50K-node fixture would take minutes. Batch methods
/// skip the per-write sync; we flush once at the end.
///
/// `resolve_symbol` has a fallback that scans the backend directly, so
/// direct inserts are sufficient for the blast-radius path to find the
/// seed node without going through `add_repo` / `project_repo`.
///
/// Visibility: gated on `#[cfg(any(test, feature = "test-utils"))]`. Unit
/// tests inside the crate (where `--test` cfg is active) see it; integration
/// tests under `tests/` only see it when the crate is built with
/// `--features test-utils`. Production binaries compile it out entirely.
#[cfg(any(test, feature = "test-utils"))]
pub fn federation_index_for_test(
    data_dir: &Path,
    num_repos: usize,
    nodes_per_repo: usize,
) -> Result<Arc<FederatedIndex>, LainError> {
    use crate::schema::GraphNode;

    let backend = Arc::new(PetgraphBackend::new(data_dir)?);

    // Build all nodes/edges up front, then bulk-insert via the batch
    // methods exposed on the underlying `GraphDatabase`. Going through
    // `PetgraphBackend::upsert_node_global` / `upsert_edge` would call
    // `save_to_disk_sync` after every write — for a 50K-node fixture
    // that's tens of thousands of fsyncs (each on the order of ms), which
    // would make the perf test setup take minutes. Batch methods skip the
    // sync; we flush once at the end so the on-disk state matches the
    // in-memory graph.
    let total = num_repos * nodes_per_repo;
    let mut nodes = Vec::with_capacity(total);
    let mut edges = Vec::with_capacity(total.saturating_sub(num_repos));
    for i in 0..num_repos {
        let repo_id = format!("repo{i}");
        for j in 0..nodes_per_repo {
            let name = format!("r{i}f{j}");
            let gid = format!("{repo_id}:Function:src/lib.rs:{name}");
            let mut node = GraphNode::new(
                NodeType::Function,
                name,
                "src/lib.rs".to_string(),
            );
            node.id = gid.clone();
            nodes.push(node);
            if j > 0 {
                let prev_name = format!("r{i}f{}", j - 1);
                let prev_gid = format!("{repo_id}:Function:src/lib.rs:{prev_name}");
                edges.push(GraphEdge::new(EdgeType::Calls, gid, prev_gid));
            }
        }
    }

    let db = backend.db();
    db.insert_nodes_batch(&nodes)?;
    db.insert_edges_batch(&edges)?;
    db.save_to_disk_sync()?;

    Ok(Arc::new(FederatedIndex::new(backend)))
}
