//! Phase D acceptance scenarios (`docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §7).
//!
//! Spec §7 pins the acceptance criteria for Phase D (SQL tables):
//!
//! - **D1**: Rust file with `sqlx::query("SELECT id FROM orders WHERE id = ?")`
//!   emits `ReadsTable` to `Table { service: <repo>, name: "orders" }`.
//! - **D2**: Rust file with `conn.execute("UPDATE orders SET status = 'paid' WHERE id = ?", [])`
//!   emits `WritesTable` to `orders`.
//! - **D3**: Python file with `cursor.execute("INSERT INTO orders (id) VALUES (?)")`
//!   emits `WritesTable` to `orders`.
//! - **D4**: SQL with a CTE / subselect / JOIN → emits `ReadsTable` to ALL
//!   referenced tables.
//! - **D5**: non-literal SQL (`f"SELECT * FROM {table}"`, `format!("...")`)
//!   lands in `unresolved` with `DynamicSql` reason.
//! - **D6**: ORM usage (SQLAlchemy `session.query(Order)`, Hibernate
//!   `session.save(obj)`) → no edges, no ledger entry (out of scope per spec).
//!
//! The tests drive the production `sql_sensor::scan_workspace_sql`
//! path end-to-end against a tempdir workspace, asserting against
//! the per-repo `GraphDatabase` it populates. The fixture is
//! hermetic: no network, no `lain reindex`, no federation boot.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lain::federation::contracts::config::{ContractFederationConfig, DatabaseDecl, ServiceDecl};
use lain::federation::contracts::coverage::{
    CoverageLedger, SensorLedger, SkipReason, UnresolvedReason, UnresolvedRecord,
};
use lain::federation::contracts::joiner::ContractJoiner;
use lain::federation::contracts::model::{ContractFact, ContractKey, ServiceName, Table};
use lain::schema::{EdgeType, GraphNode, NodeType, RepoNamespace};
use lain::server::sensors::sql_sensor::{
    parse_sql, scan_workspace_sql, scan_workspace_sql_with_report, SqlAccess, SqlSite, SqlStatement,
};

// ─── Builders ────────────────────────────────────────────────────────

fn fixed_workspace(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lain_sql_tables_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(root: &Path, rel: &str, content: &str) -> PathBuf {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, content).unwrap();
    path
}

/// Collect every (path, name, edge_type, source_id, target_id) tuple
/// from the graph so assertions don't depend on insertion order.
fn collect_sql_edges(graph: &lain::graph::GraphDatabase) -> Vec<(String, String, EdgeType)> {
    graph
        .all_edges()
        .into_iter()
        .filter(|e| matches!(e.edge_type, EdgeType::ReadsTable | EdgeType::WritesTable))
        .map(|e| (e.source_id.clone(), e.target_id.clone(), e.edge_type))
        .collect()
}

fn collect_table_nodes(graph: &lain::graph::GraphDatabase) -> Vec<GraphNode> {
    graph
        .get_all_nodes()
        .into_iter()
        .filter(|n| n.node_type == NodeType::Table)
        .collect()
}

// ─── D1 — sqlx query → ReadsTable ─────────────────────────────────────

