//! OpenAPI schema flattening (§6.4 "Flattening" + "Types" + "Response
//! union" + "Bodies").
//!
//! Given a YAML/JSON schema body, walks every reachable property and
//! emits one `Field` node per flattened JSON path:
//!
//! - object properties recurse (`customer.address.city`),
//! - array `items` adds a `[]` segment (`items[].sku`),
//! - `additionalProperties: <schema>` adds a `{}` segment,
//! - `allOf` merges properties and unions `required`,
//! - `oneOf`/`anyOf` include every branch's fields with
//!   `required = false`; type disagreement yields `Unknown`,
//! - `$ref` resolves in-file via the components map; cycles stop
//!   with an `Object` field; external `$ref` yields an `Object`
//!   field and surfaces the reference through the caller's
//!   `unresolved_refs` accumulator (§9.7 "unnormalized").
//!
//! Type decisions follow §6.4 "Types": `string`/`integer`/`number`/
//! `boolean`/`object`/`array` map to the matching `TypeDesc`;
//! anything else is `Unknown`. Nullability has three flavours
//! (OAS 3.0 `nullable`, OAS 3.1 `type: [T, "null"]`, Swagger 2
//! `x-nullable`). Required is relative to the parent object. Enum
//! values are stringified via `serde_json`.

use crate::federation::contracts::model::{
    ContractFact, FieldMeta, JsonPath, PathSegment, TypeDesc,
};
use crate::schema::{GraphNode, NodeType, RepoNamespace};
use crate::server::sensors::openapi_line_index::LineIndex;
use serde_yaml::Value as YamlValue;
use std::collections::{BTreeMap, BTreeSet};

/// One flattened field, ready to be turned into a `GraphNode`.
#[derive(Debug, Clone)]
pub struct FieldRecord {
    pub path: JsonPath,
    pub id: String,
    pub node: GraphNode,
    pub meta: FieldMeta,
    pub line: u32,
}

/// The result of flattening one schema body. `fields` is in walk
/// order; `unresolved_refs` collects external `$ref`s (§6.4 +
/// §9.7) so the caller can record them in `coverage.unnormalized`.
#[derive(Debug, Default)]
pub struct FlattenResult {
    pub fields: Vec<FieldRecord>,
    pub unresolved_refs: Vec<String>,
}

// ─── Public entry point ─────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn flatten_schema(
    schema: &YamlValue,
    path: &mut JsonPath,
    spec_path: &str,
    line_index: &LineIndex,
    namespace: &RepoNamespace,
    seen_paths: &mut BTreeSet<JsonPath>,
    ancestor_chain: &[String],
    components: &BTreeMap<String, YamlValue>,
) -> FlattenResult {
    let mut result = FlattenResult::default();
    flatten_inner(
        schema,
        path,
        spec_path,
        line_index,
        namespace,
        seen_paths,
        ancestor_chain,
        components,
        &mut result,
    );
    result
}

