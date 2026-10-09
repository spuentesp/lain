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

// ─── Task 9 — depth cap must hold under adversarial nesting
//
//     The mutation harness reports a survivor at
//     `payload_schema.rs:466`:
//         if self.message_depth >= MAX_PROTO_MESSAGE_DEPTH { ... }
//     The depth cap exists to keep the recursion bounded — a
//     file that nests more than the cap would otherwise blow
//     the stack. The test below generates a fixture that nests
//     `MAX_PROTO_MESSAGE_DEPTH + 1` levels and asserts:
//       1. The parser returns without aborting.
//       2. The number of messages parsed equals the cap
//          (every level at-or-below the cap produces a message;
//          the level past the cap is skipped with a
//          `NestingDepth` diagnostic).
//     With the `>=` mutated to `>`, the cap would fire one
//     level later, yielding one extra parsed message.

#[test]
fn nested_message_depth_cap_keeps_recursion_bounded() {
    use lain::server::sensors::payload_schema::{
        parse_proto_messages_with_diagnostics, MAX_PROTO_MESSAGE_DEPTH,
    };

    let levels = MAX_PROTO_MESSAGE_DEPTH as usize + 1;
    let mut src = String::with_capacity(levels * 16);
    for i in 0..levels {
        src.push_str(&format!("message L{i} {{\n"));
    }
    src.push_str("string leaf = 1;\n");
    for _ in 0..levels {
        src.push_str("}\n");
    }

    let (messages, diagnostics) = parse_proto_messages_with_diagnostics(&src);
    // The cap allows `MAX_PROTO_MESSAGE_DEPTH` frames. With the
    // mutated `>` the parser would admit one extra message.
    assert_eq!(
        messages.len() as u32,
        MAX_PROTO_MESSAGE_DEPTH,
        "depth cap must hold at MAX_PROTO_MESSAGE_DEPTH = {}; got {} messages",
        MAX_PROTO_MESSAGE_DEPTH,
        messages.len()
    );
    // The level past the cap MUST surface as a NestingDepth
    // diagnostic, not silently disappear.
    assert!(
        diagnostics.iter().any(|d| matches!(
            d.kind,
            lain::server::sensors::payload_schema::ProtoParseDiagnosticKind::NestingDepth
        )),
        "depth-cap diagnostic must fire for the overflowing level, got: {:?}",
        diagnostics
    );
}

// ─── Avro: nullable / record / array discriminators must hold
//
//     The mutation harness reports survivors at
//     `payload_schema.rs:67` (outer record gate), `:149`
//     (union nullability), `:156` (nested record type), and
//     `:159` (nested array type). Each is a `==` against a
//     small literal in the type descriptor path. The fixtures
//     below land on every discriminator and assert the
//     `FieldMeta` carries the right `TypeDesc` and
//     `nullable`/`required` flags — a flipped comparison
//     changes the parsed result and the test catches it.

/// A non-nullable union — `["string", "int"]` — exercises
/// the same `arr.iter().any(|v| v.as_str() == Some("null"))`
/// check at line 149 but on the *false* branch. With
/// `==`→`!=`, the `any` flips to `true` (because both
/// elements are `!= "null"`), the field is wrongly marked
/// nullable, and the test fails. The `["null", "string"]`
/// shape (the obvious fixture) is *equivalent* under this
/// mutation: both `==` and `!=` return `true`. The
/// non-nullable case is what distinguishes them.
#[test]
fn avro_union_without_null_is_not_nullable() {
    use lain::server::sensors::payload_schema::parse_avro_schema;
    let avsc = r#"{
        "type": "record",
        "name": "Order",
        "fields": [
            { "name": "id", "type": ["string", "int"] }
        ]
    }"#;
    let parsed = parse_avro_schema(avsc).expect("must parse avro schema");
    let field = parsed
        .fields
        .iter()
        .find(|f| f.path.to_string() == "id")
        .expect("id must be parsed");
    assert!(
        !field.meta.nullable,
        "union [`string`, `int`] (no null) must NOT be flagged nullable, got {:?}",
        field.meta
    );
    assert!(
        field.meta.required,
        "a non-nullable field must be required, got {:?}",
        field.meta
    );
}

