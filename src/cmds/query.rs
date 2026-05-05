use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::Mutex;
use lain::graph::GraphDatabase;
use lain::nlp::NlpEmbedder;
use lain::query::executor::Executor;
use lain::query::spec::{GraphOp, FindOp, LimitOp, QuerySpec, ConnectOp, DepthSpec, Direction, EdgeSelector, TypeSelector, NameSelector};

pub fn run_query(expression: &str, workspace: &std::path::Path) -> Result<()> {
    let memory_path = workspace.join(".lain/graph.bin");

    let graph = match GraphDatabase::new(&memory_path) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Error: Failed to load graph at {:?}: {}", memory_path, e);
            eprintln!("\nHint: Run 'lain' first to build the code graph.");
            std::process::exit(1);
        }
    };

    let embedder = NlpEmbedder::new()?;
    let cache = Arc::new(Mutex::new(HashMap::new()));
    let mut executor = Executor::new(&graph, &embedder, &cache);
    let spec = parse_query_string(expression);

    match executor.execute(&spec) {
        Ok(result) => {
            let json = serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".into());
            println!("{}", json);
        }
        Err(e) => {
            eprintln!("Query error: {}", e);
            std::process::exit(1);
        }
    }

    Ok(())
}

fn parse_query_string(expr: &str) -> QuerySpec {
    let expr = expr.trim();
    let mut ops = Vec::new();
    let mut current_type: Option<TypeSelector> = None;
    let mut current_name: Option<NameSelector> = None;
    let mut connect_edge: Option<EdgeSelector> = None;
    let mut connect_depth = DepthSpec::Single(1);
    let mut limit_count = 100;

    let parts: Vec<&str> = expr.split('|').map(|s| s.trim()).collect();

    for part in parts {
        let part = part.trim();

        if part.starts_with("find ") {
            let remainder = part[5..].trim();
            if !remainder.is_empty() && !remainder.starts_with("name ") && !remainder.starts_with("limit") {
                current_type = Some(TypeSelector::Single(remainder.split_whitespace().next().unwrap_or(remainder).into()));
            }
            if remainder.contains("name ") {
                if let Some(name_part) = remainder.split("name ").nth(1) {
                    let name = name_part.split_whitespace().next().unwrap_or(name_part).trim_matches('"');
                    current_name = Some(NameSelector::Glob(format!("*{}*", name)));
                }
            }
        } else if part.starts_with("connect ") {
            let remainder = part[8..].trim();
            let edge_name = remainder.split_whitespace().next().unwrap_or("Calls");
            connect_edge = Some(EdgeSelector::Single(edge_name.to_string()));

            if remainder.contains("depth ") {
                if let Some(depth_part) = remainder.split("depth ").nth(1) {
                    let depth_str = depth_part.split_whitespace().next().unwrap_or("1");
                    if depth_str.contains("..=") || depth_str.contains("..") {
                        let parts: Vec<&str> = depth_str.split("..").collect();
                        let min: u32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(1);
                        let max: u32 = parts.last().and_then(|s| s.trim_end_matches('=').parse().ok()).unwrap_or(min);
                        connect_depth = DepthSpec::Range { min, max };
                    } else if let Ok(d) = depth_str.parse() {
                        connect_depth = DepthSpec::Single(d);
                    }
                }
            }
        } else if part.starts_with("limit ") {
            let remainder = part[6..].trim();
            limit_count = remainder.split_whitespace().next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(100);
        }
    }

    ops.push(GraphOp::Find(FindOp {
        type_selector: current_type,
        name: current_name,
        id: None,
        label_selector: None,
        path: None,
    }));

    if let Some(edge) = connect_edge {
        ops.push(GraphOp::Connect(ConnectOp {
            edge,
            direction: Direction::Outgoing,
            depth: connect_depth,
            target: None,
        }));
    }

    ops.push(GraphOp::Limit(LimitOp { count: limit_count, offset: 0 }));
    QuerySpec::new(ops)
}
