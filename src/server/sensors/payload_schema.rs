//! Payload schema parser for event topics and RPC messages.
//!
//! Parses:
//! - Avro schemas (`.avsc`, Avro JSON)
//! - JSON Schemas (`.json`, `.schema.json`)
//! - Protobuf messages (`.proto`)
//!
//! Emits [`ParsedPayloadSchema`] records containing flattened [`ParsedField`]s
//! with JSON paths and [`FieldMeta`] type descriptors.

use crate::federation::contracts::model::{FieldMeta, JsonPath, PathSegment, TypeDesc};
use crate::server::sensors::util_tokenize::{
    find_matching_close, join_continued_lines, starts_with_keyword, strip_comments, CommentSyntax,
};
use std::collections::BTreeSet;

/// A parsed payload schema extracted from a schema file.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedPayloadSchema {
    pub name: String,
    pub topic: Option<String>,
    pub fields: Vec<ParsedField>,
}

/// One field extracted from a payload schema.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedField {
    pub path: JsonPath,
    pub meta: FieldMeta,
    pub line: u32,
}

/// What went wrong parsing a `.proto` file. The sensor turns these
/// into `UnresolvedRecord`s so the coverage ledger can see the gap
/// instead of silently passing on a file the parser could not
/// analyse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtoParseDiagnostic {
    pub line: u32,
    pub kind: ProtoParseDiagnosticKind,
    pub message: String,
}

/// Diagnostic categories. The sensor can group / count by kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtoParseDiagnosticKind {
    /// A `message` line was not followed by `{` (e.g. truncated
    /// file, garbage between `message Name` and the body).
    MessageHeader,
    /// A `oneof` line was not followed by `{`.
    OneofHeader,
    /// The field's type expression could not be classified.
    UnparseableField,
    /// A line inside a message body that looked like a field
    /// declaration but did not match the `qualifier? type name = tag;`
    /// shape. The line was dropped.
    Skipped,
    /// Nested `message` depth exceeded the safety cap.
    NestingDepth,
}

/// Parse an Avro schema JSON string.
pub fn parse_avro_schema(content: &str) -> Option<ParsedPayloadSchema> {
    let value: serde_json::Value = serde_json::from_str(content).ok()?;
    let obj = value.as_object()?;
    let is_record =
        obj.get("type").and_then(|t| t.as_str()) == Some("record") || obj.contains_key("fields");
    if !is_record {
        return None;
    }
    let name = obj
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("Payload")
        .to_string();
    let topic = obj
        .get("topic")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string());
    let fields_arr = obj.get("fields").and_then(|f| f.as_array())?;

    let mut fields = Vec::new();
    flatten_avro_fields(fields_arr, &JsonPath(Vec::new()), &mut fields);
    Some(ParsedPayloadSchema {
        name,
        topic,
        fields,
    })
}

fn flatten_avro_fields(
    fields_arr: &[serde_json::Value],
    parent_path: &JsonPath,
    out: &mut Vec<ParsedField>,
) {
    for (idx, f_val) in fields_arr.iter().enumerate() {
        let Some(f_obj) = f_val.as_object() else {
            continue;
        };
        let Some(field_name) = f_obj.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let mut cur_path = parent_path.clone();
        cur_path.0.push(PathSegment::Name(field_name.to_string()));
        let type_val = f_obj.get("type");
        let (ty, nullable, nested_fields, is_array) = parse_avro_type(type_val);

        out.push(ParsedField {
            path: cur_path.clone(),
            meta: FieldMeta {
                ty,
                required: !nullable,
                nullable,
                enum_values: None,
            },
            line: (idx as u32) + 1,
        });

        if let Some(sub_fields) = nested_fields {
            if is_array {
                let mut array_path = cur_path.clone();
                array_path.0.push(PathSegment::ArrayItems);
                flatten_avro_fields(&sub_fields, &array_path, out);
            } else {
                flatten_avro_fields(&sub_fields, &cur_path, out);
            }
        }
    }
}

