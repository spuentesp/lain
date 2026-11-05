use lain::federation::contracts::model::{
    ContractFact, ContractKey, FieldReadOrigin, GraphqlConsumerFact, GraphqlOp, SourceSite,
    TopicConsumerFact, TopicConsumerKind,
};
use lain::graph::GraphDatabase;
use lain::schema::{EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace};
use lain::server::sensors::field_access_sensor::detect_emissions;
use lain::server::sensors::util::Lang;
use std::collections::BTreeMap;

fn function_node(name: &str, path: &str, start: u32, end: u32) -> GraphNode {
    let namespace = RepoNamespace::for_test();
    let mut node = GraphNode::new_in(NodeType::Function, name.into(), path.into(), &namespace);
    node.id = GraphNode::generate_id(&NodeType::Function, path, name, Some(start), &namespace);
    node.line_start = Some(start);
    node.line_end = Some(end);
    node
}

#[test]
fn topic_and_graphql_reads_stay_scoped_to_their_operations() {
    let graph = GraphDatabase::new(&std::env::temp_dir().join("payload-lineage-graph")).unwrap();
    let orders = function_node("consume_orders", "consumer.py", 0, 3);
    let billing = function_node("consume_billing", "consumer.py", 4, 7);
    let graphql = function_node("render_orders", "consumer.py", 9, 12);
    for (node, broker, topic) in [(&orders, "kafka", "orders"), (&billing, "kafka", "billing")] {
        let mut consumer = node.clone();
        consumer.contract = vec![ContractFact::TopicConsumer(TopicConsumerFact {
            broker: broker.into(),
            name: topic.into(),
            kind: TopicConsumerKind::Subscription,
        })];
        let topic_node = GraphNode::new(NodeType::Topic, topic.into(), "events.yaml".into());
        graph.insert_nodes_batch(&[consumer, topic_node]).unwrap();
        graph
            .insert_edge(&GraphEdge {
                site: Some(SourceSite {
                    path: "consumer.py".into(),
                    line: node.line_start.unwrap() + 1,
                }),
                ..GraphEdge::new(
                    EdgeType::Consumes,
                    node.id.clone(),
                    graph.find_node_by_name(topic).unwrap().id,
                )
            })
            .unwrap();
    }
    let mut graphql_consumer = graphql.clone();
    graphql_consumer.contract = vec![ContractFact::GraphqlConsumer(GraphqlConsumerFact {
        op: GraphqlOp::Query,
        field: "orders".into(),
    })];
    graph.upsert_node(graphql_consumer).unwrap();

    let source = "def consume_orders(payload):\n    data = payload[\"customer\"][\"id\"]\n    return data\n\ndef consume_billing(payload):\n    data = payload[\"total\"]\n    return data\n\ndef render_orders(data):\n    selected = data[\"customer\"][\"id\"]\n    return selected\n";
    let emissions = detect_emissions(
        "consumer.py",
        source,
        Lang::Python,
        &graph,
        &BTreeMap::new(),
    );
    assert_eq!(emissions.len(), 3, "{emissions:#?}");
    let orders_read = emissions
        .iter()
        .find(|emission| emission.call_id == orders.id)
        .unwrap();
    assert_eq!(orders_read.origin, FieldReadOrigin::TopicConsumer);
    assert_eq!(
        orders_read.operation,
        Some(ContractKey::Topic {
            broker: "kafka".into(),
            name: "orders".into(),
        })
    );
    assert_eq!(orders_read.reads[0].chain.to_string(), "customer.id");
    let billing_read = emissions
        .iter()
        .find(|emission| emission.call_id == billing.id)
        .unwrap();
    assert_eq!(billing_read.reads[0].chain.to_string(), "total");
    let graphql_read = emissions
        .iter()
        .find(|emission| emission.call_id == graphql.id)
        .unwrap();
    assert_eq!(graphql_read.origin, FieldReadOrigin::GraphqlConsumer);
    assert_eq!(
        graphql_read.operation,
        Some(ContractKey::Graphql {
            op: GraphqlOp::Query,
            field: "orders".into(),
        })
    );
}

