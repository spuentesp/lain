//! Regression tests for the data-driven `entry_point_sensor` walker.
//!
//! Task 4 of the data-driven sensor-patterns plan migrated
//! `entry_point_sensor.rs`'s Spring, ASP.NET, and Rails detectors
//! from per-language inline tables to tree-sitter queries loaded via
//! [`Patterns::compiled_queries`]. The walker must:
//!
//!   1. Identify `@GetMapping("/foo") public void foo()` as a Spring
//!      `HttpHandler` entry point by parsing the file with
//!      `parse_for_lang(Lang::Java)` and matching against the
//!      `java/spring-entry-point.scm` query body.
//!   2. Identify `[HttpGet("/x")] public IActionResult Get(...)` as a
//!      C# ASP.NET `HttpHandler` entry point via
//!      `csharp/aspnet-entry-point.scm`.
//!   3. Identify action methods (`def index`, `def show`, …) inside a
//!      Rails `*Controller` class via `ruby/rails-entry-point.scm`.
//!   4. Honour every existing per-sensor test (Task 4 keeps all of
//!      them green).
//!
//! The hermetic `pr13_hermetic_precision_recall_over_t1_fixture` is
//! the contract this refactor must not break. The per-sensor
//! regression here is the local guard.

use lain::federation::contracts::model::EntryKind;
use lain::graph::GraphDatabase;
use lain::schema::{GraphNode, NodeType, RepoNamespace};
use lain::server::sensors::entry_point_sensor::scan_workspace_entry_points;

fn temp_graph(tag: &str) -> (tempfile::TempDir, GraphDatabase) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db = tmp.path().join(format!("db_{tag}.bin"));
    let g = GraphDatabase::new(&db).expect("GraphDatabase::new");
    (tmp, g)
}

fn write_src(tmp: &tempfile::TempDir, rel_path: &str, content: &str) {
    let p = tmp.path().join(rel_path);
    std::fs::create_dir_all(p.parent().unwrap()).ok();
    std::fs::write(&p, content).expect("write fixture");
}

fn make_function(name: &str, path: &str, line: u32) -> GraphNode {
    let ns = RepoNamespace::for_test();
    let mut n = GraphNode::new_in(NodeType::Function, name.into(), path.into(), &ns);
    n.line_start = Some(line);
    n.line_end = Some(line + 5);
    n.id = GraphNode::generate_id(&NodeType::Function, path, name, Some(line), &ns);
    n
}

fn make_method(name: &str, path: &str, line: u32) -> GraphNode {
    let ns = RepoNamespace::for_test();
    let mut n = GraphNode::new_in(NodeType::Method, name.into(), path.into(), &ns);
    n.line_start = Some(line);
    n.line_end = Some(line + 5);
    n.id = GraphNode::generate_id(&NodeType::Method, path, name, Some(line), &ns);
    n
}

fn run_scan(g: &GraphDatabase, tmp: &tempfile::TempDir) -> usize {
    scan_workspace_entry_points(g, tmp.path(), &RepoNamespace::for_test()).unwrap_or(0)
}

fn entries_named(g: &GraphDatabase, name: &str) -> Vec<GraphNode> {
    g.get_all_nodes()
        .into_iter()
        .filter(|n| n.name == name)
        .collect()
}

// ─── Step 4.1 — TDD anchor ───────────────────────────────────────

