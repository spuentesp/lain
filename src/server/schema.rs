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
    CrossRepoSameSymbol, // Federation-only: same symbol across different repos (added Task 10)
}

impl std::fmt::Display for EdgeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// True for "type-level" node kinds: structs, enums, traits, classes,
/// interfaces. A `Uses` edge is only emitted toward these kinds —
/// pointing a `Uses` edge at a function or method would be misleading
/// (the reference isn't to the function object, it's to its type). The
/// scanner's tree-sitter resolve phase enforces this so cross-file
/// symbol references resolve to declarations rather than implementations.
pub fn is_type_level_target(t: &NodeType) -> bool {
    matches!(
        t,
        NodeType::Struct
            | NodeType::Enum
            | NodeType::Trait
            | NodeType::Class
            | NodeType::Interface
    )
}

/// A node in the knowledge graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub node_type: NodeType,
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub line_start: Option<u32>,
    #[serde(default)]
    pub line_end: Option<u32>,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub docstring: Option<String>,
    #[serde(default)]
    pub embedding: Option<String>,
    #[serde(default)]
    pub fan_in: Option<u32>,
    #[serde(default)]
    pub fan_out: Option<u32>,
    #[serde(default)]
    pub anchor_score: Option<f32>,
    #[serde(default)]
    pub depth_from_main: Option<u32>,
    #[serde(default)]
    pub co_change_count: Option<usize>,
    #[serde(default)]
    pub is_deprecated: bool,
    /// Single label for the node (e.g. "test", "deprecated", "async").
    /// Used by `find ... | filter label X`. For multi-label semantics use
    /// `signature` with a structured payload.
    #[serde(default)]
    pub label: Option<String>,
    // Staleness Metadata
    #[serde(default)]
    pub last_lsp_sync: Option<i64>,
    #[serde(default)]
    pub last_git_sync: Option<i64>,
    #[serde(default)]
    pub commit_hash: Option<String>,
    #[serde(default)]
    pub is_hydrated: bool,
}

impl GraphNode {
    /// Generate a stable UUID for a graph node.
    ///
    /// `line_start` is included when known so that two symbols sharing the same
    /// (type, path, name) — e.g. a top-level `fn add` and an `impl` method
    /// `add` — get distinct IDs. Pass `None` for nodes where line range is
    /// not meaningful (e.g. sensors that produce one node per external entity).
    pub fn generate_id(
        node_type: &NodeType,
        path: &str,
        name: &str,
        line_start: Option<u32>,
    ) -> String {
        let id_input = match line_start {
            Some(line) => format!("{:?}:{}:{}:{}", node_type, path, name, line),
            None => format!("{:?}:{}:{}", node_type, path, name),
        };
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, id_input.as_bytes()).to_string()
    }

    pub fn new(node_type: NodeType, name: String, path: String) -> Self {
        let id = Self::generate_id(&node_type, &path, &name, None);

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
            label: None,
            last_lsp_sync: None,
            last_git_sync: None,
            commit_hash: None,
            is_hydrated: true,
        }
    }

    pub fn with_location(mut self, line_start: u32, line_end: u32) -> Self {
        self.line_start = Some(line_start);
        self.line_end = Some(line_end);
        // Re-derive the ID so two same-named symbols at different lines
        // (e.g. top-level fn vs impl method) get distinct IDs.
        self.id = Self::generate_id(&self.node_type, &self.path, &self.name, Some(line_start));
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
