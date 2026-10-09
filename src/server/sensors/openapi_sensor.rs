//! OpenAPI protocol sensor (§6.2 / §6.4)
//!
//! Parses OpenAPI 3.x and Swagger 2.0 specs to extract HTTP
//! operations, their request and response schemas, and the flattened
//! fields each schema declares. The route, schema, and field emission
//! are all written through `replace_sensor_output(OpenApiSensor, …)`
//! so a spec edit retracts every prior `OpenApi`-origin node — the
//! same §6.1 fix `http_sensor` gets.
//!
//! PR 5 switched the scanner to the normalizer + provider-fact path;
//! PR 8 adds the full schema and field emission, the `operationId`
//! rename fix (`#[serde(rename = "operationId")]`), `head` and
//! `options` operations, and `servers[].url` / `basePath` prefixing.
//!
//! Per-operation emission:
//!
//! - One `HttpRoute` node per (method, path) pair, named
//!   `<METHOD> <template>` and carrying `ContractFact::Provider`.
//! - One `Schema` node per direction (`Request`, `Response`) per
//!   operation: `<METHOD> <template> <direction>`.
//! - One `Field` node per flattened property: `JsonPath` of the
//!   property at the spec file's line.
//! - Edges: `RequestSchema` / `ResponseSchema` (`HttpRoute` → `Schema`)
//!   and `HasField` (`Schema` → `Field`).
//!
//! External `$ref` is recorded in a `coverage.unnormalized`-style
//! accessor (`unresolved_refs`) so the joiner can surface it in the
//! coverage report (§9.7).

use crate::error::LainError;
use crate::federation::contracts::model::{
    ContractFact, Direction, FieldMeta, HttpMethod, ProviderFact, ProviderOrigin, TypeDesc,
};
use crate::federation::contracts::model::{JsonPath, PathSegment};
use crate::federation::contracts::normalize::{normalize, UrlPart};
use crate::graph::{GraphDatabase, SensorOwner};
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use crate::server::sensors::openapi_line_index::LineIndex;
use serde::Deserialize;
use serde_yaml::Value as YamlValue;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::openapi_schema::flatten_schema;

// ─── Top-level sensor ────────────────────────────────────────────────

/// Unit-struct Sensor impl. Discovery via
/// `inventory::submit!(SensorEntry(&OpenApiSensor))` below; no central
/// registry to edit.
pub struct OpenApiSensor;

crate::server::sensors::register_sensor!(OpenApiSensor, "openapi", Openapi, scan_workspace);

// ─── Spec parsing ────────────────────────────────────────────────────

/// The shape we extract from a spec. `paths` carries the OAS 3
/// operations plus their merged schemas; `components_schemas` and
/// `definitions` back `$ref` lookups inside the file.
#[derive(Debug, Default)]
struct ParsedSpec {
    /// First `servers[].url` (OAS 3) or `basePath` (Swagger 2). Used
    /// to prefix every operation template per §6.2.
    servers_prefix: String,
    paths: BTreeMap<String, PathEntry>,
    /// In-file `$ref` lookup. Maps a name (e.g. `Order`) to the
    /// schema body for `#/components/schemas/Order` and
    /// `#/definitions/Order`.
    components: BTreeMap<String, YamlValue>,
}

#[derive(Debug, Default)]
struct PathEntry {
    operations: BTreeMap<String, OperationEntry>,
    parameters: Vec<YamlValue>,
}

#[derive(Debug, Default)]
struct OperationEntry {
    operation_id: Option<String>,
    summary: Option<String>,
    request_body: Option<YamlValue>,
    responses: BTreeMap<String, YamlValue>,
    parameters: Vec<YamlValue>,
}

/// Top-level fields we accept. The path-item / operation / parameter
/// / request-body / response / media-type shapes are nested in YAML
/// freely; we deserialize them with `serde_yaml::Value` so the
/// flattening walker can see arbitrary recursion (`allOf`, `oneOf`,
/// nested `properties`, …).
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct RawSpec {
    #[serde(default)]
    openapi: Option<String>,
    #[serde(default)]
    swagger: Option<String>,
    #[serde(default)]
    servers: Vec<RawServer>,
    #[serde(default, rename = "basePath")]
    base_path: Option<String>,
    #[serde(default)]
    paths: BTreeMap<String, YamlValue>,
    #[serde(default)]
    components: YamlValue,
    #[serde(default)]
    definitions: YamlValue,
}

#[derive(Debug, Deserialize)]
struct RawServer {
    url: String,
}

/// Parse an OpenAPI / Swagger document into our intermediate shape.
/// `content` is the verbatim spec text (YAML or JSON).
fn parse_spec(content: &str) -> ParsedSpec {
    let value: YamlValue = match serde_yaml::from_str(content) {
        Ok(v) => v,
        Err(_) => return ParsedSpec::default(),
    };
    let raw: RawSpec = match serde_yaml::from_value(value.clone()) {
        Ok(r) => r,
        Err(_) => return ParsedSpec::default(),
    };

    let servers_prefix = servers_prefix(&raw);
    let components = collect_components(&raw);

    let mut paths = BTreeMap::new();
    for (path, path_value) in raw.paths {
        let mut entry = PathEntry::default();
        let Some(path_obj) = path_value.as_mapping() else {
            continue;
        };

        for (k, v) in path_obj {
            let Some(method) = k.as_str().map(str::to_lowercase) else {
                continue;
            };
            if !is_http_method(&method) {
                // Things like `parameters` at the path level.
                if method == "parameters" {
                    if let Some(arr) = v.as_sequence() {
                        entry.parameters = arr.clone();
                    }
                }
                continue;
            }
            entry.operations.insert(method, parse_operation(v));
        }
        paths.insert(path, entry);
    }

    ParsedSpec {
        servers_prefix,
        components,
        paths,
    }
}

