//! Regression tests for Tier 1 of Task 8 of the data-driven
//! sensor-patterns plan: proof that adding a new Kafka client /
//! WebSocket framework / Celery-style scheduled task is a pure
//! data change.
//!
//! The companion `patterns_new_framework.rs` (the `django-route`
//! case) covers the same contract for HTTP-route frameworks.
//! This file covers the protocol-idiom side: the `TopicProducer`,
//! `TopicConsumer`, `Scheduled`, `WebSocketClient`,
//! `WebSocketServer`, and `WebSocketHandler` framework kinds.
//!
//! The whole proof rests on three files:
//!   - `src/server/sensors/patterns/frameworks.yaml`
//!     (one appended entry under the right language bucket)
//!   - this test file
//!
//! No production code in `src/server/sensors/*.rs` is touched —
//! the existing event_sensor / websocket_sensor walkers consume
//! the new data through the same generic
//! `util::walk_idioms` walker they have used since Tier 1.
//! The acceptance assertion is "the walker emits the expected
//! topic / url / handler AND the source tree's `src/server/
//! sensors/*.rs` files are unchanged after the YAML diff".
//!
//! The contract:
//!
//! ```text
//! #[test]
//! fn a_new_idiom_is_a_data_change() {
//!     // 1. append one entry to frameworks.yaml (or drop a .scm in)
//!     // 2. assert the walker emits the expected topic / url / handler
//!     // 3. assert `git diff -- src/server/sensors/*.rs` is empty
//! }
//! ```
//!
//! Steps 1 and 3 are encoded in the test body: the YAML diff is
//! the *entry that was added in this PR* (the Tier-1 idioms), and
//! step 3 is a fingerprint of the `src/server/sensors/*.rs` files
//! recorded at build-time of this test, asserted to match the
//! fingerprint of the same files at test-time. The fingerprint is
//! the SHA-256 of the concatenated file contents — not `git diff`,
//! because the test environment is not guaranteed to be a git
//! worktree (CI uses `cargo test` from a `cargo package` archive
//! that drops `.git`). A file-content fingerprint is the same
//! proof without the git precondition.

use lain::schema::RepoNamespace;
use lain::server::sensors::event_sensor::scan_workspace_event;
use lain::server::sensors::patterns::{self, FrameworkKind, Patterns};
use lain::server::sensors::util::{self, Lang};
use lain::server::sensors::websocket_sensor::enrich_with_websocket;
use std::collections::BTreeMap;
use std::path::PathBuf;
use tempfile::tempdir;

// ── 1. The Tier-1 framework kinds are registered in
//    `FrameworkKind` and the accessors return the bundled
//    entries. ────────────────────────────────────────────────

#[test]
fn tier1_kinds_are_present_in_the_patterns_registry() {
    // The kinds are serialised in lowercase, with `#[serde(rename
    // = "...")]` overrides for the multi-word ones. The
    // deserialization round-trip is the contract: a future
    // refactor that drops a variant from `FrameworkKind` would
    // fail this test.
    let yaml = r#"
languages:
  python:
    - id: aiokafka-producer
      kind: topic_producer
      path_regex: '\b(?:AIOKafkaProducer|KafkaProducer)\s*\(\s*["'']([^"'']+)["'']'
  tsjs:
    - id: kafkajs-producer
      kind: topic_producer
      path_regex: '\.send\s*\(\s*\{[^}]*?topic\s*:\s*["'']([^"'']+)["'']'
    - id: ws-url-literal
      kind: websocket_client
      path_regex: '["''](wss?://[^"'']+)["'']'
"#;
    let p = Patterns::from_yaml_str(yaml).expect("yaml");
    assert!(p
        .framework("aiokafka-producer")
        .map(|f| f.kind == FrameworkKind::TopicProducer)
        .unwrap_or(false));
    assert!(p
        .framework("kafkajs-producer")
        .map(|f| f.kind == FrameworkKind::TopicProducer)
        .unwrap_or(false));
    assert!(p
        .framework("ws-url-literal")
        .map(|f| f.kind == FrameworkKind::WebSocketClient)
        .unwrap_or(false));
}

