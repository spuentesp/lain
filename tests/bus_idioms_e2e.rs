//! End-to-end checks for the message-bus idioms declared in
//! `src/server/sensors/patterns/frameworks.yaml`. Each case writes one source
//! file, runs the event sensor over it, and asserts the `Topic` hub it mints:
//! `<broker>/<name>`, where the broker label comes from the pattern entry.

use lain::graph::GraphDatabase;
use lain::schema::{NodeType, RepoNamespace};
use lain::server::sensors::event_sensor::scan_workspace_event;
use std::path::{Path, PathBuf};

fn fixture(tag: &str, rel: &str, content: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lain_bus_{}_{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let file = dir.join(rel);
    std::fs::create_dir_all(file.parent().expect("parent")).unwrap();
    std::fs::write(&file, content).unwrap();
    dir
}

/// Topic names minted by the scan, sorted.
fn topics(root: &Path) -> Vec<String> {
    let graph = GraphDatabase::new(&root.join("graph.bin")).expect("open graph");
    scan_workspace_event(&graph, root, &RepoNamespace::for_test()).expect("event scan");
    let mut names: Vec<String> = graph
        .get_all_nodes()
        .into_iter()
        .filter(|n| n.node_type == NodeType::Topic)
        .map(|n| n.name)
        .collect();
    names.sort();
    names
}

fn expect_topic(tag: &str, rel: &str, content: &str, label: &str) {
    let root = fixture(tag, rel, content);
    let names = topics(&root);
    assert!(
        names.iter().any(|n| n == label),
        "expected topic {label:?} in {names:?}"
    );
}

// Python

#[test]
fn python_redis_publish_mints_a_redis_topic() {
    expect_topic(
        "py_redis_pub",
        "svc/events.py",
        "r.publish(\"orders\", msg)\n",
        "redis/orders",
    );
}

#[test]
fn python_redis_subscribe_mints_a_redis_topic() {
    expect_topic(
        "py_redis_sub",
        "svc/events.py",
        "pubsub.subscribe(\"orders\")\n",
        "redis/orders",
    );
}

#[test]
fn python_redis_stream_add_mints_a_redis_topic() {
    expect_topic(
        "py_redis_xadd",
        "svc/events.py",
        "r.xadd(\"orders\", {\"id\": 1})\n",
        "redis/orders",
    );
}

#[test]
fn python_mqtt_publish_mints_an_mqtt_topic() {
    expect_topic(
        "py_mqtt_pub",
        "svc/events.py",
        "client.publish(\"sensors/temp\", payload)\n",
        "mqtt/sensors/temp",
    );
}

#[test]
fn python_mqtt_subscribe_mints_an_mqtt_topic() {
    expect_topic(
        "py_mqtt_sub",
        "svc/events.py",
        "client.subscribe(\"sensors/temp\")\n",
        "mqtt/sensors/temp",
    );
}

#[test]
fn python_sqs_send_mints_an_sqs_topic() {
    expect_topic(
        "py_sqs_send",
        "svc/events.py",
        "sqs.send_message(QueueUrl=\"https://sqs.example/orders\", MessageBody=b)\n",
        "sqs/https://sqs.example/orders",
    );
}

#[test]
fn python_sns_publish_mints_an_sns_topic() {
    expect_topic(
        "py_sns_pub",
        "svc/events.py",
        "sns.publish(TopicArn=\"arn:aws:sns:us-east-1:1:orders\", Message=m)\n",
        "sns/arn:aws:sns:us-east-1:1:orders",
    );
}

// TypeScript / JavaScript

#[test]
fn ts_ioredis_publish_mints_a_redis_topic() {
    expect_topic(
        "ts_redis_pub",
        "src/events.ts",
        "redis.publish(\"orders\", msg);\n",
        "redis/orders",
    );
}

#[test]
fn ts_mqtt_publish_mints_an_mqtt_topic() {
    expect_topic(
        "ts_mqtt_pub",
        "src/events.ts",
        "client.publish(\"sensors/temp\", payload);\n",
        "mqtt/sensors/temp",
    );
}

#[test]
fn ts_sqs_send_command_mints_an_sqs_topic() {
    expect_topic(
        "ts_sqs_send",
        "src/events.ts",
        "new SendMessageCommand({ QueueUrl: \"https://sqs.example/orders\", MessageBody: b });\n",
        "sqs/https://sqs.example/orders",
    );
}

#[test]
fn ts_sns_publish_command_mints_an_sns_topic() {
    expect_topic(
        "ts_sns_pub",
        "src/events.ts",
        "new PublishCommand({ TopicArn: \"arn:aws:sns:us-east-1:1:orders\", Message: m });\n",
        "sns/arn:aws:sns:us-east-1:1:orders",
    );
}

// Go

#[test]
fn go_redis_publish_mints_a_redis_topic() {
    expect_topic(
        "go_redis_pub",
        "svc/events.go",
        "package svc\n\nfunc f() { rdb.Publish(ctx, \"orders\", msg) }\n",
        "redis/orders",
    );
}

// Python, batch 2

#[test]
fn python_nats_publish_mints_a_nats_topic() {
    expect_topic(
        "py_nats_pub",
        "svc/events.py",
        "await nc.publish(\"orders.created\", data)\n",
        "nats/orders.created",
    );
}

#[test]
fn python_nats_subscribe_mints_a_nats_topic() {
    expect_topic(
        "py_nats_sub",
        "svc/events.py",
        "await nc.subscribe(\"orders.created\", cb)\n",
        "nats/orders.created",
    );
}

#[test]
fn python_kinesis_put_record_mints_a_kinesis_topic() {
    expect_topic(
        "py_kin_put",
        "svc/events.py",
        "kinesis.put_record(StreamName=\"orders\", Data=d, PartitionKey=\"k\")\n",
        "kinesis/orders",
    );
}

#[test]
fn python_kinesis_get_shard_iterator_mints_a_kinesis_topic() {
    expect_topic(
        "py_kin_get",
        "svc/events.py",
        "it = kinesis.get_shard_iterator(StreamName=\"orders\", ShardId=\"s\")\n",
        "kinesis/orders",
    );
}

#[test]
fn python_eventbridge_put_events_mints_an_eventbridge_topic() {
    expect_topic(
        "py_eb_put",
        "svc/events.py",
        "events.put_events(Entries=[{\"Source\": \"app\", \"DetailType\": \"x\", \"EventBusName\": \"orders-bus\"}])\n",
        "eventbridge/orders-bus",
    );
}

#[test]
fn python_pulsar_create_producer_mints_a_pulsar_topic() {
    expect_topic(
        "py_pulsar_prod",
        "svc/events.py",
        "p = client.create_producer(topic=\"orders\")\n",
        "pulsar/orders",
    );
}

#[test]
fn python_pulsar_subscribe_mints_a_pulsar_topic() {
    expect_topic(
        "py_pulsar_sub",
        "svc/events.py",
        "c = client.subscribe(\"orders\", subscription_name=\"s\")\n",
        "pulsar/orders",
    );
}

#[test]
fn python_pubsub_topic_path_mints_a_pubsub_topic() {
    expect_topic(
        "py_pubsub",
        "svc/events.py",
        "path = publisher.topic_path(\"proj\", \"orders\")\n",
        "pubsub/orders",
    );
}

#[test]
fn python_temporal_worker_mints_a_temporal_queue() {
    expect_topic(
        "py_temporal_worker",
        "svc/events.py",
        "w = Worker(client, task_queue=\"billing\", workflows=[W])\n",
        "temporal/billing",
    );
}

#[test]
fn python_temporal_start_workflow_mints_a_temporal_queue() {
    expect_topic(
        "py_temporal_start",
        "svc/events.py",
        "h = client.start_workflow(W.run, id=\"x\", task_queue=\"billing\")\n",
        "temporal/billing",
    );
}

#[test]
fn python_dramatiq_actor_mints_a_dramatiq_queue() {
    expect_topic(
        "py_dramatiq",
        "svc/events.py",
        "@dramatiq.actor(queue_name=\"emails\")\ndef send(x):\n    pass\n",
        "dramatiq/emails",
    );
}

#[test]
fn python_kafka_python_consumer_mints_a_kafka_topic() {
    expect_topic(
        "py_kp_cons",
        "svc/events.py",
        "consumer = KafkaConsumer(\"orders\")\n",
        "kafka/orders",
    );
}

#[test]
fn python_kafka_python_producer_mints_a_kafka_topic() {
    expect_topic(
        "py_kp_prod",
        "svc/events.py",
        "producer.send(\"orders\", value)\n",
        "kafka/orders",
    );
}

// TypeScript / JavaScript, batch 2

#[test]
fn ts_nats_publish_mints_a_nats_topic() {
    expect_topic(
        "ts_nats_pub",
        "src/events.ts",
        "nc.publish(\"orders.created\", data);\n",
        "nats/orders.created",
    );
}

#[test]
fn ts_nats_subscribe_mints_a_nats_topic() {
    expect_topic(
        "ts_nats_sub",
        "src/events.ts",
        "nc.subscribe(\"orders.created\", cb);\n",
        "nats/orders.created",
    );
}

#[test]
fn ts_eventbridge_put_events_mints_an_eventbridge_topic() {
    expect_topic(
        "ts_eb_put",
        "src/events.ts",
        "new PutEventsCommand({ Entries: [{ Source: \"app\", EventBusName: \"orders-bus\" }] });\n",
        "eventbridge/orders-bus",
    );
}

#[test]
fn ts_kinesis_put_record_mints_a_kinesis_topic() {
    expect_topic(
        "ts_kin_put",
        "src/events.ts",
        "new PutRecordCommand({ StreamName: \"orders\", Data: d, PartitionKey: \"k\" });\n",
        "kinesis/orders",
    );
}

#[test]
fn ts_pubsub_topic_publish_mints_a_pubsub_topic() {
    expect_topic(
        "ts_pubsub",
        "src/events.ts",
        "pubsub.topic(\"orders\").publishMessage({ data: d });\n",
        "pubsub/orders",
    );
}

#[test]
fn ts_inngest_send_mints_an_inngest_topic() {
    expect_topic(
        "ts_inngest_send",
        "src/events.ts",
        "inngest.send({ name: \"order.created\", data: d });\n",
        "inngest/order.created",
    );
}

#[test]
fn ts_inngest_trigger_mints_an_inngest_topic() {
    expect_topic(
        "ts_inngest_trig",
        "src/events.ts",
        "inngest.createFunction({ id: \"f\", triggers: [{ event: \"order.created\" }] }, fn);\n",
        "inngest/order.created",
    );
}

// Go, batch 2

#[test]
fn go_nats_publish_mints_a_nats_topic() {
    expect_topic(
        "go_nats_pub",
        "svc/events.go",
        "package svc\n\nfunc f() { nc.Publish(\"orders.created\", data) }\n",
        "nats/orders.created",
    );
}

#[test]
fn go_nats_subscribe_mints_a_nats_topic() {
    expect_topic(
        "go_nats_sub",
        "svc/events.go",
        "package svc\n\nfunc f() { nc.Subscribe(\"orders.created\", handler) }\n",
        "nats/orders.created",
    );
}
