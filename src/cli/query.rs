use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::Mutex;
use crate::graph::GraphDatabase;
use crate::nlp::NlpEmbedder;
use crate::query::executor::Executor;
use crate::query::spec::{
    ConnectOp, DepthSpec, Direction, EdgeSelector, FilterOp, FindOp,
    GraphOp, GroupBy, GroupOp, LabelSelector, LimitOp, NameSelector,
    QuerySpec, SemanticFilterOp, SortDirection, SortField, SortOp, TypeSelector,
};

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
    let mut executor = Executor::new(&graph, &embedder, &cache, workspace);
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
    let mut connect_direction = Direction::Outgoing;
    let mut connect_depth = DepthSpec::Single(1);
    let mut limit_count = 100;
    let mut extra_ops: Vec<GraphOp> = Vec::new();

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
                    let raw = name_part.split_whitespace().next().unwrap_or(name_part).trim_matches('"');
                    current_name = Some(name_selector_from_string(raw));
                }
            }
        } else if part.starts_with("connect ") {
            let remainder = part[8..].trim();
            let edge_name = remainder.split_whitespace().next().unwrap_or("Calls");
            connect_edge = Some(EdgeSelector::Single(edge_name.to_string()));

            // direction keyword: incoming | outgoing | both
            for token in remainder.split_whitespace() {
                match token {
                    "incoming" | "in" => connect_direction = Direction::Incoming,
                    "outgoing" | "out" => connect_direction = Direction::Outgoing,
                    "both" => connect_direction = Direction::Both,
                    _ => {}
                }
            }

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
        } else if part.starts_with("filter ") {
            let remainder = part[7..].trim();
            let mut filter = FilterOp::default();
            // Supported forms:
            //   filter label X
            //   filter type X
            //   filter name X
            for (i, token) in remainder.split_whitespace().enumerate() {
                if i == 0 && matches!(token, "label" | "type" | "name") {
                    continue;
                }
                if remainder.starts_with("label ") {
                    if let Some(label) = remainder.split_whitespace().nth(1) {
                        filter.label_filter = Some(LabelSelector::Single(label.to_string()));
                    }
                    break;
                } else if remainder.starts_with("type ") {
                    if let Some(t) = remainder.split_whitespace().nth(1) {
                        filter.type_filter = Some(TypeSelector::Single(t.to_string()));
                    }
                    break;
                } else if remainder.starts_with("name ") {
                    let raw = remainder.split_whitespace().nth(1).unwrap_or("");
                    filter.name = Some(name_selector_from_string(raw));
                    break;
                }
            }
            extra_ops.push(GraphOp::Filter(filter));
        } else if part.starts_with("semantic_filter ") {
            let remainder = part[16..].trim();
            let mut like: Option<String> = None;
            let mut threshold: f32 = 0.3;
            // Parse `like 'foo bar'` or `like "foo bar"` or `like foo`
            if let Some(rest) = remainder.strip_prefix("like") {
                let rest = rest.trim();
                if let Some(stripped) = rest.strip_prefix('\'').and_then(|s| s.split_once('\'')) {
                    like = Some(stripped.0.to_string());
                } else if let Some(stripped) = rest.strip_prefix('"').and_then(|s| s.split_once('"')) {
                    like = Some(stripped.0.to_string());
                } else {
                    like = Some(rest.split_whitespace().next().unwrap_or("").to_string());
                }
            }
            if remainder.contains("threshold ") {
                if let Some(t) = remainder.split("threshold ").nth(1) {
                    threshold = t.split_whitespace().next().and_then(|s| s.parse().ok()).unwrap_or(0.3);
                }
            }
            if let Some(like_str) = like {
                extra_ops.push(GraphOp::SemanticFilter(SemanticFilterOp { like: like_str, threshold }));
            }
        } else if part.starts_with("sort ") {
            let remainder = part[5..].trim();
            let field = match remainder.split_whitespace().next().unwrap_or("name") {
                "type" => SortField::Type,
                "label" => SortField::Label,
                _ => SortField::Name,
            };
            let dir = if remainder.contains("desc") || remainder.contains("descending") {
                SortDirection::Desc
            } else {
                SortDirection::Asc
            };
            extra_ops.push(GraphOp::Sort(SortOp { by: field, direction: dir }));
        } else if part.starts_with("group ") {
            let remainder = part[6..].trim();
            let by = match remainder.split_whitespace().next().unwrap_or("type") {
                "label" => GroupBy::Label,
                "name" => GroupBy::Name,
                _ => GroupBy::Type,
            };
            extra_ops.push(GraphOp::Group(GroupOp { by }));
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
            direction: connect_direction,
            depth: connect_depth,
            target: None,
        }));
    }

    ops.extend(extra_ops);

    ops.push(GraphOp::Limit(LimitOp { count: limit_count, offset: 0 }));
    QuerySpec::new(ops)
}

/// Map a CLI name pattern to a NameSelector:
/// - patterns containing `*` or `?` are passed through as Glob
/// - patterns starting with `/` or ending with `/` are treated as anchors (StartsWith/EndsWith)
/// - everything else is treated as an exact match
fn name_selector_from_string(s: &str) -> NameSelector {
    if s.contains('*') || s.contains('?') {
        NameSelector::Glob(s.to_string())
    } else if s.starts_with('/') && s.ends_with('/') && s.len() > 2 {
        NameSelector::StartsWith(s[1..s.len() - 1].to_string())
    } else {
        NameSelector::Exact(s.to_string())
    }
}
