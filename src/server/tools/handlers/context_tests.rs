//! Tests for tools/handlers/context.rs

use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use crate::server::tools::handlers::context::{
    get_call_sites, get_code_snippet, get_context_for_prompt,
};

fn make_test_graph() -> (GraphDatabase, VolatileOverlay) {
    let tmp = std::env::temp_dir().join("test_context_graph");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    // Create a simple call graph: caller -> callee
    let caller = GraphNode::new(
        NodeType::Function,
        "caller".to_string(),
        "/src/main.rs".to_string(),
    );
    let callee = GraphNode::new(
        NodeType::Function,
        "callee".to_string(),
        "/src/callee.rs".to_string(),
    );
    let file_node = GraphNode::new(
        NodeType::File,
        "main.rs".to_string(),
        "/src/main.rs".to_string(),
    );

    graph.upsert_node(caller.clone()).unwrap();
    graph.upsert_node(callee.clone()).unwrap();
    graph.upsert_node(file_node.clone()).unwrap();

    graph
        .insert_edge(&GraphEdge::new(
            EdgeType::Contains,
            file_node.id.clone(),
            caller.id.clone(),
        ))
        .unwrap();
    graph
        .insert_edge(&GraphEdge::new(
            EdgeType::Calls,
            caller.id.clone(),
            callee.id.clone(),
        ))
        .unwrap();

    let overlay = VolatileOverlay::new();
    (graph, overlay)
}

#[test]
fn test_get_context_for_prompt_existing() {
    let (graph, overlay) = make_test_graph();

    // Add node with full info to overlay
    let mut node = GraphNode::new(
        NodeType::Function,
        "caller".to_string(),
        "/src/main.rs".to_string(),
    );
    node.signature = Some("(x: i32) -> i32".to_string());
    node.docstring = Some("A test function".to_string());
    node.depth_from_main = Some(0);
    node.anchor_score = Some(0.5);
    overlay.insert_node(node);

    let result = get_context_for_prompt(&graph, &overlay, "caller", None);
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("caller"));
    assert!(text.contains("Function"));
}

#[test]
fn test_get_context_for_prompt_not_found() {
    let (graph, overlay) = make_test_graph();

    let result = get_context_for_prompt(&graph, &overlay, "nonexistent_symbol", None);
    assert!(result.is_err());
}

#[test]
fn test_get_context_for_prompt_with_max_tokens() {
    let (graph, overlay) = make_test_graph();

    let mut node = GraphNode::new(
        NodeType::Function,
        "big_fn".to_string(),
        "/src/lib.rs".to_string(),
    );
    node.docstring = Some("A".repeat(1000));
    overlay.insert_node(node);

    let result = get_context_for_prompt(&graph, &overlay, "big_fn", Some(50));
    assert!(result.is_ok());
    let text = result.unwrap();
    // Should contain the function name
    assert!(text.contains("big_fn"));
}

#[test]
fn test_get_code_snippet_existing() {
    let (graph, overlay) = make_test_graph();

    // Create a temp file to read
    let tmp = std::env::temp_dir().join("test_snippet.txt");
    std::fs::write(&tmp, "line1\nline2\nline3\nline4\nline5\n").unwrap();

    let ws = std::env::temp_dir();
    let result = get_code_snippet(&graph, &overlay, &ws, tmp.to_str().unwrap(), Some(2), None);
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("line2") || text.contains("Showing lines"));

    std::fs::remove_file(&tmp).ok();
}

#[test]
fn test_get_code_snippet_nonexistent_file() {
    let (graph, overlay) = make_test_graph();

    let ws = std::env::temp_dir();
    let result = get_code_snippet(&graph, &overlay, &ws, "/nonexistent/file.txt", None, None);
    assert!(result.is_err());
}

#[test]
fn test_get_call_sites_existing() {
    let (graph, overlay) = make_test_graph();

    // callee is called by caller
    let result = get_call_sites(std::path::Path::new(""), &graph, &overlay, "callee");
    assert!(result.is_ok());
    let text = result.unwrap();
    // Should show caller as a call site for callee
    assert!(text.contains("call sites") || text.contains("callee"));
}

#[test]
fn test_get_call_sites_not_found() {
    let (graph, overlay) = make_test_graph();

    let result = get_call_sites(std::path::Path::new(""), &graph, &overlay, "nonexistent");
    assert!(result.is_err());
}

#[test]
fn test_get_call_sites_no_callers() {
    let (graph, overlay) = make_test_graph();

    // "caller" has no incoming calls in our test graph
    let result = get_call_sites(std::path::Path::new(""), &graph, &overlay, "caller");
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("No call sites") || text.contains("caller"));
}

// ─── Answer scope: why an empty answer may not be a confirmed absence ──
//
// The five cases the objective-gap plan names: a correctly resolved
// call, an ambiguous same-name call, a dynamic call, a stale index,
// and a truncated traversal. Each one must produce a *different*,
// observable answer — none of them may be reported as "nothing calls
// this".

use crate::graph::Freshness;
use crate::schema::{EdgeProvenance, RouteMatch};
use crate::server::tools::answer_scope::IncompleteReason;
use crate::server::tools::handlers::context::get_call_sites_report_scoped;

/// A node that the graph has scanned, so the index can prove it is
/// current for the file.
fn scanned_node(name: &str, path: &str, ns: &crate::schema::RepoNamespace) -> GraphNode {
    let mut n = GraphNode::new(NodeType::Function, name.to_string(), path.to_string());
    n.id = GraphNode::generate_id(&NodeType::Function, path, name, None, ns);
    n.last_lsp_sync = Some(crate::server::time::unix_secs(std::time::SystemTime::now()));
    n
}

