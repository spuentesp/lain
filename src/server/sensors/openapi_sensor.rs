//! OpenAPI protocol sensor
//!
//! Parses OpenAPI 3.x and Swagger 2.0 specs to extract HTTP operations
//! and maps operationId to handler implementations.
//!
//! Edges created: `CallsHttp` (route path+method → handler function).
//!
//! PR 5: switches the scanner to go through
//! `GraphDatabase::replace_sensor_output(OpenApiSensor, …)` so
//! schema/spec edits retract their previous `HttpRoute` nodes — the
//! same §6.1 fix that `http_sensor` gets. PR 8 adds the full schema
//! and field emission; this sensor still only emits routes for now
//! (the route + `Provider(OpenApi)` fact is the joiner-visible part).

use crate::error::LainError;
use crate::federation::contracts::model::{
    HttpMethod, ProviderFact, ProviderOrigin,
};
use crate::federation::contracts::normalize::{normalize, UrlPart};
use crate::graph::{GraphDatabase, SensorOwner};
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// OpenAPI operation extracted from spec
#[derive(Debug, Clone)]
pub struct OpenApiOperation {
    pub method: String,
    pub path: String,
    pub operation_id: String,
    pub summary: String,
    pub spec_path: String,
}

/// Minimal OpenAPI structure for parsing
#[derive(Debug, Deserialize)]
struct OpenApiSpec {
    #[serde(default)]
    paths: BTreeMap<String, PathItem>,
}

#[derive(Debug, Deserialize)]
struct PathItem {
    #[serde(rename = "get")]
    get: Option<Operation>,
    #[serde(rename = "post")]
    post: Option<Operation>,
    #[serde(rename = "put")]
    put: Option<Operation>,
    #[serde(rename = "delete")]
    delete: Option<Operation>,
    #[serde(rename = "patch")]
    patch: Option<Operation>,
}

#[derive(Debug, Deserialize)]
struct Operation {
    operation_id: Option<String>,
    summary: Option<String>,
}

impl OpenApiOperation {
    fn from_operation(method: &str, path: &str, op: &Operation, spec_path: &str) -> Self {
        Self {
            method: method.to_uppercase(),
            path: path.to_string(),
            operation_id: op
                .operation_id
                .clone()
                .unwrap_or_else(|| format!("{}:{}", method, path)),
            summary: op.summary.clone().unwrap_or_default(),
            spec_path: spec_path.to_string(),
        }
    }
}

/// Parse OpenAPI spec and extract operations
pub fn parse_openapi(content: &str, spec_path: &str) -> Vec<OpenApiOperation> {
    // Try JSON first, then YAML using serde_yaml
    let spec: OpenApiSpec = serde_json::from_str(content)
        .or_else(|_| serde_yaml::from_str(content))
        .unwrap_or_else(|_| OpenApiSpec {
            paths: BTreeMap::new(),
        });

    let mut operations = Vec::new();

    for (path, item) in spec.paths {
        for (method, op) in [
            ("get", &item.get),
            ("post", &item.post),
            ("put", &item.put),
            ("delete", &item.delete),
            ("patch", &item.patch),
        ] {
            if let Some(operation) = op {
                operations.push(OpenApiOperation::from_operation(
                    method, &path, operation, spec_path,
                ));
            }
        }
    }

    operations
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
    let operations = parse_openapi(&content, &crate::graph::graph_path(root, spec_path));

    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    for op in &operations {
        let route_id = GraphNode::generate_id(
            &NodeType::HttpRoute,
            &op.spec_path,
            &format!("{}:{}", op.method, op.path),
            None,
            namespace,
        );

        let mut route_node = GraphNode::new(
            NodeType::HttpRoute,
            format!("{} {}", op.method, op.path),
            op.spec_path.clone(),
        );
        route_node.id = route_id.clone();
        route_node.signature = Some(op.operation_id.clone());
        route_node.docstring = if op.summary.is_empty() {
            None
        } else {
            Some(op.summary.clone())
        };

        // §6.2 — normalize the template through §4.5. PR 8 will
        // additionally prefix with `servers[].url` / `basePath`; the
        // route shape the joiner reads (`Provider.method`,
        // `Provider.template`) is in place today.
        let template = normalize(&[UrlPart::Literal(op.path.clone())])
            .template
            .unwrap_or_else(|| "/".to_string());
        let method = match op.method.as_str() {
            "GET" => HttpMethod::Get,
            "POST" => HttpMethod::Post,
            "PUT" => HttpMethod::Put,
            "PATCH" => HttpMethod::Patch,
            "DELETE" => HttpMethod::Delete,
            "HEAD" => HttpMethod::Head,
            "OPTIONS" => HttpMethod::Options,
            _ => HttpMethod::Any,
        };
        route_node.contract = Some(crate::federation::contracts::model::ContractFact::Provider(
            ProviderFact {
                method,
                template,
                handler: None, // spec-only — no code handler claimed it
                operation_id: Some(op.operation_id.clone()),
                origin: ProviderOrigin::OpenApi,
            },
        ));

        nodes.push(route_node);

        if let Some(handler) =
            crate::server::sensors::util::find_handler_in_graph(graph, &op.operation_id)
        {
            edges.push(GraphEdge::new(
                EdgeType::CallsHttp,
                route_id,
                handler.id.clone(),
            ));
        }
    }

    Ok((nodes, edges))
}

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
/// `OpenApiSensor` output (§6.1). The replacement is global — every
/// spec is parsed, all operations are accumulated, and then
/// `replace_sensor_output(OpenApiSensor, …)` drops every prior
/// `OpenApi`-origin `HttpRoute` before inserting the new set. Per-
/// spec replacement would erase sibling specs' routes on every
/// individual spec parse.
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