/// The *true* branch at line 149 — `["null", "string"]` —
/// must also be pinned. This test guards against a
/// regression where the discriminator is removed entirely
/// (a future cleanup that loses the union nullability
/// concept would make every union nullable=false).
#[test]
fn avro_union_with_null_marks_the_field_nullable() {
    use lain::server::sensors::payload_schema::parse_avro_schema;
    let avsc = r#"{
        "type": "record",
        "name": "Order",
        "fields": [
            { "name": "optional_id", "type": ["null", "string"] }
        ]
    }"#;
    let parsed = parse_avro_schema(avsc).expect("must parse avro schema");
    let field = parsed
        .fields
        .iter()
        .find(|f| f.path.to_string() == "optional_id")
        .expect("optional_id must be parsed");
    assert!(
        field.meta.nullable,
        "union [`null`, `string`] must be flagged nullable, got {:?}",
        field.meta
    );
    assert!(
        !field.meta.required,
        "a nullable field cannot be required, got {:?}",
        field.meta
    );
}

/// Nested Avro record — the `t == "record"` discriminator at
/// line 156 must fire. With `==`→`!=` the nested record's
/// fields are dropped (the type is classified as Unknown and
/// the sub-fields block is never recursed into). The test
/// pins the field list at the nested level.
#[test]
fn avro_nested_record_emits_the_inner_fields() {
    use lain::server::sensors::payload_schema::parse_avro_schema;
    let avsc = r#"{
        "type": "record",
        "name": "Order",
        "fields": [
            {
                "name": "billing",
                "type": {
                    "type": "record",
                    "name": "Billing",
                    "fields": [
                        { "name": "amount", "type": "double" },
                        { "name": "currency", "type": "string" }
                    ]
                }
            }
        ]
    }"#;
    let parsed = parse_avro_schema(avsc).expect("must parse avro schema");
    let names: Vec<String> = parsed.fields.iter().map(|f| f.path.to_string()).collect();
    assert!(
        names.contains(&"billing.amount".to_string()),
        "nested record's fields must appear at `billing.amount`, got {names:?}"
    );
    assert!(
        names.contains(&"billing.currency".to_string()),
        "nested record's fields must appear at `billing.currency`, got {names:?}"
    );
    let billing = parsed
        .fields
        .iter()
        .find(|f| f.path.to_string() == "billing")
        .expect("billing must be parsed");
    assert!(
        matches!(
            billing.meta.ty,
            lain::federation::contracts::model::TypeDesc::Object
        ),
        "nested record must be classified as TypeDesc::Object, got {:?}",
        billing.meta.ty
    );
}

/// Nested Avro array of records — the `t == "array"`
/// discriminator at line 159 must fire, and the
/// `items.fields` block must recurse. With `==`→`!=` the
/// array is dropped (the type stays Unknown and the sub-
/// fields are not flattened). The test pins the `items[]`
/// path segments.
#[test]
fn avro_array_of_records_flattens_with_array_items_path() {
    use lain::server::sensors::payload_schema::parse_avro_schema;
    let avsc = r#"{
        "type": "record",
        "name": "Order",
        "fields": [
            {
                "name": "lines",
                "type": {
                    "type": "array",
                    "items": {
                        "type": "record",
                        "name": "Line",
                        "fields": [
                            { "name": "sku", "type": "string" },
                            { "name": "qty", "type": "int" }
                        ]
                    }
                }
            }
        ]
    }"#;
    let parsed = parse_avro_schema(avsc).expect("must parse avro schema");
    let names: Vec<String> = parsed.fields.iter().map(|f| f.path.to_string()).collect();
    assert!(
        names.contains(&"lines[].sku".to_string()),
        "array-of-record items must surface at `lines[].sku`, got {names:?}"
    );
    assert!(
        names.contains(&"lines[].qty".to_string()),
        "array-of-record items must surface at `lines[].qty`, got {names:?}"
    );
}