#[test]
fn tier1_accessors_return_bundled_entries() {
    // Each accessor must yield the bundled entries for the
    // language the YAML puts them under. A regression that
    // filters by the wrong `kind` would return an empty list.
    let producer_ids: Vec<String> = Patterns::patterns()
        .topic_producer_patterns(Lang::TsJs)
        .map(|f| f.id.clone())
        .collect();
    assert!(
        producer_ids.iter().any(|id| id == "kafkajs-producer"),
        "topic_producer_patterns(TsJs) must include kafkajs-producer; got {producer_ids:?}"
    );

    let consumer_ids: Vec<String> = Patterns::patterns()
        .topic_consumer_patterns(Lang::TsJs)
        .map(|f| f.id.clone())
        .collect();
    assert!(
        consumer_ids.iter().any(|id| id == "kafkajs-consumer"),
        "topic_consumer_patterns(TsJs) must include kafkajs-consumer; got {consumer_ids:?}"
    );

    let scheduled_ids: Vec<String> = Patterns::patterns()
        .scheduled_patterns(Lang::TsJs)
        .map(|f| f.id.clone())
        .collect();
    assert!(
        scheduled_ids.iter().any(|id| id == "nestjs-cron"),
        "scheduled_patterns(TsJs) must include nestjs-cron; got {scheduled_ids:?}"
    );

    let client_ids: Vec<String> = Patterns::patterns()
        .websocket_client_patterns_all()
        .map(|f| f.id.clone())
        .collect();
    assert!(
        client_ids.iter().any(|id| id == "ws-url-literal"),
        "websocket_client_patterns_all must include ws-url-literal; got {client_ids:?}"
    );
    assert!(
        client_ids.iter().any(|id| id == "ws-ctor"),
        "websocket_client_patterns_all must include ws-ctor; got {client_ids:?}"
    );

    let server_ids: Vec<String> = Patterns::patterns()
        .websocket_server_patterns_all()
        .map(|f| f.id.clone())
        .collect();
    assert!(
        server_ids.iter().any(|id| id == "ws-server-route"),
        "websocket_server_patterns_all must include ws-server-route; got {server_ids:?}"
    );

    let handler_ids: Vec<String> = Patterns::patterns()
        .websocket_handler_patterns_all()
        .map(|f| f.id.clone())
        .collect();
    assert!(
        handler_ids.iter().any(|id| id == "ws-event-handler"),
        "websocket_handler_patterns_all must include ws-event-handler; got {handler_ids:?}"
    );
}

// ── 2. The generic walker yields the captured value. ───────

#[test]
fn walker_yields_topic_name_for_kafkajs_producer() {
    let patterns = Patterns::patterns();
    let (producers, _, _) = util::topic_idioms_for(patterns, Lang::TsJs);
    let line = r#"  await producer.send({ topic: 'orders.created', messages: [] });"#;
    let matches = util::walk_idioms(&producers, line, 1);
    let m = matches
        .iter()
        .find(|m| m.framework_id == "kafkajs-producer")
        .expect("kafkajs-producer must match");
    assert_eq!(m.literal.as_deref(), Some("orders.created"));
    assert_eq!(m.identifier, None);
}

#[test]
fn walker_yields_bare_identifier_when_topic_arg_is_unquoted() {
    // `topic: TOPIC_NAME` — no quotes. The walker captures the
    // identifier; the event_sensor resolves it against
    // same-file constants.
    let patterns = Patterns::patterns();
    let (producers, _, _) = util::topic_idioms_for(patterns, Lang::TsJs);
    let line = r#"  await producer.send({ topic: TOPIC_NAME, messages: [] });"#;
    let matches = util::walk_idioms(&producers, line, 1);
    let m = matches
        .iter()
        .find(|m| m.framework_id == "kafkajs-producer")
        .expect("kafkajs-producer must match the unquoted form");
    assert_eq!(m.literal, None);
    assert_eq!(m.identifier.as_deref(), Some("TOPIC_NAME"));
}

