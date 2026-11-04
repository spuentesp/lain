use lain::federation::contracts::model::{
    ContractFact, FieldReadFact, FieldReadOrigin, JsonPath, SourceSite, UnresolvedReason,
};
use lain::graph::GraphDatabase;
use lain::schema::{EdgeProvenance, EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace};
use lain::server::sensors::frontend_value_sensor::{
    scan_workspace_frontend_values, scan_workspace_frontend_values_with_report,
};
use lain::server::sensors::ScanReport;
use std::str::FromStr;

const PATH: &str = "src/Orders.tsx";
const SOURCE: &str = r#"export function Orders({ data }) {
  const rendered = data.customer.name;
  const stored = data.customer.id;
  const forwarded = data.total;
  const discarded = data.internal;
  localStorage.setItem("order", stored);
  save(forwarded);
  return <section><span>{rendered}</span><Child value={forwarded} /></section>;
  const dynamic = data[key];
}
"#;

fn add_field(
    graph: &GraphDatabase,
    component: &GraphNode,
    chain: &str,
    line: u32,
    exact: bool,
) -> GraphNode {
    let namespace = RepoNamespace::for_test();
    let mut field = GraphNode::new_in(NodeType::FieldRef, chain.into(), PATH.into(), &namespace);
    field.id = GraphNode::generate_id(&NodeType::FieldRef, PATH, chain, Some(line), &namespace);
    field.line_start = Some(line);
    field.contract = vec![ContractFact::FieldRead(FieldReadFact {
        chain: JsonPath::from_str(chain).unwrap(),
        exact,
        origin: FieldReadOrigin::GraphqlConsumer,
        operation: None,
    })];
    graph.insert_nodes_batch(&[field.clone()]).unwrap();
    let mut reads = GraphEdge::new(EdgeType::ReadsField, component.id.clone(), field.id.clone());
    reads.site = Some(SourceSite {
        path: PATH.into(),
        line,
    });
    graph.insert_edges_batch(&[reads]).unwrap();
    field
}

#[test]
fn classifies_frontend_value_dispositions_without_breaking_document_links() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join(PATH), SOURCE).unwrap();
    let graph = GraphDatabase::new(&temp.path().join("graph.bin")).unwrap();
    let namespace = RepoNamespace::for_test();
    let mut component =
        GraphNode::new_in(NodeType::Function, "Orders".into(), PATH.into(), &namespace);
    component.id = GraphNode::generate_id(&NodeType::Function, PATH, "Orders", Some(1), &namespace);
    component.line_start = Some(1);
    component.line_end = Some(10);
    let hook = GraphNode::new_in(
        NodeType::Function,
        "useOrdersQuery".into(),
        "src/generated.ts".into(),
        &namespace,
    );
    let consumer = GraphNode::new_in(
        NodeType::Function,
        "graphql-call:query:orders".into(),
        "src/orders.graphql".into(),
        &namespace,
    );
    graph
        .insert_nodes_batch(&[component.clone(), hook.clone(), consumer.clone()])
        .unwrap();
    graph
        .insert_edges_batch(&[
            GraphEdge::new(EdgeType::Calls, component.id.clone(), hook.id.clone()),
            GraphEdge::new(EdgeType::Calls, hook.id.clone(), consumer.id.clone()),
        ])
        .unwrap();

    let rendered = add_field(&graph, &component, "customer.name", 2, true);
    let stored = add_field(&graph, &component, "customer.id", 3, true);
    let forwarded = add_field(&graph, &component, "total", 4, true);
    let discarded = add_field(&graph, &component, "internal", 5, true);
    let dynamic = add_field(&graph, &component, "dynamic", 9, false);

    for _ in 0..2 {
        assert_eq!(
            scan_workspace_frontend_values(&graph, temp.path(), &namespace).unwrap(),
            4
        );
    }

    let edges = graph.all_edges();
    let disposition = |edge_type, field: &GraphNode| {
        edges.iter().any(|edge| {
            edge.edge_type == edge_type
                && edge.source_id == component.id
                && edge.target_id == field.id
                && edge.site.is_some()
        })
    };
    assert!(disposition(EdgeType::Renders, &rendered));
    assert!(disposition(EdgeType::Stores, &stored));
    assert!(disposition(EdgeType::Forwards, &forwarded));
    assert!(disposition(EdgeType::Discards, &discarded));
    assert_eq!(
        edges
            .iter()
            .filter(|edge| {
                matches!(
                    edge.edge_type,
                    EdgeType::Renders | EdgeType::Stores | EdgeType::Forwards | EdgeType::Discards
                )
            })
            .count(),
        4,
        "a rescan replaces rather than duplicates dispositions: {edges:#?}"
    );
    assert!(!edges.iter().any(|edge| {
        edge.target_id == dynamic.id
            && matches!(
                edge.edge_type,
                EdgeType::Renders | EdgeType::Stores | EdgeType::Forwards | EdgeType::Discards
            )
    }));
    assert!(edges.iter().any(|edge| {
        edge.edge_type == EdgeType::Calls
            && edge.source_id == component.id
            && edge.target_id == hook.id
    }));
    assert!(edges.iter().any(|edge| {
        edge.edge_type == EdgeType::Calls
            && edge.source_id == hook.id
            && edge.target_id == consumer.id
    }));
    assert!(edges.iter().any(|edge| {
        edge.edge_type == EdgeType::Discards
            && matches!(
                edge.provenance,
                Some(EdgeProvenance::Heuristic {
                    ref detector,
                    confidence: 0.8
                }) if detector == "frontend_unused_value"
            )
    }));
}