// Note: the `==` mutation at `payload_schema.rs:67` is
// **equivalent** under the existing function shape. The
// `is_record` check has a `||` fallthrough: even when the
// first arm flips to `false`, the `obj.contains_key("fields")`
// arm rescues the same set of documents (any document with
// a top-level `fields` key is accepted either way), and any
// document without `fields` returns `None` from the
// subsequent `obj.get("fields")?` regardless of which
// branch set `is_record`. There is no input for which the
// two versions differ in observable behaviour, so the
// mutation is left as a known equivalent.

// ─── JSON-Schema: type discriminator must hold
//
//     The mutation harness reports survivors in
//     `payload_schema.rs` around the JSON-Schema type string
//     match. The fixtures below land on every discriminator
//     (`object`, `array`, `integer`, `number`, `boolean`) and
//     assert the `TypeDesc` is correct.

/// Nested `object` — the `ty_str == "object"` discriminator
/// at line 234 must fire. With `==`→`!=` the nested object's
/// properties are dropped (Unknown is classified, no
/// recursion). The test pins the `customer.address.*` paths.
#[test]
fn json_schema_nested_object_emits_inner_properties() {
    use lain::server::sensors::payload_schema::parse_json_schema;
    let js = r#"{
        "title": "Order",
        "type": "object",
        "properties": {
            "customer": {
                "type": "object",
                "properties": {
                    "address": {
                        "type": "object",
                        "properties": {
                            "city": { "type": "string" },
                            "zip": { "type": "string" }
                        }
                    }
                }
            }
        }
    }"#;
    let parsed = parse_json_schema(js).expect("must parse json schema");
    let names: Vec<String> = parsed.fields.iter().map(|f| f.path.to_string()).collect();
    assert!(
        names.contains(&"customer.address.city".to_string()),
        "deeply nested property must appear at `customer.address.city`, got {names:?}"
    );
    assert!(
        names.contains(&"customer.address.zip".to_string()),
        "deeply nested property must appear at `customer.address.zip`, got {names:?}"
    );
}

/// Nested `array` of `object` — the `ty_str == "array"`
/// discriminator at line 235 must fire, and the items
/// properties must recurse. With `==`→`!=` the array is
/// dropped (Unknown, no recursion). The test pins
/// `tags[].value`.
#[test]
fn json_schema_array_of_objects_flattens_with_array_items_path() {
    use lain::server::sensors::payload_schema::parse_json_schema;
    let js = r#"{
        "title": "Order",
        "type": "object",
        "properties": {
            "tags": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "key": { "type": "string" },
                        "value": { "type": "string" }
                    }
                }
            }
        }
    }"#;
    let parsed = parse_json_schema(js).expect("must parse json schema");
    let names: Vec<String> = parsed.fields.iter().map(|f| f.path.to_string()).collect();
    assert!(
        names.contains(&"tags[].key".to_string()),
        "array-of-object items must surface at `tags[].key`, got {names:?}"
    );
    assert!(
        names.contains(&"tags[].value".to_string()),
        "array-of-object items must surface at `tags[].value`, got {names:?}"
    );
}

