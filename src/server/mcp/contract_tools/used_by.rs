//! `used_by` walk (`docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §10.9).

use crate::federation::contracts::model::EntryKind;
use crate::graph::GraphDatabase;
use serde_json::{json, Value};
use std::collections::BTreeSet;

/// Sortable `kind` token for the `EntryPoint` wire format (§10.8).
/// `Unreferenced` sorts LAST (§10.9: results sorted by kind, then
/// `GlobalId`). The other four kinds sort alphabetically — the §10.8
/// enum order is a TypeScript-side documentation choice, not a sort
/// order; the design pins the stable order with this constant.
fn kind_rank(k: EntryKind) -> u8 {
    match k {
        EntryKind::HttpHandler => 0,
        EntryKind::Scheduled => 1,
        EntryKind::Cli => 2,
        EntryKind::Main => 3,
        // Not an `EntryKind` — represented separately in the output
        // below as the wire `kind: "unreferenced"` literal.
    }
}

/// The output of `walk_used_by`.
#[derive(Debug)]
pub struct UsedByResult {
    /// One entry per detected entry-point or unreferenced caller.
    /// Sorted by `(kind_rank, ref_id)` per §10.9.
    pub entries: Vec<Value>,
    /// `true` when `depth` ran out before any entry-point was found.
    pub truncated: bool,
}

/// Walk incoming `Calls` from `start_id` (a consumer's calling
/// function) up to `depth` levels, collecting entry-points and
/// unreferenced callers per §10.9.
///
/// Rules:
///   - The caller itself is reported if its `entry` is set, AND the
///     walk continues from it.
///   - Any other node whose `entry` is set is reported and NOT
///     expanded further.
///   - A function with no incoming `Calls` that is not an entry
///     point is reported as `kind: "unreferenced"`.
///   - If `depth` ends the walk before any entry point is found,
///     `truncated = true`.
///   - Results are sorted by `(kind, ref_id)` ascending.
pub fn walk(graph: &GraphDatabase, start_id: &str, depth: u8) -> UsedByResult {
    let mut entries: Vec<Value> = Vec::new();
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut truncated = false;

    let mut frontier: Vec<(String, u8, bool)> = vec![(start_id.to_string(), 0, true)];
    while let Some((id, d, is_self)) = frontier.pop() {
        if !visited.insert(id.clone()) {
            continue;
        }
        let Some(node) = graph.get_node(&id).ok().flatten() else {
            continue;
        };
        let name = node.name.clone();
        let entry = node.entry;
        let path = node.path.clone();
        let line = node.line_start.unwrap_or(0);

        if let Some(kind) = entry {
            // Caller itself → reported AND walk continues from it.
            // Other nodes with entry → reported, NOT expanded.
            entries.push(json!({
                "kind": kind_wire(kind),
                "name": name,
                "ref": evidence_ref(&id, &path, line),
            }));
            if !is_self {
                continue;
            }
            // Self with entry: fall through to expand.
        }

        let parents = graph.incoming_calls(&id);
        if parents.is_empty() {
            // No callers → §10.9 "unreferenced" when the node has
            // no entry (the entry-tagged case was already reported
            // above).
            if entry.is_none() {
                entries.push(json!({
                    "kind": "unreferenced",
                    "name": name,
                    "ref": evidence_ref(&id, &path, line),
                }));
            }
            continue;
        }
        if d >= depth {
            if !is_self && entry.is_none() {
                truncated = true;
            }
            continue;
        }
        for parent in parents {
            if d + 1 < depth {
                frontier.push((parent, d + 1, false));
            } else {
                // We will exhaust depth at the next pop.
                frontier.push((parent, d + 1, false));
                truncated = true;
            }
        }
    }

    // Stable sort: (kind_rank, GlobalId). Unreferenced sorts LAST per
    // the doc pin.
    entries.sort_by(|a, b| {
        let ka = a["kind"].as_str().unwrap_or("");
        let kb = b["kind"].as_str().unwrap_or("");
        let ra = kind_wire_rank(ka);
        let rb = kind_wire_rank(kb);
        ra.cmp(&rb).then_with(|| {
            let ia = a["ref"]["id"].as_str().unwrap_or("");
            let ib = b["ref"]["id"].as_str().unwrap_or("");
            ia.cmp(ib)
        })
    });

    let has_entry = entries.iter().any(|e| {
        e["kind"]
            .as_str()
            .map(|k| k != "unreferenced")
            .unwrap_or(false)
    });
    UsedByResult {
        entries,
        truncated: truncated && !has_entry,
    }
}