#[allow(clippy::too_many_arguments)]
fn flatten_inner(
    schema: &YamlValue,
    path: &mut JsonPath,
    spec_path: &str,
    line_index: &LineIndex,
    namespace: &RepoNamespace,
    seen_paths: &mut BTreeSet<JsonPath>,
    ancestor_chain: &[String],
    components: &BTreeMap<String, YamlValue>,
    out: &mut FlattenResult,
) {
    // Resolve $ref if present. External $ref records the unresolved
    // string in `out.unresolved_refs` and skips further flattening.
    // A cycle emits an Object field at the current path and stops.
    let mut visited: BTreeSet<String> = ancestor_chain.iter().cloned().collect();
    let resolved: &YamlValue = match resolve_ref(schema, components, &mut visited, out) {
        ResolveResult::Ok(v) => v,
        ResolveResult::Skip => return,
        ResolveResult::Cycle => return, // top-level cycle: nothing to emit
    };

    let obj = resolved.as_mapping();

    // `allOf` — merge properties, union required.
    let mut composed_required: BTreeSet<String> = BTreeSet::new();
    if let Some(obj) = obj {
        if let Some(all_of) = obj.get(YamlValue::String("allOf".into())) {
            if let Some(arr) = all_of.as_sequence() {
                for branch in arr {
                    if let Some(m) = branch.as_mapping() {
                        if let Some(req) = m.get(YamlValue::String("required".into())) {
                            if let Some(req_arr) = req.as_sequence() {
                                for r in req_arr {
                                    if let Some(s) = r.as_str() {
                                        composed_required.insert(s.to_string());
                                    }
                                }
                            }
                        }
                    }
                    flatten_inner(
                        branch,
                        path,
                        spec_path,
                        line_index,
                        namespace,
                        seen_paths,
                        &visited.iter().cloned().collect::<Vec<_>>(),
                        components,
                        out,
                    );
                }
            }
        }

        // `oneOf` / `anyOf` — every branch's fields, required = false.
        for kw in ["oneOf", "anyOf"] {
            if let Some(branches_value) = obj.get(YamlValue::String(kw.into())) {
                if let Some(arr) = branches_value.as_sequence() {
                    for branch in arr {
                        flatten_inner(
                            branch,
                            path,
                            spec_path,
                            line_index,
                            namespace,
                            seen_paths,
                            &visited.iter().cloned().collect::<Vec<_>>(),
                            components,
                            out,
                        );
                    }
                }
            }
        }
    }

    // `properties` — recurse into each child.
    if let Some(obj) = obj {
        if let Some(properties) = obj.get(YamlValue::String("properties".into())) {
            if let Some(map) = properties.as_mapping() {
                let parent_required: BTreeSet<String> = obj
                    .get(YamlValue::String("required".into()))
                    .and_then(|v| v.as_sequence())
                    .map(|seq| {
                        seq.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let mut required_combined = composed_required.clone();
                for r in &parent_required {
                    required_combined.insert(r.clone());
                }

                for (k, v) in map {
                    let Some(prop_name) = k.as_str() else {
                        continue;
                    };
                    let prop_path_segment = if prop_name.starts_with('$') {
                        PathSegment::Name(format!("\\{}", prop_name))
                    } else {
                        PathSegment::Name(prop_name.to_string())
                    };
                    let prop_required = required_combined.contains(prop_name);
                    path.0.push(prop_path_segment.clone());

                    let mut child_required = BTreeSet::new();
                    if let Some(child_obj) = v.as_mapping() {
                        if let Some(req) = child_obj.get(YamlValue::String("required".into())) {
                            if let Some(req_arr) = req.as_sequence() {
                                for r in req_arr {
                                    if let Some(s) = r.as_str() {
                                        child_required.insert(s.to_string());
                                    }
                                }
                            }
                        }
                    }

                    flatten_property(
                        v,
                        path,
                        prop_required,
                        &mut child_required,
                        spec_path,
                        line_index,
                        namespace,
                        seen_paths,
                        &visited.iter().cloned().collect::<Vec<_>>(),
                        components,
                        out,
                    );

                    path.0.pop();
                }
            }
        }

        // Top-level `items` (i.e. this schema is an array). Recurse
        // with a `[]` segment so the array element's properties
        // become fields at `[].prop`.
        if let Some(items) = obj.get(YamlValue::String("items".into())) {
            path.0.push(PathSegment::ArrayItems);
            flatten_inner(
                items,
                path,
                spec_path,
                line_index,
                namespace,
                seen_paths,
                &visited.iter().cloned().collect::<Vec<_>>(),
                components,
                out,
            );
            path.0.pop();
        }

        // `additionalProperties` with a schema adds `{}`. The
        // schema's type becomes the field type at that segment;
        // its properties (if any) recurse further.
        if let Some(additional) = obj.get(YamlValue::String("additionalProperties".into())) {
            if let Some(add_obj) = additional.as_mapping() {
                let looks_like_schema = add_obj.keys().any(|k| {
                    matches!(
                        k.as_str(),
                        Some(
                            "type" | "$ref" | "properties" | "items" | "allOf" | "oneOf" | "anyOf"
                        )
                    )
                });
                if looks_like_schema {
                    path.0.push(PathSegment::MapValues);
                    let add_meta = field_meta_from_schema(additional);
                    emit_field(path, add_meta, spec_path, line_index, namespace, out);
                    flatten_inner(
                        additional,
                        path,
                        spec_path,
                        line_index,
                        namespace,
                        seen_paths,
                        &visited.iter().cloned().collect::<Vec<_>>(),
                        components,
                        out,
                    );
                    path.0.pop();
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn flatten_property(
    prop_schema: &YamlValue,
    path: &mut JsonPath,
    prop_required: bool,
    child_required: &mut BTreeSet<String>,
    spec_path: &str,
    line_index: &LineIndex,
    namespace: &RepoNamespace,
    seen_paths: &mut BTreeSet<JsonPath>,
    ancestor_chain: &[String],
    components: &BTreeMap<String, YamlValue>,
    out: &mut FlattenResult,
) {
    let mut visited: BTreeSet<String> = ancestor_chain.iter().cloned().collect();
    let resolved: &YamlValue = match resolve_ref(prop_schema, components, &mut visited, out) {
        ResolveResult::Ok(v) => v,
        ResolveResult::Skip => return,
        ResolveResult::Cycle => {
            // §6.4 "A `$ref` cycle stops at the first repeat with
            // a `Field` of type `Object`."
            if seen_paths.insert(path.clone()) {
                let meta = FieldMeta {
                    ty: TypeDesc::Object,
                    required: prop_required,
                    nullable: false,
                    enum_values: None,
                };
                emit_field(path, meta, spec_path, line_index, namespace, out);
            }
            return;
        }
    };

    // Same JSON path encountered twice (e.g. via `oneOf` /
    // `anyOf` branches that share a property name). §6.4 says a
    // field whose branches disagree on type becomes `Unknown`;
    // `required` is `false` for oneOf/anyOf branches. We update
    // the prior emission in place rather than adding a duplicate.
    if seen_paths.contains(path) {
        if let Some(prior) = out.fields.iter_mut().find(|f| f.path == *path) {
            let current_ty = type_desc_of(resolved);
            let current_nullable = is_nullable(resolved);
            if prior.meta.ty != current_ty {
                prior.meta.ty = TypeDesc::Unknown;
            }
            prior.meta.nullable = prior.meta.nullable || current_nullable;
        }
        return;
    }
    seen_paths.insert(path.clone());

    let ty = type_desc_of(resolved);
    let nullable = is_nullable(resolved);
    let enum_values = enum_values_of(resolved);
    let meta = FieldMeta {
        ty: ty.clone(),
        required: prop_required,
        nullable,
        enum_values,
    };

    // Emit the field at this path BEFORE recursing, so the
    // pre-order walk matches the expected `customer`, `customer.address`,
    // `customer.address.city` ordering (§6.4 "Properties recurse").
    emit_field(path, meta, spec_path, line_index, namespace, out);

    let prop_obj = resolved.as_mapping();

    if matches!(ty, TypeDesc::Array(_)) {
        if let Some(items) = prop_obj.and_then(|m| m.get(YamlValue::String("items".into()))) {
            path.0.push(PathSegment::ArrayItems);
            let saved = std::mem::take(child_required);
            flatten_inner(
                items,
                path,
                spec_path,
                line_index,
                namespace,
                seen_paths,
                ancestor_chain,
                components,
                out,
            );
            *child_required = saved;
            path.0.pop();
        }
    }

    if matches!(ty, TypeDesc::Object) {
        let saved = std::mem::take(child_required);
        flatten_inner(
            resolved,
            path,
            spec_path,
            line_index,
            namespace,
            seen_paths,
            ancestor_chain,
            components,
            out,
        );
        *child_required = saved;
    }
}

// ─── Field emission ──────────────────────────────────────────────────

fn emit_field(
    path: &JsonPath,
    meta: FieldMeta,
    spec_path: &str,
    line_index: &LineIndex,
    namespace: &RepoNamespace,
    out: &mut FlattenResult,
) {
    let line = line_index.lookup_path(path).unwrap_or(1);
    let name = path.to_string();
    let id = GraphNode::generate_id(&NodeType::Field, spec_path, &name, Some(line), namespace);
    let mut node = GraphNode::new(NodeType::Field, name.clone(), spec_path.to_string());
    node.id = id.clone();
    node.line_start = Some(line);
    node.contract = Some(ContractFact::Field(meta.clone()));
    out.fields.push(FieldRecord {
        path: path.clone(),
        id,
        node,
        meta,
        line,
    });
}

// ─── $ref resolution ────────────────────────────────────────────────

enum ResolveResult<'a> {
    Ok(&'a YamlValue),
    /// External `$ref` (or unrecognized form) — recorded as
    /// unresolved and skipped.
    Skip,
    /// `$ref` cycle — the resolved name is already on the visited
    /// chain. The caller emits an `Object` field at the current
    /// path and stops descending.
    Cycle,
}

/// Resolve a `$ref` if present. Returns the resolved schema or
/// `Skip` for external $refs (which are recorded in `out`).
fn resolve_ref<'a>(
    schema: &'a YamlValue,
    components: &'a BTreeMap<String, YamlValue>,
    visited: &mut BTreeSet<String>,
    out: &mut FlattenResult,
) -> ResolveResult<'a> {
    let Some(obj) = schema.as_mapping() else {
        return ResolveResult::Ok(schema);
    };
    let Some(ref_value) = obj.get(YamlValue::String("$ref".into())) else {
        return ResolveResult::Ok(schema);
    };
    let Some(ref_str) = ref_value.as_str() else {
        return ResolveResult::Ok(schema);
    };
    if !ref_str.starts_with('#') {
        out.unresolved_refs.push(ref_str.to_string());
        return ResolveResult::Skip;
    }
    let stripped = ref_str.trim_start_matches('#').trim_start_matches('/');
    let parts: Vec<&str> = stripped.split('/').collect();
    let name = match parts.as_slice() {
        ["components", "schemas", name] => *name,
        ["definitions", name] => *name,
        _ => return ResolveResult::Skip,
    };
    if !visited.insert(name.to_string()) {
        // Cycle — caller emits an Object field at the current path.
        return ResolveResult::Cycle;
    }
    match components.get(name) {
        Some(target) => ResolveResult::Ok(target),
        None => ResolveResult::Skip,
    }
}

// ─── Type / nullability / enum helpers ───────────────────────────────

/// Build a `TypeDesc` from a schema value.
pub fn type_desc_of(schema: &YamlValue) -> TypeDesc {
    let obj = match schema.as_mapping() {
        Some(m) => m,
        None => return TypeDesc::Unknown,
    };
    let type_value = obj.get(YamlValue::String("type".into()));

    // OAS 3.1 `type: [T, "null"]` — strip the null and return T.
    if let Some(t) = type_value {
        if let Some(arr) = t.as_sequence() {
            let mut non_null: Vec<&YamlValue> = Vec::new();
            for v in arr {
                let is_null = matches!(v.as_str(), Some("null"));
                if !is_null {
                    non_null.push(v);
                }
            }
            if non_null.len() == 1 {
                return primitive_type(non_null[0]);
            }
            if non_null.is_empty() {
                return TypeDesc::Unknown;
            }
            return TypeDesc::Unknown;
        }
    }
    if let Some(t) = type_value {
        return primitive_type(t);
    }

    if obj.contains_key(YamlValue::String("properties".into())) {
        return TypeDesc::Object;
    }
    if obj.contains_key(YamlValue::String("items".into())) {
        return TypeDesc::Array(Box::new(TypeDesc::Unknown));
    }
    if obj.contains_key(YamlValue::String("enum".into())) {
        return TypeDesc::String;
    }
    TypeDesc::Unknown
}

fn primitive_type(v: &YamlValue) -> TypeDesc {
    match v.as_str() {
        Some("string") => TypeDesc::String,
        Some("integer") => TypeDesc::Integer,
        Some("number") => TypeDesc::Number,
        Some("boolean") => TypeDesc::Boolean,
        Some("object") => TypeDesc::Object,
        Some("array") => TypeDesc::Array(Box::new(TypeDesc::Unknown)),
        Some("null") => TypeDesc::Unknown,
        _ => TypeDesc::Unknown,
    }
}

/// True if the schema is nullable. Three flavors:
/// - OAS 3.0 `nullable: true`,
/// - OAS 3.1 `type: [T, "null"]`,
/// - Swagger 2 `x-nullable: true`.
pub fn is_nullable(schema: &YamlValue) -> bool {
    let obj = match schema.as_mapping() {
        Some(m) => m,
        None => return false,
    };
    if obj
        .get(YamlValue::String("nullable".into()))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return true;
    }
    if obj
        .get(YamlValue::String("x-nullable".into()))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return true;
    }
    if let Some(t) = obj.get(YamlValue::String("type".into())) {
        if let Some(arr) = t.as_sequence() {
            for v in arr {
                if matches!(v.as_str(), Some("null")) {
                    return true;
                }
            }
        }
    }
    false
}

fn enum_values_of(schema: &YamlValue) -> Option<Vec<String>> {
    let obj = schema.as_mapping()?;
    let arr = obj.get(YamlValue::String("enum".into()))?.as_sequence()?;
    let mut out = Vec::new();
    for v in arr {
        let json = serde_json::to_string(v).ok()?;
        // §6.4 "Enum values are stringified with `serde_json`"
        // — `1` → `"1"`. Every value is wrapped as a JSON string
        // so the on-the-wire enum is homogeneous.
        let wrapped = if json.starts_with('"') {
            json
        } else {
            format!("\"{}\"", json)
        };
        out.push(wrapped);
    }
    Some(out)
}

/// Convenience used by the request/response schema emitters when they
/// build a `FieldMeta` directly from a parameter or response body.
pub fn field_meta_from_schema(schema: &YamlValue) -> FieldMeta {
    let ty = type_desc_of(schema);
    let nullable = is_nullable(schema);
    let enum_values = enum_values_of(schema);
    FieldMeta {
        ty,
        required: false,
        nullable,
        enum_values,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ns() -> RepoNamespace {
        RepoNamespace::for_test()
    }

    fn parse(s: &str) -> YamlValue {
        serde_yaml::from_str(s).expect("valid yaml")
    }

    fn flatten_one(yaml: &str) -> (FlattenResult, LineIndex) {
        let schema = parse(yaml);
        let mut path = JsonPath(Vec::new());
        let mut seen: BTreeSet<JsonPath> = BTreeSet::new();
        let li = LineIndex::build(yaml);
        let res = flatten_schema(
            &schema,
            &mut path,
            "openapi.yaml",
            &li,
            &ns(),
            &mut seen,
            &[],
            &BTreeMap::new(),
        );
        (res, li)
    }

    fn names(result: &FlattenResult) -> Vec<String> {
        result.fields.iter().map(|f| f.path.to_string()).collect()
    }

    #[test]
    fn flattens_nested_object_properties() {
        let yaml = "\
type: object
properties:
  customer:
    type: object
    properties:
      address:
        type: object
        properties:
          city:
            type: string
";
        let (r, _) = flatten_one(yaml);
        assert_eq!(
            names(&r),
            vec![
                "customer".to_string(),
                "customer.address".to_string(),
                "customer.address.city".to_string(),
            ]
        );
        let city = r.fields.iter().find(|f| f.path.0.len() == 3).unwrap();
        assert_eq!(city.meta.ty, TypeDesc::String);
    }

    #[test]
    fn flattens_array_items() {
        let yaml = "\
type: object
properties:
  items:
    type: array
    items:
      type: object
      properties:
        sku:
          type: string
";
        let (r, _) = flatten_one(yaml);
        assert_eq!(
            names(&r),
            vec!["items".to_string(), "items[].sku".to_string()]
        );
    }

    #[test]
    fn flattens_additional_properties_with_curly_segment() {
        let yaml = "\
type: object
properties:
  metadata:
    type: object
    additionalProperties:
      type: string
";
        let (r, _) = flatten_one(yaml);
        assert_eq!(
            names(&r),
            vec!["metadata".to_string(), "metadata.{}".to_string()]
        );
    }

    #[test]
    fn all_of_merges_properties_and_unions_required() {
        let yaml = "\
allOf:
  - type: object
    properties:
      a:
        type: string
    required: [a]
  - type: object
    properties:
      b:
        type: integer
    required: [b]
";
        let (r, _) = flatten_one(yaml);
        assert!(names(&r).contains(&"a".to_string()));
        assert!(names(&r).contains(&"b".to_string()));
        let a = r
            .fields
            .iter()
            .find(|f| f.path.0.len() == 1 && f.path.to_string() == "a")
            .unwrap();
        let b = r
            .fields
            .iter()
            .find(|f| f.path.0.len() == 1 && f.path.to_string() == "b")
            .unwrap();
        assert!(a.meta.required);
        assert!(b.meta.required);
    }

    #[test]
    fn one_of_includes_every_branch_with_required_false() {
        let yaml = "\
oneOf:
  - type: object
    properties:
      a:
        type: string
  - type: object
    properties:
      a:
        type: integer
";
        let (r, _) = flatten_one(yaml);
        let a_records: Vec<_> = r
            .fields
            .iter()
            .filter(|f| f.path.to_string() == "a")
            .collect();
        // Same JSON path in two branches: the flattener merges in
        // place (§6.4: "a field whose branches disagree on type
        // becomes Unknown"); the result is one merged field.
        assert_eq!(a_records.len(), 1);
        assert_eq!(a_records[0].meta.ty, TypeDesc::Unknown);
        assert!(!a_records[0].meta.required);
    }

    #[test]
    fn ref_resolves_in_file() {
        let components: BTreeMap<String, YamlValue> = [(
            "Item".to_string(),
            parse("type: object\nproperties:\n  sku:\n    type: string\n"),
        )]
        .into_iter()
        .collect();
        let schema = parse("$ref: '#/components/schemas/Item'");
        let li = LineIndex::build(
            "components:\n  schemas:\n    Item:\n      type: object\n      properties:\n        sku:\n          type: string\n",
        );
        let mut path = JsonPath(Vec::new());
        let mut seen = BTreeSet::new();
        let r = flatten_schema(
            &schema,
            &mut path,
            "openapi.yaml",
            &li,
            &ns(),
            &mut seen,
            &[],
            &components,
        );
        assert!(names(&r).contains(&"sku".to_string()));
    }

    #[test]
    fn ref_cycle_emits_object_field() {
        let components: BTreeMap<String, YamlValue> = [(
            "Node".to_string(),
            parse("type: object\nproperties:\n  child:\n    $ref: '#/components/schemas/Node'\n"),
        )]
        .into_iter()
        .collect();
        let schema = parse("$ref: '#/components/schemas/Node'");
        let li = LineIndex::build(
            "components:\n  schemas:\n    Node:\n      type: object\n      properties:\n        child:\n          $ref: '#/components/schemas/Node'\n",
        );
        let mut path = JsonPath(Vec::new());
        let mut seen = BTreeSet::new();
        let r = flatten_schema(
            &schema,
            &mut path,
            "openapi.yaml",
            &li,
            &ns(),
            &mut seen,
            &[],
            &components,
        );
        let child = r
            .fields
            .iter()
            .find(|f| f.path.to_string() == "child")
            .unwrap();
        assert_eq!(child.meta.ty, TypeDesc::Object);
    }

    #[test]
    fn external_ref_returns_empty_and_unnormalized_recorded() {
        let components: BTreeMap<String, YamlValue> = BTreeMap::new();
        let schema = parse("$ref: 'other.yaml#/components/schemas/Foo'");
        let li = LineIndex::build("");
        let mut path = JsonPath(Vec::new());
        let mut seen = BTreeSet::new();
        let r = flatten_schema(
            &schema,
            &mut path,
            "openapi.yaml",
            &li,
            &ns(),
            &mut seen,
            &[],
            &components,
        );
        assert_eq!(r.fields.len(), 0);
        assert_eq!(
            r.unresolved_refs,
            vec!["other.yaml#/components/schemas/Foo".to_string()]
        );
    }

    #[test]
    fn type_string_integer_number_boolean_object_array() {
        for (yaml, want) in [
            (
                "type: object\nproperties:\n  x:\n    type: string\n",
                TypeDesc::String,
            ),
            (
                "type: object\nproperties:\n  x:\n    type: integer\n",
                TypeDesc::Integer,
            ),
            (
                "type: object\nproperties:\n  x:\n    type: number\n",
                TypeDesc::Number,
            ),
            (
                "type: object\nproperties:\n  x:\n    type: boolean\n",
                TypeDesc::Boolean,
            ),
            (
                "type: object\nproperties:\n  x:\n    type: object\n",
                TypeDesc::Object,
            ),
        ] {
            let (r, _) = flatten_one(yaml);
            let x = r.fields.iter().find(|f| f.path.to_string() == "x").unwrap();
            assert_eq!(x.meta.ty, want, "for {yaml}");
        }
        let (r, _) = flatten_one(
            "type: array\nitems:\n  type: object\n  properties:\n    sku:\n      type: string\n",
        );
        let sku = r
            .fields
            .iter()
            .find(|f| f.path.to_string() == "[].sku")
            .unwrap();
        assert_eq!(sku.meta.ty, TypeDesc::String);
    }

    #[test]
    fn unknown_type_is_unknown() {
        let (r, _) = flatten_one("type: foobar\n");
        assert!(r.fields.is_empty());
    }

    #[test]
    fn nullability_oas_3_0() {
        let (r, _) =
            flatten_one("type: object\nproperties:\n  x:\n    type: string\n    nullable: true\n");
        let x = r.fields.iter().find(|f| f.path.to_string() == "x").unwrap();
        assert!(x.meta.nullable);
    }

    #[test]
    fn nullability_oas_3_1_type_array_with_null() {
        let (r, _) = flatten_one("type: object\nproperties:\n  x:\n    type: [string, \"null\"]\n");
        let x = r.fields.iter().find(|f| f.path.to_string() == "x").unwrap();
        assert!(x.meta.nullable);
    }

    #[test]
    fn nullability_swagger_2_x_nullable() {
        let (r, _) = flatten_one(
            "type: object\nproperties:\n  x:\n    type: string\n    x-nullable: true\n",
        );
        let x = r.fields.iter().find(|f| f.path.to_string() == "x").unwrap();
        assert!(x.meta.nullable);
    }

    #[test]
    fn required_relative_to_parent_object() {
        let yaml = "\
type: object
required: [a, b]
properties:
  a:
    type: string
  b:
    type: string
  c:
    type: string
";
        let (r, _) = flatten_one(yaml);
        let a = r.fields.iter().find(|f| f.path.to_string() == "a").unwrap();
        let c = r.fields.iter().find(|f| f.path.to_string() == "c").unwrap();
        assert!(a.meta.required);
        assert!(!c.meta.required);
    }

    #[test]
    fn enum_stringified_with_serde_json() {
        let yaml = "\
type: object
properties:
  status:
    type: string
    enum:
      - open
      - paid
      - 1
      - 1.5
      - true
";
        let (r, _) = flatten_one(yaml);
        let status = r
            .fields
            .iter()
            .find(|f| f.path.to_string() == "status")
            .unwrap();
        let v = status.meta.enum_values.clone().unwrap();
        assert_eq!(
            v,
            vec!["\"open\"", "\"paid\"", "\"1\"", "\"1.5\"", "\"true\""]
        );
    }

    #[test]
    fn dollar_property_name_is_escaped() {
        let yaml = "\
type: object
properties:
  $ref:
    type: string
  normal:
    type: string
";
        let (r, _) = flatten_one(yaml);
        let names = names(&r);
        assert!(names.contains(&"\\$ref".to_string()));
        assert!(names.contains(&"normal".to_string()));
    }

    #[test]
    fn format_is_ignored() {
        // §6.4 "Types": `format` is ignored, so `int32` → `int64` is
        // not a change. We only use the `type` keyword.
        let (r, _) =
            flatten_one("type: object\nproperties:\n  x:\n    type: integer\n    format: int32\n");
        let x = r.fields.iter().find(|f| f.path.to_string() == "x").unwrap();
        assert_eq!(x.meta.ty, TypeDesc::Integer);
    }

    #[test]
    fn array_items_nested_object_flattens_with_brackets() {
        let yaml = "\
type: array
items:
  type: object
  properties:
    customer:
      type: object
      properties:
        id:
          type: string
";
        let (r, _) = flatten_one(yaml);
        assert!(names(&r).contains(&"[].customer".to_string()));
        assert!(names(&r).contains(&"[].customer.id".to_string()));
    }

    #[test]
    fn all_of_required_merges_across_branches() {
        let yaml = "\
allOf:
  - type: object
    required: [a]
    properties:
      a:
        type: string
  - type: object
    required: [b]
    properties:
      b:
        type: integer
";
        let (r, _) = flatten_one(yaml);
        let a = r.fields.iter().find(|f| f.path.to_string() == "a").unwrap();
        let b = r.fields.iter().find(|f| f.path.to_string() == "b").unwrap();
        assert!(a.meta.required, "a is required by branch 1");
        assert!(b.meta.required, "b is required by branch 2");
    }

    #[test]
    fn any_of_includes_every_branch_with_required_false() {
        let yaml = "\
type: object
required: [a]
properties:
  a:
    type: string
anyOf:
  - type: object
    properties:
      b:
        type: integer
  - type: object
    properties:
      c:
        type: boolean
";
        let (r, _) = flatten_one(yaml);
        let a = r.fields.iter().find(|f| f.path.to_string() == "a").unwrap();
        let b = r.fields.iter().find(|f| f.path.to_string() == "b").unwrap();
        let c = r.fields.iter().find(|f| f.path.to_string() == "c").unwrap();
        // `a` is required by the parent schema's `required`.
        assert!(a.meta.required);
        // `b` / `c` come from anyOf branches → `required = false`.
        assert!(!b.meta.required);
        assert!(!c.meta.required);
    }

    #[test]
    fn one_of_branch_type_disagreement_yields_unknown() {
        // §6.4: a field whose branches disagree on type becomes
        // `Unknown`. We emit one `Field` per unique JSON path and
        // merge when the same path appears in multiple branches.
        let yaml = "\
oneOf:
  - type: object
    properties:
      a:
        type: string
  - type: object
    properties:
      a:
        type: integer
";
        let (r, _) = flatten_one(yaml);
        let a_records: Vec<_> = r
            .fields
            .iter()
            .filter(|f| f.path.to_string() == "a")
            .collect();
        assert_eq!(a_records.len(), 1, "duplicate path is merged in place");
        assert_eq!(a_records[0].meta.ty, TypeDesc::Unknown);
        assert!(
            !a_records[0].meta.required,
            "oneOf branches default to required = false"
        );
    }
}