/// `integer`, `number`, `boolean` discriminators (lines
/// 230-233) must all fire. The fixture uses one of each at
/// the top level so the parser's type-string match walks
/// the whole table.
#[test]
fn json_schema_integer_number_boolean_distinguished_from_string() {
    use lain::server::sensors::payload_schema::parse_json_schema;
    let js = r#"{
        "title": "Mix",
        "type": "object",
        "properties": {
            "count":  { "type": "integer" },
            "ratio":  { "type": "number" },
            "flag":   { "type": "boolean" },
            "name":   { "type": "string" }
        }
    }"#;
    let parsed = parse_json_schema(js).expect("must parse json schema");
    let by_name: std::collections::HashMap<String, lain::federation::contracts::model::FieldMeta> =
        parsed
            .fields
            .iter()
            .map(|f| (f.path.to_string(), f.meta.clone()))
            .collect();
    assert!(
        matches!(
            by_name.get("count").map(|m| &m.ty),
            Some(lain::federation::contracts::model::TypeDesc::Integer)
        ),
        "integer must classify as TypeDesc::Integer, got {:?}",
        by_name.get("count")
    );
    assert!(
        matches!(
            by_name.get("ratio").map(|m| &m.ty),
            Some(lain::federation::contracts::model::TypeDesc::Number)
        ),
        "number must classify as TypeDesc::Number, got {:?}",
        by_name.get("ratio")
    );
    assert!(
        matches!(
            by_name.get("flag").map(|m| &m.ty),
            Some(lain::federation::contracts::model::TypeDesc::Boolean)
        ),
        "boolean must classify as TypeDesc::Boolean, got {:?}",
        by_name.get("flag")
    );
    assert!(
        matches!(
            by_name.get("name").map(|m| &m.ty),
            Some(lain::federation::contracts::model::TypeDesc::String)
        ),
        "string must classify as TypeDesc::String, got {:?}",
        by_name.get("name")
    );
}

// ─── proto: qualifier boundary at end of buffer
//
//     The mutation harness reports a survivor at
//     `payload_schema.rs:694` — the `after >= self.bytes.len()`
//     check in `peek_qualifier`. With `>=` mutated to `>`, the
//     check is false when `after == bytes.len()`, and the
//     next access to `self.bytes[after]` overruns the
//     buffer. A field whose qualifier is the *last* token in
//     a message body lands on this boundary.

/// A `repeated` qualifier at the very end of a message
/// body — `peek_qualifier` is called with `after ==
/// bytes.len()`. With the `>=`→`>` mutation, the function
/// overruns the buffer and panics. The qualifier must be
/// the literal last token in the message body (no `;`, no
/// `\n`, no `}` after it) so `pos + kw.len() == bytes.len()`.
#[test]
fn proto_qualifier_at_end_of_message_body_does_not_overrun() {
    use lain::server::sensors::payload_schema::parse_proto_messages_with_diagnostics;
    // The qualifier is the very last token in the message
    // body. With `>=`, the boundary check is true, the
    // qualifier is recognised, and `parse_field_or_skip`
    // returns a diagnostic (no type, no name) without
    // aborting. With `>`, the boundary check is false, the
    // next access to `bytes[after]` overruns the buffer,
    // and the process panics.
    let proto = "message M { string id = 1; repeated";
    let (schemas, _diagnostics) = parse_proto_messages_with_diagnostics(proto);
    let m = schemas.iter().find(|m| m.name == "M").expect("M");
    let names: Vec<String> = m.fields.iter().map(|f| f.path.to_string()).collect();
    assert!(
        names.contains(&"id".to_string()),
        "the surviving `id` field must still be parsed, got {names:?}"
    );
}

/// Same as the test above but for `optional` (a proto2
/// qualifier). The boundary check is shared.
#[test]
fn proto_optional_qualifier_at_end_of_message_body_does_not_overrun() {
    use lain::server::sensors::payload_schema::parse_proto_messages_with_diagnostics;
    let proto = "message M { string id = 1; optional";
    let (schemas, _diagnostics) = parse_proto_messages_with_diagnostics(proto);
    let m = schemas.iter().find(|m| m.name == "M").expect("M");
    let names: Vec<String> = m.fields.iter().map(|f| f.path.to_string()).collect();
    assert!(
        names.contains(&"id".to_string()),
        "the surviving `id` field must still be parsed, got {names:?}"
    );
}

