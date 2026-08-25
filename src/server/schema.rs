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

impl NodeType {
    /// Every variant, in declaration order.
    ///
    /// `describe_schema` builds its node-type list from this so the
    /// documented schema cannot drift from the graph again. It had:
    /// the tool advertised five types while the indexer emitted
    /// eighteen, so an agent following the documentation queried
    /// `type: "Function"` for a method and got zero results while
    /// `find_anchors` was ranking that same method first.
    pub fn all() -> &'static [NodeType] {
        &[
            NodeType::File,
            NodeType::Namespace,
            NodeType::Module,
            NodeType::Package,
            NodeType::Class,
            NodeType::Interface,
            NodeType::Struct,
            NodeType::Enum,
            NodeType::Trait,
            NodeType::Function,
            NodeType::Method,
            NodeType::Property,
            NodeType::Variable,
            NodeType::Constant,
            NodeType::HttpRoute,
            NodeType::Topic,
            NodeType::Resource,
            NodeType::Schema,
        ]
    }

    /// One-line description for `describe_schema`.
    pub fn description(&self) -> &'static str {
        match self {
            NodeType::File => "A source file",
            NodeType::Namespace => "A namespace",
            NodeType::Module => "A module",
            NodeType::Package => "A package",
            NodeType::Class => "A class definition",
            NodeType::Interface => "An interface definition",
            NodeType::Struct => "A struct definition",
            NodeType::Enum => "An enum definition",
            NodeType::Trait => "A trait definition",
            NodeType::Function => "A free function definition",
            NodeType::Method => {
                "A method defined in an impl block, class, or trait. Distinct \
                 from Function — in Rust and other impl-heavy languages most \
                 code lives here, so a query filtered to Function alone will \
                 miss it"
            }
            NodeType::Property => "A property or field",
            NodeType::Variable => "A variable binding",
            NodeType::Constant => "A constant",
            NodeType::HttpRoute => "An HTTP endpoint (e.g. GET /api/users)",
            NodeType::Topic => "A message queue topic (Kafka, RabbitMQ)",
            NodeType::Resource => "An IaC resource (Terraform, k8s)",
            NodeType::Schema => "A data schema (OpenAPI, Protobuf, JSON Schema)",
        }
    }

    /// Node properties a query may filter on.
    pub fn properties(&self) -> &'static [&'static str] {
        match self {
            NodeType::File => &["path", "language"],
            NodeType::Function | NodeType::Method => &["name", "path", "signature"],
            _ => &["name", "path"],
        }
    }

    /// Labels that may be attached to this node type.
    pub fn labels(&self) -> &'static [&'static str] {
        match self {
            NodeType::File => &["generated", "test"],
            NodeType::Function | NodeType::Method => &["test", "deprecated", "async"],
            NodeType::Class | NodeType::Struct | NodeType::Enum | NodeType::Trait => {
                &["test", "deprecated"]
            }
            _ => &[],
        }
    }

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