fn parse_avro_type(
    type_val: Option<&serde_json::Value>,
) -> (TypeDesc, bool, Option<Vec<serde_json::Value>>, bool) {
    let Some(val) = type_val else {
        return (TypeDesc::Unknown, false, None, false);
    };

    match val {
        serde_json::Value::String(s) => match s.as_str() {
            "string" => (TypeDesc::String, false, None, false),
            "int" | "long" => (TypeDesc::Integer, false, None, false),
            "float" | "double" => (TypeDesc::Number, false, None, false),
            "boolean" => (TypeDesc::Boolean, false, None, false),
            "bytes" => (TypeDesc::String, false, None, false),
            _ => (TypeDesc::Unknown, false, None, false),
        },
        serde_json::Value::Array(arr) => {
            // Union type, e.g. ["null", "string"]
            let nullable = arr.iter().any(|v| v.as_str() == Some("null"));
            let non_null = arr.iter().find(|v| v.as_str() != Some("null"));
            let (ty, _, sub, is_arr) = parse_avro_type(non_null);
            (ty, nullable, sub, is_arr)
        }
        serde_json::Value::Object(obj) => {
            let t = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if t == "record" {
                let sub = obj.get("fields").and_then(|f| f.as_array()).cloned();
                (TypeDesc::Object, false, sub, false)
            } else if t == "array" {
                let items = obj.get("items");
                let (item_ty, nullable, sub, _) = parse_avro_type(items);
                if sub.is_some() {
                    (
                        TypeDesc::Array(Box::new(TypeDesc::Object)),
                        nullable,
                        sub,
                        true,
                    )
                } else {
                    (TypeDesc::Array(Box::new(item_ty)), nullable, None, false)
                }
            } else {
                (TypeDesc::Unknown, false, None, false)
            }
        }
        _ => (TypeDesc::Unknown, false, None, false),
    }
}

/// Parse a JSON schema with `properties`.
pub fn parse_json_schema(content: &str) -> Option<ParsedPayloadSchema> {
    let value: serde_json::Value = serde_json::from_str(content).ok()?;
    let obj = value.as_object()?;
    let properties = obj.get("properties").and_then(|p| p.as_object())?;
    let title = obj
        .get("title")
        .or_else(|| obj.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("Payload")
        .to_string();
    let topic = obj
        .get("topic")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string());
    let required_set: BTreeSet<&str> = obj
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let mut fields = Vec::new();
    flatten_json_schema_properties(
        properties,
        &required_set,
        &JsonPath(Vec::new()),
        &mut fields,
    );
    Some(ParsedPayloadSchema {
        name: title,
        topic,
        fields,
    })
}

