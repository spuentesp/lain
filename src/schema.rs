//! Graph schema definitions for Lain
//!
//! Defines nodes, edges, and attributes for the knowledge graph.

use serde::{Deserialize, Serialize};

/// Node types in the Lain graph
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum NodeType {
    File,
    Namespace,
    Module,
    Package,
    Class,
    Interface,
    Struct,
    Enum,
    Trait,
    Function,
    Method,
    Property,
    Variable,
    Constant,
    // Cross-runtime node types
    HttpRoute,  // HTTP endpoint (e.g., GET /api/users)
    Topic,      // Message queue topic (Kafka, RabbitMQ)
    Resource,   // IaC resource (Terraform, k8s)
    Schema,     // Data schema (OpenAPI, Protobuf, JSON Schema)
}

impl std::fmt::Display for NodeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// Edge types in the Lain graph
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EdgeType {
    Contains,       // File -> Symbol
    Calls,         // Function -> Function
    Uses,          // Code -> Variable/Type
    Implements,   // Class -> Interface
    Imports,       // File -> File
    CoChangedWith, // File -> File (Git temporal coupling)
    Pattern,       // Semantic boundary indicator (path prefixes, topic names)
    // Cross-runtime edge types
    CallsHttp,     // HTTP route -> handler (method, path pattern)
    Produces,      // Producer -> Topic (Kafka producer, event emitter)
    Consumes,      // Consumer -> Topic (Kafka consumer, queue listener)
    DeployedTo,    // IaC resource -> cloud resource (k8s, AWS, etc.)
}

impl std::fmt::Display for EdgeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// A node in the knowledge graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub node_type: NodeType,
    pub name: String,
    pub path: String,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    pub signature: Option<String>,
    pub docstring: Option<String>,
    pub embedding: Option<String>,
    pub fan_in: Option<u32>,
    pub fan_out: Option<u32>,
    pub anchor_score: Option<f32>,
    pub depth_from_main: Option<u32>,
    pub co_change_count: Option<usize>,
    pub is_deprecated: bool,
    // Staleness Metadata
    pub last_lsp_sync: Option<i64>,
    pub last_git_sync: Option<i64>,
    pub commit_hash: Option<String>,
    pub is_hydrated: bool,
}

impl GraphNode {
    pub fn generate_id(node_type: &NodeType, path: &str, name: &str) -> String {
        let id_input = format!("{:?}:{}:{}", node_type, path, name);
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, id_input.as_bytes()).to_string()
    }

    pub fn new(node_type: NodeType, name: String, path: String) -> Self {
        let id = Self::generate_id(&node_type, &path, &name);

        Self {
            id,
            node_type,
            name,
            path,
            line_start: None,
            line_end: None,
            signature: None,
            docstring: None,
            embedding: None,
            fan_in: None,
            fan_out: None,
            anchor_score: None,
            depth_from_main: None,
            co_change_count: None,
            is_deprecated: false,
            last_lsp_sync: None,
            last_git_sync: None,
            commit_hash: None,
            is_hydrated: true,
        }
    }

    pub fn with_location(mut self, line_start: u32, line_end: u32) -> Self {
        self.line_start = Some(line_start);
        self.line_end = Some(line_end);
        self
    }
}

/// An edge in the knowledge graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub edge_type: EdgeType,
    pub source_id: String,
    pub target_id: String,
    pub weight: Option<f32>,
}

impl GraphEdge {
    pub fn new(edge_type: EdgeType, source_id: String, target_id: String) -> Self {
        Self {
            edge_type,
            source_id,
            target_id,
            weight: None,
        }
    }
}
