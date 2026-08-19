//! Static-graph retract detection at claim time (Task 1.6, PR 1).
//!
//! When an agent calls `claim_files` with a `plan_revision`, the server
//! should populate `world_state.changed_symbols` with an entry per
//! requested symbol that is no longer present in the static graph
//! (`change_kind: Retracted`). The lookup exercises the federation's
//! `GraphBackend::remove_nodes` path (the same path `project_repo`
//! uses to retract symbols after a re-index drops them).
//!
//! Reads `claim_files` end-to-end through the MCP dispatcher so the
//! test exercises the same code path that hooks reach in production.

use lain::federation::federated_index::FederatedIndex;
use lain::federation::graph_backend::{GraphBackend, PetgraphBackend};
use lain::federation::repo_id::{GlobalId, RepoId};
use lain::schema::NodeType;
use lain::server::mcp::presence_tools::{run_claim_files, run_register_agent};
use lain::server::LainServer;
use std::sync::Arc;

/// Convenience: assert `claim_files` succeeds and return the parsed JSON.
fn claim(server: &Arc<LainServer>, agent_id: &str, token: &str, plan_revision: u64, symbols: &[&str]) -> serde_json::Value {
    let syms: Vec<String> = symbols.iter().map(|s| s.to_string()).collect();
    let args = serde_json::json!({
        "agent_id": agent_id,
        "session_token": token,
        "files": [{
            "path": "auth.rs",
            "symbols": syms,
            "intent": "edit",
            "plan_revision": plan_revision,
        }],
    });
    run_claim_files(server, args).expect("claim_files should succeed")
}

/// Build a federation-backed `LainServer` whose backend is a fresh
/// `PetgraphBackend` rooted at `tmp`. The federation has no repos
/// loaded — the test only manipulates the backend directly via
/// `upsert_node_global` / `remove_nodes`, which is the same surface
/// `project_repo` uses to retract symbols after a re-index.
fn build_federation_server(tmp: &std::path::Path) -> (Arc<LainServer>, Arc<dyn GraphBackend>) {
    let backend: Arc<dyn GraphBackend> = Arc::new(PetgraphBackend::new(tmp).expect("backend"));
    let fed = Arc::new(FederatedIndex::new(backend.clone()));
    let server = LainServer::with_federation(fed, lain::server::Transport::Stdio, 0, None, None)
        .expect("with_federation");
    (Arc::new(server), backend)
}

/// Insert a node with name `verify_token` into the federation backend
/// using the global id format `repo:Kind:path:name`. Mirrors what
/// `project_repo` writes during a re-index; the test then retracts
/// the same node via `remove_nodes` to project the
/// post-`project_repo`-deduplication state.
fn insert_verify_token(backend: &dyn GraphBackend) {
    let repo = RepoId::new("test").unwrap();
    let gid = GlobalId::new(&repo, NodeType::Function, "src/auth.rs", "verify_token");
    backend
        .upsert_node_global(gid.as_str(), NodeType::Function, "src/auth.rs", "verify_token")
        .expect("upsert_node_global");
}

#[tokio::test]
async fn claim_with_retracted_symbol_populates_world_state() {
    // ── Setup ────────────────────────────────────────────────────────────
    let tmp = tempfile::tempdir().unwrap();
    let (server, backend) = build_federation_server(tmp.path());

    let v = run_register_agent(&server, serde_json::json!({"name": "alice"})).unwrap();
    let agent_id = v["agent_id"].as_str().unwrap().to_string();
    let token = v["session_token"].as_str().unwrap().to_string();

    // ── Step 1: symbol present in static graph ──────────────────────────
    insert_verify_token(&*backend);

    let resp = claim(&server, &agent_id, &token, 0, &["verify_token"]);
    // Claim granted (no conflict).
    assert_eq!(
        resp["granted"].as_array().unwrap().len(),
        1,
        "first claim should be granted; resp={resp}"
    );
    // `world_state` is populated because `plan_revision` was supplied.
    let ws = &resp["world_state"];
    assert!(
        !ws.is_null(),
        "world_state must be Some when plan_revision is provided; resp={resp}"
    );
    // No retract entries: verify_token exists in the static graph.
    assert_eq!(
        ws["changed_symbols"].as_array().unwrap().len(),
        0,
        "verify_token is indexed, must not be flagged Retracted; resp={resp}"
    );
    assert!(
        ws.get("note").is_none() || ws["note"].is_null(),
        "note should be absent on the success path; resp={resp}"
    );

    // ── Step 2: retract the symbol from the static graph ───────────────
    let live = backend.find_nodes_by_name("verify_token").unwrap();
    assert_eq!(live.len(), 1, "seed should be visible before retraction");
    let ids: Vec<String> = live.iter().map(|n| n.id.clone()).collect();
    let removed = backend.remove_nodes(&ids).expect("remove_nodes");
    assert_eq!(removed, 1, "remove_nodes should retract the seed");
    // Sanity: the backend no longer sees the symbol.
    let after = backend.find_nodes_by_name("verify_token").unwrap();
    assert!(after.is_empty(), "verify_token should be gone after retraction");

    // ── Step 3: claim again — verify_token must show Retracted ──────────
    let resp2 = claim(&server, &agent_id, &token, 0, &["verify_token"]);
    let ws2 = &resp2["world_state"];
    assert!(!ws2.is_null(), "world_state must be Some after retraction");
    let symbols = ws2["changed_symbols"].as_array().unwrap();
    assert_eq!(
        symbols.len(),
        1,
        "expected exactly one Retracted entry; got {symbols:?}"
    );
    assert_eq!(symbols[0]["name"], "verify_token");
    assert_eq!(symbols[0]["change_kind"], "Retracted");
    // `at_revision` is the revision at the moment of claim detection; we
    // don't pin the exact value but it must be a non-negative integer.
    assert!(symbols[0]["at_revision"].is_u64());
}

/// Companion to the test above: a claim without `plan_revision` must
/// leave `world_state` unwritten (`None`), even when the federation
/// is empty (the legacy hook path — these callers don't track plan
/// revisions and shouldn't see the new field).
#[tokio::test]
async fn claim_without_plan_revision_omits_world_state() {
    let tmp = tempfile::tempdir().unwrap();
    let (server, _backend) = build_federation_server(tmp.path());

    let v = run_register_agent(&server, serde_json::json!({"name": "bob"})).unwrap();
    let agent_id = v["agent_id"].as_str().unwrap().to_string();
    let token = v["session_token"].as_str().unwrap().to_string();

    let args = serde_json::json!({
        "agent_id": agent_id,
        "session_token": token,
        "files": [{
            "path": "auth.rs",
            "symbols": ["verify_token"],
            "intent": "edit",
        }],
    });
    let resp = run_claim_files(&server, args).expect("claim_files should succeed");
    assert_eq!(resp["granted"].as_array().unwrap().len(), 1);
    // Legacy callers must not see `world_state` in the response.
    assert!(
        resp.get("world_state").is_none(),
        "world_state must be None when plan_revision is None; resp={resp}"
    );
}