fn flatten_json_schema_properties(
    properties: &serde_json::Map<String, serde_json::Value>,
    parent_required: &BTreeSet<&str>,
    parent_path: &JsonPath,
    out: &mut Vec<ParsedField>,
) {
    for (idx, (prop_name, prop_val)) in properties.iter().enumerate() {
        let Some(p_obj) = prop_val.as_object() else {
            continue;
        };
        let mut cur_path = parent_path.clone();
        cur_path.0.push(PathSegment::Name(prop_name.clone()));

        let ty_str = p_obj.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let (ty, nullable) = match ty_str {
            "string" => (TypeDesc::String, false),
            "integer" => (TypeDesc::Integer, false),
            "number" => (TypeDesc::Number, false),
            "boolean" => (TypeDesc::Boolean, false),
            "object" => (TypeDesc::Object, false),
            "array" => (TypeDesc::Array(Box::new(TypeDesc::Unknown)), false),
            _ => (TypeDesc::Unknown, false),
        };
        let required = parent_required.contains(prop_name.as_str());

        out.push(ParsedField {
            path: cur_path.clone(),
            meta: FieldMeta {
                ty,
                required,
                nullable,
                enum_values: None,
            },
            line: (idx as u32) + 1,
        });

        if let Some(sub_props) = p_obj.get("properties").and_then(|p| p.as_object()) {
            let sub_req: BTreeSet<&str> = p_obj
                .get("required")
                .and_then(|r| r.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            flatten_json_schema_properties(sub_props, &sub_req, &cur_path, out);
        } else if let Some(items) = p_obj.get("items").and_then(|i| i.as_object()) {
            if let Some(sub_props) = items.get("properties").and_then(|p| p.as_object()) {
                let mut array_path = cur_path.clone();
                array_path.0.push(PathSegment::ArrayItems);
                let sub_req: BTreeSet<&str> = items
                    .get("required")
                    .and_then(|r| r.as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();
                flatten_json_schema_properties(sub_props, &sub_req, &array_path, out);
            }
        }
    }
}

/// Parse Protobuf messages in `.proto` files into payload schemas.
///
/// The previous implementation was a single-pass line walker that
/// silently mis-parsed ordinary proto shapes: `optional string id`
/// became a field named `"string"`, `string id = 1; // the id` was
/// dropped, `oneof { … }` truncated the enclosing message, and an
/// empty `message Foo { }` was not emitted at all. The replacement
/// routes the file through the same comment-strip + line-continuation
/// join the gRPC provider parser already uses, then walks the bytes
/// with a small state machine that tracks `message` and `oneof` block
/// nesting, a per-message field buffer, and a depth cap. Diagnostics
/// for unparseable shapes are exposed via
/// [`parse_proto_messages_with_diagnostics`] so the sensor can
/// surface them through `ScanReport::unresolved`.
pub fn parse_proto_messages(content: &str) -> Vec<ParsedPayloadSchema> {
    parse_proto_messages_with_diagnostics(content).0
}

/// Maximum nested `message` depth the parser will descend into. The
/// parser pushes a frame per nested message; an adversarial file
/// that nests thousands of messages would otherwise blow the stack
/// and (worse) the recursion is unnecessary — proto disallows
/// non-trivial nesting in practice. 64 is a generous cap (a 64-level
/// deep `message` is unidiomatic proto, not real code) and matches
/// the depth most tree-sitter walkers use.
pub const MAX_PROTO_MESSAGE_DEPTH: u32 = 64;

/// Parse proto messages and return diagnostics for every shape the
/// parser could not classify. See [`parse_proto_messages`] for the
/// design notes; the diagnostics list is the extra output the
/// previous parser swallowed.
pub fn parse_proto_messages_with_diagnostics(
    content: &str,
) -> (Vec<ParsedPayloadSchema>, Vec<ProtoParseDiagnostic>) {
    let stripped = strip_comments(content, CommentSyntax::CStyle);
    let joined = join_continued_lines(&stripped);

    let mut parser = ProtoMessageParser::new(&joined);
    parser.run();
    (parser.messages, parser.diagnostics)
}

struct ProtoMessageParser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    line: u32,
    messages: Vec<ParsedPayloadSchema>,
    diagnostics: Vec<ProtoParseDiagnostic>,
    /// Stack of open message frames plus marker frames for blocks
    /// that must be skipped past (`option { … }`, `extensions { … }`,
    /// anonymous `oneof` blocks). The innermost message is the
    /// field-collecting target.
    frames: Vec<Frame>,
    /// How many `Message` frames are currently open. Bounded by
    /// [`MAX_PROTO_MESSAGE_DEPTH`].
    message_depth: u32,
}

#[allow(dead_code)]
enum Frame {
    /// An open `message Foo { … }` whose fields we are collecting.
    Message {
        name: String,
        fields: Vec<ParsedField>,
        line: u32,
    },
    /// An `oneof Foo { … }` block — its inner fields are added to
    /// the enclosing `Message` frame, the closing `}` only pops
    /// this frame.
    Oneof,
    /// A brace-balanced block we don't otherwise care about
    /// (`option { … }`, `extensions 100 to 200 { … }`). Parser
    /// state does not change inside.
    Skip,
}

impl<'a> ProtoMessageParser<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            pos: 0,
            line: 1,
            messages: Vec::new(),
            diagnostics: Vec::new(),
            frames: Vec::new(),
            message_depth: 0,
        }
    }

    fn run(&mut self) {
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            // Track newlines for line-number math.
            if b == b'\n' {
                self.line += 1;
                self.pos += 1;
                continue;
            }
            if (b as char).is_ascii_whitespace() {
                self.pos += 1;
                continue;
            }
            // Top-level / outer keyword handling. These are
            // recognised at any nesting depth, not just the
            // outermost level, so an `option` inside a `message`
            // also matches.
            if starts_with_keyword(self.src, self.pos, "message") {
                self.parse_message_header();
                continue;
            }
            if starts_with_keyword(self.src, self.pos, "oneof") {
                self.parse_oneof_header();
                continue;
            }
            if b == b'}' {
                self.close_brace();
                continue;
            }
            if b == b';' {
                self.pos += 1;
                continue;
            }
            if b == b'{' {
                // Unbraced `{` (e.g. inside `option foo = { … };`).
                // Consume the balanced block, counting newlines.
                self.consume_balanced_block();
                continue;
            }
            // Inside a `Message` frame, try to parse a field
            // declaration. Outside a message, anything else is
            // top-level (service, rpc, syntax, package, …) which
            // `parse_proto_providers` already handles; here we just
            // skip the line.
            if self.in_message() {
                self.parse_field_or_skip();
            } else {
                self.skip_to_semicolon_or_newline();
            }
        }
        // Drain any frames that were never closed (truncated file).
        while let Some(frame) = self.frames.pop() {
            if let Frame::Message { name, fields, .. } = frame {
                self.messages.push(ParsedPayloadSchema {
                    name,
                    topic: None,
                    fields,
                });
            }
        }
    }

    fn in_message(&self) -> bool {
        // The innermost message frame is the field-collecting
        // target. A `oneof` block nested inside a message still
        // belongs to that message (the field-collecting frame is
        // the message two levels down), so look for a `Message`
        // frame in the stack, ignoring `Oneof` and `Skip` frames.
        self.frames
            .iter()
            .any(|f| matches!(f, Frame::Message { .. }))
    }

    fn innermost_message_mut(&mut self) -> Option<&mut Frame> {
        // Walk the stack top-down so a nested `message` inside an
        // outer `message` collects its fields into the inner one,
        // not the outer. The `Oneof` and `Skip` frames in between
        // are ignored.
        self.frames
            .iter_mut()
            .rev()
            .find(|f| matches!(f, Frame::Message { .. }))
    }

    fn parse_message_header(&mut self) {
        let start_line = self.line;
        self.pos += "message".len();
        self.skip_ws();
        let name = self.read_identifier();
        self.skip_ws();
        if self.peek() != Some(b'{') {
            self.diagnostics.push(ProtoParseDiagnostic {
                line: start_line,
                kind: ProtoParseDiagnosticKind::MessageHeader,
                message: format!("`message {}` not followed by `{{`", name),
            });
            // Skip to the next `;` or newline so the rest of the
            // file is still parseable.
            self.skip_to_semicolon_or_newline();
            return;
        }
        self.pos += 1; // consume `{`
        if self.message_depth >= MAX_PROTO_MESSAGE_DEPTH {
            self.diagnostics.push(ProtoParseDiagnostic {
                line: start_line,
                kind: ProtoParseDiagnosticKind::NestingDepth,
                message: format!(
                    "nested `message {}` exceeds depth cap {}",
                    name, MAX_PROTO_MESSAGE_DEPTH
                ),
            });
            // Skip the balanced block.
            self.consume_balanced_block_from_open_brace();
            return;
        }
        self.message_depth += 1;
        self.frames.push(Frame::Message {
            name,
            fields: Vec::new(),
            line: start_line,
        });
    }

    fn parse_oneof_header(&mut self) {
        let start_line = self.line;
        self.pos += "oneof".len();
        self.skip_ws();
        let _name = self.read_identifier();
        self.skip_ws();
        if self.peek() != Some(b'{') {
            self.diagnostics.push(ProtoParseDiagnostic {
                line: start_line,
                kind: ProtoParseDiagnosticKind::OneofHeader,
                message: format!("`oneof {}` not followed by `{{`", _name),
            });
            self.skip_to_semicolon_or_newline();
            return;
        }
        self.pos += 1; // consume `{`
        self.frames.push(Frame::Oneof);
    }

    fn close_brace(&mut self) {
        self.pos += 1;
        if let Some(frame) = self.frames.pop() {
            match frame {
                Frame::Message { name, fields, .. } => {
                    self.message_depth = self.message_depth.saturating_sub(1);
                    self.messages.push(ParsedPayloadSchema {
                        name,
                        topic: None,
                        fields,
                    });
                }
                Frame::Oneof | Frame::Skip => {}
            }
        }
    }

    /// Read `[qualifier] type-expr name = tag;` if the cursor is at
    /// a field declaration; otherwise skip the line and (best-effort)
    /// record a diagnostic.
    fn parse_field_or_skip(&mut self) {
        let start_line = self.line;
        // Skip qualifiers. `repeated` and (proto2) `optional` /
        // `required` are the canonical ones.
        let mut repeated = false;
        while let Some(kw) = self.peek_qualifier() {
            self.pos += kw.len();
            self.skip_ws();
            if kw == "repeated" {
                repeated = true;
            }
        }
        // Type expression: either `map<K, V>` (everything up to the
        // matching `>`) or a single identifier. The previous parser
        // only saw `split_whitespace` chunks, so a `map<string,
        // string>` token was opaque and the type-name field-name
        // split silently misfired.
        let type_desc = self.read_type_expression();
        if type_desc.is_none() {
            self.diagnostics.push(ProtoParseDiagnostic {
                line: start_line,
                kind: ProtoParseDiagnosticKind::UnparseableField,
                message: "field has no type expression".into(),
            });
            self.skip_to_semicolon_or_newline();
            return;
        }
        let inner_ty = type_desc.unwrap();
        self.skip_ws();
        let name = self.read_identifier();
        if name.is_empty() {
            self.diagnostics.push(ProtoParseDiagnostic {
                line: start_line,
                kind: ProtoParseDiagnosticKind::UnparseableField,
                message: "field is missing a name".into(),
            });
            self.skip_to_semicolon_or_newline();
            return;
        }
        self.skip_ws();
        // Expect `= <tag>`.
        if self.peek() != Some(b'=') {
            self.diagnostics.push(ProtoParseDiagnostic {
                line: start_line,
                kind: ProtoParseDiagnosticKind::UnparseableField,
                message: format!("field `{}` is missing `= <tag>`", name),
            });
            self.skip_to_semicolon_or_newline();
            return;
        }
        self.pos += 1; // consume `=`
        self.skip_ws();
        // Consume the tag (digits).
        let tag_start = self.pos;
        while let Some(b) = self.peek() {
            if (b as char).is_ascii_digit() {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == tag_start {
            self.diagnostics.push(ProtoParseDiagnostic {
                line: start_line,
                kind: ProtoParseDiagnosticKind::UnparseableField,
                message: format!("field `{}` is missing a numeric tag", name),
            });
            self.skip_to_semicolon_or_newline();
            return;
        }
        self.skip_ws();
        // Options like `[deprecated = true]` may follow before `;`.
        // Skip them by consuming any `[ ... ]` block and continuing.
        if self.peek() == Some(b'[') {
            self.pos += 1;
            // Find the matching `]` (no nested brackets in proto
            // field options).
            while let Some(b) = self.peek() {
                if b == b']' {
                    self.pos += 1;
                    break;
                }
                self.pos += 1;
            }
            self.skip_ws();
        }
        if self.peek() == Some(b';') {
            self.pos += 1;
        }
        // Synthesise the field record. Repeated fields are wrapped
        // in `TypeDesc::Array` so the path carries `[]`.
        let ty = if repeated {
            TypeDesc::Array(Box::new(inner_ty))
        } else {
            inner_ty
        };
        let mut path = JsonPath(Vec::new());
        path.0.push(PathSegment::Name(name.clone()));
        if repeated {
            path.0.push(PathSegment::ArrayItems);
        }
        let field = ParsedField {
            path,
            meta: FieldMeta {
                ty,
                required: !repeated,
                nullable: false,
                enum_values: None,
            },
            line: start_line,
        };
        if let Some(Frame::Message { fields, .. }) = self.innermost_message_mut() {
            fields.push(field);
        }
    }

    /// Read a type expression: `map<K, V>` or a bare identifier.
    /// Returns `None` when the cursor is not at a type token.
    fn read_type_expression(&mut self) -> Option<TypeDesc> {
        if self.peek() == Some(b'm') && starts_with_keyword(self.src, self.pos, "map") {
            self.pos += "map".len();
            self.skip_ws();
            if self.peek() != Some(b'<') {
                // `map` without `<...>` is malformed; treat as the
                // bare type. The caller will see a `None` next round
                // (no name) and produce a diagnostic.
                return Some(TypeDesc::Unknown);
            }
            self.pos += 1; // consume `<`
                           // Read K (identifier).
            self.skip_ws();
            let _key = self.read_identifier();
            self.skip_ws();
            // Expect `,`.
            if self.peek() == Some(b',') {
                self.pos += 1;
            }
            self.skip_ws();
            let value = self.read_identifier();
            self.skip_ws();
            // Expect `>`.
            if self.peek() == Some(b'>') {
                self.pos += 1;
            }
            // `TypeDesc` has no `Map` variant, so the value type is
            // the best we can carry. The plan accepts Unknown here.
            return Some(classify_proto_type(&value));
        }
        // Bare identifier.
        let name = self.read_identifier();
        if name.is_empty() {
            None
        } else {
            Some(classify_proto_type(&name))
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_qualifier(&self) -> Option<&'static str> {
        for kw in ["repeated", "optional", "required"] {
            if starts_with_keyword(self.src, self.pos, kw) {
                // The qualifier must be followed by whitespace /
                // `;` / `}` to count — otherwise the word is part of
                // a longer identifier (e.g. a field name `repeated_id`).
                let after = self.pos + kw.len();
                let is_boundary = after >= self.bytes.len()
                    || !(self.bytes[after] as char).is_ascii_alphanumeric()
                    || self.bytes[after] == b'_';
                if is_boundary {
                    return Some(kw);
                }
            }
        }
        None
    }

    fn read_identifier(&mut self) -> String {
        let start = self.pos;
        while let Some(b) = self.peek() {
            let c = b as char;
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
                self.pos += 1;
            } else {
                break;
            }
        }
        std::str::from_utf8(&self.bytes[start..self.pos])
            .unwrap_or("")
            .to_string()
    }

    fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            if (b as char).is_ascii_whitespace() {
                if b == b'\n' {
                    self.line += 1;
                }
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn skip_to_semicolon_or_newline(&mut self) {
        while let Some(b) = self.peek() {
            if b == b';' || b == b'\n' {
                if b == b';' {
                    self.pos += 1;
                } else {
                    self.line += 1;
                    self.pos += 1;
                }
                return;
            }
            self.pos += 1;
        }
    }

    fn consume_balanced_block(&mut self) {
        // Caller has not yet consumed the `{`. Walk forward to the
        // matching `}` using the shared brace balancer, counting
        // newlines so line numbers stay correct.
        if let Some(end) = find_matching_close(self.src, self.pos) {
            for j in self.pos..=end {
                if self.bytes[j] == b'\n' {
                    self.line += 1;
                }
            }
            self.pos = end + 1;
        } else {
            // Unbalanced — consume to EOF. Lines are counted in the
            // main loop.
            self.pos = self.bytes.len();
        }
    }

    fn consume_balanced_block_from_open_brace(&mut self) {
        // Used after a `message` / `oneof` header that has already
        // consumed its opening `{`. We need to skip the matching
        // close — the helper above takes the `{` position, so back
        // up one.
        if self.pos == 0 || self.bytes[self.pos - 1] != b'{' {
            return;
        }
        if let Some(end) = find_matching_close(self.src, self.pos - 1) {
            for j in self.pos..=end {
                if self.bytes[j] == b'\n' {
                    self.line += 1;
                }
            }
            self.pos = end + 1;
        } else {
            self.pos = self.bytes.len();
        }
    }
}

fn classify_proto_type(name: &str) -> TypeDesc {
    // Strip the leading dot of `.foo.Bar` and trailing dot
    // artifacts so `com.acme.orders.GetReq` falls back to Unknown
    // (a real reference, but the joiner is what resolves it).
    let bare = name.trim_start_matches('.').trim_end_matches('.');
    if bare.is_empty() {
        return TypeDesc::Unknown;
    }
    // If the type has a `.` in it, it is a package-qualified
    // reference to another message. The parser can only carry a
    // primitive `TypeDesc`, so Unknown is honest.
    if bare.contains('.') {
        return TypeDesc::Unknown;
    }
    match bare {
        "string" | "bytes" => TypeDesc::String,
        "bool" => TypeDesc::Boolean,
        "int32" | "int64" | "uint32" | "uint64" | "sint32" | "sint64" | "fixed32" | "fixed64"
        | "sfixed32" | "sfixed64" => TypeDesc::Integer,
        "float" | "double" => TypeDesc::Number,
        _ => TypeDesc::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_avro_schema() {
        let avsc = r#"{
            "type": "record",
            "name": "OrderCreated",
            "topic": "orders.events",
            "fields": [
                { "name": "order_id", "type": "string" },
                { "name": "amount", "type": "double" },
                { "name": "customer_id", "type": ["null", "string"] },
                {
                    "name": "items",
                    "type": {
                        "type": "array",
                        "items": {
                            "type": "record",
                            "name": "OrderItem",
                            "fields": [
                                { "name": "sku", "type": "string" },
                                { "name": "quantity", "type": "int" }
                            ]
                        }
                    }
                }
            ]
        }"#;

        let parsed = parse_avro_schema(avsc).expect("must parse avro schema");
        assert_eq!(parsed.name, "OrderCreated");
        assert_eq!(parsed.topic.as_deref(), Some("orders.events"));
        let field_names: Vec<String> = parsed.fields.iter().map(|f| f.path.to_string()).collect();
        assert_eq!(
            field_names,
            vec![
                "order_id",
                "amount",
                "customer_id",
                "items",
                "items[].sku",
                "items[].quantity"
            ]
        );
        let cust = parsed
            .fields
            .iter()
            .find(|f| f.path.to_string() == "customer_id")
            .unwrap();
        assert!(cust.meta.nullable);
        assert!(!cust.meta.required);
    }

    #[test]
    fn test_parse_json_schema() {
        let js = r#"{
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "PaymentEvent",
            "topic": "payments",
            "type": "object",
            "properties": {
                "payment_id": { "type": "string" },
                "amount": { "type": "number" }
            },
            "required": ["payment_id"]
        }"#;

        let parsed = parse_json_schema(js).expect("must parse json schema");
        assert_eq!(parsed.name, "PaymentEvent");
        assert_eq!(parsed.topic.as_deref(), Some("payments"));
        let p_id = parsed
            .fields
            .iter()
            .find(|f| f.path.to_string() == "payment_id")
            .unwrap();
        assert!(p_id.meta.required);
        let amt = parsed
            .fields
            .iter()
            .find(|f| f.path.to_string() == "amount")
            .unwrap();
        assert!(!amt.meta.required);
    }

    #[test]
    fn test_parse_proto_messages() {
        let proto = r#"
        syntax = "proto3";
        package orders;

        message OrderSubmitted {
            string order_id = 1;
            int64 user_id = 2;
            repeated string tag_ids = 3;
        }
        "#;

        let schemas = parse_proto_messages(proto);
        assert_eq!(schemas.len(), 1);
        let s = &schemas[0];
        assert_eq!(s.name, "OrderSubmitted");
        let field_names: Vec<String> = s.fields.iter().map(|f| f.path.to_string()).collect();
        assert_eq!(field_names, vec!["order_id", "user_id", "tag_ids[]"]);
    }
}
