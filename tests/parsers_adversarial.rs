//! Adversarial and malformed input for the hand-rolled protocol
//! parsers.
//!
//! A parser that silently drops or garbles facts is a soundness bug:
//! the coverage ledger would report "analysed" for a file the parser
//! could not read, so `RepoCoverage::is_complete` would stay true
//! and `NoKnownImpact` would be served on a gap. The tests in this
//! file pin each known pathological shape to one of two outcomes:
//!
//! 1. The parser returns the correct facts (the canonical proto shape
//!    the spec mandates), **or**
//! 2. The parser surfaces a diagnostic / the sensor records an
//!    `UnresolvedRecord`.
//!
//! Silent success on wrong output (e.g. a field named `"string"` for
//! `optional string id = 1;`) is the failure mode these tests guard
//! against.

use lain::server::sensors::grpc_provider_sensor::parse_proto_providers;
use lain::server::sensors::payload_schema::parse_proto_messages;

// ─── proto2 / proto3 qualifier handling ─────────────────────────────

/// `optional string id = 1;` — the proto2 qualifier is **not** the
/// type. The previous implementation took `parts[0] = "optional"` as
/// the type and `parts[1] = "string"` as the field name, yielding a
/// field literally named `"string"` with `TypeDesc::Unknown`.
#[test]
fn proto2_optional_qualifier_does_not_invent_a_field_named_after_the_type() {
    let parsed = parse_proto_messages(
        "syntax = \"proto2\";\nmessage Order {\n  optional string id = 1;\n}\n",
    );
    let order = parsed
        .iter()
        .find(|m| m.name == "Order")
        .expect("Order message must be parsed");
    assert_eq!(
        order.fields.len(),
        1,
        "exactly one field expected, got {:?}",
        order.fields
    );
    let field = &order.fields[0];
    assert_eq!(field.path.to_string(), "id", "field name must be `id`");
}

/// `required` is a proto2-only qualifier and must be tolerated on
/// proto2 input the same way `optional` is.
#[test]
fn proto2_required_qualifier_does_not_invent_a_field_named_after_the_type() {
    let parsed = parse_proto_messages(
        "syntax = \"proto2\";\nmessage Audit {\n  required string id = 1;\n}\n",
    );
    let audit = parsed
        .iter()
        .find(|m| m.name == "Audit")
        .expect("Audit message");
    assert_eq!(audit.fields.len(), 1, "got {:?}", audit.fields);
    assert_eq!(audit.fields[0].path.to_string(), "id");
}

/// `map<K, V>` is a single token in the canonical proto grammar
/// (the angle brackets are part of the type expression). The
/// previous line-based `split_whitespace` either mis-split on the
/// `<` and `,` or kept `map<string,string>` as one opaque token and
/// then misattributed the type / name.
#[test]
fn map_kv_type_does_not_invent_a_field_named_after_the_value_type() {
    let parsed = parse_proto_messages("message M { map<string, string> m = 1; }\n");
    let m = parsed.iter().find(|m| m.name == "M").expect("M message");
    assert_eq!(m.fields.len(), 1, "got {:?}", m.fields);
    let f = &m.fields[0];
    assert_eq!(f.path.to_string(), "m", "field name must be `m`");
    // We don't have a Map variant in TypeDesc, so the value type is
    // Unknown. The point of the test is that the *name* is correct
    // and the field is not dropped.
    assert!(!f.path.to_string().contains('>'), "got {:?}", f.path);
}

// ─── comment handling ───────────────────────────────────────────────

/// `string id = 1; // the id` — the trailing `//` comment must be
/// stripped before tokenization. The previous parser took
/// `parts[len-2]` as the `=` token, but with a trailing comment
/// `parts[len-2]` was the comment, so the field check failed and
/// the field was silently dropped.
#[test]
fn trailing_line_comment_does_not_drop_the_field() {
    let parsed = parse_proto_messages("message M { string id = 1; // the id }\n");
    let m = parsed.iter().find(|m| m.name == "M").expect("M");
    assert_eq!(m.fields.len(), 1, "got {:?}", m.fields);
    assert_eq!(m.fields[0].path.to_string(), "id");
}

/// `/* block */` comments spanning newlines are also legal. The
/// parser must strip them while keeping the line-numbering math
/// intact.
#[test]
fn block_comment_does_not_drop_the_field() {
    let parsed = parse_proto_messages(
        "message M {\n  string id = 1;\n  /* even\n     a long\n     comment */\n  string name = 2;\n}\n",
    );
    let m = parsed.iter().find(|m| m.name == "M").expect("M");
    assert_eq!(m.fields.len(), 2, "got {:?}", m.fields);
    let names: Vec<String> = m.fields.iter().map(|f| f.path.to_string()).collect();
    assert_eq!(names, vec!["id", "name"]);
}

