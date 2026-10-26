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

// Ruby and Java, batch 3

#[test]
fn ruby_sidekiq_push_mints_a_sidekiq_queue() {
    expect_topic(
        "rb_sidekiq_push",
        "app/jobs/notify.rb",
        "Sidekiq::Client.push('class' => 'Mailer', 'queue' => 'emails', 'args' => [1])\n",
        "sidekiq/emails",
    );
}

#[test]
fn ruby_sidekiq_options_mints_a_sidekiq_queue() {
    expect_topic(
        "rb_sidekiq_opts",
        "app/workers/mailer.rb",
        "class Mailer\n  include Sidekiq::Worker\n  sidekiq_options queue: 'emails'\nend\n",
        "sidekiq/emails",
    );
}

#[test]
fn ruby_resque_enqueue_to_mints_a_resque_queue() {
    expect_topic(
        "rb_resque_to",
        "app/jobs/notify.rb",
        "Resque.enqueue_to('emails', Mailer, 1)\n",
        "resque/emails",
    );
}

#[test]
fn ruby_resque_queue_ivar_mints_a_resque_queue() {
    expect_topic(
        "rb_resque_ivar",
        "app/jobs/mailer.rb",
        "class Mailer\n  @queue = :emails\nend\n",
        "resque/emails",
    );
}

#[test]
fn java_spring_kafka_listener_mints_a_kafka_topic() {
    expect_topic(
        "java_listener",
        "src/main/java/Orders.java",
        "class Orders {\n  @KafkaListener(topics = \"orders\")\n  void on(String m) {}\n}\n",
        "kafka/orders",
    );
}

#[test]
fn java_kafka_template_send_mints_a_kafka_topic() {
    expect_topic(
        "java_template",
        "src/main/java/Orders.java",
        "class Orders {\n  void f() { kafkaTemplate.send(\"orders\", v); }\n}\n",
        "kafka/orders",
    );
}

// Huey and Redis Streams, batch 4

#[test]
fn python_huey_instance_mints_a_huey_queue() {
    expect_topic(
        "py_huey",
        "worker/tasks.py",
        "from huey import RedisHuey\nhuey = RedisHuey('emails')\n",
        "huey/emails",
    );
}

#[test]
fn python_redis_stream_group_read_mints_a_redis_topic() {
    expect_topic(
        "py_xreadgroup",
        "svc/events.py",
        "msgs = r.xreadgroup(\"billing\", \"w1\", {\"orders\": \">\"}, count=10)\n",
        "redis/orders",
    );
}

#[test]
fn ts_ioredis_xadd_mints_a_redis_topic() {
    expect_topic(
        "ts_xadd",
        "src/events.ts",
        "await redis.xadd('orders', '*', 'id', id);\n",
        "redis/orders",
    );
}

#[test]
fn ts_ioredis_xreadgroup_mints_a_redis_topic() {
    expect_topic(
        "ts_xreadgroup",
        "src/events.ts",
        "await redis.xreadgroup('GROUP', 'billing', 'w1', 'STREAMS', 'orders', '>');\n",
        "redis/orders",
    );
}

#[test]
fn go_redis_xadd_mints_a_redis_topic() {
    expect_topic(
        "go_xadd",
        "svc/events.go",
        "package svc\n\nfunc f() { rdb.XAdd(ctx, &redis.XAddArgs{Stream: \"orders\", Values: v}) }\n",
        "redis/orders",
    );
}

#[test]
fn go_redis_xreadgroup_mints_a_redis_topic() {
    expect_topic(
        "go_xreadgroup",
        "svc/events.go",
        "package svc\n\nfunc f() { rdb.XReadGroup(ctx, &redis.XReadGroupArgs{Streams: []string{\"orders\", \">\"}}) }\n",
        "redis/orders",
    );
}

// Real-shape negatives, copied from code in the test-repos corpus. Each one is
// a call whose queue is not a literal at the call site, or a non-queue API
// that shares the method name. None may mint a topic.

#[test]
fn saleor_style_dynamic_sqs_send_mints_nothing() {
    // saleor/saleor/core/sqs.py: the queue URL lives in kwargs, not a literal.
    let root = fixture(
        "saleor_sqs",
        "core/sqs.py",
        "c = self.sqs(queue=name)\nc.send_message(**kwargs)\n",
    );
    assert!(
        topics(&root).is_empty(),
        "dynamic SQS send must not mint a topic"
    );
}