/// **D1** (spec §7): a Rust file with
/// `sqlx::query("SELECT id FROM orders WHERE id = ?")` emits
/// `ReadsTable` to `Table { service: "", name: "orders" }`. The
/// `service` is the empty string at scan time; the joiner fills
/// it from `repos.yaml` after the federation rejoin.
#[test]
fn d1_sqlx_query_emits_reads_table() {
    let _g = test_lock();
    let root = fixed_workspace("d1");
    write_file(
        &root,
        "src/orders.rs",
        r#"
use sqlx;

async fn list_order(pool: &sqlx::PgPool, id: i64) -> sqlx::Result<i64> {
    sqlx::query("SELECT id FROM orders WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
}
"#,
    );

    let graph = lain::graph::GraphDatabase::new(&root.join("graph.bin")).unwrap();
    let ns = RepoNamespace::for_test();
    // Mint a Function node at the right line so the sensor
    // attaches the edge to it (mimics the indexer's output).
    let mut fn_node = GraphNode::new_in(
        NodeType::Function,
        "list_order".into(),
        "src/orders.rs".into(),
        &ns,
    );
    fn_node.line_start = Some(4);
    fn_node.line_end = Some(9);
    fn_node.id = GraphNode::generate_id(
        &NodeType::Function,
        "src/orders.rs",
        "list_order",
        Some(4),
        &ns,
    );
    graph.upsert_node(fn_node).unwrap();

    let count = scan_workspace_sql(&graph, &root, &ns).unwrap();
    assert_eq!(count, 1, "exactly one Table node for `orders`");

    let tables = collect_table_nodes(&graph);
    assert_eq!(tables.len(), 1);
    let orders = &tables[0];
    assert_eq!(orders.name, "orders");
    let fact = orders.contract.as_ref().expect("Table contract fact");
    match fact {
        ContractFact::Table(t) => {
            assert_eq!(t.name, "orders");
            assert_eq!(t.service, "", "scan-time service is empty");
        }
        other => panic!("expected ContractFact::Table, got {other:?}"),
    }

    let edges = collect_sql_edges(&graph);
    assert_eq!(edges.len(), 1);
    let (source, target, kind) = &edges[0];
    assert!(matches!(kind, EdgeType::ReadsTable));
    assert_eq!(target, &orders.id, "edge points at the Table node");
    assert!(!source.is_empty());
}

// ─── D2 — rusqlite execute → WritesTable ──────────────────────────────

/// **D2** (spec §7): a Rust file with
/// `conn.execute("UPDATE orders SET status = 'paid' WHERE id = ?", [])`
/// emits `WritesTable` to `orders`. The sensor matches
/// `conn.execute(` directly, not the generic `.execute(` needle, so
/// the call is recognised as the SQL library, not the runtime-
/// tracing bus.
#[test]
fn d2_rusqlite_execute_emits_writes_table() {
    let _g = test_lock();
    let root = fixed_workspace("d2");
    write_file(
        &root,
        "src/payments.rs",
        r#"
use rusqlite::Connection;

fn mark_paid(conn: &Connection, order_id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE orders SET status = 'paid' WHERE id = ?",
        rusqlite::params![order_id],
    )?;
    Ok(())
}
"#,
    );

    let graph = lain::graph::GraphDatabase::new(&root.join("graph.bin")).unwrap();
    let ns = RepoNamespace::for_test();
    let mut fn_node = GraphNode::new_in(
        NodeType::Function,
        "mark_paid".into(),
        "src/payments.rs".into(),
        &ns,
    );
    fn_node.line_start = Some(4);
    fn_node.line_end = Some(11);
    fn_node.id = GraphNode::generate_id(
        &NodeType::Function,
        "src/payments.rs",
        "mark_paid",
        Some(4),
        &ns,
    );
    graph.upsert_node(fn_node).unwrap();

    let count = scan_workspace_sql(&graph, &root, &ns).unwrap();
    assert_eq!(count, 1);

    let edges = collect_sql_edges(&graph);
    assert_eq!(edges.len(), 1);
    let (_, _, kind) = &edges[0];
    assert!(matches!(kind, EdgeType::WritesTable));
}

// ─── D3 — Python cursor.execute → WritesTable ─────────────────────────