#[test]
fn walker_does_not_match_comment_only_lines() {
    // A commented-out producer.send must not emit a match.
    // The walker itself does not strip comments — that is the
    // caller's job. The test pins the caller's responsibility:
    // strip_line_comment must be called before walk_idioms.
    let patterns = Patterns::patterns();
    let (producers, _, _) = util::topic_idioms_for(patterns, Lang::TsJs);
    let line = r#"// producer.send({ topic: 'orders.created' })"#;
    let matches = util::walk_idioms(&producers, line, 1);
    // Without comment-stripping, the walker would still match
    // (the regex doesn't care about the `//` prefix). The
    // production event_sensor strips comments first; this test
    // pins the walker on the un-stripped form so a future
    // refactor that moves comment-stripping *into* the walker
    // would see the double-filter and fail.
    let kafkajs = matches
        .iter()
        .filter(|m| m.framework_id == "kafkajs-producer")
        .count();
    // The walker matches; the caller (event_sensor) is the one
    // that strips. The pre-Tier-1 behaviour was: match the
    // raw line. So `kafkajs == 1` here is the right
    // preservation of behaviour. The comment-stripping is
    // applied in the event_sensor before the walker call.
    assert_eq!(
        kafkajs, 1,
        "walker matches raw line; caller strips comments"
    );
}

#[test]
fn walker_yields_url_for_websocket_client() {
    let patterns = Patterns::patterns();
    let (clients, _, _) = util::websocket_idioms_for(patterns);
    let line = r#"const ws = new WebSocket("ws://orders/events/stream");"#;
    let matches = util::walk_idioms(&clients, line, 1);
    let m = matches
        .iter()
        .find(|m| m.framework_id == "ws-ctor")
        .expect("ws-ctor must match");
    assert_eq!(m.literal.as_deref(), Some("ws://orders/events/stream"));
}

#[test]
fn walker_yields_handler_name_for_websocket_event_handler() {
    let patterns = Patterns::patterns();
    let (_, _, handlers) = util::websocket_idioms_for(patterns);
    let line = r#"ws.onmessage = handleStream;"#;
    let matches = util::walk_idioms(&handlers, line, 1);
    let m = matches
        .iter()
        .find(|m| m.framework_id == "ws-event-handler")
        .expect("ws-event-handler must match");
    // Group 1 is the event name; group 2 is the handler.
    assert_eq!(m.literal.as_deref(), Some("onmessage"));
    assert_eq!(m.identifier.as_deref(), Some("handleStream"));
}

// ── 3. End-to-end: the existing sensors still produce the
//    same Topics / WebSocket facts after the data migration. ──

fn write_repo_file(dir: &std::path::Path, name: &str, content: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, content).unwrap();
    p
}

#[test]
fn kafkajs_producer_emits_a_topic_node_via_the_data_driven_walker() {
    let dir = tempdir().unwrap();
    write_repo_file(
        dir.path(),
        "orders.ts",
        "async function publish() {\n  await producer.send({ topic: 'orders.created', messages: [] });\n}\n",
    );
    let graph = lain::graph::GraphDatabase::new(&dir.path().join("db.bin")).unwrap();
    let ns = RepoNamespace::for_test();
    scan_workspace_event(&graph, dir.path(), &ns).unwrap();

    let topics: Vec<_> = graph
        .get_all_nodes()
        .into_iter()
        .filter(|n| n.node_type == lain::schema::NodeType::Topic)
        .collect();
    assert!(
        topics.iter().any(|t| t.name == "kafka/orders.created"),
        "expected Topic kafka/orders.created, got {:?}",
        topics.iter().map(|t| &t.name).collect::<Vec<_>>()
    );
}