#[test]
fn email_service_send_message_mints_nothing() {
    // microservices-demo emailservice: a mail API, not a queue.
    let root = fixture(
        "email_send",
        "emailservice/email_server.py",
        "response = client.send_message(sender = client.sender_path(p, r, s), envelope_from_authority = '')\n",
    );
    assert!(
        topics(&root).is_empty(),
        "a mail send must not mint a topic"
    );
}

// BullMQ, Bull, RQ and RabbitMQ, batch 5

#[test]
fn ts_bullmq_queue_mints_a_bullmq_queue() {
    expect_topic(
        "ts_bullmq_queue",
        "src/queues.ts",
        "export const emails = new Queue('emails', { connection });\n",
        "bullmq/emails",
    );
}

#[test]
fn ts_bullmq_worker_mints_a_bullmq_queue() {
    expect_topic(
        "ts_bullmq_worker",
        "src/workers.ts",
        "new Worker('emails', async (job) => send(job.data), { connection });\n",
        "bullmq/emails",
    );
}

#[test]
fn ts_legacy_bull_queue_mints_a_bull_queue() {
    expect_topic(
        "ts_bull",
        "src/queues.ts",
        "const q = new Bull('emails');\n",
        "bull/emails",
    );
}

#[test]
fn ts_queue_built_from_a_variable_mints_nothing() {
    let root = fixture(
        "ts_bullmq_var",
        "src/queues.ts",
        "const q = new Queue(name, { connection });\n",
    );
    assert!(
        topics(&root).is_empty(),
        "a non-literal queue name must not mint a topic"
    );
}

#[test]
fn python_rq_queue_mints_an_rq_queue() {
    expect_topic(
        "py_rq",
        "worker/jobs.py",
        "q = Queue('emails', connection=redis)\n",
        "rq/emails",
    );
}

#[test]
fn python_stdlib_queue_mints_nothing() {
    let root = fixture(
        "py_stdlib_queue",
        "worker/jobs.py",
        "q = queue.Queue(maxsize=10)\n",
    );
    assert!(
        topics(&root).is_empty(),
        "stdlib queue.Queue must not mint an rq topic"
    );
}

#[test]
fn java_rabbit_convert_and_send_mints_an_amqp_topic() {
    expect_topic(
        "java_rabbit_send",
        "src/main/java/Shipping.java",
        "class S {\n  void f() { rabbitTemplate.convertAndSend(\"shipping-task\", shipment); }\n}\n",
        "amqp/shipping-task",
    );
}

#[test]
fn java_rabbit_listener_annotation_mints_an_amqp_topic() {
    expect_topic(
        "java_rabbit_listener",
        "src/main/java/Consumer.java",
        "class C {\n  @RabbitListener(queues = \"shipping-task\")\n  void on(String m) {}\n}\n",
        "amqp/shipping-task",
    );
}

// Real-shape case, adapted from Weaveworks Sock Shop (Apache-2.0), queue-master
// ShippingConsumerConfiguration: the queue name is a constant field read back
// through `this.queueName`.
#[test]
fn sock_shop_set_queue_names_resolves_the_constant_field() {
    expect_topic(
        "sock_shop_consumer",
        "src/main/java/works/weave/socks/queuemaster/configuration/ShippingConsumerConfiguration.java",
        "class ShippingConsumerConfiguration {\n    protected final String queueName = \"shipping-task\";\n    SimpleMessageListenerContainer listenerContainer() {\n        SimpleMessageListenerContainer container = new SimpleMessageListenerContainer();\n        container.setQueueNames(this.queueName);\n        return container;\n    }\n}\n",
        "amqp/shipping-task",
    );
}

// ActiveJob (Ruby) and asynq (Go), batch 6

#[test]
fn ruby_activejob_queue_as_mints_an_activejob_queue() {
    expect_topic(
        "rb_queue_as",
        "app/jobs/mailer_job.rb",
        "class MailerJob < ApplicationJob\n  queue_as :emails\nend\n",
        "activejob/emails",
    );
}

#[test]
fn ruby_activejob_set_queue_mints_an_activejob_queue() {
    expect_topic(
        "rb_set_queue",
        "app/jobs/notify.rb",
        "MailerJob.set(queue: :emails).perform_later(user)\n",
        "activejob/emails",
    );
}

#[test]
fn go_asynq_queue_option_mints_an_asynq_queue() {
    expect_topic(
        "go_asynq_opt",
        "svc/jobs.go",
        "package svc\n\nfunc f() { client.Enqueue(task, asynq.Queue(\"critical\")) }\n",
        "asynq/critical",
    );
}

