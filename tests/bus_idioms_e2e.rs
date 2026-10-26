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