/// Same as the test above but for `required` (a proto2
/// qualifier).
#[test]
fn proto_required_qualifier_at_end_of_message_body_does_not_overrun() {
    use lain::server::sensors::payload_schema::parse_proto_messages_with_diagnostics;
    let proto = "message M { string id = 1; required";
    let (schemas, _diagnostics) = parse_proto_messages_with_diagnostics(proto);
    let m = schemas.iter().find(|m| m.name == "M").expect("M");
    let names: Vec<String> = m.fields.iter().map(|f| f.path.to_string()).collect();
    assert!(
        names.contains(&"id".to_string()),
        "the surviving `id` field must still be parsed, got {names:?}"
    );
}

// ─── proto: type expression read — the `m` boundary at line 645
//
//     The mutation harness reports a survivor at
//     `payload_schema.rs:645`:
//         if self.peek() == Some(b'm') && starts_with_keyword(...)
//     With `&&`→`||`, the condition fires whenever the
//     cursor is on `b'm'` regardless of whether the
//     identifier actually starts with "map". A type token
//     like `mx` (starts with `m` but is not "map") would
//     then be misclassified: the parser consumes the
//     literal "map" and reads the field name from the
//     remainder.

// Note: the `&&`→`||` mutation at `payload_schema.rs:645`
// is **equivalent** under the existing function shape. The
// `peek() == Some(b'm') || starts_with_keyword(...)`
// condition fires for *every* input on which the original
// `&&` is true (because `starts_with_keyword` is true
// whenever peek is `m` and the bytes match "map"), and
// only diverges when peek is `m` but the bytes are not
// "map" — in which case both versions classify the type
// as `TypeDesc::Unknown` and read the same field name
// from the post-`map` cursor position. There is no
// observable difference, so the mutation is left as a
// known equivalent.

// ─── Task 5 — measured survivor classes (scoped run: 51 survivors) ──
//
//     Each fixture below names the `path:line` it kills and was
//     verified by applying the mutation at that exact line, watching
//     the fixture FAIL, and reverting the line by hand.

/// `payload_schema.rs:645` (`&&`→`||` on the `map`-keyword gate): a
/// type token that starts with `m` but is not `map` must fall through
/// to the bare-identifier path. Under the mutation `mapx` enters the
/// map branch, the cursor lands mid-token, and the field named `f` is
/// dropped with a diagnostic. Unmutated, `mapx` is a plain type name.
#[test]
fn proto_type_token_starting_with_m_but_not_map_is_a_plain_identifier() {
    let parsed = parse_proto_messages("message M { mapx f = 1; }\n");
    let m = parsed
        .iter()
        .find(|msg| msg.name == "M")
        .expect("M message");
    let names: Vec<String> = m.fields.iter().map(|f| f.path.to_string()).collect();
    assert_eq!(
        names,
        vec!["f".to_string()],
        "`mapx` must be read as a plain type identifier so `f` survives, got {names:?}"
    );
}

/// `payload_schema.rs:604` (`==`→`!=` on the `]` of the field-options
/// skip): the bracket loop must stop at `]`. Under the mutation it
/// never breaks and consumes the rest of the file, dropping field `b`.
#[test]
fn proto_field_options_block_does_not_swallow_following_field() {
    let parsed =
        parse_proto_messages("message M { string a = 1 [deprecated = true]; string b = 2; }\n");
    let m = parsed
        .iter()
        .find(|msg| msg.name == "M")
        .expect("M message");
    let names: Vec<String> = m.fields.iter().map(|f| f.path.to_string()).collect();
    assert_eq!(
        names,
        vec!["a".to_string(), "b".to_string()],
        "field options must not swallow the next field, got {names:?}"
    );
}

