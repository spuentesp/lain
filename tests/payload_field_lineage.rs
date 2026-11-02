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
