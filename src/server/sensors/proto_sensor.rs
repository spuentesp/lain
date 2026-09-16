//! gRPC protocol sensor
//!
//! Parses .proto files to extract service/method definitions and maps them
//! to handler implementations via package path matching.
//!
//! Edges created: Implements (handler -> gRPC service method)

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use std::path::Path;

/// A gRPC service method extracted from a .proto file
#[derive(Debug, Clone)]
pub struct ProtoService {
    pub package: String,
    pub service_name: String,
    pub method_name: String,
    pub input_type: String,
    pub output_type: String,
    pub proto_path: String,
    pub line: u32,
}

/// Parse a .proto file and extract service definitions
pub fn parse_proto(content: &str, proto_path: &str) -> Vec<ProtoService> {
    let mut services = Vec::new();

    // Extract package
    let package = content
        .lines()
        .find(|l| l.trim().starts_with("package "))
        .map(|l| {
            l.trim()
                .trim_start_matches("package")
                .trim()
                .trim_end_matches(';')
                .trim()
                .to_string()
        })
        .unwrap_or_default();

    // Find all service blocks
    let mut in_service = false;
    let mut current_service = String::new();

    for (line_no, line) in content.lines().enumerate() {
        let line = line.trim();

        if line.starts_with("service ") {
            in_service = true;
            current_service = line
                .trim_start_matches("service")
                .trim()
                .trim_end_matches('{')
                .trim()
                .to_string();
        } else if in_service && line == "}" {
            in_service = false;
        } else if in_service && line.starts_with("rpc ") {
            let inner = line.trim_start_matches("rpc").trim();
            if let Some(paren_end) = inner.find('(') {
                let method_name = inner[..paren_end].trim().to_string();
                let rest = inner[paren_end..].trim_start_matches('(');

                if let Some(paren_close) = rest.find(')') {
                    let input_type = rest[..paren_close].trim().to_string();
                    let returns_part = rest[paren_close..]
                        .trim_start_matches(")")
                        .trim()
                        .trim_start_matches("returns")
                        .trim();
                    let output_type = returns_part
                        .trim_start_matches('(')
                        .trim_end_matches(')')
                        .trim()
                        .to_string();

                    if !method_name.is_empty() && !input_type.is_empty() {
                        services.push(ProtoService {
                            package: package.clone(),
                            service_name: current_service.clone(),
                            method_name,
                            input_type,
                            output_type,
                            proto_path: proto_path.to_string(),
                            line: line_no as u32 + 1,
                        });
                    }
                }
            }
        }
    }

    services
}

/// Enrich graph with gRPC service definitions
pub fn enrich_with_proto(
    graph: &GraphDatabase,
    proto_path: &Path,
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

    let content = std::fs::read_to_string(proto_path)?;
    let services = parse_proto(&content, &crate::graph::graph_path(root, proto_path));

    let mut count = 0;
    for svc in &services {
        let service_key = format!("{}.{}", svc.package, svc.service_name);
        let service_id = GraphNode::generate_id(
            &NodeType::Module,
            &svc.proto_path,
            &service_key,
            None,
            namespace,
        );

        let mut service_node = GraphNode::new(
            NodeType::Module,
            format!("{}.{}", svc.service_name, svc.method_name),
            svc.proto_path.clone(),
        );
        service_node.id = service_id.clone();
        service_node.signature = Some(format!("{} -> {}", svc.input_type, svc.output_type));
        graph.upsert_node(service_node)?;

        // Find handler by method name (not package-qualified)
        if let Some(handler) =
            crate::server::sensors::util::find_handler_in_graph(graph, &svc.method_name)
        {
            let edge = GraphEdge::new(EdgeType::Implements, handler.id.clone(), service_id);
            graph.insert_edge(&edge)?;
            count += 1;
        }
    }

    Ok(count)
}

/// Scan workspace for .proto files and enrich graph
pub fn scan_workspace(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &crate::schema::RepoNamespace,
) -> Result<usize, LainError> {
    let mut count = 0;

    for entry in crate::server::sensors::util::walk_workspace(root) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("proto") {
            match enrich_with_proto(graph, path, root, namespace) {
                Ok(n) => count += n,
                Err(e) => tracing::warn!("Failed to parse {:?}: {}", path, e),
            }
        }
    }

    Ok(count)
}

/// Unit-struct Sensor impl. Discovery via
/// `inventory::submit!(SensorEntry(&ProtoSensor))` below; no central
/// registry to edit.
pub struct ProtoSensor;

impl crate::server::sensors::Sensor for ProtoSensor {
    fn name(&self) -> &'static str {
        "proto"
    }
    fn count_field(&self) -> crate::server::sensors::SensorCountField {
        crate::server::sensors::SensorCountField::Proto
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

inventory::submit!(crate::server::sensors::SensorEntry(&ProtoSensor));