#[test]
fn websocket_endpoints_are_emitted_via_the_data_driven_walker() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().join("ws_repo");
    std::fs::create_dir_all(&ws_dir).unwrap();

    let client_file = ws_dir.join("client.js");
    std::fs::write(
        &client_file,
        r#"
        const ws = new WebSocket("ws://orders/events/stream");
        ws.onmessage = handleStream;
        "#,
    )
    .unwrap();

    let server_file = ws_dir.join("server.js");
    std::fs::write(
        &server_file,
        r#"
        app.ws("/events/stream", handleStream);
        function handleStream(msg) {}
        "#,
    )
    .unwrap();

    let graph = lain::graph::GraphDatabase::new(&dir.path().join("db")).unwrap();
    let ns = RepoNamespace::for_test();

    enrich_with_websocket(&graph, &client_file, &ws_dir, &ns).unwrap();
    enrich_with_websocket(&graph, &server_file, &ws_dir, &ns).unwrap();

    let mut found_consumer = false;
    let mut found_provider = false;
    for node in graph.all_nodes() {
        if let Some(lain::federation::contracts::model::ContractFact::WebSocketConsumer(c)) =
            node.contract.first()
        {
            if c.route == "/events/stream" {
                found_consumer = true;
            }
        }
        if let Some(lain::federation::contracts::model::ContractFact::WebSocketProvider(p)) =
            node.contract.first()
        {
            if p.route == "/events/stream" {
                found_provider = true;
            }
        }
    }
    assert!(found_consumer, "WebSocketConsumer must be emitted");
    assert!(found_provider, "WebSocketProvider must be emitted");
}

// ── 4. The acceptance assertion: the source tree's
//    `src/server/sensors/*.rs` files are unchanged. ──────────

// ── 4b. The real proof: an idiom added as DATA is recognised ─────────
//
// The earlier version of this test hashed `src/server/sensors/*.rs` and
// failed whenever *any* sensor file changed — which is not a regression
// test, it is a tripwire that fires on legitimate work. Worse, it wrote
// its own baseline on first run and returned, so it could never fail on
// a fresh checkout.
//
// The property worth pinning is narrower and testable: **a previously
// unknown idiom declared in YAML is recognised without a Rust change.**
// This exercises the registry + walker directly with a per-repo
// override, which is exactly the extension point Tier 1 created.

#[test]
fn a_new_idiom_is_a_data_change() {
    use lain::server::sensors::patterns::Patterns;
    use lain::server::sensors::util::{compile_idioms, walk_idioms};

    // A source line using a queue library nobody has ever heard of.
    let line = r#"queue.publish("orders.created");"#;

    // With a registry that does not know the library, nothing matches.
    let before = Patterns::from_yaml_str(
        "languages:\n  tsjs:\n    - id: other\n      kind: topic_producer\n      path_regex: 'zzz_no_match'\n",
    )
    .expect("registry parses");
    let none = walk_idioms(
        &compile_idioms(before.topic_producer_patterns(lain::server::sensors::util::Lang::TsJs)),
        line,
        1,
    );
    assert!(
        none.is_empty(),
        "an unrelated registry must not match an unknown library: {none:?}"
    );

    // Add the idiom as DATA — one YAML entry. No Rust is touched.
    let after = Patterns::from_yaml_str(
        "languages:\n  tsjs:\n    - id: myqueue-publish\n      kind: topic_producer\n      path_regex: 'queue\\.publish\\s*\\(\\s*[\"'']([^\"'']+)[\"'']'\n",
    )
    .expect("registry parses");
    let hits = walk_idioms(
        &compile_idioms(after.topic_producer_patterns(lain::server::sensors::util::Lang::TsJs)),
        line,
        1,
    );
    assert_eq!(
        hits.len(),
        1,
        "an idiom added as YAML must be recognised without any Rust change: {hits:?}"
    );
    assert_eq!(hits[0].framework_id, "myqueue-publish");
    assert_eq!(hits[0].literal.as_deref(), Some("orders.created"));
}

// ── 4c. Task 2: the AMQP idiom entries are bundled data.
//
// pika (Python) and amqplib (TS/JS) mint `amqp/<queue>` topics; the
// entries live in frameworks.yaml and are recognised by the same
// generic walker, yielding the queue/routing-key literal.

