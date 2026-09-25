use crate::graph::GraphDatabase;
use crate::nlp::NlpEmbedder;
use crate::query::executor::Executor;
use crate::query::spec::{
    ConnectOp, DepthSpec, Direction, EdgeSelector, FilterOp, FindOp, GraphOp, GroupBy, GroupOp,
    LabelSelector, LimitOp, NameSelector, QuerySpec, SemanticFilterOp, SortDirection, SortField,
    SortOp, TypeSelector,
};
use anyhow::Result;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

pub fn run_query(expression: &str, workspace: Option<&std::path::Path>) -> Result<()> {
    // Resolve the workspace root: explicit `--workspace`, else walk up
    // for `.git` like `lain mcp` does. The graph lives at
    // `<workspace>/.lain/graph.bin` (written by `lain mcp` / `lain
    // server` indexing).
    let root = match workspace {
        Some(p) => p.to_path_buf(),
        None => crate::cli::workspace::find_git_workspace_root(None)
            .ok()
            .flatten()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no `.git` found in any parent directory — pass `--workspace PATH` to override"
                )
            })?,
    };
    let memory_path = root.join(".lain/graph.bin");
    // Opening a missing graph "succeeds" with an empty one, so an
    // unindexed repository answered every query with `count: 0`, exit 0.
    if !memory_path.is_file() {
        eprintln!(
            "Error: {} has no index yet ({} is missing).\n\nHint: run `lain oneshot find_anchors` \
             there to build it (an agent's `lain mcp` also builds it on first start).",
            root.display(),
            memory_path.display()
        );
        std::process::exit(1);
    }

    let graph = match GraphDatabase::new(&memory_path) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Error: Failed to load graph at {:?}: {}", memory_path, e);
            eprintln!("\nHint: Run 'lain mcp' (or 'lain server') first to build the code graph.");
            std::process::exit(1);
        }
    };

    let embedder = NlpEmbedder::new()?;
    let cache = Arc::new(Mutex::new(HashMap::new()));
    let mut executor = Executor::new(&graph, &embedder, &cache, &root);
    let spec = match parse_query(expression) {
        Ok(spec) => spec,
        Err(e) => {
            eprintln!("Query error: {e}");
            std::process::exit(2);
        }
    };

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

/// A `query_graph` ops array as JSON (`{"ops": [...]}` or `[...]`, as the
/// docs describe), or the pipe syntax (`find Function name X | connect
/// Calls incoming depth 2`). Unrecognised input is an error: it used to be
/// ignored, and the query returned every node with exit 0.
fn parse_query(expr: &str) -> Result<QuerySpec, String> {
    let t = expr.trim();
    if t.starts_with('{') {
        return serde_json::from_str(t).map_err(|e| format!("invalid JSON query: {e}"));
    }
    if t.starts_with('[') {
        let ops: serde_json::Value =
            serde_json::from_str(t).map_err(|e| format!("invalid JSON ops array: {e}"))?;
        return serde_json::from_value(serde_json::json!({ "ops": ops }))
            .map_err(|e| format!("invalid JSON ops array: {e}"));
    }
    const STEPS: &[&str] = &[
        "find",
        "connect",
        "filter",
        "semantic_filter",
        "sort",
        "group",
        "limit",
    ];
    for part in t.split('|').map(str::trim) {
        let word = part.split_whitespace().next().unwrap_or("");
        if !STEPS.contains(&word) {
            return Err(format!(
                "unknown query step '{part}'; expected one of: {}, or a JSON ops array",
                STEPS.join(", ")
            ));
        }
        check_step_words(part)?;
    }
    Ok(parse_query_string(t))
}