impl EdgeType {
    /// Every variant, in declaration order. `describe_schema` builds
    /// its edge-type list from this.
    ///
    /// The hand-written list had drifted badly: it advertised `Defines`,
    /// `Inherits`, `TestedBy` and `Import` — none of which exist — while
    /// omitting `Imports`, `CoChangedWith` (the co-change coupling lain
    /// advertises as a headline feature), `Pattern`, and every
    /// cross-runtime and federation edge. An agent building a query
    /// from the documented schema was choosing from a menu that was
    /// half fiction.
    pub fn all() -> &'static [EdgeType] {
        &[
            EdgeType::Contains,
            EdgeType::Calls,
            EdgeType::Uses,
            EdgeType::Implements,
            EdgeType::Imports,
            EdgeType::CoChangedWith,
            EdgeType::Pattern,
            EdgeType::CallsHttp,
            EdgeType::Produces,
            EdgeType::Consumes,
            EdgeType::DeployedTo,
            EdgeType::CrossRepoSameSymbol,
        ]
    }

    pub fn description(&self) -> &'static str {
        match self {
            EdgeType::Contains => "A file or module contains a symbol",
            EdgeType::Calls => "A function calls another function",
            EdgeType::Uses => "Code uses a variable or type",
            EdgeType::Implements => "A class implements an interface",
            EdgeType::Imports => "A file imports another file or module",
            EdgeType::CoChangedWith => {
                "Two files change together in git history — temporal coupling, \
                 not a static dependency"
            }
            EdgeType::Pattern => "A semantic boundary indicator (path prefix, topic name)",
            EdgeType::CallsHttp => "An HTTP route points at its handler",
            EdgeType::Produces => "A producer writes to a topic",
            EdgeType::Consumes => "A consumer reads from a topic",
            EdgeType::DeployedTo => "An IaC resource maps to a cloud resource",
            EdgeType::CrossRepoSameSymbol => {
                "Federation-only: the same symbol found in two different repos"
            }
        }
    }

    /// Node types an edge of this kind can start from.
    pub fn source_types(&self) -> &'static [NodeType] {
        match self {
            EdgeType::Contains => &[NodeType::File, NodeType::Module, NodeType::Namespace],
            EdgeType::Calls | EdgeType::Uses => &[NodeType::Function, NodeType::Method],
            EdgeType::Implements => &[NodeType::Class, NodeType::Struct],
            EdgeType::Imports | EdgeType::Pattern => &[NodeType::File],
            EdgeType::CoChangedWith => &[NodeType::File],
            EdgeType::CallsHttp => &[NodeType::HttpRoute],
            EdgeType::Produces | EdgeType::Consumes => &[NodeType::Function, NodeType::Method],
            EdgeType::DeployedTo => &[NodeType::Resource],
            EdgeType::CrossRepoSameSymbol => &[NodeType::Function, NodeType::Method],
        }
    }

    /// Node types an edge of this kind can point at.
    pub fn target_types(&self) -> &'static [NodeType] {
        match self {
            EdgeType::Contains => &[
                NodeType::Function,
                NodeType::Method,
                NodeType::Class,
                NodeType::Struct,
                NodeType::Enum,
                NodeType::Trait,
            ],
            EdgeType::Calls => &[NodeType::Function, NodeType::Method],
            EdgeType::Uses => &[
                NodeType::Class,
                NodeType::Struct,
                NodeType::Enum,
                NodeType::Variable,
                NodeType::Constant,
            ],
            EdgeType::Implements => &[NodeType::Interface, NodeType::Trait],
            EdgeType::Imports | EdgeType::CoChangedWith | EdgeType::Pattern => &[NodeType::File],
            EdgeType::CallsHttp => &[NodeType::Function, NodeType::Method],
            EdgeType::Produces | EdgeType::Consumes => &[NodeType::Topic],
            EdgeType::DeployedTo => &[NodeType::Resource],
            EdgeType::CrossRepoSameSymbol => &[NodeType::Function, NodeType::Method],
        }
    }
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
    /// Incoming edges of **every** kind, including the `Contains` edge
    /// from the symbol's own file. Useful as a coupling signal; useless
    /// as a "who calls this?" answer — see [`Self::calls_in`].
    #[serde(default)]
    pub fan_in: Option<u32>,
    /// Outgoing edges of every kind. See [`Self::calls_out`].
    #[serde(default)]
    pub fan_out: Option<u32>,
    /// Incoming `Calls` edges only — the number of callers.
    ///
    /// Separate from `fan_in` because conflating them is a live trap:
    /// every symbol has an incoming `Contains` edge from its file, so
    /// `fan_in == 0` is essentially never true and any dead-code check
    /// written against it silently reports nothing.
    #[serde(default)]
    pub calls_in: Option<u32>,
    /// Outgoing `Calls` edges only — the number of callees.
    #[serde(default)]
    pub calls_out: Option<u32>,
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
            calls_in: None,
            calls_out: None,
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