/// Unit-struct Sensor impl. Discovery via
/// `inventory::submit!(SensorEntry(&OpenApiSensor))` below; no central
/// registry to edit.
pub struct OpenApiSensor;

impl crate::server::sensors::Sensor for OpenApiSensor {
    fn name(&self) -> &'static str {
        "openapi"
    }
    fn count_field(&self) -> crate::server::sensors::SensorCountField {
        crate::server::sensors::SensorCountField::Openapi
    }
    fn scan(
        &self,
        graph: &GraphDatabase,
        root: &std::path::Path,
        namespace: &crate::schema::RepoNamespace,
    ) -> Result<usize, LainError> {
        scan_workspace(graph, root, namespace)
    }
}

inventory::submit!(crate::server::sensors::SensorEntry(&OpenApiSensor));

#[cfg(test)]
mod tests {
    use super::*;

    /// A spec redaction across siblings must not erase another spec's
    /// routes — `replace_sensor_output(OpenApiSensor, …)` is global.
    /// Walking the workspace collects every spec; the replacement
    /// only fires once, after accumulation.
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

        let n = scan_workspace(
            &graph,
            dir_path,
            &crate::schema::RepoNamespace::for_test(),
        )
        .unwrap();
        assert_eq!(n, 2, "both spec files contribute routes");

        let routes: Vec<_> = graph
            .get_all_nodes()
            .into_iter()
            .filter(|n| n.node_type == NodeType::HttpRoute)
            .collect();
        let names: std::collections::BTreeSet<_> =
            routes.iter().map(|n| n.name.clone()).collect();
        assert!(names.contains("GET /a"));
        assert!(names.contains("GET /b"));
    }

    /// Deleting a spec file's content while leaving the file in place
    /// must retract its routes on the next scan.
    #[test]
    fn a_spec_with_no_paths_retracts_previous_openapi_routes() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path();
        let tmp_graph = std::env::temp_dir().join("openapi_retract_db");
        let _ = std::fs::remove_dir_all(&tmp_graph);
        let graph = GraphDatabase::new(&tmp_graph).unwrap();

        // First scan: a real spec.
        std::fs::write(
            dir_path.join("openapi.yaml"),
            "openapi: 3.0.0\npaths:\n  /a:\n    get:\n      operationId: getA\n",
        )
        .unwrap();
        let n1 = scan_workspace(
            &graph,
            dir_path,
            &crate::schema::RepoNamespace::for_test(),
        )
        .unwrap();
        assert_eq!(n1, 1);
        assert!(graph.find_node_by_name("GET /a").is_some());

        // Second scan: empty paths.
        std::fs::write(
            dir_path.join("openapi.yaml"),
            "openapi: 3.0.0\npaths: {}\n",
        )
        .unwrap();
        let n2 = scan_workspace(
            &graph,
            dir_path,
            &crate::schema::RepoNamespace::for_test(),
        )
        .unwrap();
        assert_eq!(n2, 0);
        assert!(
            graph.find_node_by_name("GET /a").is_none(),
            "stale openapi route must be gone after rescan (§6.1)"
        );
    }
}