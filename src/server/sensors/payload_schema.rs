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
pub fn parse_proto_messages(content: &str) -> Vec<ParsedPayloadSchema> {
    let mut schemas = Vec::new();
    let mut in_message = false;
    let mut current_name = String::new();
    let mut current_fields = Vec::new();

    for (line_idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") {
            continue;
        }
        if trimmed.starts_with("message ") {
            if in_message && !current_fields.is_empty() {
                schemas.push(ParsedPayloadSchema {
                    name: current_name.clone(),
                    topic: None,
                    fields: current_fields,
                });
                current_fields = Vec::new();
            }
            in_message = true;
            current_name = trimmed
                .trim_start_matches("message")
                .trim()
                .trim_end_matches('{')
                .trim()
                .to_string();
            continue;
        }
        if in_message {
            if trimmed == "}" {
                in_message = false;
                if !current_fields.is_empty() {
                    schemas.push(ParsedPayloadSchema {
                        name: current_name.clone(),
                        topic: None,
                        fields: current_fields,
                    });
                    current_fields = Vec::new();
                }
                continue;
            }
            // Parse field: `[repeated] <type> <name> = <tag>;`
            let without_semicolon = trimmed.trim_end_matches(';').trim();
            let parts: Vec<&str> = without_semicolon.split_whitespace().collect();
            if parts.len() >= 4 && parts[parts.len() - 2] == "=" {
                let is_repeated = parts[0] == "repeated";
                let type_idx = if is_repeated { 1 } else { 0 };
                let type_name = parts[type_idx];
                let field_name = parts[type_idx + 1];

                let inner_ty = match type_name {
                    "string" => TypeDesc::String,
                    "int32" | "int64" | "uint32" | "uint64" | "sint32" | "sint64" => {
                        TypeDesc::Integer
                    }
                    "float" | "double" => TypeDesc::Number,
                    "bool" => TypeDesc::Boolean,
                    "bytes" => TypeDesc::String,
                    _ => TypeDesc::Unknown,
                };

                let ty = if is_repeated {
                    TypeDesc::Array(Box::new(inner_ty))
                } else {
                    inner_ty
                };

                let mut path = JsonPath(Vec::new());
                path.0.push(PathSegment::Name(field_name.to_string()));
                if is_repeated {
                    path.0.push(PathSegment::ArrayItems);
                }

                current_fields.push(ParsedField {
                    path,
                    meta: FieldMeta {
                        ty,
                        required: !is_repeated,
                        nullable: false,
                        enum_values: None,
                    },
                    line: line_idx as u32 + 1,
                });
            }
        }
    }

    if in_message && !current_fields.is_empty() {
        schemas.push(ParsedPayloadSchema {
            name: current_name,
            topic: None,
            fields: current_fields,
        });
    }

    schemas
}

/// Parse any supported schema file into payload schemas.
pub fn parse_payload_file(content: &str, ext: &str) -> Vec<ParsedPayloadSchema> {
    match ext {
        "avsc" => parse_avro_schema(content).into_iter().collect(),
        "json" => {
            if let Some(avro) = parse_avro_schema(content) {
                vec![avro]
            } else if let Some(js) = parse_json_schema(content) {
                vec![js]
            } else {
                Vec::new()
            }
        }
        "proto" => parse_proto_messages(content),
        _ => Vec::new(),
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
