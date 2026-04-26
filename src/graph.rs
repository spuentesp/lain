//! Graph database with KùzuDB backend
//!
//! Manages the core knowledge graph of code relationships using native Cypher queries.

use crate::error::LainError;
use crate::schema::{GraphEdge, GraphNode, NodeType, EdgeType, SCHEMA_SQL};
use kuzu::{Database, Connection, SystemConfig, Value, LogicalType};
use std::path::Path;
use std::sync::Arc;
use tracing::{info, error};

/// Graph database using Kùzu
#[derive(Clone)]
pub struct GraphDatabase {
    db: Arc<Database>,
}

impl GraphDatabase {
    /// Create a new graph database, initializing schema if needed
    pub fn new(memory_path: &Path) -> Result<Self, LainError> {
        info!("Initializing KùzuDB at {:?}", memory_path);

        // Ensure parent directory exists
        if let Some(parent) = memory_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let db = Database::new(memory_path, SystemConfig::default())
            .map_err(|e| LainError::Database(format!("Failed to initialize KùzuDB: {}", e)))?;
        
        let db_arc = Arc::new(db);
        let instance = Self {
            db: db_arc,
        };

        // Initialize schema if Metadata table doesn't exist
        if !instance.has_table("Metadata")? {
            info!("Initializing new KùzuDB schema");
            instance.initialize_schema()?;
        }

        Ok(instance)
    }