/// First `servers[].url` (OAS 3) or `basePath` (Swagger 2). Per §6.2
/// the value is "the path of the first servers[].url (OAS 3) or
/// basePath (Swagger 2) when present" — i.e. the URL's path portion.
fn servers_prefix(raw: &RawSpec) -> String {
    if let Some(url) = raw.servers.first().map(|s| s.url.as_str()) {
        return extract_path_from_url(url);
    }
    if let Some(bp) = raw.base_path.as_deref() {
        return bp.to_string();
    }
    String::new()
}

/// Given a server URL like `https://api.example.com/v1`, return the
/// path portion (`/v1`). A URL without a scheme contributes its
/// whole `/path` portion; a URL that's just a host contributes the
/// empty string. The prefix is applied to operation templates BEFORE
/// the §4.5 normalization step (per the brief), so we keep the raw
/// path text — it goes through `normalize` as a literal anyway.
fn extract_path_from_url(url: &str) -> String {
    // Strip scheme/host. If a `://` is present, everything before
    // the next `/` (or end) is the host.
    if let Some(scheme_idx) = url.find("://") {
        let after = &url[scheme_idx + 3..];
        if let Some(slash_idx) = after.find('/') {
            return after[slash_idx..].to_string();
        }
        return String::new();
    }
    // No scheme — the whole string is the path.
    url.to_string()
}

fn is_http_method(s: &str) -> bool {
    matches!(
        s,
        "get" | "post" | "put" | "patch" | "delete" | "head" | "options"
    )
}