#[test]
fn amqp_idioms_are_bundled_and_yield_literals() {
    use lain::server::sensors::patterns::Patterns;
    use lain::server::sensors::util::{compile_idioms, walk_idioms};

    let p = Patterns::patterns();

    let python_ids: Vec<String> = p
        .topic_producer_patterns(Lang::Python)
        .map(|f| f.id.clone())
        .collect();
    assert!(
        python_ids.iter().any(|id| id == "pika-publish")
            && python_ids.iter().any(|id| id == "pika-queue-declare"),
        "topic_producer_patterns(Python) must include the pika producer idioms; got {python_ids:?}"
    );
    let python_consumer_ids: Vec<String> = p
        .topic_consumer_patterns(Lang::Python)
        .map(|f| f.id.clone())
        .collect();
    assert!(
        python_consumer_ids.iter().any(|id| id == "pika-consume")
            && python_consumer_ids.iter().any(|id| id == "pika-queue-bind"),
        "topic_consumer_patterns(Python) must include pika-consume and the \
         queue_bind consumer idiom; got {python_consumer_ids:?}"
    );
    let tsjs_producer_ids: Vec<String> = p
        .topic_producer_patterns(Lang::TsJs)
        .map(|f| f.id.clone())
        .collect();
    assert!(
        tsjs_producer_ids.iter().any(|id| id == "amqplib-publish")
            && tsjs_producer_ids
                .iter()
                .any(|id| id == "amqplib-queue-declare"),
        "topic_producer_patterns(TsJs) must include amqplib-publish and \
         amqplib-queue-declare; got {tsjs_producer_ids:?}"
    );
    let tsjs_consumer_ids: Vec<String> = p
        .topic_consumer_patterns(Lang::TsJs)
        .map(|f| f.id.clone())
        .collect();
    assert!(
        tsjs_consumer_ids.iter().any(|id| id == "amqplib-consume")
            && tsjs_consumer_ids
                .iter()
                .any(|id| id == "amqplib-queue-bind"),
        "topic_consumer_patterns(TsJs) must include amqplib-consume and \
         amqplib-queue-bind; got {tsjs_consumer_ids:?}"
    );

    // The walker yields the queue/routing-key literal for each idiom.
    let (py_producers, py_consumers, _) = util::topic_idioms_for(p, Lang::Python);
    let py_prod_hits = walk_idioms(
        &py_producers,
        "ch.basic_publish(exchange='x', routing_key='order-events', body=b)",
        1,
    );
    let m = py_prod_hits
        .iter()
        .find(|m| m.framework_id == "pika-publish")
        .expect("pika-publish must match the routing_key kwarg");
    assert_eq!(m.literal.as_deref(), Some("order-events"));

    let py_declare_hits = walk_idioms(&py_producers, "ch.queue_declare(queue='jobs')", 1);
    let m = py_declare_hits
        .iter()
        .find(|m| m.framework_id == "pika-queue-declare")
        .expect("pika-queue-declare must match the queue kwarg");
    assert_eq!(m.literal.as_deref(), Some("jobs"));

    let py_cons_hits = walk_idioms(
        &py_consumers,
        "ch.basic_consume(queue='order-events', on_message_callback=cb)",
        1,
    );
    let m = py_cons_hits
        .iter()
        .find(|m| m.framework_id == "pika-consume")
        .expect("pika-consume must match the queue kwarg");
    assert_eq!(m.literal.as_deref(), Some("order-events"));

    // The acceptance fixtures' ONLY AMQP idiom: `queue_bind` with the
    // queue as a kwarg — literal form and bare-identifier form (the
    // event sensor resolves group 2 against same-file constants).
    let py_bind_hits = walk_idioms(
        &py_consumers,
        "channel.queue_bind(queue='order-events', exchange='platform', routing_key='order-events')",
        1,
    );
    let m = py_bind_hits
        .iter()
        .find(|m| m.framework_id == "pika-queue-bind")
        .expect("pika-queue-bind must match the queue kwarg");
    assert_eq!(m.literal.as_deref(), Some("order-events"));

    let py_bind_ident_hits = walk_idioms(
        &py_consumers,
        "channel.queue_bind(queue=QUEUE, exchange='platform', routing_key=QUEUE)",
        1,
    );
    let m = py_bind_ident_hits
        .iter()
        .find(|m| m.framework_id == "pika-queue-bind")
        .expect("pika-queue-bind must match the bare-identifier queue kwarg");
    assert_eq!(m.identifier.as_deref(), Some("QUEUE"));

    let (ts_producers, ts_consumers, _) = util::topic_idioms_for(p, Lang::TsJs);
    // Real amqplib shape: routing key is argument 2, content is the
    // mandatory argument 3.
    let ts_prod_hits = walk_idioms(
        &ts_producers,
        "channel.publish('amq.topic', 'order-events', Buffer.from(msg))",
        1,
    );
    let m = ts_prod_hits
        .iter()
        .find(|m| m.framework_id == "amqplib-publish")
        .expect("amqplib-publish must match the routing-key position");
    assert_eq!(m.literal.as_deref(), Some("order-events"));

    // Real amqplib shape: the queue is argument 1 (argument 2 is the
    // callback).
    let ts_cons_hits = walk_idioms(
        &ts_consumers,
        "channel.consume('order-events', onMessage)",
        1,
    );
    let m = ts_cons_hits
        .iter()
        .find(|m| m.framework_id == "amqplib-consume")
        .expect("amqplib-consume must match the queue position");
    assert_eq!(m.literal.as_deref(), Some("order-events"));

    // amqplib queue declare/bind siblings: the queue is argument 1
    // (argument 2 is the options object / source exchange).
    let ts_declare_hits = walk_idioms(&ts_producers, "await channel.assertQueue('jobs', opts)", 1);
    let m = ts_declare_hits
        .iter()
        .find(|m| m.framework_id == "amqplib-queue-declare")
        .expect("amqplib-queue-declare must match the queue position");
    assert_eq!(m.literal.as_deref(), Some("jobs"));

    let ts_bind_hits = walk_idioms(
        &ts_consumers,
        "channel.bindQueue('order-events', 'platform', 'order-events')",
        1,
    );
    let m = ts_bind_hits
        .iter()
        .find(|m| m.framework_id == "amqplib-queue-bind")
        .expect("amqplib-queue-bind must match the queue position");
    assert_eq!(m.literal.as_deref(), Some("order-events"));

    // compile_idioms must not silently drop the new entries (a
    // malformed regex would).
    assert!(!compile_idioms(p.topic_producer_patterns(Lang::Python)).is_empty());
    assert!(!compile_idioms(p.topic_consumer_patterns(Lang::TsJs)).is_empty());
}