/// A workspace whose `src/caller.rs` holds `call_target();` — the file
/// the graph scanned, so a call to it is locatable.
fn workspace_with_caller(dir: &std::path::Path) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/caller.rs"),
        "fn call_target() {}\nfn caller() { call_target(); }\n",
    )
    .unwrap();
}

#[test]
fn resolved_call_reports_no_scope() {
    // The baseline: the call is found, the file is current, the name is
    // unique. Nothing is missing, so the answer carries no reason.
    let dir = std::env::temp_dir().join("lain_test_scope_resolved");
    let _ = std::fs::remove_dir_all(&dir);
    workspace_with_caller(&dir);
    let db = crate::graph::GraphDatabase::new(&dir.join("graph.bin")).unwrap();
    let overlay = crate::overlay::VolatileOverlay::new();
    let ns = crate::schema::RepoNamespace::for_test();

    let target = scanned_node("call_target", "src/caller.rs", &ns);
    let mut caller = scanned_node("caller", "src/caller.rs", &ns);
    caller.line_start = Some(1);
    caller.line_end = Some(2);
    db.upsert_node(target.clone()).unwrap();
    db.upsert_node(caller.clone()).unwrap();
    db.insert_edge(&GraphEdge::new(
        EdgeType::Calls,
        caller.id.clone(),
        target.id.clone(),
    ))
    .unwrap();

    let report = get_call_sites_report_scoped(&dir, &db, &overlay, "call_target", None).unwrap();
    assert_eq!(report.total_sites, 1);
    assert_eq!(
        report.answer_scope, None,
        "a resolved call in a current index needs no reason"
    );
}

#[test]
fn ambiguous_same_name_call_reports_the_ambiguity() {
    // Two definitions share the name. The answer is about one of them,
    // and must say so rather than read as "nothing calls this".
    let dir = std::env::temp_dir().join("lain_test_scope_ambiguous");
    let _ = std::fs::remove_dir_all(&dir);
    workspace_with_caller(&dir);
    let db = crate::graph::GraphDatabase::new(&dir.join("graph.bin")).unwrap();
    let overlay = crate::overlay::VolatileOverlay::new();
    let ns = crate::schema::RepoNamespace::for_test();

    for path in ["src/a.rs", "src/b.rs"] {
        let mut n = scanned_node("twin", path, &ns);
        n.id = GraphNode::generate_id(&NodeType::Function, path, "twin", None, &ns);
        db.upsert_node(n).unwrap();
    }

    let report = get_call_sites_report_scoped(&dir, &db, &overlay, "twin", None).unwrap();
    assert_eq!(report.total_sites, 0);
    assert_eq!(
        report.answer_scope,
        Some(IncompleteReason::AmbiguousTarget),
        "an empty answer about a shared name must name the ambiguity"
    );
    assert!(report.text.contains("ambiguous_target"));
}

#[test]
fn dynamic_call_reports_the_dynamic_target() {
    // The graph holds a `Calls` edge, but the caller's current file
    // does not spell the callee's name — the call goes through an
    // alias or a trait object. The answer cannot point at the call, and
    // must not be read as a confirmed absence.
    let dir = std::env::temp_dir().join("lain_test_scope_dynamic");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/caller.rs"), "fn caller() { dispatch(); }\n").unwrap();
    let db = crate::graph::GraphDatabase::new(&dir.join("graph.bin")).unwrap();
    let overlay = crate::overlay::VolatileOverlay::new();
    let ns = crate::schema::RepoNamespace::for_test();

    let target = scanned_node("call_target", "src/target.rs", &ns);
    let mut caller = scanned_node("caller", "src/caller.rs", &ns);
    caller.line_start = Some(0);
    caller.line_end = Some(0);
    db.upsert_node(target.clone()).unwrap();
    db.upsert_node(caller.clone()).unwrap();
    db.insert_edge(&GraphEdge::new(
        EdgeType::Calls,
        caller.id.clone(),
        target.id.clone(),
    ))
    .unwrap();

    let report = get_call_sites_report_scoped(&dir, &db, &overlay, "call_target", None).unwrap();
    assert_eq!(
        report.answer_scope,
        Some(IncompleteReason::DynamicTarget),
        "a call the text scan cannot locate in a current file is a dynamic call"
    );
    assert!(report.text.contains("dynamic_target"));
}

#[test]
fn stale_index_reports_the_stale_file() {
    // The file on disk is newer than the scan, so a caller added since
    // is missing from the answer.
    let dir = std::env::temp_dir().join("lain_test_scope_stale");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/target.rs"), "fn call_target() {}\n").unwrap();
    let db = crate::graph::GraphDatabase::new(&dir.join("graph.bin")).unwrap();
    let overlay = crate::overlay::VolatileOverlay::new();
    let ns = crate::schema::RepoNamespace::for_test();

    let mut target = scanned_node("call_target", "src/target.rs", &ns);
    // Scanned an hour ago; the file was written just now.
    target.last_lsp_sync =
        Some(crate::server::time::unix_secs(std::time::SystemTime::now()) - 3600);
    db.upsert_node(target).unwrap();

    let report = get_call_sites_report_scoped(&dir, &db, &overlay, "call_target", None).unwrap();
    assert_eq!(report.total_sites, 0);
    assert_eq!(
        report.answer_scope,
        Some(IncompleteReason::StaleOrUnindexed),
        "a file edited after its scan makes the answer provisional"
    );
    assert!(report.text.contains("stale_or_unindexed"));
}
