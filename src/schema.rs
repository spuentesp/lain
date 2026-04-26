//! Graph schema definitions for Lain
//!
//! Defines nodes, edges, and attributes for the KùzuDB graph.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Node types in the Lain graph
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum NodeType {
    File,
    Module,
    Namespace,
    Package,
    Class,
    Method,
    Property,
    Interface,
    Function,
    Variable,
    Constant,
    Struct,
    Enum,
    Trait,
}

/// Edge types in the Lain graph
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "UPPERCASE")]
pub enum EdgeType {
    Contains,
    Imports,
    Calls,
    Inherits,
    Implements,
    Extends,
    Uses,
    References,
    DefinedAt,
    IsTypeOf,
    CoChangedWith,
}

/// A node in the Lain graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub node_type: NodeType,
    pub name: String,
    pub path: String,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    pub docstring: Option<String>,
    pub embedding: Option<String>,
    pub signature: Option<String>,
    pub anchor_score: Option<f32>,
    pub fan_in: Option<u32>,
    pub fan_out: Option<u32>,
    pub depth_from_main: Option<u32>,
    pub co_change_count: Option<u32>,
    pub is_deprecated: bool,
    // Staleness Metadata
    pub last_lsp_sync: Option<i64>,
    pub last_git_sync: Option<i64>,
    pub commit_hash: Option<String>,
}

impl GraphNode {
    pub fn new(node_type: NodeType, name: String, path: String) -> Self {
        // Deterministic ID based on type, path and name
        let id_input = format!("{:?}:{}:{}", node_type, path, name);
        let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, id_input.as_bytes()).to_string();

        Self {
            id,
            node_type,
            name,
            path,
            line_start: None,
            line_end: None,
            docstring: None,
            embedding: None,
            signature: None,
            anchor_score: None,
            fan_in: None,
            fan_out: None,
            depth_from_main: None,
            co_change_count: None,
            is_deprecated: false,
            last_lsp_sync: None,
            last_git_sync: None,
            commit_hash: None,
        }
    }

    pub fn with_location(mut self, line_start: u32, line_end: u32) -> Self {
        self.line_start = Some(line_start);
        self.line_end = Some(line_end);
        self
    }
}

/// An edge in the Lain graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub id: String,
    pub edge_type: EdgeType,
    pub source_id: String,
    pub target_id: String,
    pub weight: Option<f32>,
}

impl GraphEdge {
    pub fn new(edge_type: EdgeType, source_id: String, target_id: String) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            edge_type,
            source_id,
            target_id,
            weight: None,
        }
    }
}

/// Result of a graph query
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

/// Blast radius result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlastRadiusResult {
    pub affected_paths: Vec<String>,
    pub total_nodes: usize,
    pub depth: usize,
}

/// Semantic search result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticSearchResult {
    pub node: GraphNode,
    pub score: f32,
}

/// Central node info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CentralNode {
    pub name: String,
    pub path: String,
    pub connections: usize,
}

/// Architecture summary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureSummary {
    pub files: Vec<CentralNode>,
    pub modules: Vec<CentralNode>,
    pub central_nodes: Vec<CentralNode>,
    pub patterns: Vec<String>,
}

/// KùzuDB schema SQL
pub const SCHEMA_SQL: &str = r#"
CREATE NODE TABLE File (id STRING, name STRING, path STRING, line_start INT64, line_end INT64, docstring STRING, embedding STRING, signature STRING, anchor_score FLOAT, fan_in INT64, fan_out INT64, depth_from_main INT64, co_change_count INT64, PRIMARY KEY(id));
CREATE NODE TABLE Module (id STRING, name STRING, path STRING, line_start INT64, line_end INT64, docstring STRING, embedding STRING, signature STRING, anchor_score FLOAT, fan_in INT64, fan_out INT64, depth_from_main INT64, co_change_count INT64, PRIMARY KEY(id));
CREATE NODE TABLE Class (id STRING, name STRING, path STRING, line_start INT64, line_end INT64, docstring STRING, embedding STRING, signature STRING, anchor_score FLOAT, fan_in INT64, fan_out INT64, depth_from_main INT64, co_change_count INT64, PRIMARY KEY(id));
CREATE NODE TABLE Function (id STRING, name STRING, path STRING, line_start INT64, line_end INT64, docstring STRING, embedding STRING, signature STRING, anchor_score FLOAT, fan_in INT64, fan_out INT64, depth_from_main INT64, co_change_count INT64, PRIMARY KEY(id));
CREATE NODE TABLE Interface (id STRING, name STRING, path STRING, line_start INT64, line_end INT64, docstring STRING, embedding STRING, signature STRING, anchor_score FLOAT, fan_in INT64, fan_out INT64, depth_from_main INT64, co_change_count INT64, PRIMARY KEY(id));
CREATE NODE TABLE Struct (id STRING, name STRING, path STRING, line_start INT64, line_end INT64, docstring STRING, embedding STRING, signature STRING, anchor_score FLOAT, fan_in INT64, fan_out INT64, depth_from_main INT64, co_change_count INT64, PRIMARY KEY(id));
CREATE NODE TABLE Enum (id STRING, name STRING, path STRING, line_start INT64, line_end INT64, docstring STRING, embedding STRING, signature STRING, anchor_score FLOAT, fan_in INT64, fan_out INT64, depth_from_main INT64, co_change_count INT64, PRIMARY KEY(id));
CREATE NODE TABLE Trait (id STRING, name STRING, path STRING, line_start INT64, line_end INT64, docstring STRING, embedding STRING, signature STRING, anchor_score FLOAT, fan_in INT64, fan_out INT64, depth_from_main INT64, co_change_count INT64, PRIMARY KEY(id));
CREATE NODE TABLE Constant (id STRING, name STRING, path STRING, line_start INT64, line_end INT64, docstring STRING, embedding STRING, signature STRING, anchor_score FLOAT, fan_in INT64, fan_out INT64, depth_from_main INT64, co_change_count INT64, PRIMARY KEY(id));
CREATE NODE TABLE Metadata (key STRING, value STRING, PRIMARY KEY(key));

CREATE REL TABLE CONTAINS (FROM File TO Module, Class, Function, Interface, Struct, Enum, Trait, Constant);
CREATE REL TABLE IMPORTS (FROM Module, Class, Function, Struct, Enum, Trait TO Module, Class, Function, Interface, Struct, Enum, Trait, Constant);
CREATE REL TABLE CALLS (FROM Function TO Function);
CREATE REL TABLE CO_CHANGED_WITH (FROM File TO File, weight FLOAT);
"#;