#[test]
fn dynamic_payload_keys_mark_lineage_incomplete_without_an_exact_path() {
    let graph = GraphDatabase::new(&std::env::temp_dir().join("payload-dynamic-graph")).unwrap();
    let orders = function_node("consume_orders", "consumer.py", 0, 3);
    let mut consumer = orders.clone();
    consumer.contract = vec![ContractFact::TopicConsumer(TopicConsumerFact {
        broker: "kafka".into(),
        name: "orders".into(),
        kind: TopicConsumerKind::Subscription,
    })];
    let topic_node = GraphNode::new(NodeType::Topic, "orders".into(), "events.yaml".into());
    graph
        .insert_nodes_batch(&[consumer, topic_node.clone()])
        .unwrap();
    graph
        .insert_edge(&GraphEdge {
            site: Some(SourceSite {
                path: "consumer.py".into(),
                line: 1,
            }),
            ..GraphEdge::new(EdgeType::Consumes, orders.id.clone(), topic_node.id)
        })
        .unwrap();
    let emissions = detect_emissions(
        "consumer.py",
        "def consume_orders(payload, key):\n    return payload[key]\n",
        Lang::Python,
        &graph,
        &BTreeMap::new(),
    );
    assert_eq!(emissions.len(), 1);
    assert!(!emissions[0].reads_complete);
    assert!(emissions[0].reads.is_empty());
}

#[test]
fn unrelated_locals_are_not_payload_roots_and_all_operations_are_retained() {
    let graph = GraphDatabase::new(&std::env::temp_dir().join("payload-roots-graph")).unwrap();
    let consumer = function_node("consume_orders", "consumer.py", 0, 4);
    let mut consumer = consumer;
    consumer.contract = vec![
        ContractFact::TopicConsumer(TopicConsumerFact {
            broker: "kafka".into(),
            name: "orders".into(),
            kind: TopicConsumerKind::Subscription,
        }),
        ContractFact::TopicConsumer(TopicConsumerFact {
            broker: "kafka".into(),
            name: "billing".into(),
            kind: TopicConsumerKind::Subscription,
        }),
    ];
    let orders = GraphNode::new(NodeType::Topic, "orders".into(), "events.yaml".into());
    let billing = GraphNode::new(NodeType::Topic, "billing".into(), "events.yaml".into());
    graph
        .insert_nodes_batch(&[consumer.clone(), orders.clone(), billing.clone()])
        .unwrap();
    graph
        .insert_edges_batch(&[
            GraphEdge {
                site: Some(SourceSite {
                    path: "consumer.py".into(),
                    line: 1,
                }),
                ..GraphEdge::new(EdgeType::Consumes, consumer.id.clone(), orders.id)
            },
            GraphEdge {
                site: Some(SourceSite {
                    path: "consumer.py".into(),
                    line: 1,
                }),
                ..GraphEdge::new(EdgeType::Consumes, consumer.id.clone(), billing.id)
            },
        ])
        .unwrap();

    let unrelated = detect_emissions(
        "consumer.py",
        "def consume_orders(payload):\n    data = {\"total\": 0}\n    return data[\"total\"]\n",
        Lang::Python,
        &graph,
        &BTreeMap::new(),
    );
    assert!(
        unrelated.iter().all(|emission| emission.reads.is_empty()),
        "{unrelated:#?}"
    );

    let emissions = detect_emissions(
        "consumer.py",
        "def consume_orders(payload):\n    return payload[\"total\"]\n",
        Lang::Python,
        &graph,
        &BTreeMap::new(),
    );
    assert_eq!(emissions.len(), 2, "{emissions:#?}");
    let operations: std::collections::BTreeSet<String> = emissions
        .iter()
        .map(|emission| emission.operation.clone().unwrap().to_string())
        .collect();
    assert_eq!(operations.len(), 2);
}