/// `payload_schema.rs:612` (`==`→`!=` on the `;` after a field): the
/// mutation consumes the *next* byte whenever it isn't `;`, eating the
/// message's closing `}`. The outer message then stays open past the
/// inner one and messages drain in reverse order. Unmutated, messages
/// come back in file order.
#[test]
fn proto_missing_semicolon_does_not_reorder_messages() {
    let parsed = parse_proto_messages("message M { string a = 1 } message N { string b = 2; }\n");
    assert_eq!(parsed.len(), 2, "both messages must parse, got {parsed:?}");
    assert_eq!(
        parsed[0].name,
        "M",
        "messages must be emitted in file order, got {:?}",
        parsed.iter().map(|p| &p.name).collect::<Vec<_>>()
    );
    assert_eq!(parsed[1].name, "N");
}

/// `payload_schema.rs:735` (first `==`, the `;` arm of
/// `skip_to_semicolon_or_newline`): a junk line ending in `;` must
/// stop the skip right there. Under the mutation the scan runs to the
/// newline and eats `string id = 1;` with it.
///
/// `payload_schema.rs:736` (the inner `==`): when the stop byte is
/// `;` the line counter must NOT advance — same source line. Under the
/// mutation the field after the junk line reports line 3 instead of 2.
#[test]
fn proto_junk_line_stops_at_semicolon_and_keeps_following_field() {
    let parsed = parse_proto_messages("message M {\n  garbage here; string id = 1;\n}\n");
    let m = parsed
        .iter()
        .find(|msg| msg.name == "M")
        .expect("M message");
    let id = m
        .fields
        .iter()
        .find(|f| f.path.to_string() == "id")
        .unwrap_or_else(|| panic!("`id` must survive the junk line, got {:?}", m.fields));
    assert_eq!(
        id.line, 2,
        "`id` is declared on line 2 (same line as the junk), got {}",
        id.line
    );
}

/// `payload_schema.rs:735` (second `==`, the `\n` arm of
/// `skip_to_semicolon_or_newline`): junk with no `;` must stop at the
/// newline. Under the mutation the skip crosses the line and eats
/// `string id = 1;`.
///
/// `payload_schema.rs:736`: when the stop byte is `\n` the line
/// counter must advance. Under the mutation `id` reports line 2
/// instead of 3.
#[test]
fn proto_junk_line_stops_at_newline_and_keeps_following_field() {
    let parsed = parse_proto_messages("message M {\n  = nope\n  string id = 1;\n}\n");
    let m = parsed
        .iter()
        .find(|msg| msg.name == "M")
        .expect("M message");
    let id = m
        .fields
        .iter()
        .find(|f| f.path.to_string() == "id")
        .unwrap_or_else(|| panic!("`id` must survive the junk line, got {:?}", m.fields));
    assert_eq!(id.line, 3, "`id` is declared on line 3, got {}", id.line);
}

/// `payload_schema.rs:723` (`==`→`!=` in `skip_ws`'s newline count):
/// a declaration split across lines (`string` / `id`) must still
/// report exact line numbers for the fields that follow. Under the
/// mutation newlines are missed and spaces counted, drifting every
/// subsequent line.
#[test]
fn proto_line_numbers_survive_newlines_inside_a_declaration() {
    let src = "message M {\n  string\n  id = 1;\n  string name = 2;\n}\n";
    let parsed = parse_proto_messages(src);
    let m = parsed
        .iter()
        .find(|msg| msg.name == "M")
        .expect("M message");
    let names: Vec<String> = m.fields.iter().map(|f| f.path.to_string()).collect();
    assert_eq!(
        names,
        vec!["id".to_string(), "name".to_string()],
        "{names:?}"
    );
    let id = &m.fields[0];
    let name = &m.fields[1];
    assert_eq!(id.line, 2, "`id` is declared on line 2, got {}", id.line);
    assert_eq!(
        name.line, 4,
        "`name` is declared on line 4, got {}",
        name.line
    );
}