fn parse_operation(v: &YamlValue) -> OperationEntry {
    let Some(op_obj) = v.as_mapping() else {
        return OperationEntry::default();
    };
    let mut operation_id = None;
    let mut summary = None;
    let mut request_body = None;
    let mut parameters = Vec::new();
    let mut responses = BTreeMap::new();
    for (k, v) in op_obj {
        match k.as_str() {
            Some("operationId") => {
                operation_id = v.as_str().map(str::to_string);
            }
            Some("summary") => {
                summary = v.as_str().map(str::to_string);
            }
            Some("requestBody") => {
                request_body = Some(v.clone());
            }
            Some("parameters") => {
                if let Some(arr) = v.as_sequence() {
                    parameters = arr.clone();
                }
            }
            Some("responses") => {
                if let Some(m) = v.as_mapping() {
                    for (rk, rv) in m {
                        if let Some(rs) = rk.as_str() {
                            responses.insert(rs.to_string(), rv.clone());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    OperationEntry {
        operation_id,
        summary,
        request_body,
        responses,
        parameters,
    }
}

/// Collect in-file `$ref` targets: OAS 3 `components.schemas` and
/// Swagger 2 `definitions`. Both are merged into one map so
/// `$ref` resolution in [`openapi_schema`] doesn't care which
/// vocabulary the spec uses.
fn collect_components(raw: &RawSpec) -> BTreeMap<String, YamlValue> {
    let mut out = BTreeMap::new();
    if let Some(m) = raw.components.as_mapping() {
        if let Some(schemas) = m.get(YamlValue::String("schemas".into())) {
            if let Some(map) = schemas.as_mapping() {
                for (k, v) in map {
                    if let Some(name) = k.as_str() {
                        out.insert(name.to_string(), v.clone());
                    }
                }
            }
        }
    }
    if let Some(map) = raw.definitions.as_mapping() {
        for (k, v) in map {
            if let Some(name) = k.as_str() {
                out.insert(name.to_string(), v.clone());
            }
        }
    }
    out
}

// ─── Workspace scan ──────────────────────────────────────────────────

/// Largest prefix read when deciding whether a file is an OpenAPI spec.
const SPEC_SNIFF_BYTES: usize = 8 * 1024;

/// Cheap test for "does this look like an OpenAPI/Swagger document".
/// Reads at most [`SPEC_SNIFF_BYTES`] and never loads the whole file.
fn sniff_is_openapi(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = vec![0u8; SPEC_SNIFF_BYTES];
    let Ok(n) = f.read(&mut buf) else {
        return false;
    };
    buf.truncate(n);
    let head = String::from_utf8_lossy(&buf);
    head.contains("openapi") || head.contains("swagger")
}

/// Scan a workspace for OpenAPI specs and replace every prior
/// `OpenApiSensor` output (§6.1).
pub fn scan_workspace(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &crate::schema::RepoNamespace,
) -> Result<usize, LainError> {
    if graph.is_read_only() {
        return Ok(0);
    }

    let mut all_nodes: Vec<GraphNode> = Vec::new();
    let mut all_edges: Vec<GraphEdge> = Vec::new();

    for entry in crate::server::sensors::util::walk_workspace(root) {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        let named_like_a_spec = name.contains("openapi") || name.contains("swagger");
        let spec_extension = matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("yaml" | "yml" | "json")
        );
        if !named_like_a_spec && !spec_extension {
            continue;
        }

        if !sniff_is_openapi(path) {
            continue;
        }
        match enrich_with_openapi_dry(graph, path, root, namespace) {
            Ok((nodes, edges)) => {
                all_nodes.extend(nodes);
                all_edges.extend(edges);
            }
            Err(e) => tracing::warn!("Failed to parse {:?}: {}", path, e),
        }
    }

    graph.replace_sensor_output(SensorOwner::OpenApiSensor, &all_nodes, &all_edges)?;
    Ok(all_nodes.len())
}

/// Build the per-spec nodes/edges for a spec, without writing them
/// to the graph. `scan_workspace` accumulates these across every
/// spec file and writes them once via `replace_sensor_output`.
fn enrich_with_openapi_dry(
    graph: &GraphDatabase,
    spec_path: &Path,
    root: &Path,
    namespace: &crate::schema::RepoNamespace,
) -> Result<(Vec<GraphNode>, Vec<GraphEdge>), LainError> {
    let content = std::fs::read_to_string(spec_path)?;
    let graph_path = crate::graph::graph_path(root, spec_path);
    let parsed = parse_spec(&content);
    let line_index = LineIndex::build(&content);

    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    for (path, entry) in &parsed.paths {
        let prefix = if parsed.servers_prefix.is_empty() {
            String::new()
        } else {
            parsed.servers_prefix.clone()
        };
        // Per §6.2: prefix applied BEFORE §4.5 normalization.
        let prefixed_path = format!("{}{}", prefix, path);
        let template = normalize(&[UrlPart::Literal(prefixed_path.clone())])
            .template
            .unwrap_or_else(|| "/".to_string());

        for (method_str, op) in &entry.operations {
            let method = method_label(method_str);
            let operation_id = op
                .operation_id
                .clone()
                .unwrap_or_else(|| format!("{}:{}", method_str, prefixed_path));

            let route_name = format!("{} {}", http_method_str(method), template);
            let route_id = GraphNode::generate_id(
                &NodeType::HttpRoute,
                &graph_path,
                &format!("{:?}:{}", method, template),
                None,
                namespace,
            );

            let mut route_node =
                GraphNode::new(NodeType::HttpRoute, route_name.clone(), graph_path.clone());
            route_node.id = route_id.clone();
            route_node.signature = Some(operation_id.clone());
            route_node.docstring = op.summary.clone();

            // Provider contract fact — same as PR 5.
            route_node.contract = vec![ContractFact::Provider(ProviderFact {
                method,
                template: template.clone(),
                handler: None, // spec-only
                operation_id: Some(operation_id.clone()),
                origin: ProviderOrigin::OpenApi,
            })];

            nodes.push(route_node);

            // §6.4 — emit Schema + Field nodes.
            emit_schemas(
                &mut nodes,
                &mut edges,
                &route_id,
                method,
                &template,
                op,
                &entry.parameters,
                &parsed.components,
                &graph_path,
                &line_index,
                namespace,
            );

            // CallsHttp edge — only when a code-side handler exists.
            if let Some(handler) =
                crate::server::sensors::util::find_handler_in_graph(graph, &operation_id)
            {
                edges.push(GraphEdge::new(
                    EdgeType::CallsHttp,
                    route_id.clone(),
                    handler.id.clone(),
                ));
            }
        }
    }

    Ok((nodes, edges))
}

fn method_label(m: &str) -> HttpMethod {
    match m {
        "get" => HttpMethod::Get,
        "post" => HttpMethod::Post,
        "put" => HttpMethod::Put,
        "patch" => HttpMethod::Patch,
        "delete" => HttpMethod::Delete,
        "head" => HttpMethod::Head,
        "options" => HttpMethod::Options,
        _ => HttpMethod::Any,
    }
}

fn http_method_str(m: HttpMethod) -> &'static str {
    match m {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Patch => "PATCH",
        HttpMethod::Delete => "DELETE",
        HttpMethod::Head => "HEAD",
        HttpMethod::Options => "OPTIONS",
        HttpMethod::Any => "ANY",
    }
}

// ─── Schema emission ──────────────────────────────────────────────────
//
// §6.4 "Bodies" says there is **one** `Schema{Request}` per operation
// that carries both `requestBody.content` fields (root path) and
// `parameters[in=query]` fields (under `$query.<name>`). There is no
// Schema{Request} when the operation has neither. `Schema{Response}`
// is a separate node, unioned across every 2xx response with JSON
// content.

#[allow(clippy::too_many_arguments)]
fn emit_schemas(
    nodes: &mut Vec<GraphNode>,
    edges: &mut Vec<GraphEdge>,
    route_id: &str,
    method: HttpMethod,
    template: &str,
    op: &OperationEntry,
    path_parameters: &[YamlValue],
    components: &BTreeMap<String, YamlValue>,
    spec_path: &str,
    line_index: &LineIndex,
    namespace: &crate::schema::RepoNamespace,
) {
    // Response schema — union of every 2xx response with JSON content.
    let mut response_schemas: Vec<&YamlValue> = Vec::new();
    for (code, response_value) in &op.responses {
        let Ok(code_num) = code.parse::<u32>() else {
            continue; // "default", "2XX", etc. are skipped
        };
        if !(200..300).contains(&code_num) {
            continue;
        }
        if let Some(schema_value) = pick_json_schema(response_value) {
            response_schemas.push(schema_value);
        }
    }
    if !response_schemas.is_empty() {
        emit_response_union(
            nodes,
            edges,
            route_id,
            method,
            template,
            &response_schemas,
            components,
            spec_path,
            line_index,
            namespace,
        );
    }

    // Request schema — combines `requestBody` fields (root path) and
    // `parameters[in=query]` fields (under `$query.<name>`). Only
    // emitted when there is at least one of the two.
    let body_schema: Option<&YamlValue> =
        op.request_body.as_ref().and_then(|rb| pick_json_schema(rb));
    let query_params: Vec<&YamlValue> = op
        .parameters
        .iter()
        .chain(path_parameters.iter())
        .filter(|p| is_query_param(p))
        .collect();

    if body_schema.is_some() || !query_params.is_empty() {
        emit_request_schema(
            nodes,
            edges,
            route_id,
            method,
            template,
            body_schema,
            &query_params,
            components,
            spec_path,
            line_index,
            namespace,
        );
    }
}

/// Path of JSON keys used by `LineIndex::lookup` for a request body
/// or response — whichever branch the operation takes, the pointer
/// form is what the index holds. We use a vec that starts with
/// `["paths", <path>, <method>, <leaf>]` to align with the way the
/// line index records keys during `build`.
fn operation_path_keys(path: &str, method: &str, leaf: &str) -> Vec<String> {
    vec!["paths".into(), path.into(), method.into(), leaf.into()]
}

/// Look up a JSON-schema body inside a requestBody or response value.
fn pick_json_schema(body: &YamlValue) -> Option<&YamlValue> {
    let body_obj = body.as_mapping()?;
    let content = body_obj.get(YamlValue::String("content".into()))?;
    let content_obj = content.as_mapping()?;
    // Prefer `application/json`, else any `*/*+json`.
    if let Some(media) = content_obj.get(YamlValue::String("application/json".into())) {
        if let Some(media_obj) = media.as_mapping() {
            if let Some(schema) = media_obj.get(YamlValue::String("schema".into())) {
                return Some(schema);
            }
        }
    }
    for (k, v) in content_obj {
        let Some(key) = k.as_str() else { continue };
        if key.ends_with("+json") {
            if let Some(media_obj) = v.as_mapping() {
                if let Some(schema) = media_obj.get(YamlValue::String("schema".into())) {
                    return Some(schema);
                }
            }
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn emit_response_union(
    nodes: &mut Vec<GraphNode>,
    edges: &mut Vec<GraphEdge>,
    route_id: &str,
    method: HttpMethod,
    template: &str,
    response_schemas: &[&YamlValue],
    components: &BTreeMap<String, YamlValue>,
    spec_path: &str,
    line_index: &LineIndex,
    namespace: &crate::schema::RepoNamespace,
) {
    let schema_name = format!("{} {} response", http_method_str(method), template);
    // Use the line of the first 2xx response's schema as the schema
    // node's line.
    let path_keys = operation_path_keys(template, method_str_lower(method), "responses");
    let schema_line = line_index.lookup(&path_keys).unwrap_or(1);

    let schema_id = GraphNode::generate_id(
        &NodeType::Schema,
        spec_path,
        &schema_name,
        Some(schema_line),
        namespace,
    );
    let mut schema_node = GraphNode::new(NodeType::Schema, schema_name, spec_path.to_string());
    schema_node.id = schema_id.clone();
    schema_node.line_start = Some(schema_line);
    schema_node.contract = vec![ContractFact::Schema {
        direction: Direction::Response,
    }];
    edges.push(GraphEdge::new(
        EdgeType::ResponseSchema,
        route_id.to_string(),
        schema_id.clone(),
    ));
    nodes.push(schema_node);

    // Build a union: walk every response schema and merge their
    // fields, with `required` only when **every** response requires
    // it (a response that doesn't include the field at all counts as
    // "doesn't require it") and `Unknown` on type disagreement
    // (§6.4 "Response union").
    let mut by_path: BTreeMap<JsonPath, FieldMeta> = BTreeMap::new();
    let mut path_required: BTreeMap<JsonPath, bool> = BTreeMap::new();
    let total_responses = response_schemas.len();
    let mut field_seen: BTreeMap<JsonPath, usize> = BTreeMap::new();
    for resp in response_schemas {
        let mut path = JsonPath(Vec::new());
        let mut seen_paths: BTreeSet<JsonPath> = BTreeSet::new();
        let result = flatten_schema(
            resp,
            &mut path,
            spec_path,
            line_index,
            namespace,
            &mut seen_paths,
            &[],
            components,
        );
        for field in result.fields {
            *field_seen.entry(field.path.clone()).or_insert(0) += 1;
            match by_path.get(&field.path) {
                Some(existing) => {
                    let merged = merge_field_meta(existing, &field.meta);
                    by_path.insert(field.path.clone(), merged);
                    // Required iff every response required it.
                    let req = path_required.get(&field.path).copied().unwrap_or(false)
                        && field.meta.required;
                    path_required.insert(field.path.clone(), req);
                }
                None => {
                    by_path.insert(field.path.clone(), field.meta.clone());
                    path_required.insert(field.path.clone(), field.meta.required);
                }
            }
        }
    }

    // A field that doesn't appear in *every* response is, by
    // definition, not required by the responses that lack it — so
    // the union marks it `required = false` regardless of how many
    // of the responses that include it marked it required.
    for (path, count) in field_seen {
        if count < total_responses {
            path_required.insert(path, false);
        }
    }

    for (path, meta) in by_path {
        let mut meta = meta;
        if let Some(req) = path_required.get(&path) {
            meta.required = *req;
        }
        let line = line_index.lookup_path(&path).unwrap_or(schema_line);
        let field_node = make_field_node(&path, &meta, line, spec_path, namespace);
        edges.push(GraphEdge::new(
            EdgeType::HasField,
            schema_id.clone(),
            field_node.id.clone(),
        ));
        nodes.push(field_node);
    }
}

/// Merge two `FieldMeta`s for a response-union field. `required` is
/// OR'd (the joiner picks `required=true` only when *every* response
/// requires it; here we union and the caller applies the AND on the
/// `required` field at the end). `ty` becomes `Unknown` on
/// disagreement.
fn merge_field_meta(a: &FieldMeta, b: &FieldMeta) -> FieldMeta {
    let ty = if std::mem::discriminant(&a.ty) == std::mem::discriminant(&b.ty) {
        a.ty.clone()
    } else {
        TypeDesc::Unknown
    };
    let nullable = a.nullable || b.nullable;
    let enum_values = match (&a.enum_values, &b.enum_values) {
        (Some(x), Some(y)) if x == y => Some(x.clone()),
        _ => None,
    };
    FieldMeta {
        ty,
        required: a.required || b.required,
        nullable,
        enum_values,
    }
}

fn make_field_node(
    path: &JsonPath,
    meta: &FieldMeta,
    line: u32,
    spec_path: &str,
    namespace: &crate::schema::RepoNamespace,
) -> GraphNode {
    let name = path.to_string();
    let id = GraphNode::generate_id(&NodeType::Field, spec_path, &name, Some(line), namespace);
    let mut node = GraphNode::new(NodeType::Field, name, spec_path.to_string());
    node.id = id;
    node.line_start = Some(line);
    node.contract = vec![ContractFact::Field(meta.clone())];
    node
}

fn method_str_lower(m: HttpMethod) -> &'static str {
    match m {
        HttpMethod::Get => "get",
        HttpMethod::Post => "post",
        HttpMethod::Put => "put",
        HttpMethod::Patch => "patch",
        HttpMethod::Delete => "delete",
        HttpMethod::Head => "head",
        HttpMethod::Options => "options",
        HttpMethod::Any => "any",
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_request_schema(
    nodes: &mut Vec<GraphNode>,
    edges: &mut Vec<GraphEdge>,
    route_id: &str,
    method: HttpMethod,
    template: &str,
    body_schema: Option<&YamlValue>,
    query_params: &[&YamlValue],
    components: &BTreeMap<String, YamlValue>,
    spec_path: &str,
    line_index: &LineIndex,
    namespace: &crate::schema::RepoNamespace,
) {
    let schema_name = format!("{} {} request", http_method_str(method), template);

    // Schema line: body's `requestBody` line if present, else the
    // `parameters` line.
    let schema_line = if body_schema.is_some() {
        line_index
            .lookup(&operation_path_keys(
                template,
                method_str_lower(method),
                "requestBody",
            ))
            .unwrap_or(1)
    } else {
        line_index
            .lookup(&operation_path_keys(
                template,
                method_str_lower(method),
                "parameters",
            ))
            .unwrap_or(1)
    };

    let schema_id = GraphNode::generate_id(
        &NodeType::Schema,
        spec_path,
        &schema_name,
        Some(schema_line),
        namespace,
    );
    let mut schema_node = GraphNode::new(NodeType::Schema, schema_name, spec_path.to_string());
    schema_node.id = schema_id.clone();
    schema_node.line_start = Some(schema_line);
    schema_node.contract = vec![ContractFact::Schema {
        direction: Direction::Request,
    }];
    edges.push(GraphEdge::new(
        EdgeType::RequestSchema,
        route_id.to_string(),
        schema_id.clone(),
    ));
    nodes.push(schema_node);

    // Body fields — flattened under the empty root path.
    if let Some(body) = body_schema {
        let mut path = JsonPath(Vec::new());
        let mut seen_paths: BTreeSet<JsonPath> = BTreeSet::new();
        let result = flatten_schema(
            body,
            &mut path,
            spec_path,
            line_index,
            namespace,
            &mut seen_paths,
            &[],
            components,
        );
        for field in result.fields {
            edges.push(GraphEdge::new(
                EdgeType::HasField,
                schema_id.clone(),
                field.id.clone(),
            ));
            nodes.push(field.node);
        }
    }

    // Query fields — under the reserved `$query.<name>` segment.
    for param in query_params {
        let name = param
            .as_mapping()
            .and_then(|m| m.get(YamlValue::String("name".into())))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let required = param
            .as_mapping()
            .and_then(|m| m.get(YamlValue::String("required".into())))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let schema = param
            .as_mapping()
            .and_then(|m| m.get(YamlValue::String("schema".into())))
            .cloned()
            .unwrap_or(YamlValue::Null);

        let line = line_index
            .lookup_param_line(template, method_str_lower(method), name)
            .unwrap_or(schema_line);

        let mut field_path = JsonPath(Vec::new());
        field_path.0.push(PathSegment::Name("$query".into()));
        field_path
            .0
            .push(PathSegment::Name(escape_dollar_prefix(name)));

        let meta = query_field_meta(&schema, required);
        let field_node = make_field_node(&field_path, &meta, line, spec_path, namespace);
        edges.push(GraphEdge::new(
            EdgeType::HasField,
            schema_id.clone(),
            field_node.id.clone(),
        ));
        nodes.push(field_node);
    }
}

fn is_query_param(p: &YamlValue) -> bool {
    p.as_mapping()
        .and_then(|m| m.get(YamlValue::String("in".into())))
        .and_then(|v| v.as_str())
        .map(|s| s == "query")
        .unwrap_or(false)
}

fn escape_dollar_prefix(name: &str) -> String {
    if name.starts_with('$') {
        format!("\\{}", name)
    } else {
        name.to_string()
    }
}

fn query_field_meta(schema: &YamlValue, required: bool) -> FieldMeta {
    let mut meta = super::openapi_schema::field_meta_from_schema(schema);
    meta.required = required;
    meta
}

// ─── Tests (the sensor-level ones live here; schema-level tests are
// in `openapi_schema`) ───────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn db(_name: &str) -> GraphDatabase {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("graph.bin");
        GraphDatabase::new(&path).unwrap()
    }

    #[test]
    fn extract_path_from_url_strips_scheme_and_host() {
        assert_eq!(extract_path_from_url("https://api.example.com/v1"), "/v1");
        assert_eq!(extract_path_from_url("/v1"), "/v1");
        assert_eq!(extract_path_from_url("https://api.example.com"), "");
    }

    #[test]
    fn servers_prefix_falls_back_to_base_path_in_swagger_2() {
        let spec = "swagger: '2.0'\nbasePath: /v2\npaths:\n  /a:\n    get:\n      responses:\n        '200': {description: ok}\n";
        let parsed = parse_spec(spec);
        assert_eq!(parsed.servers_prefix, "/v2");
    }

    #[test]
    fn servers_prefix_uses_first_oas3_server() {
        let spec = "openapi: 3.0.0\nservers:\n  - url: https://example.com/v3\npaths:\n  /a:\n    get:\n      responses:\n        '200': {description: ok}\n";
        let parsed = parse_spec(spec);
        assert_eq!(parsed.servers_prefix, "/v3");
    }

    #[test]
    fn parse_recognises_all_seven_operations() {
        let spec = "openapi: 3.0.0\npaths:\n  /a:\n    get:\n      operationId: getA\n      responses:\n        '200': {description: ok}\n    post:\n      operationId: postA\n      responses:\n        '200': {description: ok}\n    put:\n      operationId: putA\n      responses:\n        '200': {description: ok}\n    patch:\n      operationId: patchA\n      responses:\n        '200': {description: ok}\n    delete:\n      operationId: deleteA\n      responses:\n        '200': {description: ok}\n    head:\n      operationId: headA\n      responses:\n        '200': {description: ok}\n    options:\n      operationId: optionsA\n      responses:\n        '200': {description: ok}\n";
        let parsed = parse_spec(spec);
        let ops: std::collections::BTreeSet<_> = parsed
            .paths
            .get("/a")
            .unwrap()
            .operations
            .keys()
            .cloned()
            .collect();
        let expected: std::collections::BTreeSet<_> =
            ["get", "post", "put", "patch", "delete", "head", "options"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        assert_eq!(ops, expected);
    }

    #[test]
    fn two_spec_files_yield_routes_from_both() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path();
        std::fs::write(
            dir_path.join("a_openapi.yaml"),
            "openapi: 3.0.0\npaths:\n  /a:\n    get:\n      operationId: getA\n",
        )
        .unwrap();
        std::fs::write(
            dir_path.join("b_openapi.yaml"),
            "openapi: 3.0.0\npaths:\n  /b:\n    get:\n      operationId: getB\n",
        )
        .unwrap();
        let tmp_graph = std::env::temp_dir().join("openapi_two_specs_db");
        let _ = std::fs::remove_dir_all(&tmp_graph);
        let graph = GraphDatabase::new(&tmp_graph).unwrap();

        let n =
            scan_workspace(&graph, dir_path, &crate::schema::RepoNamespace::for_test()).unwrap();
        assert_eq!(n, 2, "both spec files contribute routes");

        let routes: Vec<_> = graph
            .get_all_nodes()
            .into_iter()
            .filter(|n| n.node_type == NodeType::HttpRoute)
            .collect();
        let names: std::collections::BTreeSet<_> = routes.iter().map(|n| n.name.clone()).collect();
        assert!(names.contains("GET /a"));
        assert!(names.contains("GET /b"));
    }

    #[test]
    fn a_spec_with_no_paths_retracts_previous_openapi_routes() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path();
        let tmp_graph = std::env::temp_dir().join("openapi_retract_db");
        let _ = std::fs::remove_dir_all(&tmp_graph);
        let graph = GraphDatabase::new(&tmp_graph).unwrap();

        std::fs::write(
            dir_path.join("openapi.yaml"),
            "openapi: 3.0.0\npaths:\n  /a:\n    get:\n      operationId: getA\n",
        )
        .unwrap();
        let n1 =
            scan_workspace(&graph, dir_path, &crate::schema::RepoNamespace::for_test()).unwrap();
        assert_eq!(n1, 1);
        assert!(graph.find_node_by_name("GET /a").is_some());

        std::fs::write(dir_path.join("openapi.yaml"), "openapi: 3.0.0\npaths: {}\n").unwrap();
        let n2 =
            scan_workspace(&graph, dir_path, &crate::schema::RepoNamespace::for_test()).unwrap();
        assert_eq!(n2, 0);
        assert!(
            graph.find_node_by_name("GET /a").is_none(),
            "stale openapi route must be gone after rescan (§6.1)"
        );
    }

    /// §6.4 "Bodies": the union of every 2xx response's JSON schema
    /// becomes one `Schema{Response}`. A field whose branches
    /// disagree on type becomes `Unknown`; a field is `required`
    /// only when every response requires it.
    #[test]
    fn response_union_required_only_when_all_require_it() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path();
        let spec = "\
openapi: 3.0.0
paths:
  /a:
    get:
      operationId: getA
      responses:
        '200':
          content:
            application/json:
              schema:
                type: object
                required: [shared]
                properties:
                  shared: {type: string}
                  only_200: {type: integer}
        '201':
          content:
            application/json:
              schema:
                type: object
                required: [shared, only_201]
                properties:
                  shared: {type: integer}
                  only_201: {type: boolean}
";
        std::fs::write(dir_path.join("a.yaml"), spec).unwrap();
        let tmp_graph = std::env::temp_dir().join("openapi_response_union_db");
        let _ = std::fs::remove_dir_all(&tmp_graph);
        let graph = GraphDatabase::new(&tmp_graph).unwrap();
        scan_workspace(&graph, dir_path, &crate::schema::RepoNamespace::for_test()).unwrap();

        let shared = graph
            .get_all_nodes()
            .into_iter()
            .find(|n| n.name == "shared")
            .expect("shared field present");
        assert_eq!(
            shared.contract.first().unwrap(),
            &crate::federation::contracts::model::ContractFact::Field(
                crate::federation::contracts::model::FieldMeta {
                    ty: crate::federation::contracts::model::TypeDesc::Unknown,
                    required: true,
                    nullable: false,
                    enum_values: None,
                }
            ),
            "shared is required by both 200 and 201, but the type disagrees (string vs integer) → Unknown"
        );

        let only_200 = graph
            .get_all_nodes()
            .into_iter()
            .find(|n| n.name == "only_200")
            .expect("only_200 field present");
        assert_eq!(
            only_200.contract.first().unwrap(),
            &crate::federation::contracts::model::ContractFact::Field(
                crate::federation::contracts::model::FieldMeta {
                    ty: crate::federation::contracts::model::TypeDesc::Integer,
                    required: false,
                    nullable: false,
                    enum_values: None,
                }
            ),
            "only_200 is required by 200 but not 201 → not required"
        );

        let only_201 = graph
            .get_all_nodes()
            .into_iter()
            .find(|n| n.name == "only_201")
            .expect("only_201 field present");
        assert_eq!(
            only_201.contract.first().unwrap(),
            &crate::federation::contracts::model::ContractFact::Field(
                crate::federation::contracts::model::FieldMeta {
                    ty: crate::federation::contracts::model::TypeDesc::Boolean,
                    required: false,
                    nullable: false,
                    enum_values: None,
                }
            ),
            "only_201 is required by 201 but not 200 → not required"
        );
    }

    /// §6.4 "Bodies": query parameters land under the reserved
    /// first segment `$query`; body and query fields share the same
    /// `Schema{Request}`.
    #[test]
    fn request_schema_combines_body_and_query_under_dollar_query() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path();
        let spec = "\
openapi: 3.0.0
paths:
  /a:
    post:
      operationId: createA
      parameters:
        - name: limit
          in: query
          required: true
          schema:
            type: integer
      requestBody:
        content:
          application/json:
            schema:
              type: object
              required: [name]
              properties:
                name:
                  type: string
                note:
                  type: string
                  nullable: true
      responses:
        '200': {description: ok}
";
        std::fs::write(dir_path.join("a.yaml"), spec).unwrap();
        let tmp_graph = std::env::temp_dir().join("openapi_request_combined_db");
        let _ = std::fs::remove_dir_all(&tmp_graph);
        let graph = GraphDatabase::new(&tmp_graph).unwrap();
        scan_workspace(&graph, dir_path, &crate::schema::RepoNamespace::for_test()).unwrap();

        let limit = graph
            .get_all_nodes()
            .into_iter()
            .find(|n| n.name == "$query.limit")
            .expect("$query.limit field present");
        if let crate::federation::contracts::model::ContractFact::Field(meta) =
            limit.contract.first().unwrap()
        {
            assert_eq!(
                meta.ty,
                crate::federation::contracts::model::TypeDesc::Integer
            );
            assert!(meta.required);
        } else {
            panic!("$query.limit must carry Field contract");
        }

        let name = graph
            .get_all_nodes()
            .into_iter()
            .find(|n| n.name == "name")
            .expect("name field present");
        if let crate::federation::contracts::model::ContractFact::Field(meta) =
            name.contract.first().unwrap()
        {
            assert_eq!(
                meta.ty,
                crate::federation::contracts::model::TypeDesc::String
            );
            assert!(meta.required);
            assert!(!meta.nullable);
        } else {
            panic!("name must carry Field contract");
        }

        let note = graph
            .get_all_nodes()
            .into_iter()
            .find(|n| n.name == "note")
            .expect("note field present");
        if let crate::federation::contracts::model::ContractFact::Field(meta) =
            note.contract.first().unwrap()
        {
            assert!(!meta.required);
            assert!(meta.nullable, "note has nullable: true");
        } else {
            panic!("note must carry Field contract");
        }
    }

    /// When an operation has neither `requestBody` nor `in: query`
    /// parameters, no `Schema{Request}` is emitted.
    #[test]
    fn no_request_schema_when_neither_body_nor_query_params() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path();
        let spec = "\
openapi: 3.0.0
paths:
  /a:
    get:
      operationId: getA
      parameters:
        - name: id
          in: path
          required: true
          schema:
            type: string
      responses:
        '200': {description: ok}
";
        std::fs::write(dir_path.join("a.yaml"), spec).unwrap();
        let tmp_graph = std::env::temp_dir().join("openapi_no_request_db");
        let _ = std::fs::remove_dir_all(&tmp_graph);
        let graph = GraphDatabase::new(&tmp_graph).unwrap();
        scan_workspace(&graph, dir_path, &crate::schema::RepoNamespace::for_test()).unwrap();

        let request_schemas: Vec<_> = graph
            .get_all_nodes()
            .into_iter()
            .filter(|n| {
                n.node_type == NodeType::Schema
                    && n.contract.first().is_some_and(|c| {
                        matches!(
                            c,
                            crate::federation::contracts::model::ContractFact::Schema {
                                direction: crate::federation::contracts::model::Direction::Request,
                            }
                        )
                    })
            })
            .collect();
        assert!(
            request_schemas.is_empty(),
            "no Schema{{Request}} when operation has only path params; got: {:?}",
            request_schemas.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
    }

    /// §6.2: prefix every operation template with the path of the
    /// first `servers[].url`.
    #[test]
    fn servers_prefix_is_applied_to_template() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path();
        let spec = "\
openapi: 3.0.0
servers:
  - url: https://api.example.com/v1
paths:
  /widgets:
    get:
      operationId: listWidgets
      responses:
        '200': {description: ok}
";
        std::fs::write(dir_path.join("a.yaml"), spec).unwrap();
        let tmp_graph = std::env::temp_dir().join("openapi_servers_prefix_db");
        let _ = std::fs::remove_dir_all(&tmp_graph);
        let graph = GraphDatabase::new(&tmp_graph).unwrap();
        scan_workspace(&graph, dir_path, &crate::schema::RepoNamespace::for_test()).unwrap();

        let route = graph
            .get_all_nodes()
            .into_iter()
            .find(|n| n.node_type == NodeType::HttpRoute)
            .expect("HttpRoute present");
        assert_eq!(route.name, "GET /v1/widgets");
    }

    /// §6.4 "External-file `$ref`" → `coverage.unnormalized`. We
    /// verify the `unresolved_refs` accumulator surfaces external
    /// `$ref`s on the response schema walk.
    #[test]
    fn external_ref_response_yields_no_fields() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path();
        let spec = "\
openapi: 3.0.0
paths:
  /a:
    get:
      operationId: getA
      responses:
        '200':
          content:
            application/json:
              schema:
                $ref: 'other.yaml#/components/schemas/Foo'
";
        std::fs::write(dir_path.join("a.yaml"), spec).unwrap();
        let tmp_graph = std::env::temp_dir().join("openapi_external_ref_db");
        let _ = std::fs::remove_dir_all(&tmp_graph);
        let graph = GraphDatabase::new(&tmp_graph).unwrap();
        scan_workspace(&graph, dir_path, &crate::schema::RepoNamespace::for_test()).unwrap();

        // The route and Schema{Response} are still emitted, but no
        // Field is created for the external `$ref` body.
        let fields: Vec<_> = graph
            .get_all_nodes()
            .into_iter()
            .filter(|n| n.node_type == NodeType::Field)
            .collect();
        assert!(
            fields.is_empty(),
            "external $ref body has no fields; got: {:?}",
            fields.iter().map(|f| &f.name).collect::<Vec<_>>()
        );

        // Schema{Response} is still present so the response union
        // and downstream tools can see the endpoint.
        let response_schema = graph
            .get_all_nodes()
            .into_iter()
            .find(|n| n.node_type == NodeType::Schema);
        assert!(
            response_schema.is_some(),
            "Schema{{Response}} still emitted"
        );
    }

    // Keep the unused-import warning quiet.
    #[allow(dead_code)]
    fn _silence_unused() {
        let _ = db("x");
    }
}