#[test]
fn workspace_scope_extension_preserves_topic_payload_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("consumer.py"),
        "def consume_orders(payload):\n    data = payload[\"customer\"]\n    return helper(data)\n\ndef helper(value):\n    return value\n",
    )
    .unwrap();
    let graph = GraphDatabase::new(&root.join("graph.bin")).unwrap();
    let namespace = RepoNamespace::for_test();
    let consumer = function_node("consume_orders", "consumer.py", 1, 3);
    let helper = function_node("helper", "consumer.py", 5, 6);
    let mut consumer_with_fact = consumer.clone();
    consumer_with_fact.contract = vec![ContractFact::TopicConsumer(TopicConsumerFact {
        broker: "kafka".into(),
        name: "orders".into(),
        kind: TopicConsumerKind::Subscription,
    })];
    let topic = GraphNode::new(NodeType::Topic, "orders".into(), "events.yaml".into());
    graph
        .insert_nodes_batch(&[consumer_with_fact, helper.clone(), topic.clone()])
        .unwrap();
    graph
        .insert_edges_batch(&[
            GraphEdge {
                site: Some(SourceSite {
                    path: "consumer.py".into(),
                    line: 1,
                }),
                ..GraphEdge::new(EdgeType::Consumes, consumer.id.clone(), topic.id)
            },
            GraphEdge::new(EdgeType::Calls, consumer.id.clone(), helper.id.clone()),
        ])
        .unwrap();

    lain::server::sensors::field_access_sensor::scan_workspace_field_access(
        &graph,
        root,
        &namespace,
        &lain::federation::repo_id::RepoId::new("payload-lineage").unwrap(),
    )
    .unwrap();

    let fields: Vec<_> = graph
        .get_all_nodes()
        .into_iter()
        .filter(|node| node.node_type == NodeType::FieldRef)
        .collect();
    assert_eq!(fields.len(), 1, "{fields:#?}");
    let field = &fields[0];
    assert_eq!(field.name, "customer");
    let [ContractFact::FieldRead(read)] = field.contract.as_slice() else {
        panic!("expected one field-read fact: {field:#?}");
    };
    assert_eq!(read.chain.to_string(), "customer");
    assert_eq!(read.origin, FieldReadOrigin::TopicConsumer);
    assert_eq!(
        read.operation,
        Some(ContractKey::Topic {
            broker: "kafka".into(),
            name: "orders".into(),
        })
    );
    let edges = graph.all_edges();
    assert!(edges.iter().any(|edge| {
        edge.edge_type == EdgeType::ReadsField
            && edge.source_id == consumer.id
            && edge.target_id == field.id
    }));
    assert!(edges.iter().any(|edge| {
        edge.edge_type == EdgeType::ReadsFrom
            && edge.source_id == field.id
            && edge.target_id == consumer.id
    }));
}

#[test]
fn workspace_report_records_dynamic_topic_and_graphql_payload_reads() {
    use lain::server::sensors::field_access_sensor::FieldAccessSensor;
    use lain::server::sensors::Sensor;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("consumer.py"),
        "def consume_orders(payload, key):\n    return payload[key]\n\ndef render_orders(payload, key):\n    return payload[key]\n",
    )
    .unwrap();
    let graph = GraphDatabase::new(&root.join("graph.bin")).unwrap();
    let namespace = RepoNamespace::for_test();
    let consumer = function_node("consume_orders", "consumer.py", 1, 2);
    let graphql = function_node("render_orders", "consumer.py", 4, 5);
    let mut consumer_with_fact = consumer.clone();
    consumer_with_fact.contract = vec![ContractFact::TopicConsumer(TopicConsumerFact {
        broker: "kafka".into(),
        name: "orders".into(),
        kind: TopicConsumerKind::Subscription,
    })];
    let mut graphql_with_fact = graphql.clone();
    graphql_with_fact.contract = vec![ContractFact::GraphqlConsumer(GraphqlConsumerFact {
        op: GraphqlOp::Query,
        field: "orders".into(),
    })];
    let topic = GraphNode::new(NodeType::Topic, "orders".into(), "events.yaml".into());
    graph
        .insert_nodes_batch(&[consumer_with_fact, graphql_with_fact, topic.clone()])
        .unwrap();
    graph
        .insert_edge(&GraphEdge {
            site: Some(SourceSite {
                path: "consumer.py".into(),
                line: 1,
            }),
            ..GraphEdge::new(EdgeType::Consumes, consumer.id.clone(), topic.id)
        })
        .unwrap();

    let report = FieldAccessSensor
        .scan_with_report(&graph, root, &namespace)
        .unwrap();

    assert_eq!(report.emitted, 0);
    assert_eq!(
        report
            .unresolved
            .iter()
            .map(|record| record.count)
            .sum::<usize>(),
        2,
        "{report:#?}"
    );
    let sample_ids: std::collections::BTreeSet<String> = report
        .unresolved
        .iter()
        .flat_map(|record| record.sample_ids.iter().cloned())
        .collect();
    assert_eq!(
        sample_ids,
        std::collections::BTreeSet::from([
            "graphql:query:orders".to_string(),
            "topic:kafka/orders".to_string(),
        ])
    );
    assert!(graph
        .get_all_nodes()
        .iter()
        .all(|node| node.node_type != NodeType::FieldRef));
}