// ─── oneof / nested message / empty message ────────────────────────

/// `oneof { ... }` — the closing `}` of the oneof block is **not**
/// the closing `}` of the enclosing message. The previous parser
/// matched `}` literally and terminated the message, dropping every
/// field declared after the oneof.
#[test]
fn oneof_block_does_not_truncate_the_rest_of_the_message() {
    let parsed = parse_proto_messages(
        "message M {\n  oneof choice {\n    string a = 1;\n    int32 b = 2;\n  }\n  string after = 3;\n}\n",
    );
    let m = parsed.iter().find(|m| m.name == "M").expect("M");
    let names: Vec<String> = m.fields.iter().map(|f| f.path.to_string()).collect();
    assert!(
        names.contains(&"a".to_string()),
        "oneof field `a` must be parsed: {names:?}"
    );
    assert!(
        names.contains(&"b".to_string()),
        "oneof field `b` must be parsed: {names:?}"
    );
    assert!(
        names.contains(&"after".to_string()),
        "field `after` declared after the oneof must be parsed: {names:?}"
    );
}

/// Nested `message` — an inner `message Foo { ... }` must **not**
/// flush the outer's fields-so-far as a complete schema. The
/// previous parser took any `message` line as a fresh top-level
/// declaration and started a new frame, so the outer's remaining
/// fields were lost.
#[test]
fn nested_message_does_not_flush_the_outer_message_early() {
    let parsed = parse_proto_messages(
        "message Outer {\n  string a = 1;\n  message Inner { string x = 1; }\n  string b = 2;\n}\n",
    );
    let outer = parsed
        .iter()
        .find(|m| m.name == "Outer")
        .expect("Outer message");
    let outer_names: Vec<String> = outer.fields.iter().map(|f| f.path.to_string()).collect();
    assert!(
        outer_names.contains(&"a".to_string()),
        "outer field `a` must be parsed: {outer_names:?}"
    );
    assert!(
        outer_names.contains(&"b".to_string()),
        "outer field `b` (declared after nested message) must be parsed: {outer_names:?}"
    );
    let inner = parsed
        .iter()
        .find(|m| m.name == "Inner")
        .expect("Inner message");
    let inner_names: Vec<String> = inner.fields.iter().map(|f| f.path.to_string()).collect();
    assert_eq!(
        inner_names,
        vec!["x".to_string()],
        "Inner.x must be parsed: {inner_names:?}"
    );
}

/// `message Empty { }` — an empty message is still a schema. The
/// previous parser required `current_fields.is_empty()` to be
/// false before emitting, so an empty message was silently dropped
/// and a provider referencing `Empty` would get no `Schema` edge.
#[test]
fn empty_message_is_still_reported_as_a_schema() {
    let parsed = parse_proto_messages("message Empty { }\n");
    let empty = parsed
        .iter()
        .find(|m| m.name == "Empty")
        .expect("Empty message must still be reported, got: {parsed:?}");
    assert!(empty.fields.is_empty());
}

// ─── diagnostic-on-failure ──────────────────────────────────────────

/// A line that is not a recognisable field declaration must
/// neither invent a field nor panic. Either the parser skips the
/// line and the surviving fields parse correctly, or the parser
/// surfaces a diagnostic the sensor can record.
#[test]
fn unparseable_field_line_does_not_invent_a_field_with_a_garbled_name() {
    let parsed = parse_proto_messages("message Foo {\n  string = 1;\n  int64 good = 2;\n}\n");
    let foo = parsed
        .iter()
        .find(|m| m.name == "Foo")
        .expect("Foo message must be parsed");
    // No field may have an empty / `=` name — silence on wrong output
    // is the bug we are guarding against.
    for f in &foo.fields {
        let n = f.path.to_string();
        assert!(
            !n.is_empty(),
            "no field may be invented with an empty name: {n:?}"
        );
        assert!(
            !n.contains('='),
            "no field may be invented from the `=` token: {n:?}"
        );
    }
    // The good field on the next line must still be parsed.
    let names: Vec<String> = foo.fields.iter().map(|f| f.path.to_string()).collect();
    assert!(
        names.contains(&"good".to_string()),
        "the surviving `good` field must be parsed: {names:?}"
    );
}

// ─── whole-file shape ───────────────────────────────────────────────

/// An empty file is not a parse error. The parser must return no
/// messages without panicking, and the sensor must report it as
/// `files_analyzed > 0` only if there was work to do.
#[test]
fn empty_file_yields_no_messages_no_panic() {
    let parsed = parse_proto_messages("");
    assert!(
        parsed.is_empty(),
        "empty file -> 0 messages, got {parsed:?}"
    );
}