#[test]
fn dispositions_resolve_shadowing_sibling_scopes_and_declaration_order() {
    let source = r#"export function Orders({ data }) {
  const outer = data.outer;
  {
    const outer = data.inner;
    return <span>{outer}</span>;
  }
  const sibling = data.sibling;
  {
    const hidden = data.hidden;
  }
  return <span>{later}</span>;
  const later = data.later;
}
"#;
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join(PATH), source).unwrap();
    let graph = GraphDatabase::new(&temp.path().join("graph.bin")).unwrap();
    let namespace = RepoNamespace::for_test();
    let mut component =
        GraphNode::new_in(NodeType::Function, "Orders".into(), PATH.into(), &namespace);
    component.id = GraphNode::generate_id(&NodeType::Function, PATH, "Orders", Some(1), &namespace);
    component.line_start = Some(1);
    component.line_end = Some(13);
    graph.insert_nodes_batch(&[component.clone()]).unwrap();
    let outer = add_field(&graph, &component, "outer", 2, true);
    let inner = add_field(&graph, &component, "inner", 4, true);
    let sibling = add_field(&graph, &component, "sibling", 7, true);
    let hidden = add_field(&graph, &component, "hidden", 9, true);
    let later = add_field(&graph, &component, "later", 12, true);

    scan_workspace_frontend_values(&graph, temp.path(), &namespace).unwrap();
    let edges = graph.all_edges();
    let has = |kind, field: &GraphNode| {
        edges.iter().any(|edge| {
            edge.edge_type == kind && edge.source_id == component.id && edge.target_id == field.id
        })
    };
    assert!(has(EdgeType::Renders, &inner), "{edges:#?}");
    assert!(!has(EdgeType::Renders, &outer), "{edges:#?}");
    for field in [&outer, &sibling, &hidden, &later] {
        assert!(
            has(EdgeType::Discards, field),
            "missing discard for {field:#?}"
        );
    }
}

struct FrontendFixture {
    _temp: tempfile::TempDir,
    graph: GraphDatabase,
    component: GraphNode,
    fields: std::collections::BTreeMap<String, GraphNode>,
}

impl FrontendFixture {
    fn new(source: &str) -> Self {
        let (_temp, graph, component, fields) = build_exact_frontend_graph(source);
        Self {
            _temp,
            graph,
            component,
            fields,
        }
    }

    fn scan(&self) -> ScanReport {
        scan_workspace_frontend_values_with_report(
            &self.graph,
            self._temp.path(),
            &RepoNamespace::for_test(),
        )
        .unwrap()
    }

    fn has(&self, edge_type: EdgeType, field: &str) -> bool {
        let field = &self.fields[field];
        self.graph.all_edges().iter().any(|edge| {
            edge.edge_type == edge_type
                && edge.source_id == self.component.id
                && edge.target_id == field.id
        })
    }
}

fn build_exact_frontend_graph(
    source: &str,
) -> (
    tempfile::TempDir,
    GraphDatabase,
    GraphNode,
    std::collections::BTreeMap<String, GraphNode>,
) {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join(PATH), source).unwrap();
    let graph = GraphDatabase::new(&temp.path().join("graph.bin")).unwrap();
    let namespace = RepoNamespace::for_test();
    let mut component =
        GraphNode::new_in(NodeType::Function, "Orders".into(), PATH.into(), &namespace);
    component.id = GraphNode::generate_id(&NodeType::Function, PATH, "Orders", Some(1), &namespace);
    component.line_start = Some(1);
    component.line_end = Some(20);
    graph.insert_nodes_batch(&[component.clone()]).unwrap();
    let fields = [("customer.id", 4), ("total", 5), ("internal", 6)]
        .into_iter()
        .map(|(name, line)| {
            (
                name.to_string(),
                add_field(&graph, &component, name, line, true),
            )
        })
        .collect();
    (temp, graph, component, fields)
}

#[test]
fn frontend_storage_uses_bindings_and_unclassified_use_is_not_discarded() {
    let source = r#"import { useState as useReactState } from "react";
export function Orders({ data }) {
  const [ignored, refresh] = useReactState(null);
  const stored = data.customer.id;
  const returned = data.total;
  const unused = data.internal;
  refresh(stored);
  setCustomer(returned);
  let assigned;
  assigned = returned;
  return returned;
}
"#;
    let fixture = FrontendFixture::new(source);
    let report = fixture.scan();
    assert!(fixture.has(EdgeType::Stores, "customer.id"));
    assert!(!fixture.has(EdgeType::Stores, "total"));
    assert!(fixture.has(EdgeType::Forwards, "total"));
    assert!(fixture.has(EdgeType::Discards, "internal"));
    assert!(!fixture.has(EdgeType::Discards, "total"));
    assert!(report.unresolved.iter().any(|record| {
        record.reason == UnresolvedReason::FrontendValueUnclassified
            && record
                .sample_ids
                .iter()
                .any(|site| site.contains("Orders.tsx"))
    }));
}

#[test]
fn react_storage_requires_real_imports_and_accepts_enclosing_scope_setters() {
    let source = r#"import { useState as useX } from "not-react";
import { useState } from "react";
export function Orders({ data }) {
  const stored = data.customer.id;
  const [ignored, refresh] = useState(null);
  const [other, wrong] = useX(null);
  {
    refresh(stored);
    wrong(stored);
  }
}
"#;
    let fixture = FrontendFixture::new(source);
    fixture.scan();
    assert!(fixture.has(EdgeType::Stores, "customer.id"));
    assert!(fixture.has(EdgeType::Forwards, "customer.id"));
}
