//! OpenAPI protocol sensor
//!
//! Parses OpenAPI 3.x and Swagger 2.0 specs to extract HTTP operations
//! and maps operationId to handler implementations.
//!
//! Edges created: CallsHttp (route path+method -> handler function)

use crate::graph::GraphDatabase;
use crate::schema::{GraphNode, GraphEdge, NodeType, EdgeType};
use crate::error::LainError;
use std::collections::BTreeMap;
use std::path::Path;
use serde::Deserialize;

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
            operation_id: op.operation_id.clone().unwrap_or_else(|| format!("{}:{}", method, path)),
            summary: op.summary.clone().unwrap_or_default(),
            spec_path: spec_path.to_string(),
        }
    }
}

/// Find handler in graph by operationId
fn find_handler(graph: &GraphDatabase, operation_id: &str) -> Option<GraphNode> {
    graph.find_node_by_name(operation_id)
        .or_else(|| graph.find_node_by_name(&to_snake_case(operation_id)))
        .or_else(|| graph.find_node_by_name(&to_camel_case(operation_id)))
}

fn to_snake_case(name: &str) -> String {
    let mut result = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            result.push('_');
        }
        result.push(c.to_ascii_lowercase());
    }
    result
}

fn to_camel_case(name: &str) -> String {
    let mut result = String::new();
    let mut capitalize = false;
    for c in name.chars() {
        if c == '_' {
            capitalize = true;
        } else if capitalize {
            result.push(c.to_ascii_uppercase());
            capitalize = false;
        } else {
            result.push(c);
        }
    }
    result
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
                    method,
                    &path,
                    operation,
                    spec_path,
                ));
            }
        }
    }

    operations
}

/// Enrich graph with OpenAPI operations
pub fn enrich_with_openapi(
    graph: &GraphDatabase,
    spec_path: &Path,
) -> Result<usize, LainError> {
    let content = std::fs::read_to_string(spec_path)?;
    let operations = parse_openapi(&content, &spec_path.to_string_lossy());

    let mut count = 0;
    for op in &operations {
        let route_id = GraphNode::generate_id(
            &NodeType::HttpRoute,
            &op.spec_path,
            &format!("{}:{}", op.method, op.path),
        );

        let mut route_node = GraphNode::new(
            NodeType::HttpRoute,
            format!("{} {}", op.method, op.path),
            op.spec_path.clone(),
        );
        route_node.id = route_id.clone();
        route_node.signature = Some(op.operation_id.clone());
        route_node.docstring = if op.summary.is_empty() { None } else { Some(op.summary.clone()) };
        graph.upsert_node(route_node)?;

        if let Some(handler) = find_handler(graph, &op.operation_id) {
            let edge = GraphEdge::new(
                EdgeType::CallsHttp,
                route_id,
                handler.id.clone(),
            );
            graph.insert_edge(&edge)?;
            count += 1;
        }
    }

    Ok(count)
}

/// Scan workspace for OpenAPI specs
pub fn scan_workspace(
    graph: &GraphDatabase,
    root: &Path,
) -> Result<usize, LainError> {
    let mut count = 0;

    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .build();

    for entry in walker.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        let is_spec = name.contains("openapi") || name.contains("swagger") ||
           matches!(path.extension().and_then(|e| e.to_str()), Some("yaml" | "yml" | "json"));

        if is_spec {
            if let Ok(content) = std::fs::read_to_string(path) {
                if content.contains("openapi") || content.contains("swagger") {
                    match enrich_with_openapi(graph, path) {
                        Ok(n) => count += n,
                        Err(e) => tracing::warn!("Failed to parse {:?}: {}", path, e),
                    }
                }
            }
        }
    }

    Ok(count)
}