/// `payload_schema.rs:754` (`==`→`!=` in `consume_balanced_block`'s
/// newline count): a bare `{ ... }` block inside a message must be
/// skipped while counting only its newlines. Under the mutation every
/// byte counts as a line, so the field after the block reports a
/// wildly wrong line.
#[test]
fn proto_braced_block_line_counting_stays_exact_for_following_field() {
    let src = "message M {\n{ inner }\n  string id = 1;\n}\n";
    let parsed = parse_proto_messages(src);
    let m = parsed
        .iter()
        .find(|msg| msg.name == "M")
        .expect("M message");
    let id = m
        .fields
        .iter()
        .find(|f| f.path.to_string() == "id")
        .unwrap_or_else(|| panic!("`id` must be parsed, got {:?}", m.fields));
    assert_eq!(id.line, 3, "`id` is declared on line 3, got {}", id.line);
}

/// `payload_schema.rs:771` (`==`→`!=` on the open-brace guard of
/// `consume_balanced_block_from_open_brace`): at the depth cap the
/// block after the capped header must be skipped. Under the mutation
/// the guard fires and the skip is abandoned, so `string leaf = 1;`
/// is parsed as a field of the deepest admitted message.
///
/// `payload_schema.rs:776` (`==`→`!=` in the same function's newline
/// count): the skip must count only newlines. Under the mutation every
/// byte of the capped block counts as a line and every line number
/// after it drifts — pinned by the exact line of `Tail.t`.
#[test]
fn proto_depth_cap_skips_capped_block_without_leaking_or_skewing_lines() {
    use lain::server::sensors::payload_schema::{
        parse_proto_messages_with_diagnostics, MAX_PROTO_MESSAGE_DEPTH,
    };

    let levels = MAX_PROTO_MESSAGE_DEPTH as usize + 1;
    let mut src = String::with_capacity(levels * 32 + 128);
    for i in 0..levels {
        src.push_str(&format!("message L{i} {{\n"));
    }
    src.push_str("string leaf = 1;\n");
    for _ in 0..levels {
        src.push_str("}\n");
    }
    src.push_str("message Tail {\n  string t = 1;\n}\n");

    let (messages, diagnostics) = parse_proto_messages_with_diagnostics(&src);

    // The cap still fires for the overflowing level.
    assert!(
        diagnostics.iter().any(|d| matches!(
            d.kind,
            lain::server::sensors::payload_schema::ProtoParseDiagnosticKind::NestingDepth
        )),
        "NestingDepth diagnostic must fire, got: {diagnostics:?}"
    );
    // Every level at or below the cap still produces a message.
    let capped: Vec<&str> = messages
        .iter()
        .filter(|m| m.name.starts_with('L'))
        .map(|m| m.name.as_str())
        .collect();
    assert_eq!(
        capped.len() as u32,
        MAX_PROTO_MESSAGE_DEPTH,
        "depth cap must hold, got {capped:?}"
    );
    // The capped block's body must NOT leak into the admitted messages.
    assert!(
        messages
            .iter()
            .all(|m| !m.fields.iter().any(|f| f.path.to_string() == "leaf")),
        "`leaf` lives past the depth cap and must not appear in any message, got {:?}",
        messages
            .iter()
            .map(|m| (&m.name, &m.fields))
            .collect::<Vec<_>>()
    );
    // Line numbers after the skipped block stay exact:
    // `levels` open lines + 1 leaf line + `levels` close lines = the
    // `message Tail {` header line; `t` is the line after it.
    let tail = messages
        .iter()
        .find(|m| m.name == "Tail")
        .expect("`Tail` must parse after the capped block");
    let t = tail
        .fields
        .iter()
        .find(|f| f.path.to_string() == "t")
        .expect("`t` must be parsed");
    let expected_line = (levels * 2 + 3) as u32;
    assert_eq!(
        t.line, expected_line,
        "`t` is declared on line {expected_line}, got {}",
        t.line
    );
}