fn evidence_ref(id: &str, path: &str, line: u32) -> Value {
    json!({
        "id": id,
        "repo": "",
        "commit": "",
        "path": path,
        "line": line,
        "text": "",
    })
}

fn kind_wire(k: EntryKind) -> &'static str {
    match k {
        EntryKind::HttpHandler => "http_handler",
        EntryKind::Scheduled => "scheduled",
        EntryKind::Cli => "cli",
        EntryKind::Main => "main",
    }
}

fn kind_wire_rank(s: &str) -> u8 {
    match s {
        "http_handler" => kind_rank(EntryKind::HttpHandler),
        "scheduled" => kind_rank(EntryKind::Scheduled),
        "cli" => kind_rank(EntryKind::Cli),
        "main" => kind_rank(EntryKind::Main),
        "unreferenced" => 99,
        _ => 100,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::contracts::model::EntryKind;
    use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace};
    use tempfile::TempDir;

    fn temp_graph(tag: &str) -> (TempDir, GraphDatabase) {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join(format!("usedby_{tag}.bin"));
        let g = GraphDatabase::new(&db).unwrap();
        (tmp, g)
    }

    fn make_fn(_graph: &GraphDatabase, name: &str, path: &str, line: u32) -> GraphNode {
        let ns = RepoNamespace::for_test();
        let mut n = GraphNode::new_in(NodeType::Function, name.into(), path.into(), &ns);
        n.line_start = Some(line);
        n.line_end = Some(line + 5);
        n.id = GraphNode::generate_id(&NodeType::Function, path, name, Some(line), &ns);
        n
    }

    #[test]
    fn walk_returns_empty_for_unknown_start() {
        let (_tmp, g) = temp_graph("unknown");
        let r = walk(&g, "missing", 4);
        assert!(r.entries.is_empty());
        assert!(!r.truncated);
    }

    #[test]
    fn walk_reports_unreferenced_when_no_callers() {
        let (_tmp, g) = temp_graph("unreferenced");
        let start = make_fn(&g, "caller", "src/x.py", 1);
        g.insert_nodes_batch(std::slice::from_ref(&start)).unwrap();
        let r = walk(&g, &start.id, 4);
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.entries[0]["kind"], json!("unreferenced"));
        assert_eq!(r.entries[0]["name"], json!("caller"));
    }

    #[test]
    fn walk_stops_at_entry_point_one_level_up() {
        let (_tmp, g) = temp_graph("stop_at_entry");
        let path = "src/x.py";
        let a = make_fn(&g, "a", path, 1);
        let b = make_fn(&g, "b", path, 10);
        let c = make_fn(&g, "c", path, 20);
        g.insert_nodes_batch(&[a.clone(), b.clone(), c.clone()])
            .unwrap();
        // Edge semantics: "source calls target", so
        // incoming_calls(target) returns [source]. Build the chain
        // a → b → c: a calls b, b calls c. Then incoming_calls(c) = [b],
        // incoming_calls(b) = [a].
        g.insert_edges_batch(&[
            GraphEdge::new(EdgeType::Calls, a.id.clone(), b.id.clone()),
            GraphEdge::new(EdgeType::Calls, b.id.clone(), c.id.clone()),
        ])
        .unwrap();
        // Mark `b` as Scheduled — walk should report `b` and stop
        // (not expand past it).
        g.set_entry(&b.id, EntryKind::Scheduled).unwrap();

        let r = walk(&g, &c.id, 4);
        let kinds: Vec<&str> = r
            .entries
            .iter()
            .map(|e| e["kind"].as_str().unwrap())
            .collect();
        assert!(kinds.contains(&"scheduled"), "b must be reported: {r:?}");
        assert!(!r.truncated);
    }

    #[test]
    fn walk_reports_both_kinds_when_one_function_is_both() {
        // Function `f` is the start; it's scheduled (entry=Scheduled)
        // AND called by an HTTP handler `h`. Both `h` and `f` must be
        // reported; `f` because it's the start (caller is reported
        // if its entry is set and the walk continues from it).
        let (_tmp, g) = temp_graph("both_kinds");
        let path = "src/x.py";
        let h = make_fn(&g, "h", path, 1);
        let f = make_fn(&g, "f", path, 10);
        g.insert_nodes_batch(&[h.clone(), f.clone()]).unwrap();
        // "h calls f" → edge (source=h, target=f) → incoming_calls(f) = [h].
        g.insert_edges_batch(&[GraphEdge::new(EdgeType::Calls, h.id.clone(), f.id.clone())])
            .unwrap();
        g.set_entry(&h.id, EntryKind::HttpHandler).unwrap();
        g.set_entry(&f.id, EntryKind::Scheduled).unwrap();

        let r = walk(&g, &f.id, 4);
        let names: Vec<String> = r
            .entries
            .iter()
            .map(|e| e["name"].as_str().unwrap_or("").to_string())
            .collect();
        assert!(
            names.contains(&"f".to_string()),
            "f (start) must be reported: {names:?}"
        );
        assert!(
            names.contains(&"h".to_string()),
            "h must be reported: {names:?}"
        );
        // `f` is reported with kind=scheduled; `h` with http_handler.
        let f_entry = r.entries.iter().find(|e| e["name"] == json!("f")).unwrap();
        let h_entry = r.entries.iter().find(|e| e["name"] == json!("h")).unwrap();
        assert_eq!(f_entry["kind"], json!("scheduled"));
        assert_eq!(h_entry["kind"], json!("http_handler"));
    }

    #[test]
    fn walk_reports_truncated_when_depth_runs_out() {
        let (_tmp, g) = temp_graph("truncated");
        let path = "src/x.py";
        let mut nodes = Vec::new();
        for i in 0..6u32 {
            nodes.push(make_fn(&g, &format!("n{i}"), path, 1 + i * 5));
        }
        g.insert_nodes_batch(&nodes).unwrap();
        // "n0 calls n1, n1 calls n2, ... n4 calls n5" — chain of 5
        // callers; n5 is the start. incoming_calls(n5) = [n4], and so on.
        let mut edges = Vec::new();
        for i in 0..5u32 {
            edges.push(GraphEdge::new(
                EdgeType::Calls,
                nodes[i as usize].id.clone(),
                nodes[i as usize + 1].id.clone(),
            ));
        }
        g.insert_edges_batch(&edges).unwrap();
        let r = walk(&g, &nodes[5].id, 2);
        assert!(
            r.truncated,
            "depth=2 must truncate the chain of 5 hops: {r:?}"
        );
    }

    #[test]
    fn walk_results_sorted_by_kind_then_global_id() {
        let (_tmp, g) = temp_graph("sorted");
        let path = "src/x.py";
        let f = make_fn(&g, "f", path, 1);
        let c = make_fn(&g, "c", path, 10);
        let m = make_fn(&g, "m", path, 20);
        g.insert_nodes_batch(&[f.clone(), c.clone(), m.clone()])
            .unwrap();
        // "m calls c" (start). "f calls c". incoming_calls(c) = [m, f].
        g.insert_edges_batch(&[
            GraphEdge::new(EdgeType::Calls, m.id.clone(), c.id.clone()),
            GraphEdge::new(EdgeType::Calls, f.id.clone(), c.id.clone()),
        ])
        .unwrap();
        g.set_entry(&f.id, EntryKind::HttpHandler).unwrap();
        g.set_entry(&c.id, EntryKind::Cli).unwrap();
        g.set_entry(&m.id, EntryKind::Main).unwrap();

        let r = walk(&g, &c.id, 4);
        let names: Vec<String> = r
            .entries
            .iter()
            .map(|e| e["name"].as_str().unwrap_or("").to_string())
            .collect();
        // Expected order: http_handler (f) < cli (c) < main (m).
        assert_eq!(
            names,
            vec!["f".to_string(), "c".to_string(), "m".to_string()]
        );
    }
}