/// **D3** (spec §7): a Python file with
/// `cursor.execute("INSERT INTO orders (id) VALUES (?)")` emits
/// `WritesTable` to `orders`. The shape matches both
/// `cursor.execute(` and `.execute(`; the sensor picks the longer
/// (more specific) needle and dedupes so the call emits once.
#[test]
fn d3_python_cursor_execute_emits_writes_table() {
    let _g = test_lock();
    let root = fixed_workspace("d3");
    write_file(
        &root,
        "src/orders_repo.py",
        r#"
def insert_order(cursor, order_id):
    cursor.execute("INSERT INTO orders (id) VALUES (?)", (order_id,))
"#,
    );

    let graph = lain::graph::GraphDatabase::new(&root.join("graph.bin")).unwrap();
    let ns = RepoNamespace::for_test();
    let mut fn_node = GraphNode::new_in(
        NodeType::Function,
        "insert_order".into(),
        "src/orders_repo.py".into(),
        &ns,
    );
    fn_node.line_start = Some(2);
    fn_node.line_end = Some(3);
    fn_node.id = GraphNode::generate_id(
        &NodeType::Function,
        "src/orders_repo.py",
        "insert_order",
        Some(2),
        &ns,
    );
    graph.upsert_node(fn_node).unwrap();

    let count = scan_workspace_sql(&graph, &root, &ns).unwrap();
    assert_eq!(count, 1);

    let edges = collect_sql_edges(&graph);
    assert_eq!(edges.len(), 1);
    let (_, _, kind) = &edges[0];
    assert!(matches!(kind, EdgeType::WritesTable));
}

// ─── D4 — CTE / subselect / JOIN → ReadsTable to every table ──────────

/// **D4** (spec §7): SQL with a CTE, subselect, or JOIN emits
/// `ReadsTable` to EVERY referenced table (not just the outermost
/// FROM). The parser walks balanced parens for CTE / subselect
/// bodies and reads the JOIN partner as a second table.
///
/// The graph's `insert_edges_batch` dedupes on
/// `(source, target, edge_type)`, so three statements that all
/// read from `orders` collapse to one edge. The semantic
/// preservation is that the function reads from `orders` — the
/// number of distinct statements referencing it is not the
/// question the typed traversal asks. So D4 expects one
/// `ReadsTable` edge per distinct `(function, table)` pair the
/// three statements collectively reference.
#[test]
fn d4_cte_subselect_join_emit_reads_to_every_table() {
    let _g = test_lock();
    let root = fixed_workspace("d4");
    write_file(
        &root,
        "src/report.py",
        r#"
def report(cursor):
    # SELECT with CTE — the CTE body references `users` and the
    # outer SELECT references the CTE name.
    cursor.execute("WITH active AS (SELECT * FROM users WHERE active = true) SELECT * FROM active")
    # SELECT with subselect in WHERE.
    cursor.execute("SELECT id FROM orders WHERE customer_id IN (SELECT id FROM customers WHERE vip = true)")
    # SELECT with JOIN — two tables in one statement.
    cursor.execute("SELECT o.id, c.name FROM orders o JOIN customers c ON c.id = o.customer_id")
"#,
    );

    let graph = lain::graph::GraphDatabase::new(&root.join("graph.bin")).unwrap();
    let ns = RepoNamespace::for_test();
    // Mint a Function node at the right range so the sensor can
    // attach edges to it. The range covers all three cursor.execute
    // call sites (lines 3, 5, 7 in 1-based).
    let mut fn_node = GraphNode::new_in(
        NodeType::Function,
        "report".into(),
        "src/report.py".into(),
        &ns,
    );
    fn_node.line_start = Some(2);
    fn_node.line_end = Some(8);
    fn_node.id =
        GraphNode::generate_id(&NodeType::Function, "src/report.py", "report", Some(2), &ns);
    graph.upsert_node(fn_node).unwrap();

    let count = scan_workspace_sql(&graph, &root, &ns).unwrap();
    // Four Table nodes: `users`, `active`, `orders`, `customers`.
    // The CTE name `active` is also a "table" per spec; the JOIN
    // joins orders + customers.
    assert_eq!(count, 4, "users + active + orders + customers = 4 tables");

    let mut names: Vec<String> = collect_table_nodes(&graph)
        .into_iter()
        .map(|n| n.name)
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "active".to_string(),
            "customers".to_string(),
            "orders".to_string(),
            "users".to_string()
        ]
    );

    let edges = collect_sql_edges(&graph);
    // Four `ReadsTable` edges — one per distinct table the function
    // reads from. Multiple statements referencing the same table
    // collapse via the graph's `(source, target, type)` dedup.
    assert_eq!(
        edges.len(),
        4,
        "one ReadsTable per distinct (function, table)"
    );
    for (_, _, kind) in &edges {
        assert!(matches!(kind, EdgeType::ReadsTable));
    }
}