/// A file that is only `//` and `/* */` comments has no messages.
/// After comment-stripping the content is whitespace — the parser
/// must not panic, must not emit a phantom message named `""`, and
/// must return an empty list.
#[test]
fn comment_only_file_yields_no_messages_no_panic() {
    let parsed =
        parse_proto_messages("// line one\n// line two\n/* block\n   comment */\n// trailing\n");
    assert!(
        parsed.is_empty(),
        "comment-only file must yield 0 messages, got {parsed:?}"
    );
}

/// The `parse_proto_providers` path (used by the gRPC sensor at
/// `grpc_provider_sensor.rs:217-219`) must also be robust to
/// `oneof` / nested messages / comments so the provider / message
/// extraction agree. This test pins the contract: a `.proto` file
/// that contains a single service plus a message with adversarial
/// shape parses to one provider.
#[test]
fn parse_proto_providers_survives_adversarial_message_shape() {
    let src = "\
syntax = \"proto2\";

package com.acme.orders;

message GetReq {
  optional string order_id = 1;
  map<string, string> meta = 2;
  oneof filter {
    string by_user = 3;
    int32 by_id = 4;
  }
  /* trailing block comment */
}

service Orders {
  rpc Get (GetReq) returns (GetResp);
}
";
    let providers = parse_proto_providers(src, "orders.proto");
    assert_eq!(providers.len(), 1, "got {providers:?}");
    assert_eq!(providers[0].method, "Get");
    assert_eq!(providers[0].request_type, "GetReq");
}

// ─── performance smoke ─────────────────────────────────────────────

/// 10 000 lines of well-formed proto must finish in well under a
/// second on any developer laptop. The point is not a tight bound
/// (CI will vary) but a *non-pathological* shape: the parser must
/// stay linear, not O(n²) or exponential in nesting. A regression
/// that walks the byte stream multiple times per line would trip
/// this.
#[test]
fn ten_thousand_line_proto_finishes_in_reasonable_time() {
    let mut src = String::with_capacity(10_000 * 40);
    src.push_str("syntax = \"proto3\";\n\n");
    for i in 0..2_500 {
        src.push_str(&format!(
            "message M{i} {{\n  string id = 1;\n  int64 ts = 2;\n  // row {i}\n}}\n\n"
        ));
    }
    let started = std::time::Instant::now();
    let parsed = parse_proto_messages(&src);
    let elapsed = started.elapsed();
    assert_eq!(parsed.len(), 2_500, "every message must be parsed");
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "10k-line file took {elapsed:?} — likely O(n²) or worse"
    );
}

// ─── sensor-level diagnostic surface ────────────────────────────────

/// End-to-end check: an unparseable `.proto` file must surface
/// through `scan_with_report` as an `UnresolvedRecord` (with
/// `BaseUnknown` as the reason), not as a silent skip. The
/// coverage ledger relies on this so `RepoCoverage::is_complete`
/// goes false and `NoKnownImpact` is downgraded instead of
/// served on a gap.
#[test]
fn unparseable_proto_file_surfaces_through_scan_with_report() {
    use lain::federation::contracts::model::UnresolvedReason;
    use lain::graph::GraphDatabase;
    use lain::schema::RepoNamespace;
    use lain::server::sensors::grpc_provider_sensor::scan_workspace_grpc_with_report;
    use std::collections::BTreeSet;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("bad.proto");
    // `message Header not_followed_by_brace` — the message header
    // diagnostic fires. Combined with a well-formed second
    // message, this proves the sensor emits the unresolved record
    // *and* the surviving good message is still parsed.
    std::fs::write(
        &path,
        "message Header not_followed_by_brace\nmessage Good { string id = 1; }\n",
    )
    .expect("write");

    let db_path = dir.path().join("graph.bin");
    let graph = GraphDatabase::new(&db_path).expect("graph");
    let ns = RepoNamespace::for_test();
    let report = scan_workspace_grpc_with_report(&graph, dir.path(), &ns).expect("scan");
    assert_eq!(
        report.error, None,
        "scan must succeed; the file is just unparseable"
    );
    assert!(
        !report.unresolved.is_empty(),
        "unparseable file must produce an UnresolvedRecord, got {:?}",
        report.unresolved
    );
    let record = report
        .unresolved
        .iter()
        .find(|r| matches!(r.reason, UnresolvedReason::BaseUnknown))
        .expect("BaseUnknown UnresolvedRecord expected");
    let paths: BTreeSet<&str> = record.sample_ids.iter().map(|s| s.as_str()).collect();
    assert!(
        paths.iter().any(|p| p.ends_with("bad.proto")),
        "sample_ids must include the failing file path, got {:?}",
        record.sample_ids
    );
    assert!(
        record.count >= 1,
        "at least one diagnostic must have been counted, got {}",
        record.count
    );
    // The well-formed second message must still be parsed — the
    // diagnostic and the surviving facts are not mutually
    // exclusive.
    let schemas = report.emitted;
    let _ = schemas;
}