#[test]
fn go_asynq_server_queues_mints_an_asynq_queue() {
    expect_topic(
        "go_asynq_server",
        "svc/worker.go",
        "package svc\n\nfunc f() { asynq.Config{Queues: map[string]int{\"critical\": 6, \"default\": 3}} }\n",
        "asynq/critical",
    );
}

#[test]
fn ruby_plain_job_without_a_queue_mints_nothing() {
    let root = fixture(
        "rb_no_queue",
        "app/jobs/plain_job.rb",
        "class PlainJob < ApplicationJob\n  def perform; end\nend\n",
    );
    assert!(
        topics(&root).is_empty(),
        "a job with no queue declaration must not mint a topic"
    );
}

// Coverage exposure: producers whose topic is not a literal are reported as
// `DynamicTopic`; consumers keep their own path (the joiner's sentinel).

fn report_for(
    tag: &str,
    rel: &str,
    content: &str,
) -> Vec<lain::federation::contracts::coverage::UnresolvedRecord> {
    let root = fixture(tag, rel, content);
    let graph = GraphDatabase::new(&root.join("graph.bin")).expect("open graph");
    lain::server::sensors::event_sensor::scan_workspace_event_with_report(
        &graph,
        &root,
        &RepoNamespace::for_test(),
    )
    .expect("event scan")
    .unresolved
}

#[test]
fn dynamic_producer_topic_is_reported_as_dynamic_topic() {
    let records = report_for(
        "dyn_producer",
        "src/events.ts",
        "producer.send({ topic: topicName, messages: [m] });\n",
    );
    assert_eq!(records.len(), 1, "one DynamicTopic record; got {records:?}");
    assert!(matches!(
        records[0].reason,
        lain::federation::contracts::model::UnresolvedReason::DynamicTopic
    ));
    assert_eq!(records[0].count, 1);
    assert_eq!(records[0].sample_ids, vec!["src/events.ts:1".to_string()]);
}

#[test]
fn literal_producer_topic_is_not_reported() {
    let records = report_for(
        "lit_producer",
        "src/events.ts",
        "producer.send({ topic: 'orders.created', messages: [m] });\n",
    );
    assert!(
        records.is_empty(),
        "a resolved topic is not a coverage gap; got {records:?}"
    );
}

#[test]
fn dynamic_consumer_is_left_to_the_joiner_sentinel() {
    // Consumers already reach the joiner as the dynamic-topic sentinel; the
    // producer report must not count them a second time.
    let records = report_for(
        "dyn_consumer",
        "src/events.ts",
        "consumer.subscribe({ topic: topicName });\n",
    );
    assert!(
        records.is_empty(),
        "a dynamic consumer must not be counted here; got {records:?}"
    );
}

// Environment-backed topic names. The env sensor runs first and fills the
// scalar channel; the event sensor resolves constants through it.