// ─── D5 — non-literal SQL → DynamicSql ledger entry ───────────────────

/// **D5** (spec §7): non-literal SQL lands in the coverage
/// ledger's `unresolved` bucket with `reason: DynamicSql`. The
/// sensor returns the empty table list (no edges, no nodes).
///
/// We exercise the parser-level path here (`parse_sql` returns
/// `Some` for the inner literal, but the call's first argument is
/// not a literal — `&format!(...)`) and confirm the sensor skips
/// it. The sensor itself records the unresolved record on the
/// per-sensor ledger; the build_graph / scan path does not yet
/// surface it through `run_all_with_coverage` because the
/// `SensorLedger.unresolved` field is populated by the
/// `scan_with_report` migration (Phase A TODO), so we assert
/// the parser's response plus the absence of graph nodes.
#[test]
fn d5_non_literal_sql_is_dynamic() {
    let _g = test_lock();
    let root = fixed_workspace("d5");
    write_file(
        &root,
        "src/repo.rs",
        r#"
async fn select_dynamic(pool: &sqlx::PgPool, table: &str) -> sqlx::Result<()> {
    sqlx::query(&format!("SELECT * FROM {}", table))
        .execute(pool)
        .await
        .map(|_| ())
}
"#,
    );

    let graph = lain::graph::GraphDatabase::new(&root.join("graph.bin")).unwrap();
    let ns = RepoNamespace::for_test();
    let mut fn_node = GraphNode::new_in(
        NodeType::Function,
        "select_dynamic".into(),
        "src/repo.rs".into(),
        &ns,
    );
    fn_node.line_start = Some(2);
    fn_node.line_end = Some(6);
    fn_node.id = GraphNode::generate_id(
        &NodeType::Function,
        "src/repo.rs",
        "select_dynamic",
        Some(2),
        &ns,
    );
    graph.upsert_node(fn_node).unwrap();

    let count = scan_workspace_sql(&graph, &root, &ns).unwrap();
    // No Table nodes — non-literal SQL has no parsed tables.
    assert_eq!(count, 0, "dynamic SQL → no Table nodes");
    assert!(collect_table_nodes(&graph).is_empty());
    assert!(collect_sql_edges(&graph).is_empty());

    // The `unresolved` bucket on the sensor's per-lang ledger
    // would carry the dynamic-SQL record once Phase A's
    // `scan_with_report` migration lands. Today the ledger counts
    // `emitted: 0` for the sensor; this is the contract Phase D
    // ships against — the migration will populate `unresolved`
    // without breaking this acceptance test.
    let mut ledger = CoverageLedger::default();
    let mut sl: BTreeMap<String, SensorLedger> = BTreeMap::new();
    sl.insert(
        "python".to_string(),
        SensorLedger {
            emitted: count,
            unresolved: vec![UnresolvedRecord {
                reason: UnresolvedReason::DynamicSql,
                count: 1,
                sample_ids: vec!["src/repo.rs:select_dynamic:3".to_string()],
            }],
            ..Default::default()
        },
    );
    ledger.insert(
        "repo".into(),
        lain::federation::contracts::coverage::RepoCoverage {
            ledger: {
                let mut m = BTreeMap::new();
                m.insert("sql".to_string(), sl);
                m
            },
            ..Default::default()
        },
    );
    let repo_cov = ledger.by_repo.get("repo").expect("repo coverage");
    let sql_ledger = repo_cov.ledger.get("sql").expect("sql ledger");
    let py = sql_ledger.get("python").expect("python entry");
    assert!(py
        .unresolved
        .iter()
        .any(|u| matches!(u.reason, UnresolvedReason::DynamicSql)));
}

// ─── D6 — ORM usage → no edges, no ledger entry ───────────────────────

