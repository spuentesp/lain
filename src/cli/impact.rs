//! `lain impact` — machine-checkable impact claims for one symbol
//! (issue #294).
//!
//! Answers "who is affected if I change `<symbol>`?" as copy-pasteable
//! protocol lines instead of prose agents have to reformat:
//!
//! ```text
//! AFFECTED: <repo>:<file>:<symbol>  EVIDENCE: <verified|needs-investigation|missing>
//! ```
//!
//! Reads `<workspace>/.lain/graph.bin` directly — same workspace
//! resolution as `lain query` (walk up for `.git`, `--workspace` to
//! override) — so it works without a running server.
//!
//! Evidence classes come from what the graph already records:
//! static-provenance `Calls`/`Uses` chains classify `verified`,
//! heuristic dispatch/bus/router edges and uncommitted overlay
//! callers classify `needs-investigation` with the reason named, and
//! known-unknowns (an empty blast radius, an edge from a node the
//! index cannot resolve) classify `missing` — never a confident "no
//! impact". Symbols with no edge to the seed never appear: claims are
//! derived from real edges only.

use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use anyhow::Result;

/// Load the workspace graph and print `<symbol>`'s impact claims.
/// `format` is `claims` (the protocol lines) or `json`.
pub fn run_impact(symbol: &str, workspace: Option<&std::path::Path>, format: &str) -> Result<()> {
    // Same workspace resolution as `cli::query::run_query`.
    let root = match workspace {
        Some(p) => p.to_path_buf(),
        None => crate::cli::workspace::find_git_workspace_root(None)
            .ok()
            .flatten()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no `.git` found in any parent directory — pass `--workspace PATH` to override"
                )
            })?,
    };
    let memory_path = root.join(".lain/graph.bin");
    // Opening a missing graph "succeeds" with an empty one; refuse
    // that here for the same reason `lain query` does — an empty
    // answer about an unindexed repository would read as "no impact".
    if !memory_path.is_file() {
        eprintln!(
            "Error: {} has no index yet ({} is missing).\n\nHint: run `lain oneshot find_anchors` \
             there to build it (an agent's `lain mcp` also builds it on first start).",
            root.display(),
            memory_path.display()
        );
        std::process::exit(1);
    }

    let graph = match GraphDatabase::new(&memory_path) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Error: Failed to load graph at {:?}: {}", memory_path, e);
            eprintln!("\nHint: Run 'lain mcp' (or 'lain server') first to build the code graph.");
            std::process::exit(1);
        }
    };
    let overlay = VolatileOverlay::new();

    // Single-workspace graphs keep `GraphNode::repo_id = None`; the
    // workspace directory name is the repository identity the claim
    // line uses (matches `repos.yaml` ids by convention).
    let repo_fallback = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let claims =
        match crate::server::claims::claims_from_graph(&graph, &overlay, symbol, &repo_fallback) {
            Ok(claims) => crate::server::claims::merge_claims(claims),
            Err(e) => {
                eprintln!("Error: {e}");
                eprintln!(
                    "\nHint: `lain impact` answers from this workspace's index — run `lain mcp` \
                 (or `lain oneshot find_anchors`) first if the graph is missing or the \
                 symbol is not indexed."
                );
                std::process::exit(1);
            }
        };

    match format {
        "json" => println!("{}", serde_json::to_string_pretty(&claims)?),
        _ => print!(
            "{}",
            crate::server::claims::render_claim_lines(claims, None)
        ),
    }
    Ok(())
}