/// Reject words the pipe parser would silently skip: `find Function nmae
/// x` returned every function, `connect Calls incomng` ran outgoing.
fn check_step_words(part: &str) -> Result<(), String> {
    let words: Vec<&str> = part.split_whitespace().collect();
    let unknown = |w: &str, expected: &str| {
        Err(format!(
            "unknown word '{w}' in `{part}`; expected {expected}"
        ))
    };
    match words.first().copied() {
        Some("find") => {
            let mut i = 1;
            // An optional node type first.
            if words
                .get(i)
                .is_some_and(|w| !matches!(*w, "name" | "limit"))
            {
                i += 1;
            }
            while i < words.len() {
                match words[i] {
                    "name" | "limit" if i + 1 < words.len() => i += 2,
                    w => return unknown(w, "`name <pattern>` or `limit <n>` after the type"),
                }
            }
            Ok(())
        }
        Some("connect") => {
            let mut i = 2; // `connect <EdgeType>`
            while i < words.len() {
                match words[i] {
                    "incoming" | "in" | "outgoing" | "out" | "both" => i += 1,
                    "depth" if i + 1 < words.len() => i += 2,
                    w => {
                        return unknown(
                            w,
                            "incoming, outgoing, both or `depth <n|a..b>` after the edge type",
                        )
                    }
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
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

        if let Some(rest) = part.strip_prefix("find ") {
            let remainder = rest.trim();
            if !remainder.is_empty()
                && !remainder.starts_with("name ")
                && !remainder.starts_with("limit")
            {
                current_type = Some(TypeSelector::Single(
                    remainder
                        .split_whitespace()
                        .next()
                        .unwrap_or(remainder)
                        .into(),
                ));
            }
            if remainder.contains("name ") {
                if let Some(name_part) = remainder.split("name ").nth(1) {
                    let raw = name_part
                        .split_whitespace()
                        .next()
                        .unwrap_or(name_part)
                        .trim_matches('"');
                    current_name = Some(name_selector_from_string(raw));
                }
            }
        } else if let Some(rest) = part.strip_prefix("connect ") {
            let remainder = rest.trim();
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
                        // `depth_str` like `1..=5` splits to `["1", "=5"]`.
                        // `=5` has `=` at the START, not the end, so
                        // `trim_end_matches('=')` is a no-op and the
                        // `parse()` fails — silently degrading `..=`
                        // to single-depth. Strip the leading `=` (and
                        // any trailing whitespace) instead.
                        let max: u32 = parts
                            .last()
                            .and_then(|s| s.trim_start_matches('=').parse().ok())
                            .unwrap_or(min);
                        connect_depth = DepthSpec::Range { min, max };
                    } else if let Ok(d) = depth_str.parse() {
                        connect_depth = DepthSpec::Single(d);
                    }
                }
            }
        } else if let Some(rest) = part.strip_prefix("filter ") {
            let remainder = rest.trim();
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
        } else if let Some(rest) = part.strip_prefix("semantic_filter ") {
            let remainder = rest.trim();
            let mut like: Option<String> = None;
            let mut threshold: f32 = 0.3;
            // Parse `like 'foo bar'` or `like "foo bar"` or `like foo`
            if let Some(rest) = remainder.strip_prefix("like") {
                let rest = rest.trim();
                if let Some(stripped) = rest.strip_prefix('\'').and_then(|s| s.split_once('\'')) {
                    like = Some(stripped.0.to_string());
                } else if let Some(stripped) =
                    rest.strip_prefix('"').and_then(|s| s.split_once('"'))
                {
                    like = Some(stripped.0.to_string());
                } else {
                    like = Some(rest.split_whitespace().next().unwrap_or("").to_string());
                }
            }
            if remainder.contains("threshold ") {
                if let Some(t) = remainder.split("threshold ").nth(1) {
                    threshold = t
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0.3);
                }
            }
            if let Some(like_str) = like {
                extra_ops.push(GraphOp::SemanticFilter(SemanticFilterOp {
                    like: like_str,
                    threshold,
                }));
            }
        } else if let Some(rest) = part.strip_prefix("sort ") {
            let remainder = rest.trim();
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
            extra_ops.push(GraphOp::Sort(SortOp {
                by: field,
                direction: dir,
            }));
        } else if let Some(rest) = part.strip_prefix("group ") {
            let remainder = rest.trim();
            let by = match remainder.split_whitespace().next().unwrap_or("type") {
                "label" => GroupBy::Label,
                "name" => GroupBy::Name,
                _ => GroupBy::Type,
            };
            extra_ops.push(GraphOp::Group(GroupOp { by }));
        } else if let Some(rest) = part.strip_prefix("limit ") {
            let remainder = rest.trim();
            limit_count = remainder
                .split_whitespace()
                .next()
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

    ops.push(GraphOp::Limit(LimitOp {
        count: limit_count,
        offset: 0,
    }));
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

#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn json_pipe_and_garbage() {
        let json = r#"{"ops":[{"op":"find","type":"Function","name":"helper"}]}"#;
        assert_eq!(parse_query(json).unwrap().ops.len(), 1);
        let arr = r#"[{"op":"find","type":"Function","name":"helper"}]"#;
        assert_eq!(parse_query(arr).unwrap().ops.len(), 1);
        assert!(parse_query("find Function name helper | connect Calls incoming depth 2").is_ok());
        assert!(parse_query("garbage").is_err());
        assert!(parse_query("find Function | frobnicate").is_err());
        assert!(parse_query("find Function nmae hello").is_err());
        assert!(parse_query("find Function name hello | connect Calls incomng").is_err());
        assert!(parse_query("find name hello limit 5").is_ok());
        assert!(parse_query("find Function | connect Calls out depth 1..3").is_ok());
        assert!(parse_query("{not json").is_err());
    }
}