/// TDD red → green: the detector must identify
/// `@GetMapping("/foo") public void foo()` as a Spring `HttpHandler`
/// entry point by matching against the `java/spring-entry-point.scm`
/// query body (vs. the prior regex table). The route is implied by
/// the annotation (`http:GET /foo`) — the function gets the
/// `HttpHandler` kind and the http_sensor sees the same annotation
/// for the matching `HttpRoute`.
#[test]
fn java_spring_annotation_detected() {
    let (tmp, g) = temp_graph("java_spring_annotation");
    write_src(
        &tmp,
        "src/main/java/com/example/FooController.java",
        "@RestController\npublic class FooController {\n    @GetMapping(\"/foo\")\n    public void foo() {\n    }\n}\n",
    );
    let f = make_function("foo", "src/main/java/com/example/FooController.java", 4);
    g.insert_nodes_batch(&[f]).unwrap();
    let _ = run_scan(&g, &tmp);
    let entries = entries_named(&g, "foo");
    assert!(
        entries
            .iter()
            .any(|n| n.entry == Some(EntryKind::HttpHandler)),
        "foo must be tagged HttpHandler (route: http:GET /foo); nodes: {entries:?}"
    );
}

// ─── Local guards for the new entry-point queries ────────────────

/// Every entry-point framework the YAML / .scm registry advertises
/// for Spring, ASP.NET, and Ruby must surface a non-empty `.scm`
/// body. Mirrors the `every_outbound_framework_has_a_compiled_scm_body`
/// guard from `http_client_sensor_patterns.rs`.
#[test]
fn every_entry_point_framework_has_a_compiled_scm_body() {
    let keys = [
        "java/spring-entry-point.scm",
        "csharp/aspnet-entry-point.scm",
        "ruby/rails-entry-point.scm",
    ];
    let compiled = lain::server::sensors::patterns::Patterns::patterns().compiled_queries();
    for key in keys {
        let body = compiled
            .iter()
            .find(|(k, _, _, _)| *k == key)
            .map(|(_, _, _, body)| *body)
            .unwrap_or_else(|| panic!("{key} must be in compiled_queries()"));
        let has_query = body.lines().any(|line| {
            let trimmed = line.trim_start();
            !trimmed.is_empty() && !trimmed.starts_with(';')
        });
        assert!(
            has_query,
            "{key} is comment-only — the walker would skip it and silently drop the entry-point detection"
        );
    }
}

/// ASP.NET regression: `[HttpGet("/api/users/{id}")] public
/// IActionResult Get(int id) { ... }` resolves the method `Get` to
/// `HttpHandler` via `csharp/aspnet-entry-point.scm`.
#[test]
fn csharp_aspnet_attribute_detected() {
    let (tmp, g) = temp_graph("csharp_aspnet_attribute");
    write_src(
        &tmp,
        "src/Controllers/FooController.cs",
        "[ApiController]\npublic class FooController : Controller {\n    [HttpGet(\"/api/users/{id}\")]\n    public IActionResult Get(int id) { return null; }\n}\n",
    );
    let f = make_method("Get", "src/Controllers/FooController.cs", 4);
    g.insert_nodes_batch(&[f]).unwrap();
    let _ = run_scan(&g, &tmp);
    let entries = entries_named(&g, "Get");
    assert!(
        entries
            .iter()
            .any(|n| n.entry == Some(EntryKind::HttpHandler)),
        "Get must be tagged HttpHandler (route: http:GET /api/users/{{id}}); nodes: {entries:?}"
    );
}

/// Rails regression: every action method inside a `*Controller`
/// class becomes `HttpHandler`.
#[test]
fn ruby_rails_controller_actions_detected() {
    let (tmp, g) = temp_graph("ruby_rails_actions");
    write_src(
        &tmp,
        "app/controllers/users_controller.rb",
        "class UsersController < ApplicationController\n  def index\n    @users = User.all\n  end\n  def show\n    @user = User.find(params[:id])\n  end\nend\n",
    );
    let idx = make_function("index", "app/controllers/users_controller.rb", 2);
    let show = make_function("show", "app/controllers/users_controller.rb", 5);
    g.insert_nodes_batch(&[idx, show]).unwrap();
    let _ = run_scan(&g, &tmp);
    for name in ["index", "show"] {
        let entries = entries_named(&g, name);
        assert!(
            entries
                .iter()
                .any(|n| n.entry == Some(EntryKind::HttpHandler)),
            "{name} must be tagged HttpHandler; nodes: {entries:?}"
        );
    }
}
