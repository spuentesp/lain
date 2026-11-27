//! Measure peak RSS for the §8.5 "Memory ceiling" measurement
//! (§15.1 fixture + tokio + bytes federation as snapshots).
//!
//! Runs the snapshot indexer in `IndexMode::Snapshot` against
//! each repo path given on the command line, hydrates each into
//! a `PetgraphBackend::ephemeral()` federation, runs
//! `ContractJoiner::run` to derive a `ContractIndex`, and records
//! the process's peak RSS (`VmHWM`) per phase.
//!
//! Uses the server's allocator (mimalloc) so the numbers match what
//! `lain server` pays. Prints one `phase=<name> rss_bytes=<N>
//! peak_bytes=<N>` line per phase (peak since the previous phase), and
//! a final `peak_bytes=<N>` (the largest phase peak) on stdout. The shell wrapper
//! (`scripts/measure_snapshot_memory.sh`) runs this binary twice —
//! once against the §15.1 fixture, once against tokio + bytes —
//! and combines the two peaks into the final ceiling.
//!
//! Usage: `measure_snapshot_memory <repo-1> [<repo-2> ...]`

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use lain::federation::contracts::index_cache::{build_manifest, CacheKey, IndexCache};
use lain::federation::contracts::snapshots::manager::{
    build_snapshot_contract_index, hydrate_graph, project_graph,
};
use lain::federation::graph_backend::PetgraphBackend;
use lain::federation::repo_id::RepoId;
use lain::git::{AnyGitSensor, GitSensorMode};
use lain::graph::GraphDatabase;
use lain::schema::RepoNamespace;
use lain::server::ingest::ingestion::{index_one_repo, IndexMode, IndexRequest};
use lain::server::mem_probe;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Print the phase line and reset the kernel's peak counter so the
/// next phase reports its own peak. Returns the phase peak in bytes.
fn report_phase(name: &str) -> u64 {
    let (rss_kb, peak_kb) = mem_probe::rss_and_peak_kb().unwrap_or((0, 0));
    mem_probe::reset_peak();
    println!(
        "phase={name} rss_bytes={} peak_bytes={}",
        rss_kb * 1024,
        peak_kb * 1024
    );
    peak_kb * 1024
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let repo_paths: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    if repo_paths.is_empty() {
        return Err("usage: measure_snapshot_memory <repo-1> [<repo-2> ...]".into());
    }
    let work_dir = std::env::temp_dir().join(format!(
        "lain-mem-measure-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&work_dir)?;
    let cache = IndexCache::new(&work_dir);
    let mut max_peak = report_phase("start");
    let analyzer_version = lain::federation::contracts::analyzer_version();

    // Index every repo into the cache. The graph bytes are kept
    // in `cache`; the peak RSS during the indexing pass is what we
    // want to measure.
    for repo_path in &repo_paths {
        let repo_id = infer_repo_id(repo_path);
        let sha = head_sha(repo_path);
        let key = CacheKey::new(&repo_id, &sha, analyzer_version.clone());
        if cache.has_entry(&key) {
            continue;
        }
        let staging = work_dir.join(format!("{}-{}.graph", repo_id, &sha[..12]));
        let db = GraphDatabase::new(&staging)?;
        let namespace = RepoNamespace::from_repo_id(&RepoId::new(&repo_id)?);
        let cancel = tokio_util::sync::CancellationToken::new();
        let git = AnyGitSensor::new(repo_path, GitSensorMode::InProcess)
            .map_err(|e| format!("git: {e}"))?;
        let request = IndexRequest {
            path: repo_path,
            graph: &db,
            lsp_pool: None,
            git: &git,
            overlay: None,
            resolver: None,
            source_repo: None,
            namespace: &namespace,
            force: true,
            cancel: &cancel,
            mode: IndexMode::Snapshot,
        };
        index_one_repo(request).await?;
        let bytes = std::fs::read(&staging)?;
        let manifest = build_manifest(
            &repo_id,
            &sha,
            &analyzer_version,
            vec![],
            std::collections::BTreeMap::new(),
            bytes.len() as u64,
        );
        cache.write_entry(&key, &bytes, &manifest)?;
        let _ = std::fs::remove_file(&staging);
        max_peak = max_peak.max(report_phase(&format!("index:{repo_id}")));
    }

    // Hydrate each into a federation over `PetgraphBackend::ephemeral`
    // and run the joiner. The peak RSS during this hydration is
    // the federation-snapshot peak the brief cares about.
    let fed_path = work_dir.join("federated_graph.bin");
    let backend = std::sync::Arc::new(PetgraphBackend::ephemeral(&fed_path));
    for repo_path in &repo_paths {
        let repo_id = infer_repo_id(repo_path);
        let sha = head_sha(repo_path);
        let key = CacheKey::new(&repo_id, &sha, analyzer_version.clone());
        let bytes = cache.read_graph_bytes(&key)?;
        let db = hydrate_graph(&bytes, &fed_path)?;
        max_peak = max_peak.max(report_phase(&format!("hydrate:{repo_id}")));
        project_graph(&db, &repo_id, backend.clone())?;
        drop(db);
        drop(bytes);
        max_peak = max_peak.max(report_phase(&format!("project:{repo_id}")));
    }
    // Force the joiner to run so peak RSS during the contract
    // derivation is captured (per §8.5 — the federation
    // snapshot's peak is what we measure).
    let _join = build_snapshot_contract_index(
        backend.as_ref(),
        &lain::federation::contracts::config::ContractFederationConfig::default(),
    )?;

    max_peak = max_peak.max(report_phase("join"));
    drop(backend);
    max_peak = max_peak.max(report_phase("drop_federation"));
    println!("peak_bytes={max_peak}");
    Ok(())
}

/// Infer the repo id from the directory name. The fixture script
/// names repos `orders`, `billing`, `reports`, `platform`; tokio +
/// bytes clones carry the names they ship with. Falls back to
/// `unknown-<hash>` so the cache key stays unique.
fn infer_repo_id(repo_path: &Path) -> String {
    repo_path
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("unknown-{:x}", hash_path(repo_path)))
}

fn hash_path(p: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.hash(&mut h);
    h.finish()
}

fn head_sha(repo_root: &Path) -> String {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_root)
        .output()
        .expect("git rev-parse");
    assert!(
        out.status.success(),
        "git rev-parse failed for {}: stderr={}",
        repo_root.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