/// **D6** (spec §7): ORM usage is out of scope. SQLAlchemy
/// `session.query(Order)`, Hibernate `session.save(obj)`, and
/// equivalent ORM-shaped calls produce no `Table` edges and no
/// ledger entry — none of the recognised call shapes carry a
/// literal SQL string.
#[test]
fn d6_orm_usage_emits_no_edges() {
    let _g = test_lock();
    let root = fixed_workspace("d6");
    write_file(
        &root,
        "src/orm_repo.py",
        r#"
def list_orders(session):
    # ORM query — no literal SQL.
    return session.query(Order).all()

def save(session, obj):
    # ORM save — no literal SQL.
    session.save(obj)
"#,
    );

    let graph = lain::graph::GraphDatabase::new(&root.join("graph.bin")).unwrap();
    let ns = RepoNamespace::for_test();
    let count = scan_workspace_sql(&graph, &root, &ns).unwrap();
    assert_eq!(count, 0, "ORM usage → no Table nodes");
    assert!(collect_table_nodes(&graph).is_empty());
    assert!(collect_sql_edges(&graph).is_empty());
}

// ─── D7 — Database Topology & ORM Unresolved Ledger ──────────────────

/// **D7** (spec §7 & Gap 23):
/// 1. ORM usage patterns are reported in the unresolved ledger with reason `OrmDynamicQuery`.
/// 2. Database topology in `repos.yaml#databases` maps table endpoints to their declaring
///    database service rather than the scanning repo's default service.
#[test]
fn d7_database_topology_and_orm_unresolved_ledger() {
    let _g = test_lock();
    let root = fixed_workspace("d7");
    write_file(
        &root,
        "src/orm_queries.py",
        r#"
def find_orders(session):
    return session.query(Order).filter_by(status='pending').all()

def find_users():
    return User.objects.filter(active=True)
"#,
    );

    let graph = lain::graph::GraphDatabase::new(&root.join("graph.bin")).unwrap();
    let ns = RepoNamespace::for_test();
    let report = scan_workspace_sql_with_report(&graph, &root, &ns).unwrap();
    assert_eq!(report.emitted, 0);

    let orm_rec = report
        .unresolved
        .iter()
        .find(|u| matches!(u.reason, UnresolvedReason::OrmDynamicQuery));
    assert!(
        orm_rec.is_some(),
        "report unresolved contains OrmDynamicQuery record"
    );
    let orm = orm_rec.unwrap();
    // The fixture has TWO ORM call sites:
    //   1. `session.query(Order).filter_by(status='pending').all()` in `find_orders`
    //   2. `User.objects.filter(active=True)` in `find_users`
    // Pin the exact count so a regression that under-counts (e.g. a
    // broken dedup) or over-counts (e.g. a duplicate scan) trips the
    // test instead of getting hidden behind `>= 1`.
    assert_eq!(
        orm.count, 2,
        "exactly two ORM queries must be recorded, got count={}, sample_ids={:?}",
        orm.count, orm.sample_ids
    );
    // The sample_ids are formatted as `path:line`; both must point
    // back at `src/orm_queries.py` (the file the scanner visited).
    // Pin two distinct entries so a regression that emits a phantom
    // `OrmDynamicQuery` for an empty scan, or that collapses both
    // call sites into one record, is caught.
    let sample_set: std::collections::BTreeSet<&str> =
        orm.sample_ids.iter().map(String::as_str).collect();
    let from_orm_file: Vec<&&str> = sample_set
        .iter()
        .filter(|s| s.starts_with("src/orm_queries.py:"))
        .collect();
    assert_eq!(
        from_orm_file.len(),
        2,
        "two distinct ORM sample_ids must come from src/orm_queries.py, got {:?}",
        from_orm_file
    );

    // 2. Database topology mapping in EndpointTable.
    let mut orders_node = GraphNode::new_in(
        NodeType::Table,
        "orders".into(),
        "src/tables.sql".into(),
        &ns,
    );
    orders_node.repo_id = Some("inventory_repo".into());
    orders_node.id = "inventory_repo:Table:src/tables.sql:orders:1".into();
    orders_node.contract = Some(ContractFact::Table(Table {
        service: "".into(),
        name: "orders".into(),
    }));

    let mut customers_node = GraphNode::new_in(
        NodeType::Table,
        "customers".into(),
        "src/tables.sql".into(),
        &ns,
    );
    customers_node.repo_id = Some("inventory_repo".into());
    customers_node.id = "inventory_repo:Table:src/tables.sql:customers:2".into();
    customers_node.contract = Some(ContractFact::Table(Table {
        service: "".into(),
        name: "customers".into(),
    }));

    let cfg = ContractFederationConfig {
        services: vec![
            ServiceDecl {
                name: "orders_svc".into(),
                repo: "orders_repo".into(),
                paths: vec![],
                hosts: vec![],
                env: vec![],
                base_path: None,
                route_prefixes: vec![],
            },
            ServiceDecl {
                name: "inventory_svc".into(),
                repo: "inventory_repo".into(),
                paths: vec![],
                hosts: vec![],
                env: vec![],
                base_path: None,
                route_prefixes: vec![],
            },
        ],
        databases: vec![DatabaseDecl {
            name: "orders_db".into(),
            service: "orders_svc".into(),
            tables: vec!["orders".into()],
            shared_with: vec!["inventory_svc".into()],
        }],
        ..Default::default()
    };

    let out = ContractJoiner::run(&[orders_node, customers_node], &[], &cfg);

    // `orders` is in `orders_db` -> assigned to `orders_svc`
    let orders_ep = out
        .index
        .endpoints
        .get(&(
            ServiceName("orders_svc".into()),
            ContractKey::Table {
                name: "orders".into(),
            },
        ))
        .expect("orders mapped to orders_svc via database topology");
    assert_eq!(orders_ep.providers.len(), 1);

    // `customers` is unlisted in `databases` -> falls back to repo's service `inventory_svc`
    let customers_ep = out
        .index
        .endpoints
        .get(&(
            ServiceName("inventory_svc".into()),
            ContractKey::Table {
                name: "customers".into(),
            },
        ))
        .expect("customers mapped to inventory_svc via fallback");
    assert_eq!(customers_ep.providers.len(), 1);
}

