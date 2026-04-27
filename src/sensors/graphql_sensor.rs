//! GraphQL protocol sensor
//!
//! Parses GraphQL schema and query definitions to extract operations
//! and maps them to resolver implementations.
//!
//! Edges created: Uses (resolver -> GraphQL type)

use crate::graph::GraphDatabase;
use crate::schema::{GraphNode, GraphEdge, NodeType, EdgeType};
use crate::error::LainError;
use std::path::Path;

/// GraphQL operation extracted from schema
#[derive(Debug, Clone)]
pub struct GraphQlOperation {
    pub operation_type: String,  // Query, Mutation, Subscription
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
                let field = line.trim_start_matches("query").trim().split('(').next().unwrap_or(line).trim().to_string();
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
                let field = line.trim_start_matches("mutation").trim().split('(').next().unwrap_or(line).trim().to_string();
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
                let field = line.trim_start_matches("subscription").trim().split('(').next().unwrap_or(line).trim().to_string();
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
        if line.starts_with("type Query") || line.starts_with("type Mutation") || line.starts_with("type Subscription") {
            // Root type detected — schema-based sensor identified
        }
    }

    // Also parse standalone query/mutation/subscription definitions
    for (line_no, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.starts_with("query ") || line.starts_with("mutation ") || line.starts_with("subscription ") {
            let parts: Vec<&str> = line.split(&[' ', '('][..]).collect();
            if parts.len() >= 2 {
                let op_type = if line.starts_with("query") { "Query" } else if line.starts_with("mutation") { "Mutation" } else { "Subscription" };
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

/// Find resolver in graph by field name
fn find_resolver(graph: &GraphDatabase, field_name: &str) -> Option<GraphNode> {
    graph.find_node_by_name(field_name)
        .or_else(|| graph.find_node_by_name(&to_camel_case(field_name)))
        .or_else(|| graph.find_node_by_name(&to_snake_case(field_name)))
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

/// Enrich graph with GraphQL operations
pub fn enrich_with_graphql(
    graph: &GraphDatabase,
    schema_path: &Path,
) -> Result<usize, LainError> {
    let content = std::fs::read_to_string(schema_path)?;
    let operations = parse_graphql(&content, &schema_path.to_string_lossy());

    let mut count = 0;
    for op in &operations {
        let node_id = GraphNode::generate_id(
            &NodeType::Interface,
            &op.schema_path,
            &format!("{}:{}", op.operation_type, op.field_name),
        );

        let mut node = GraphNode::new(
            NodeType::Interface,
            format!("{}: {}", op.operation_type, op.field_name),
            op.schema_path.clone(),
        );
        node.id = node_id.clone();
        node.signature = Some(op.type_name.clone());
        graph.upsert_node(node)?;

        if let Some(resolver) = find_resolver(graph, &op.field_name) {
            let edge = GraphEdge::new(
                EdgeType::Uses,
                resolver.id.clone(),
                node_id,
            );
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
) -> Result<usize, LainError> {
    let mut count = 0;

    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .build();

    for entry in walker.flatten() {
        let path = entry.path();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if ext == "graphql" || ext == " gql" {
                match enrich_with_graphql(graph, path) {
                    Ok(n) => count += n,
                    Err(e) => tracing::warn!("Failed to parse {:?}: {}", path, e),
                }
            }
        }
    }

    Ok(count)
}