fn fixture_files(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lain_bus_env_{}_{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (rel, content) in files {
        let file = dir.join(rel);
        std::fs::create_dir_all(file.parent().expect("parent")).unwrap();
        std::fs::write(&file, content).unwrap();
    }
    dir
}

/// Topic names minted with the env channel populated for `root`.
fn topics_with_env(root: &Path) -> Vec<String> {
    let graph = GraphDatabase::new(&root.join("graph.bin")).expect("open graph");
    let ns = RepoNamespace::for_test();
    lain::server::sensors::env_sensor::scan_workspace_env(&graph, root, &ns).expect("env scan");
    scan_workspace_event(&graph, root, &ns).expect("event scan");
    let mut names: Vec<String> = graph
        .get_all_nodes()
        .into_iter()
        .filter(|n| n.node_type == NodeType::Topic)
        .map(|n| n.name)
        .collect();
    names.sort();
    names
}

// Adapted from open-telemetry/opentelemetry-demo, src/accounting/Consumer.cs
// (Apache-2.0): the topic is an env read with a literal default.
#[test]
fn csharp_env_default_topic_resolves_through_the_dotenv() {
    let root = fixture_files(
        "cs_accounting",
        &[
            (".env", "KAFKA_TOPIC=orders\n"),
            (
                "src/Consumer.cs",
                "internal class Consumer {\n    private static readonly string TopicName = Environment.GetEnvironmentVariable(\"KAFKA_TOPIC\") ?? \"orders\";\n    void Run() { _consumer.Subscribe(TopicName); }\n}\n",
            ),
        ],
    );
    let names = topics_with_env(&root);
    assert!(names.iter().any(|n| n == "kafka/orders"), "got {names:?}");
}

// Adapted from open-telemetry/opentelemetry-demo, src/fraud-detection
// (Apache-2.0): a Kotlin `val` with an env read and a fallback, subscribed as a
// list. No .env here, so the code default is the only source.
#[test]
fn kotlin_env_default_topic_resolves_to_the_fallback() {
    let root = fixture_files(
        "kt_fraud",
        &[(
            "src/main/kotlin/main.kt",
            "val topic: String = System.getenv(\"KAFKA_TOPIC\") ?: \"orders\"\nfun run(consumer: KafkaConsumer<String, ByteArray>) {\n    consumer.subscribe(listOf(topic))\n}\n",
        )],
    );
    let names = topics_with_env(&root);
    assert!(names.iter().any(|n| n == "kafka/orders"), "got {names:?}");
}

#[test]
fn a_conflicting_env_value_mints_no_topic_from_the_default() {
    let root = fixture_files(
        "cs_conflict",
        &[
            (".env", "KAFKA_TOPIC=orders\n"),
            ("compose.yaml", "services:\n  x:\n    environment:\n      KAFKA_TOPIC: orders-v2\n"),
            (
                "src/Consumer.cs",
                "internal class Consumer {\n    private static readonly string TopicName = Environment.GetEnvironmentVariable(\"KAFKA_TOPIC\") ?? \"orders\";\n    void Run() { _consumer.Subscribe(TopicName); }\n}\n",
            ),
        ],
    );
    let names = topics_with_env(&root);
    assert!(
        !names.iter().any(|n| n.starts_with("kafka/")),
        "a conflict is not a value, and the default must not stand in for it; got {names:?}"
    );
}

#[test]
fn an_env_read_with_no_value_and_no_default_mints_nothing() {
    let root = fixture_files(
        "cs_unmapped",
        &[(
            "src/Consumer.cs",
            "internal class Consumer {\n    private static readonly string TopicName = Environment.GetEnvironmentVariable(\"KAFKA_TOPIC\");\n    void Run() { _consumer.Subscribe(TopicName); }\n}\n",
        )],
    );
    let names = topics_with_env(&root);
    assert!(
        names.is_empty(),
        "an unmapped var with no default is not a topic; got {names:?}"
    );
}

// Adapted from open-telemetry/opentelemetry-demo, src/fraud-detection
// (Apache-2.0): inside `.apply { }` the call has no receiver.
#[test]
fn kotlin_subscribe_inside_apply_has_no_receiver_and_still_matches() {
    expect_topic(
        "kt_apply",
        "src/main/kotlin/main.kt",
        "val consumer = KafkaConsumer<String, ByteArray>(props).apply {\n    subscribe(listOf(\"orders\"))\n}\n",
        "kafka/orders",
    );
}

// Adapted from open-telemetry/opentelemetry-demo, src/checkout (Apache-2.0):
// the producer names `kafka.Topic`, a package value in another directory
// whose function reads the environment with a fallback.
const CHECKOUT_PRODUCER: &str = "package kafka\n\nimport (\n\t\"os\"\n\n\t\"github.com/IBM/sarama\"\n)\n\nvar (\n\tTopic           = getTopic()\n\tProtocolVersion = sarama.V3_0_0_0\n)\n\nfunc getTopic() string {\n\tif topic := os.Getenv(\"KAFKA_TOPIC\"); topic != \"\" {\n\t\treturn topic\n\t}\n\treturn \"orders\"\n}\n";

const CHECKOUT_SEND: &str = "package main\n\nimport (\n\t\"github.com/IBM/sarama\"\n\t\"example/checkout/kafka\"\n)\n\nfunc send(v []byte) {\n\tmsg := sarama.ProducerMessage{\n\t\tTopic: kafka.Topic,\n\t\tValue: sarama.ByteEncoder(v),\n\t}\n\t_ = msg\n}\n";

#[test]
fn go_checkout_producer_resolves_across_packages_through_the_env() {
    let root = fixture_files(
        "go_checkout_env",
        &[
            (".env", "KAFKA_TOPIC=orders\n"),
            ("checkout/kafka/producer.go", CHECKOUT_PRODUCER),
            ("checkout/main.go", CHECKOUT_SEND),
        ],
    );
    let names = topics_with_env(&root);
    assert!(names.iter().any(|n| n == "kafka/orders"), "got {names:?}");
}

#[test]
fn go_checkout_producer_falls_back_to_the_function_default() {
    let root = fixture_files(
        "go_checkout_default",
        &[
            ("checkout/kafka/producer.go", CHECKOUT_PRODUCER),
            ("checkout/main.go", CHECKOUT_SEND),
        ],
    );
    let names = topics_with_env(&root);
    assert!(names.iter().any(|n| n == "kafka/orders"), "got {names:?}");
}

#[test]
fn a_bare_topic_field_without_the_client_import_mints_nothing() {
    let root = fixture_files(
        "go_no_sarama",
        &[(
            "billing/cfg.go",
            "package billing\n\nvar cfg = Config{\n\tTopic: \"billing\",\n}\n",
        )],
    );
    let names = topics_with_env(&root);
    assert!(
        names.is_empty(),
        "a struct field outside the client must not mint a topic; got {names:?}"
    );
}

// Producers whose topic is an environment read the env channel cannot resolve
// are reported with that reason, not as a generic dynamic topic.

fn producer_reports_with_env(
    root: &Path,
) -> Vec<lain::federation::contracts::coverage::UnresolvedRecord> {
    let graph = GraphDatabase::new(&root.join("graph.bin")).expect("open graph");
    let ns = RepoNamespace::for_test();
    lain::server::sensors::env_sensor::scan_workspace_env(&graph, root, &ns).expect("env scan");
    lain::server::sensors::event_sensor::scan_workspace_event_with_report(&graph, root, &ns)
        .expect("event scan")
        .unresolved
}

#[test]
fn unmapped_env_topic_on_a_producer_is_reported_as_env_unmapped() {
    let root = fixture_files(
        "cs_unmapped_producer",
        &[(
            "src/Publisher.cs",
            "internal class Publisher {\n    private static readonly string TopicName = Environment.GetEnvironmentVariable(\"KAFKA_TOPIC\");\n    void Run() { _producer.Produce(TopicName, msg); }\n}\n",
        )],
    );
    let records = producer_reports_with_env(&root);
    assert_eq!(records.len(), 1, "one record; got {records:?}");
    assert!(matches!(
        records[0].reason,
        lain::federation::contracts::model::UnresolvedReason::EnvUnmapped
    ));
    assert_eq!(
        records[0].sample_ids,
        vec!["src/Publisher.cs:3".to_string()]
    );
}

#[test]
fn conflicting_env_topic_on_a_producer_is_reported_as_env_ambiguous() {
    let root = fixture_files(
        "cs_ambiguous_producer",
        &[
            (".env", "KAFKA_TOPIC=orders\n"),
            ("compose.yaml", "services:\n  x:\n    environment:\n      KAFKA_TOPIC: orders-v2\n"),
            (
                "src/Publisher.cs",
                "internal class Publisher {\n    private static readonly string TopicName = Environment.GetEnvironmentVariable(\"KAFKA_TOPIC\") ?? \"orders\";\n    void Run() { _producer.Produce(TopicName, msg); }\n}\n",
            ),
        ],
    );
    let records = producer_reports_with_env(&root);
    assert_eq!(records.len(), 1, "one record; got {records:?}");
    assert!(matches!(
        records[0].reason,
        lain::federation::contracts::model::UnresolvedReason::EnvAmbiguous
    ));
}

#[test]
fn conflicting_env_value_reaches_a_cross_package_go_producer_as_ambiguous() {
    let root = fixture_files(
        "go_checkout_ambiguous",
        &[
            (".env", "KAFKA_TOPIC=orders\n"),
            (
                "compose.yaml",
                "services:\n  checkout:\n    environment:\n      KAFKA_TOPIC: orders-v2\n",
            ),
            ("checkout/kafka/producer.go", CHECKOUT_PRODUCER),
            ("checkout/main.go", CHECKOUT_SEND),
        ],
    );
    let records = producer_reports_with_env(&root);
    assert!(
        records.iter().any(|r| matches!(
            r.reason,
            lain::federation::contracts::model::UnresolvedReason::EnvAmbiguous
        )),
        "checkout's producer must report the env conflict; got {records:?}"
    );
    let names = topics_with_env(&root);
    assert!(
        !names.iter().any(|n| n.starts_with("kafka/")),
        "no topic from a conflict; got {names:?}"
    );
}