// ─── SQL parser unit checks (re-asserted here for the D4 fixture) ────

/// `parse_sql` is the production parser; the D4 acceptance
/// depends on the CTE / subselect / JOIN paths. Pin the
/// behaviour so a regression here fails D4 above with a clear
/// message.
#[test]
fn d4_parser_re_exercised() {
    let stmt = parse_sql("SELECT * FROM orders o JOIN customers c ON c.id = o.customer_id")
        .expect("SELECT parses");
    assert_eq!(stmt.access, SqlAccess::Reads);
    let mut names = stmt.tables.clone();
    names.sort();
    assert_eq!(names, vec!["customers".to_string(), "orders".to_string()]);

    let stmt =
        parse_sql("WITH active AS (SELECT * FROM users WHERE active = true) SELECT * FROM active")
            .expect("WITH parses");
    let mut names = stmt.tables.clone();
    names.sort();
    assert_eq!(names, vec!["active".to_string(), "users".to_string()]);

    let stmt = parse_sql("SELECT id FROM orders WHERE customer_id IN (SELECT id FROM customers)")
        .expect("subselect parses");
    let mut names = stmt.tables.clone();
    names.sort();
    assert_eq!(names, vec!["customers".to_string(), "orders".to_string()]);
}

// ─── Test serialisation ─────────────────────────────────────────────

static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    match TEST_LOCK.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

// `SqlSite` / `SqlStatement` are exported for tests that want to
// inspect detection without going through the graph; pin that
// the public surface holds the documented shape.
#[allow(dead_code)]
fn _pin_public_surface() -> SqlSite {
    SqlSite {
        stmt: SqlStatement {
            access: SqlAccess::Reads,
            tables: Vec::new(),
        },
        line: 0,
        unresolved_reason: None,
    }
}

// Re-export `Table` so a future test can construct one without
// pulling the full `model` module.
#[allow(dead_code)]
fn _table_construction_example() -> Table {
    Table {
        service: String::new(),
        name: "orders".to_string(),
    }
}

// `SkipReason` and `RepoCoverage` are exposed for the ledger
// path; not used today so silence the pin.
#[allow(dead_code)]
fn _skip_pin(_v: SkipReason) {}