    fn get_conn(&self) -> Result<Connection<'_>, LainError> {
        Connection::new(&self.db)
            .map_err(|e| LainError::Database(format!("Failed to create Kùzu connection: {}", e)))
    }

    fn has_table(&self, table_name: &str) -> Result<bool, LainError> {
        let conn = self.get_conn()?;
        let mut result = conn.query("CALL SHOW_TABLES() RETURN name")
            .map_err(|e| LainError::Database(format!("Failed to check tables: {}", e)))?;
        
        while let Some(row) = result.next() {
            if let Value::String(name) = &row[0] {
                if name.to_lowercase() == table_name.to_lowercase() {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn initialize_schema(&self) -> Result<(), LainError> {
        let conn = self.get_conn()?;
        // Execute multi-statement schema SQL
        for statement in SCHEMA_SQL.split(';') {
            let trimmed = statement.trim();
            if !trimmed.is_empty() {
                conn.query(trimmed)
                    .map_err(|e| {
                        error!("Schema init failed at [{}]: {}", trimmed, e);
                        LainError::Database(format!("Schema init failed: {}", e))
                    })?;
            }
        }
        Ok(())
    }

    /// Insert a node into the graph
    pub fn insert_node(&self, node: &GraphNode) -> Result<(), LainError> {
        let conn = self.get_conn()?;
        let table = node_type_to_table(node.node_type.clone());
        
        let query = format!(
            "MERGE (n:{} {{id: $id}}) 
             SET n.name = $name, 
                 n.path = $path, 
                 n.line_start = $line_start, 
                 n.line_end = $line_end,
                 n.signature = $signature,
                 n.anchor_score = $anchor_score,
                 n.fan_in = $fan_in,
                 n.fan_out = $fan_out,
                 n.depth_from_main = $depth_from_main,
                 n.co_change_count = $co_change_count",
            table
        );

        let mut stmt = conn.prepare(&query)
            .map_err(|e| LainError::Database(format!("Failed to prepare node insert: {}", e)))?;

        conn.execute(&mut stmt, vec![
            ("id", Value::String(node.id.clone())),
            ("name", Value::String(node.name.clone())),
            ("path", Value::String(node.path.clone())),
            ("line_start", node.line_start.map(|v| Value::Int64(v as i64)).unwrap_or_else(|| Value::Null(LogicalType::Int64))),
            ("line_end", node.line_end.map(|v| Value::Int64(v as i64)).unwrap_or_else(|| Value::Null(LogicalType::Int64))),
            ("signature", node.signature.as_ref().map(|v| Value::String(v.clone())).unwrap_or_else(|| Value::Null(LogicalType::String))),
            ("anchor_score", node.anchor_score.map(|v| Value::Float(v)).unwrap_or_else(|| Value::Null(LogicalType::Float))),
            ("fan_in", node.fan_in.map(|v| Value::Int64(v as i64)).unwrap_or_else(|| Value::Null(LogicalType::Int64))),
            ("fan_out", node.fan_out.map(|v| Value::Int64(v as i64)).unwrap_or_else(|| Value::Null(LogicalType::Int64))),
            ("depth_from_main", node.depth_from_main.map(|v| Value::Int64(v as i64)).unwrap_or_else(|| Value::Null(LogicalType::Int64))),
            ("co_change_count", node.co_change_count.map(|v| Value::Int64(v as i64)).unwrap_or_else(|| Value::Null(LogicalType::Int64))),
        ]).map_err(|e| LainError::Database(format!("Failed to execute node insert: {}", e)))?;

        Ok(())
    }

    /// Insert multiple nodes into the graph
    pub fn insert_nodes_batch(&self, new_nodes: &[GraphNode]) -> Result<(), LainError> {
        for node in new_nodes {
            self.insert_node(node)?;
        }
        Ok(())
    }

    /// Insert an edge into the graph
    pub fn insert_edge(&self, edge: &GraphEdge) -> Result<(), LainError> {
        let conn = self.get_conn()?;
        let rel_type = format!("{:?}", edge.edge_type).to_uppercase();
        
        let query = format!(
            "MATCH (a), (b) WHERE a.id = $source_id AND b.id = $target_id 
             MERGE (a)-[r:{}]->(b)
             SET r.weight = $weight",
            rel_type
        );

        let mut stmt = conn.prepare(&query)
            .map_err(|e| LainError::Database(format!("Failed to prepare edge insert: {}", e)))?;

        conn.execute(&mut stmt, vec![
            ("source_id", Value::String(edge.source_id.clone())),
            ("target_id", Value::String(edge.target_id.clone())),
            ("weight", edge.weight.map(|v| Value::Float(v)).unwrap_or_else(|| Value::Null(LogicalType::Float))),
        ]).map_err(|e| LainError::Database(format!("Failed to execute edge insert: {}", e)))?;

        Ok(())
    }

    /// Insert multiple edges into the graph
    pub fn insert_edges_batch(&self, new_edges: &[GraphEdge]) -> Result<(), LainError> {
        for edge in new_edges {
            self.insert_edge(edge)?;
        }
        Ok(())
    }

    /// Get a node by ID
    pub fn get_node(&self, id: &str) -> Result<Option<GraphNode>, LainError> {
        let conn = self.get_conn()?;
        let mut result = conn.query(&format!("MATCH (n) WHERE n.id = '{}' RETURN n, label(n)", id))
            .map_err(|e| LainError::Database(format!("Failed to query node: {}", e)))?;

        if let Some(row) = result.next() {
            return Ok(Some(self.extract_node_from_row(&row[0], &row[1].to_string())?));
        }
        Ok(None)
    }

    /// Alias for get_node to maintain compatibility with navigation handlers
    pub fn get_node_by_id(&self, id: &str) -> Result<Option<GraphNode>, LainError> {
        self.get_node(id)
    }

    /// Get all nodes of a specific type
    pub fn get_nodes_by_type(&self, node_type: NodeType) -> Result<Vec<GraphNode>, LainError> {
        let conn = self.get_conn()?;
        let table = node_type_to_table(node_type);
        let mut result = conn.query(&format!("MATCH (n:{}) RETURN n, '{}'", table, table))
            .map_err(|e| LainError::Database(format!("Failed to query nodes by type: {}", e)))?;

        let mut nodes = Vec::new();
        while let Some(row) = result.next() {
            nodes.push(self.extract_node_from_row(&row[0], &row[1].to_string())?);
        }
        Ok(nodes)
    }

    fn extract_node_from_row(&self, n_val: &Value, label: &str) -> Result<GraphNode, LainError> {
        if let Value::Node(node_val) = n_val {
            let props = node_val.get_properties();
            let mut id = String::new();
            let mut name = String::new();
            let mut path = String::new();
            
            let mut line_start = None;
            let mut line_end = None;
            let mut signature = None;
            let mut anchor_score = None;
            let mut fan_in = None;
            let mut fan_out = None;
            let mut depth_from_main = None;
            let mut co_change_count = None;

            for (p_name, p_val) in props {
                match p_name.as_str() {
                    "id" => if let Value::String(s) = p_val { id = s.to_string(); },
                    "name" => if let Value::String(s) = p_val { name = s.to_string(); },
                    "path" => if let Value::String(s) = p_val { path = s.to_string(); },
                    "line_start" => if let Value::Int64(i) = p_val { line_start = Some(*i as u32); },
                    "line_end" => if let Value::Int64(i) = p_val { line_end = Some(*i as u32); },
                    "signature" => if let Value::String(s) = p_val { signature = Some(s.to_string()); },
                    "anchor_score" => if let Value::Float(f) = p_val { anchor_score = Some(*f); },
                    "fan_in" => if let Value::Int64(i) = p_val { fan_in = Some(*i as u32); },
                    "fan_out" => if let Value::Int64(i) = p_val { fan_out = Some(*i as u32); },
                    "depth_from_main" => if let Value::Int64(i) = p_val { depth_from_main = Some(*i as u32); },
                    "co_change_count" => if let Value::Int64(i) = p_val { co_change_count = Some(*i as u32); },
                    _ => {}
                }
            }

            let node_type = table_to_node_type(label);
            let mut node = GraphNode::new(node_type, name, path);
            node.id = id;
            node.line_start = line_start;
            node.line_end = line_end;
            node.signature = signature;
            node.anchor_score = anchor_score;
            node.fan_in = fan_in;
            node.fan_out = fan_out;
            node.depth_from_main = depth_from_main;
            node.co_change_count = co_change_count;
            
            Ok(node)
        } else {
            Err(LainError::Database("Value is not a Node".to_string()))
        }
    }

    /// Get all outgoing edges from a node
    pub fn get_edges_from(&self, source_id: &str) -> Result<Vec<GraphEdge>, LainError> {
        let conn = self.get_conn()?;
        let query = format!("MATCH (a)-[r]->(b) WHERE a.id = '{}' RETURN b.id, type(r), r.weight", source_id);
        
        let mut result = conn.query(&query)
            .map_err(|e| LainError::Database(format!("Failed to execute edges query: {}", e)))?;

        let mut edges = Vec::new();
        while let Some(row) = result.next() {
            let target_id = row[0].to_string();
            let rel_type_str = row[1].to_string();
            let weight = match &row[2] { Value::Float(f) => Some(*f), _ => None };
            
            let edge_type = str_to_edge_type(&rel_type_str);
            let mut edge = GraphEdge::new(edge_type, source_id.to_string(), target_id);
            edge.weight = weight;
            edges.push(edge);
        }
        Ok(edges)
    }

    /// Calculate anchor scores for all nodes using native graph aggregations
    pub fn calculate_anchor_scores(&self) -> Result<(), LainError> {
        info!("Calculating anchor scores via Cypher");
        let conn = self.get_conn()?;
        
        let query = "
            MATCH (n)
            OPTIONAL MATCH (n)<-[in]-()
            OPTIONAL MATCH (n)-[out]->()
            WITH n, count(DISTINCT in) as f_in, count(DISTINCT out) as f_out
            SET n.fan_in = f_in,
                n.fan_out = f_out,
                n.anchor_score = to_float(f_in) / (to_float(f_out) + 1.0)
        ";
        
        conn.query(query)
            .map_err(|e| LainError::Database(format!("Failed to calculate anchor scores: {}", e)))?;
        
        Ok(())
    }

    /// Find structural anchors
    pub fn find_anchors(&self, limit: usize) -> Result<Vec<GraphNode>, LainError> {
        let conn = self.get_conn()?;
        let query = format!(
            "MATCH (n) WHERE n.anchor_score IS NOT NULL 
             RETURN n, label(n) 
             ORDER BY n.anchor_score DESC LIMIT {}", 
            limit
        );
        
        let mut result = conn.query(&query)
            .map_err(|e| LainError::Database(format!("Failed to query anchors: {}", e)))?;

        let mut nodes = Vec::new();
        while let Some(row) = result.next() {
            nodes.push(self.extract_node_from_row(&row[0], &row[1].to_string())?);
        }
        Ok(nodes)
    }

    /// Calculate distance from entry points using recursive path finding
    pub fn calculate_depths(&self) -> Result<(), LainError> {
        info!("Calculating depths via recursive Cypher matching");
        let conn = self.get_conn()?;

        conn.query("MATCH (n) SET n.depth_from_main = NULL")
            .map_err(|e| LainError::Database(format!("Failed to reset depths: {}", e)))?;

        conn.query("
            MATCH (n)
            WHERE n.name = 'main' OR n.name = 'App' OR n.name ENDS WITH '::main'
               OR NOT EXISTS { MATCH ()-[:CONTAINS]->(n) }
            SET n.depth_from_main = 0
        ").map_err(|e| LainError::Database(format!("Failed to set Layer 0: {}", e)))?;

        conn.query("
            MATCH (start)-[path:CONTAINS*]->(target)
            WHERE start.depth_from_main = 0 AND target.depth_from_main IS NULL
            WITH target, min(length(path)) as depth
            SET target.depth_from_main = depth
        ").map_err(|e| LainError::Database(format!("Failed to propagate depths: {}", e)))?;

        Ok(())
    }

    pub fn find_entry_points(&self) -> Result<Vec<GraphNode>, LainError> {
        let conn = self.get_conn()?;
        let query = "MATCH (n) WHERE n.name = 'main' OR n.name = 'App' OR n.name ENDS WITH '::main' RETURN n, label(n)";
        
        let mut result = conn.query(query)
            .map_err(|e| LainError::Database(format!("Failed to find entry points: {}", e)))?;

        let mut nodes = Vec::new();
        while let Some(row) = result.next() {
            nodes.push(self.extract_node_from_row(&row[0], &row[1].to_string())?);
        }
        Ok(nodes)
    }

    pub fn insert_co_change_edges(&self, pairs: &[(String, String, usize)]) -> Result<(), LainError> {
        let conn = self.get_conn()?;
        
        for (p1, p2, count) in pairs {
            let query = format!(
                "MATCH (a:File {{path: '{}'}}), (b:File {{path: '{}'}})
                MERGE (a)-[r:CO_CHANGED_WITH]->(b)
                SET r.weight = to_float({}),
                    a.co_change_count = {},
                    b.co_change_count = {}",
                p1, p2, count, count, count
            );
            conn.query(&query).map_err(|e| LainError::Database(e.to_string()))?;
        }
        Ok(())
    }

    pub fn get_co_change_partners(&self, file_path: &str) -> Result<Vec<(String, usize)>, LainError> {
        let conn = self.get_conn()?;
        let query = format!(
            "MATCH (a:File {{path: '{}'}})-[r:CO_CHANGED_WITH]-(b:File)
            RETURN b.path, r.weight
            ORDER BY r.weight DESC",
            file_path
        );
        
        let mut result = conn.query(&query).map_err(|e| LainError::Database(e.to_string()))?;

        let mut partners = Vec::new();
        while let Some(row) = result.next() {
            let path = row[0].to_string();
            let weight = match &row[1] { Value::Float(f) => *f as usize, _ => 0 };
            partners.push((path, weight));
        }
        Ok(partners)
    }

    pub fn get_last_commit(&self) -> Result<Option<String>, LainError> {
        let conn = self.get_conn()?;
        let query = "MATCH (m:Metadata {key: 'last_commit'}) RETURN m.value";
        let mut result = conn.query(query).map_err(|e| LainError::Database(e.to_string()))?;
        
        if let Some(row) = result.next() {
            return Ok(Some(row[0].to_string()));
        }
        Ok(None)
    }

    pub fn set_last_commit(&self, hash: String) -> Result<(), LainError> {
        let conn = self.get_conn()?;
        let query = format!("MERGE (m:Metadata {{key: 'last_commit'}}) SET m.value = '{}'", hash);
        conn.query(&query).map_err(|e| LainError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn get_stats(&self) -> (usize, usize) {
        let conn = match self.get_conn() {
            Ok(c) => c,
            Err(_) => return (0, 0),
        };
        
        let nodes = conn.query("MATCH (n) RETURN count(n)")
            .map(|mut r| if let Some(row) = r.next() { row[0].to_string().parse::<usize>().unwrap_or(0) } else { 0 })
            .unwrap_or(0);

        let edges = conn.query("MATCH ()-[r]->() RETURN count(r)")
            .map(|mut r| if let Some(row) = r.next() { row[0].to_string().parse::<usize>().unwrap_or(0) } else { 0 })
            .unwrap_or(0);
            
        (nodes, edges)
    }

    pub fn get_node_at_location(&self, path: &str, line: u32) -> Option<GraphNode> {
        let conn = self.get_conn().ok()?;
        let query = format!(
            "MATCH (n) 
            WHERE n.path = '{}' AND label(n) <> 'File'
              AND n.line_start <= {} AND n.line_end >= {}
            RETURN n, label(n)
            ORDER BY (n.line_end - n.line_start) ASC
            LIMIT 1",
            path, line, line
        );
        let mut result = conn.query(&query).ok()?;

        if let Some(row) = result.next() {
            return self.extract_node_from_row(&row[0], &row[1].to_string()).ok();
        }
        None
    }

    pub fn has_references_from(&self, id: &str) -> bool {
        let conn = match self.get_conn() {
            Ok(c) => c,
            Err(_) => return false,
        };
        let query = format!("MATCH (a)-[r]->() WHERE a.id = '{}' AND (type(r) = 'CALLS' OR type(r) = 'USES') RETURN count(r) > 0", id);
        let mut result = match conn.query(&query) { Ok(r) => r, Err(_) => return false };
        
        if let Some(row) = result.next() {
            return row[0].to_string() == "true";
        }
        false
    }
}

fn node_type_to_table(nt: NodeType) -> &'static str {
    match nt {
        NodeType::File => "File",
        NodeType::Module | NodeType::Namespace | NodeType::Package => "Module",
        NodeType::Class => "Class",
        NodeType::Method | NodeType::Function => "Function",
        NodeType::Interface => "Interface",
        NodeType::Struct => "Struct",
        NodeType::Enum => "Enum",
        NodeType::Trait => "Trait",
        NodeType::Constant | NodeType::Variable | NodeType::Property => "Constant",
    }
}

fn table_to_node_type(table: &str) -> NodeType {
    match table {
        "File" => NodeType::File,
        "Module" => NodeType::Module,
        "Class" => NodeType::Class,
        "Function" => NodeType::Function,
        "Interface" => NodeType::Interface,
        "Struct" => NodeType::Struct,
        "Enum" => NodeType::Enum,
        "Trait" => NodeType::Trait,
        "Constant" => NodeType::Constant,
        _ => NodeType::Module,
    }
}

fn str_to_edge_type(s: &str) -> EdgeType {
    match s.to_uppercase().as_str() {
        "CONTAINS" => EdgeType::Contains,
        "IMPORTS" => EdgeType::Imports,
        "CALLS" => EdgeType::Calls,
        "INHERITS" => EdgeType::Inherits,
        "IMPLEMENTS" => EdgeType::Implements,
        "EXTENDS" => EdgeType::Extends,
        "USES" => EdgeType::Uses,
        "REFERENCES" => EdgeType::References,
        "DEFINED_AT" => EdgeType::DefinedAt,
        "IS_TYPE_OF" => EdgeType::IsTypeOf,
        "CO_CHANGED_WITH" => EdgeType::CoChangedWith,
        _ => EdgeType::Contains,
    }
}
