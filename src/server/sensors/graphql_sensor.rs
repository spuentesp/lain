//! GraphQL protocol sensor
//!
//! Parses GraphQL schema and query definitions to extract operations
//! and maps them to resolver implementations.
//!
//! Edges created: Uses (resolver -> GraphQL type)

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use std::path::Path;

/// GraphQL operation extracted from schema
#[derive(Debug, Clone)]
pub struct GraphQlOperation {
    pub operation_type: String, // Query, Mutation, Subscription
    pub field_name: String,
    pub type_name: String,
    pub schema_path: String,
    pub line: u32,
}

/// Parse a GraphQL schema file
pub fn parse_graphql(content: &str, schema_path: &str) -> Vec<GraphQlOperation> {
    let mut operations = Vec::new();

    let mut in_type = false;
    let mut current_type = String::new();

    for (line_no, line) in content.lines().enumerate() {
        let line = line.trim();

        // Type definition
        if line.starts_with("type ") && !line.contains("{") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                current_type = parts[1].to_string();
                in_type = true;
            }
        } else if line == "}" {
            in_type = false;
        }

        // Query, Mutation, Subscription fields
        if in_type {
            if line.starts_with("query ") {
                let field = line
                    .trim_start_matches("query")
                    .trim()
                    .split('(')
                    .next()
                    .unwrap_or(line)
                    .trim()
                    .to_string();
                if !field.is_empty() && !field.starts_with('{') {
                    operations.push(GraphQlOperation {
                        operation_type: "Query".to_string(),
                        field_name: field,
                        type_name: current_type.clone(),
                        schema_path: schema_path.to_string(),
                        line: line_no as u32 + 1,
                    });
                }
            } else if line.starts_with("mutation ") {
                let field = line
                    .trim_start_matches("mutation")
                    .trim()
                    .split('(')
                    .next()
                    .unwrap_or(line)
                    .trim()
                    .to_string();
                if !field.is_empty() && !field.starts_with('{') {
                    operations.push(GraphQlOperation {
                        operation_type: "Mutation".to_string(),
                        field_name: field,
                        type_name: current_type.clone(),
                        schema_path: schema_path.to_string(),
                        line: line_no as u32 + 1,
                    });
                }
            } else if line.starts_with("subscription ") {
                let field = line
                    .trim_start_matches("subscription")
                    .trim()
                    .split('(')
                    .next()
                    .unwrap_or(line)
                    .trim()
                    .to_string();
                if !field.is_empty() && !field.starts_with('{') {
                    operations.push(GraphQlOperation {
                        operation_type: "Subscription".to_string(),
                        field_name: field,
                        type_name: current_type.clone(),
                        schema_path: schema_path.to_string(),
                        line: line_no as u32 + 1,
                    });
                }
            }
        }

        // Standalone query/mutation/subscription definitions
        if line.starts_with("type Query")
            || line.starts_with("type Mutation")
            || line.starts_with("type Subscription")
        {
            // Root type detected — schema-based sensor identified
        }
    }

    // Also parse standalone query/mutation/subscription definitions
    for (line_no, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.starts_with("query ")
            || line.starts_with("mutation ")
            || line.starts_with("subscription ")
        {
            let parts: Vec<&str> = line.split(&[' ', '('][..]).collect();
            if parts.len() >= 2 {
                let op_type = if line.starts_with("query") {
                    "Query"
                } else if line.starts_with("mutation") {
                    "Mutation"
                } else {
                    "Subscription"
                };
                let field = parts[1].to_string();
                operations.push(GraphQlOperation {
                    operation_type: op_type.to_string(),
                    field_name: field,
                    type_name: "Query".to_string(),
                    schema_path: schema_path.to_string(),
                    line: line_no as u32 + 1,
                });
            }
        }
    }

    operations
}

/// Enrich graph with GraphQL operations
pub fn enrich_with_graphql(
    graph: &GraphDatabase,
    schema_path: &Path,
    root: &Path,
    namespace: &crate::schema::RepoNamespace,
) -> Result<usize, LainError> {
    if graph.is_read_only() {
        return Ok(0);
    }
    // `root` is needed only to key nodes the way the rest of the graph is
    // keyed. The walker yields absolute paths; every other node path is
    // relative to the workspace, and the orphan sweep compares against
    // `graph_path`-reduced tracked files — so an absolute path made these
    // nodes look untracked and they were pruned in the same index pass
    // that created them.

    let content = std::fs::read_to_string(schema_path)?;
    let operations = parse_graphql(&content, &crate::graph::graph_path(root, schema_path));

    let mut count = 0;
    for op in &operations {
        let node_id = GraphNode::generate_id(
            &NodeType::Interface,
            &op.schema_path,
            &format!("{}:{}", op.operation_type, op.field_name),
            None,
            namespace,
        );

        let mut node = GraphNode::new(
            NodeType::Interface,
            format!("{}: {}", op.operation_type, op.field_name),
            op.schema_path.clone(),
        );
        node.id = node_id.clone();
        node.signature = Some(op.type_name.clone());
        graph.upsert_node(node)?;

        if let Some(resolver) =
            crate::server::sensors::util::find_handler_in_graph(graph, &op.field_name)
        {
            let edge = GraphEdge::new(EdgeType::Uses, resolver.id.clone(), node_id);
            graph.insert_edge(&edge)?;
            count += 1;
        }
    }

    Ok(count)
}

/// Scan workspace for GraphQL schemas
pub fn scan_workspace(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &crate::schema::RepoNamespace,
) -> Result<usize, LainError> {
    let mut count = 0;

    for entry in crate::server::sensors::util::walk_workspace(root) {
        let path = entry.path();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            // `" gql"` — with a leading space — could never match any
            // extension, so every `.gql` file was skipped. Nothing caught it
            // because `scan_workspace` had no caller.
            if ext == "graphql" || ext == "gql" {
                match enrich_with_graphql(graph, path, root, namespace) {
                    Ok(n) => count += n,
                    Err(e) => tracing::warn!("Failed to parse {:?}: {}", path, e),
                }
            }
        }
    }

    Ok(count)
}

/// Unit-struct Sensor impl. Discovery via
/// `inventory::submit!(SensorEntry(&GraphQlSensor))` below; no central
/// registry to edit.
pub struct GraphQlSensor;

impl crate::server::sensors::Sensor for GraphQlSensor {
    fn name(&self) -> &'static str {
        "graphql"
    }
    fn count_field(&self) -> crate::server::sensors::SensorCountField {
        crate::server::sensors::SensorCountField::Graphql
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

inventory::submit!(crate::server::sensors::SensorEntry(&GraphQlSensor));