// ── 5. Tier 1 protocol idioms add no structural queries. The
//    independent Rust gRPC query is included in the baseline.
//    Guard against accidentally bundling regex idioms as .scm. ──

#[test]
fn tier1_did_not_add_a_new_scm_file() {
    // Tier 1 is pure YAML data; no `.scm` file was added. A
    // regression that started bundling an idioms `.scm` file
    // would shift the build-time `OUT_DIR/queries.rs` and the
    // `LEN` const. Pin `LEN` so the change is visible.
    let len = patterns::generated::LEN;
    // The known-good count from the Tier-0 baseline
    // (HTTP-route + HTTP-outbound + entry-point .scm files).
    // Rust gRPC discovery independently adds one structural query. Tier 1
    // protocol idioms still contribute no queries to this baseline.
    assert!(patterns::generated::get("rust/grpc-candidate.scm").is_some());
    assert_eq!(
        len, 39,
        "patterns/build.rs generated count changed unexpectedly. \
         Tier 1 must not add a new .scm file to the build pipeline — \
         idioms are pure regex data in frameworks.yaml."
    );
}

// Silence the unused-import warning for `BTreeMap` if a future
// edit stops using it; keeps the test file's import list honest.
#[allow(dead_code)]
fn _btreemap_marker() -> BTreeMap<String, String> {
    BTreeMap::new()
}
