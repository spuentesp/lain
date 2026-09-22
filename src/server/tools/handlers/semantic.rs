//! Semantic Agent API — AGENT_UX_ROADMAP.md Milestone 6.
//!
//! Five intent-organized tools that compose existing internals so
//! an agent reaches for one of these first when its intent matches a
//! known pattern. Each tool here is a thin composition layer; the
//! low-level tools it composes (`find_anchors`, `get_blast_radius`,
//! `explain_symbol`, etc.) remain in their respective handler
//! modules and remain reachable through the dispatcher.
//!
//! Each function takes the same set of inputs the underlying tool
//! takes, plus any cross-cutting knobs (`limit`, `depth`,
//! `include_*` flags), and returns Markdown that follows the
//! section-header conventions the existing tools established:
//!
//! ```text
//! ## <header>
//!
//! - item
//! - item
//! ```
//!
//! Determinism: every section is sorted (alphabetical for names,
//! score-desc for anchor lists, depth-asc for blast-radius). The
//! only sources of nondeterminism are timestamps in the underlying
//! tools; those are unchanged.

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::nlp::{CrossEncoder, NlpEmbedder};
use crate::overlay::VolatileOverlay;
use crate::schema::NodeType;
use crate::server::presence::OccupancyMap;
use crate::server::tools::utils::{required_str_arg, resolve_node_ambiguous, str_arg, usize_arg};
use crate::tuning::TuningConfig;
use parking_lot::Mutex;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// M6 tool 1 of 5: `find_symbol` ("Where is X?")
///
/// Returns the set of graph nodes matching `name`, optionally
/// narrowed by `path_hint` and `type_filter`. A single match
/// produces a `Use this:` shortcut so the agent can paste the id
/// straight into the next call. Multiple matches produce a
/// numbered list and explicitly tell the agent to disambiguate
/// by `path`.
pub fn find_symbol(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    args: &Map<String, Value>,
) -> Result<String, LainError> {
    let _ = overlay; // kept for API parity with the other M6 handlers
    let name = required_str_arg(args, "name")?;
    let path_hint = str_arg(args, "path_hint");
    let type_filter = str_arg(args, "type_filter").to_lowercase();
    let type_filter: Option<NodeType> = if type_filter.is_empty() {
        None
    } else {
        Some(parse_node_type(&type_filter)?)
    };

    let mut hits = graph.find_all_nodes_by_name(&name);
    if let Some(ref hint) = path_hint_option(&path_hint) {
        hits.retain(|n| n.path.contains(hint));
    }
    if let Some(target) = type_filter {
        hits.retain(|n| n.node_type == target);
    }
    // Deterministic order: anchor score desc, then path asc, then
    // name asc as a tiebreaker for the rare dedup tie.
    hits.sort_by(|a, b| {
        b.anchor_score
            .partial_cmp(&a.anchor_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.name.cmp(&b.name))
    });

    if hits.is_empty() {
        return Ok(format!(
            "## find_symbol: {name}\n\nNo matches.\n\n\
             Try a broader name, drop `path_hint`, or use `query_graph` \
             with a `find` op for a structural search."
        ));
    }

    let mut out = format!("## find_symbol: {name}\n\n");
    if hits.len() == 1 {
        let n = &hits[0];
        out.push_str(&format!(
            "Use this: `{}`  (path=`{}`, type={})\n\n",
            n.id,
            n.path,
            node_type_label(&n.node_type)
        ));
    } else {
        out.push_str(&format!(
            "Found {} matches. Disambiguate with the `path` argument:\n\n",
            hits.len()
        ));
    }
    for (i, n) in hits.iter().enumerate() {
        let anchor = n
            .anchor_score
            .map(|s| format!(" anchor={s:.2}"))
            .unwrap_or_default();
        let line = n.line_start.map(|l| format!(":{l}")).unwrap_or_default();
        out.push_str(&format!(
            "{}. `{}` {} {}{}{}\n",
            i + 1,
            n.id,
            node_type_label(&n.node_type),
            n.path,
            line,
            anchor
        ));
    }
    Ok(out)
}

/// M6 tool 2 of 5: `get_context` ("Explain X")
///
/// Composes `explain_symbol` for the dossier, `get_call_sites` for
/// callers, `trace_dependency` for callees (both at the requested
/// `depth`), and `get_code_snippet` for the source body. Sections
/// are stable so an agent can pattern-match the headers.
pub async fn get_context(
    workspace: &std::path::Path,
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    occupancy: &OccupancyMap,
    args: &Map<String, Value>,
) -> Result<String, LainError> {
    let symbol = required_str_arg(args, "symbol")?;
    let depth = usize_arg(args, "depth").unwrap_or(1).clamp(0, 3);

    let dossier = crate::server::tools::handlers::metrics::explain_symbol(
        workspace, graph, overlay, occupancy, &symbol,
    )?;

    let source_section = match resolve_for_snippet(graph, overlay, &symbol) {
        Ok((Some(ls), Some(le), Some(ref p))) => {
            match crate::server::tools::handlers::context::get_code_snippet(
                graph,
                overlay,
                workspace,
                p.to_str().unwrap_or(""),
                Some(ls),
                Some(le.saturating_sub(ls) as usize),
            ) {
                Ok(s) => format!("\n## Source\n\n{}", trim_for_section(&s, 30)),
                Err(_) => String::new(),
            }
        }
        _ => String::new(),
    };

    let caller_section = if depth >= 1 {
        match crate::server::tools::handlers::context::get_call_sites(
            workspace, graph, overlay, &symbol,
        ) {
            Ok(s) => format!("\n## Callers\n\n{}", trim_for_section(&s, 12)),
            Err(_) => String::new(),
        }
    } else {
        String::new()
    };
    let callee_section = if depth >= 1 {
        match crate::server::tools::handlers::navigation::trace_dependency(graph, overlay, &symbol)
        {
            Ok(s) => format!("\n## Callees\n\n{}", trim_for_section(&s, 12)),
            Err(_) => String::new(),
        }
    } else {
        String::new()
    };

    Ok(format!(
        "## get_context: {symbol}\n\n## Definition\n\n{}{}{}{}",
        trim_for_section(&dossier, 60),
        caller_section,
        callee_section,
        source_section,
    ))
}

/// M6 tool 3 of 5: `find_related` ("What is connected to X?")
///
/// Composes `trace_dependency` for direct graph neighbors,
/// `get_coupling_radar` for co-change partners, and `semantic_search`
/// for semantic neighbors (when an NLP model is loaded).
pub async fn find_related(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    workspace: &std::path::Path,
    embedder: &NlpEmbedder,
    cross_encoder: &CrossEncoder,
    embedding_cache: &Arc<Mutex<HashMap<String, Vec<f32>>>>,
    tuning: &TuningConfig,
    args: &Map<String, Value>,
    ui_link: crate::server::tools::UiLink<'_>,
) -> Result<String, LainError> {
    let symbol = required_str_arg(args, "symbol")?;
    let include_coupling = !matches!(
        args.get("include_coupling").and_then(Value::as_bool),
        Some(false)
    );
    let _limit = usize_arg(args, "limit").unwrap_or(10);

    let _ = resolve_node_ambiguous(graph, overlay, &symbol)?;

    let graph_section =
        match crate::server::tools::handlers::navigation::trace_dependency(graph, overlay, &symbol)
        {
            Ok(s) => format!("\n## Graph neighbors\n\n{}", trim_for_section(&s, 20)),
            Err(_) => String::new(),
        };

    let coupling_section = if include_coupling {
        match crate::server::tools::handlers::impact::get_coupling_radar(
            graph, overlay, &symbol, ui_link,
        )
        .await
        {
            Ok(s) => format!("\n## Co-change partners\n\n{}", trim_for_section(&s, 20)),
            Err(_) => String::new(),
        }
    } else {
        String::new()
    };

    let semantic_section = if !embedder.is_stub() {
        match crate::server::tools::handlers::search::semantic_search(
            workspace,
            graph,
            overlay,
            embedder,
            cross_encoder,
            embedding_cache,
            tuning,
            &symbol,
            5,
        ) {
            Ok(s) => format!(
                "\n## Semantic neighbors (optional)\n\n{}",
                trim_for_section(&s, 8)
            ),
            Err(crate::error::LainError::Unavailable(_)) => String::new(),
            Err(_) => String::new(),
        }
    } else {
        String::new()
    };

    Ok(format!(
        "## find_related: {symbol}\n\n{graph_section}{coupling_section}{semantic_section}"
    ))
}

/// M6 tool 4 of 5: `assess_change` ("What breaks if I change X?")
///
/// Composes `get_blast_radius` for transitive dependents,
/// `get_call_sites` for direct call lines, `find_untested_functions`
/// for the dependents that lack tests, and `get_coupling_radar`
/// for files that co-change with the target. A one-line risk
/// summary at the end caps the response.
pub async fn assess_change(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    workspace: &std::path::Path,
    args: &Map<String, Value>,
    ui_link: crate::server::tools::UiLink<'_>,
) -> Result<String, LainError> {
    let symbol = required_str_arg(args, "symbol")?;
    let depth = str_arg(args, "depth");
    let include_tests = !matches!(
        args.get("include_tests").and_then(Value::as_bool),
        Some(false)
    );

    let blast = crate::server::tools::handlers::impact::get_blast_radius(
        // `include_weak_edges=true`: the pre-edit risk verdict below
        // is meaningless if it can't see dynamic-dispatch callers. A
        // `bus.publish` site with only heuristic callers would
        // otherwise report `direct=0, transitive=0, risk=low` and the
        // agent would ship the regression `explain_dispatch` was
        // built to prevent. Heuristic callers are tagged with `~`
        // and `[heuristic, conf=X.XX]` so the agent can tell them
        // apart from type-resolved calls.
        graph, overlay, &symbol, true, true, ui_link,
    )
    .await?;
    let callsites =
        crate::server::tools::handlers::context::get_call_sites(workspace, graph, overlay, &symbol);
    let untested = if include_tests {
        crate::server::tools::handlers::testing::find_untested_functions(
            graph,
            overlay,
            Some(usize_arg(args, "limit").unwrap_or(20)),
        )
    } else {
        Ok(String::new())
    };

    let direct = extract_section(&callsites.unwrap_or_default(), "Direct dependents");
    let transitive = extract_section(&blast, "indirect");
    let untested_section = match untested {
        Ok(s) => format!("\n## Untested dependents\n\n{}", trim_for_section(&s, 8)),
        Err(_) => String::new(),
    };

    let direct_count = count_bullets(&direct);
    let transitive_count = count_bullets(&transitive);
    // The blast-radius output carries a separate section ("~ N heuristic
    // caller(s) included") that the section extractors above don't see.
    // When the static graph is empty but heuristic evidence exists, the
    // verdict is NOT actually 'low' — the agent should treat it the
    // same way it would treat explain_dispatch's
    // 'insufficient_evidence' (or 'heuristic_only'). Surfacing this in
    // the risk line is the single change that closes the gap.
    let heuristic_only = direct_count == 0
        && transitive_count == 0
        && blast.contains("heuristic caller(s) included");
    let risk = if heuristic_only {
        "low* — heuristic-only (see ~ N heuristic caller(s) below)"
    } else if direct_count == 0 && transitive_count == 0 {
        "low"
    } else if direct_count <= 3 && transitive_count <= 20 {
        "medium"
    } else {
        "high"
    };

    let depth_caveat = if depth.is_empty() {
        String::new()
    } else {
        format!(" (depth={depth})")
    };

    Ok(format!(
        "## assess_change: {symbol}{depth_caveat}\n\n\
         ## Direct dependents\n\n{}\n\n\
         ## Transitive reach\n\n{}\n\
         {untested_section}\n\
         ## Risk summary\n\nDirect: {direct_count}; transitive: {transitive_count}; verdict: **{risk}**.\n",
        trim_for_section(&direct, 20),
        trim_for_section(&transitive, 12),
    ))
}

/// M6 tool 5 of 5: `search_code` ("Find code that does Y")
///
/// Mode-dispatched. `lexical` uses the graph's name index;
/// `semantic` uses the NLP model; `auto` (default) tries
/// semantic first and falls back to lexical, recording the
/// fallback in the response so the agent knows what happened.
pub fn search_code(
    workspace: &std::path::Path,
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    embedder: &NlpEmbedder,
    cross_encoder: &CrossEncoder,
    embedding_cache: &Arc<Mutex<HashMap<String, Vec<f32>>>>,
    tuning: &TuningConfig,
    args: &Map<String, Value>,
) -> Result<String, LainError> {
    let query = required_str_arg(args, "query")?;
    let mode_arg = str_arg(args, "mode").to_lowercase();
    let mode = if mode_arg.is_empty() {
        "auto".to_string()
    } else {
        mode_arg
    };
    let limit = usize_arg(args, "limit").unwrap_or(10);

    let mut fell_back = false;
    let (effective_mode, body) = match mode.as_str() {
        "lexical" => ("lexical", lexical_search(graph, &query, limit)),
        "semantic" => match semantic_call(
            workspace,
            graph,
            overlay,
            embedder,
            cross_encoder,
            embedding_cache,
            tuning,
            &query,
            limit,
        ) {
            Ok(s) => ("semantic", s),
            Err(crate::error::LainError::Unavailable(msg)) => {
                fell_back = true;
                (
                    "lexical",
                    format!(
                        "Semantic search unavailable: {msg}\n\nFalling back to lexical:\n\n{}",
                        lexical_search(graph, &query, limit)
                    ),
                )
            }
            Err(e) => return Err(e),
        },
        _ => {
            // auto
            if !embedder.is_stub() {
                match semantic_call(
                    workspace,
                    graph,
                    overlay,
                    embedder,
                    cross_encoder,
                    embedding_cache,
                    tuning,
                    &query,
                    limit,
                ) {
                    Ok(s) => ("semantic", s),
                    Err(crate::error::LainError::Unavailable(msg)) => {
                        fell_back = true;
                        (
                            "lexical",
                            format!(
                                "Semantic search unavailable: {msg}\n\nFalling back to lexical:\n\n{}",
                                lexical_search(graph, &query, limit)
                            ),
                        )
                    }
                    Err(_) => {
                        fell_back = true;
                        ("lexical", lexical_search(graph, &query, limit))
                    }
                }
            } else {
                ("lexical", lexical_search(graph, &query, limit))
            }
        }
    };

    Ok(format!(
        "## search_code: {query} (mode={effective_mode}, fell_back={fell_back})\n\n{body}"
    ))
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn parse_node_type(s: &str) -> Result<NodeType, LainError> {
    Ok(match s {
        "function" | "func" | "fn" => NodeType::Function,
        "struct" | "structure" => NodeType::Struct,
        "trait" | "interface" => NodeType::Trait,
        "module" | "namespace" | "ns" => NodeType::Module,
        "file" => NodeType::File,
        "method" => NodeType::Method,
        "class" => NodeType::Class,
        other => {
            return Err(LainError::Other(format!(
                "unsupported type_filter `{other}` (try function/struct/trait/module/file)"
            )))
        }
    })
}

fn node_type_label(nt: &NodeType) -> &'static str {
    match nt {
        NodeType::File => "file",
        NodeType::Namespace => "namespace",
        NodeType::Module => "module",
        NodeType::Package => "package",
        NodeType::Class => "class",
        NodeType::Interface => "interface",
        NodeType::Struct => "struct",
        NodeType::Enum => "enum",
        NodeType::Trait => "trait",
        NodeType::Function => "function",
        NodeType::Method => "method",
        NodeType::Property => "property",
        NodeType::Variable => "variable",
        NodeType::Constant => "constant",
        NodeType::HttpRoute => "http-route",
        NodeType::Topic => "topic",
        NodeType::Resource => "resource",
        NodeType::Schema => "schema",
        NodeType::Synthetic => "synthetic",
    }
}

fn path_hint_option(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn resolve_for_snippet(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    symbol: &str,
) -> Result<(Option<u32>, Option<u32>, Option<PathBuf>), LainError> {
    let (node, _other) = resolve_node_ambiguous(graph, overlay, symbol)?;
    Ok((
        node.line_start,
        node.line_end,
        Some(PathBuf::from(&node.path)),
    ))
}

fn trim_for_section(body: &str, max_lines: usize) -> String {
    let trimmed: Vec<&str> = body
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(max_lines)
        .collect();
    if trimmed.is_empty() {
        "(none)".to_string()
    } else {
        trimmed.join("\n")
    }
}

fn extract_section(body: &str, header_marker: &str) -> String {
    // F5 — the body produced by `get_call_sites` and `get_blast_radius`
    // is NOT structured with `## ` H2 headings. `get_call_sites`
    // outputs single-dash bullets (`- **caller** (...) calls it at
    // line N`); `get_blast_radius` outputs `\n- Direct dependents
    // (N):` and `\n  - depth N: count` lines. Pre-fix this function
    // scanned for `## `, never matched, and always fell through to
    // the fallback "first 20 non-empty lines" — which is then fed to
    // `count_bullets` and produces 0 every time. Result: `direct_count`
    // was always zero, and `transitive_count` was the only non-zero
    // count (the fallback body never contained bullets either, so
    // transitive was zero too).
    //
    // Fix: keep the public API but make the function actually find
    // the bullet section. Match lines that contain the header marker
    // (case-insensitive) at any indentation (we've seen `- `, `## `,
    // and `  - ` styles). Once the header is found, take the body up
    // until the next blank line OR a different heading — the body
    // forms of these calls don't actually use a closing marker, so
    // we fall through at EOF.
    let mut in_section = false;
    let mut buf: Vec<&str> = Vec::new();
    let header_lower = header_marker.to_lowercase();
    for line in body.lines() {
        let trimmed = line.trim_start();
        let trimmed_lower = trimmed.to_lowercase();
        if !in_section {
            if trimmed_lower.starts_with("- ") || trimmed_lower.starts_with("## ") {
                if trimmed_lower.contains(&header_lower) {
                    in_section = true;
                }
            }
        } else if trimmed.is_empty() {
            // Blank line ends the section (body sections are
            // consistently followed by a blank or next heading).
            break;
        } else if trimmed.starts_with("## ") || trimmed.starts_with("# ") {
            // Hit the next heading without seeing the marker again.
            break;
        } else {
            buf.push(line);
        }
    }
    if buf.is_empty() {
        body.lines()
            .filter(|l| !l.trim().is_empty())
            .take(20)
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        buf.join("\n")
    }
}

fn count_bullets(section: &str) -> usize {
    // F5 — the actual caller-bullet format from `get_call_sites` is
    // `- **caller_name** (...) calls it at line N`; the blast-radius
    // body uses `  - depth N: count` for indented bullets. Both start
    // with `- **` (the caller bullet) or `  - ` (the depth line).
    // Pre-fix `count_bullets` checked only `  - ` and `- - `, neither
    // of which matched the real caller output, so the count was
    // always zero and the risk verdict degenerated to `low` even
    // when callers existed. Match `- **` (the caller marker) AND
    // `  - ` (the indented depth marker); exclude header lines like
    // `- leaf_helper (Function)` and `- Overlay freshness: live`
    // that aren't caller bullets.
    section
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            if t == "-" || t == "--" || t.starts_with("---") {
                return false;
            }
            // Caller bullets: `- **name**` (get_call_sites) or
            // `  - depth N: count` (get_blast_radius indented).
            t.starts_with("- **") || t.starts_with("  - ")
        })
        .count()
}

fn semantic_call(
    workspace: &std::path::Path,
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    embedder: &NlpEmbedder,
    cross_encoder: &CrossEncoder,
    embedding_cache: &Arc<Mutex<HashMap<String, Vec<f32>>>>,
    tuning: &TuningConfig,
    query: &str,
    limit: usize,
) -> Result<String, LainError> {
    crate::server::tools::handlers::search::semantic_search(
        workspace,
        graph,
        overlay,
        embedder,
        cross_encoder,
        embedding_cache,
        tuning,
        query,
        limit,
    )
}

fn lexical_search(graph: &GraphDatabase, query: &str, limit: usize) -> String {
    let needle = query.to_lowercase();
    let mut hits: Vec<_> = graph
        .get_all_nodes()
        .into_iter()
        .filter(|n| n.name.to_lowercase().contains(&needle))
        .collect();
    hits.sort_by(|a, b| {
        b.anchor_score
            .partial_cmp(&a.anchor_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.path.cmp(&b.path))
    });
    if hits.is_empty() {
        return format!("No lexical matches for `{query}`.");
    }
    let mut out = String::new();
    for (i, n) in hits.iter().take(limit).enumerate() {
        let line = n.line_start.map(|l| format!(":{l}")).unwrap_or_default();
        let anchor = n
            .anchor_score
            .map(|s| format!(" anchor={s:.2}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "{}. `{}` {} {}{}{}\n",
            i + 1,
            n.id,
            node_type_label(&n.node_type),
            n.path,
            line,
            anchor
        ));
    }
    out
}

#[cfg(test)]
mod m6_tests {
    use super::*;
    use crate::schema::{EdgeType, GraphEdge, GraphNode};

    fn make_test_graph() -> (GraphDatabase, VolatileOverlay) {
        let tmp = std::env::temp_dir().join("test_semantic_handlers");
        let _ = std::fs::remove_dir_all(&tmp);
        let graph = GraphDatabase::new(&tmp).unwrap();

        let mut a = GraphNode::new(
            NodeType::Function,
            "parse".to_string(),
            "/src/main.rs".to_string(),
        );
        a.line_start = Some(10);
        a.line_end = Some(20);
        let mut b = GraphNode::new(
            NodeType::Function,
            "parse".to_string(),
            "/src/util.rs".to_string(),
        );
        b.line_start = Some(40);
        b.line_end = Some(50);
        let c = GraphNode::new(
            NodeType::Function,
            "render".to_string(),
            "/src/main.rs".to_string(),
        );
        graph.upsert_node(a.clone()).unwrap();
        graph.upsert_node(b).unwrap();
        graph.upsert_node(c).unwrap();

        // Caller → parse (one Calls edge so get_blast_radius has data).
        let caller = GraphNode::new(
            NodeType::Function,
            "handle".to_string(),
            "/src/main.rs".to_string(),
        );
        graph.upsert_node(caller.clone()).unwrap();
        graph
            .insert_edge(&GraphEdge::new(
                EdgeType::Calls,
                caller.id.clone(),
                a.id.clone(),
            ))
            .unwrap();

        let overlay = VolatileOverlay::new();
        (graph, overlay)
    }

    fn args(pairs: &[(&str, &str)]) -> Map<String, Value> {
        let mut m = Map::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), Value::String(v.to_string()));
        }
        m
    }

    /// `find_symbol` returns a `Use this:` shortcut when a single
    /// node matches the name.
    #[test]
    fn find_symbol_single_match_produces_use_this_shortcut() {
        let (graph, overlay) = make_test_graph();
        let m = args(&[("name", "render")]);
        let out = find_symbol(&graph, &overlay, &m).unwrap();
        assert!(out.starts_with("## find_symbol: render"));
        assert!(out.contains("Use this:"));
    }

    /// `find_symbol` enumerates matches when the name is ambiguous
    /// and tells the agent to disambiguate by path.
    #[test]
    fn find_symbol_ambiguous_lists_all_matches() {
        let (graph, overlay) = make_test_graph();
        let m = args(&[("name", "parse")]);
        let out = find_symbol(&graph, &overlay, &m).unwrap();
        assert!(out.contains("Disambiguate with the `path` argument"));
        assert!(out.contains("/src/main.rs"));
        assert!(out.contains("/src/util.rs"));
    }

    /// `find_symbol` with no matches returns the "Try a broader
    /// name" guidance.
    #[test]
    fn find_symbol_no_match_returns_guidance() {
        let (graph, overlay) = make_test_graph();
        let m = args(&[("name", "nonexistent_symbol")]);
        let out = find_symbol(&graph, &overlay, &m).unwrap();
        assert!(out.contains("No matches"));
        assert!(out.contains("query_graph"));
    }

    /// `find_symbol` `type_filter` narrows the result set.
    #[test]
    fn find_symbol_type_filter_narrows() {
        let (graph, overlay) = make_test_graph();
        let m = args(&[("name", "parse"), ("type_filter", "function")]);
        let out = find_symbol(&graph, &overlay, &m).unwrap();
        assert!(out.contains("Disambiguate with the `path` argument"));
        // The two `parse` nodes are Functions — narrowing kept both.
        assert_eq!(out.matches("/src/").count(), 2);
    }

    /// `search_code` lexical mode works without an embedding model.
    #[test]
    fn search_code_lexical_returns_ranked_matches() {
        let (graph, overlay) = make_test_graph();
        let mut a = Map::new();
        a.insert("query".to_string(), Value::String("render".to_string()));
        a.insert("mode".to_string(), Value::String("lexical".to_string()));
        let out = search_code_lexical_only(&graph, &overlay, &a).unwrap();
        assert!(out.contains("## search_code: render"));
        assert!(out.contains("mode=lexical"));
        assert!(out.contains("fell_back=false"));
    }

    /// `search_code` lexical mode returns "No lexical matches"
    /// when nothing matches.
    #[test]
    fn search_code_lexical_no_match() {
        let (graph, overlay) = make_test_graph();
        let mut a = Map::new();
        a.insert(
            "query".to_string(),
            Value::String("absolutely_nothing_matches_this".to_string()),
        );
        let out = search_code_lexical_only(&graph, &overlay, &a).unwrap();
        assert!(out.contains("No lexical matches"));
    }

    /// `assess_change` on a missing symbol returns the underlying
    /// `NotFound` error verbatim (the composition surfaces it).
    #[tokio::test(flavor = "current_thread")]
    async fn assess_change_missing_symbol_returns_not_found() {
        let (graph, overlay) = make_test_graph();
        let mut a = Map::new();
        a.insert(
            "symbol".to_string(),
            Value::String("does_not_exist".to_string()),
        );
        let res = assess_change(&graph, &overlay, std::path::Path::new("/"), &a, None).await;
        assert!(res.is_err(), "expected NotFound, got {res:?}");
    }

    /// `assess_change` must surface heuristic (dynamic-dispatch)
    /// callers in the risk verdict — the regression iter 14 closed.
    /// Without this, a `bus.publish` site with only heuristic callers
    /// would report `direct=0, transitive=0, risk=low` and the agent
    /// would ship the regression `explain_dispatch` was built to
    /// prevent.
    ///
    /// The fixture below builds a graph with one `parse` target
    /// (the symbol under assessment) and one BusTopic heuristic edge
    /// pointing at it. With `include_weak_edges=true` (the iter 14
    /// default), the transitive list must include the heuristic
    /// caller line tagged with `[heuristic, conf=...]`.
    #[tokio::test(flavor = "current_thread")]
    async fn assess_change_surfaces_heuristic_callers_in_risk_verdict() {
        use crate::schema::{EdgeProvenance, NodeType, RepoNamespace};

        let tmp = std::env::temp_dir().join("test_assess_heuristic");
        let _ = std::fs::remove_dir_all(&tmp);
        let graph = GraphDatabase::new(&tmp).unwrap();
        let overlay = VolatileOverlay::new();
        let ns = RepoNamespace::for_test();

        let target = GraphNode::new(
            NodeType::Function,
            "handle_order".to_string(),
            "/src/orders.py".to_string(),
        );
        graph.upsert_node(target.clone()).unwrap();

        // Heuristic edge: orders.py → Hub:message_bus_publisher. We
        // swap the synthetic hub's id for the target's so the BFS
        // walks into the symbol under assessment (mirrors the
        // existing blast-radius heuristic test fixture).
        let hub_name = "Hub:message_bus_publisher";
        let hub_id = GraphNode::generate_id(&NodeType::Synthetic, "__hub__", hub_name, None, &ns);
        let caller_id = GraphNode::generate_id(&NodeType::File, "/src/orders.py", "", None, &ns);
        graph
            .upsert_node({
                let mut n =
                    GraphNode::new(NodeType::File, String::new(), "/src/orders.py".to_string());
                n.id = caller_id.clone();
                n
            })
            .unwrap();
        graph
            .upsert_node({
                let mut n =
                    GraphNode::new(NodeType::Synthetic, hub_name.to_string(), String::new());
                n.id = hub_id.clone();
                n
            })
            .unwrap();
        graph
            .insert_edges_batch(&[GraphEdge {
                edge_type: EdgeType::BusTopic,
                source_id: caller_id,
                target_id: target.id.clone(),
                weight: Some(0.7),
                cross_repo: false,
                provenance: Some(EdgeProvenance::Heuristic {
                    detector: "message_bus_publisher".to_string(),
                    confidence: 0.7,
                }),
            }])
            .unwrap();

        let mut a = Map::new();
        a.insert(
            "symbol".to_string(),
            Value::String("handle_order".to_string()),
        );
        let out = assess_change(&graph, &overlay, std::path::Path::new("/"), &a, None)
            .await
            .expect("assess_change on a known symbol must succeed");

        // Pre-iter-14: a symbol with one heuristic caller and zero
        // type-resolved callers would report risk=low and no heuristic
        // tag in the transitive section. Post-iter-14: the heuristic
        // caller surfaces and the verdict reflects it.
        assert!(
            out.contains("[heuristic") || out.contains("heuristic caller(s) included"),
            "expected heuristic marker in assess_change output, got:\n{out}"
        );
        assert!(
            out.contains("conf=0.70") || out.contains("confidence"),
            "expected confidence tag in assess_change output, got:\n{out}"
        );
        // Risk must NOT be `low` — a single heuristic caller at 0.7
        // confidence is enough to push the verdict above the empty-
        // blast-radius threshold.
        assert!(
            !out.contains("verdict: **low**"),
            "heuristic callers must not let assess_change report risk=low, got:\n{out}"
        );
    }

    /// When the static graph is empty but a heuristic caller exists,
    /// `assess_change` must surface that — the previous contract
    /// reported `risk=low` in this case, which is the false negative
    /// `explain_dispatch` was built to prevent. Pinned by the
    /// `low* — heuristic-only` line on the verdict.
    #[tokio::test(flavor = "current_thread")]
    async fn assess_change_heuristic_only_verdict_does_not_say_low() {
        use crate::schema::{EdgeProvenance, NodeType, RepoNamespace};

        let tmp = std::env::temp_dir().join("test_assess_heuristic_only");
        let _ = std::fs::remove_dir_all(&tmp);
        let graph = GraphDatabase::new(&tmp).unwrap();
        let overlay = VolatileOverlay::new();
        let ns = RepoNamespace::for_test();

        // Single Function target — no static callers, only one
        // BusTopic heuristic edge pointing at it.
        let target = GraphNode::new(
            NodeType::Function,
            "fire_handler".to_string(),
            "/src/handler.py".to_string(),
        );
        graph.upsert_node(target.clone()).unwrap();

        let hub_name = "Hub:message_bus_publisher";
        let hub_id = GraphNode::generate_id(&NodeType::Synthetic, "__hub__", hub_name, None, &ns);
        let caller_id = GraphNode::generate_id(&NodeType::File, "/src/caller.py", "", None, &ns);
        graph
            .upsert_node({
                let mut n =
                    GraphNode::new(NodeType::File, String::new(), "/src/caller.py".to_string());
                n.id = caller_id.clone();
                n
            })
            .unwrap();
        graph
            .upsert_node({
                let mut n =
                    GraphNode::new(NodeType::Synthetic, hub_name.to_string(), String::new());
                n.id = hub_id.clone();
                n
            })
            .unwrap();
        graph
            .insert_edges_batch(&[GraphEdge {
                edge_type: EdgeType::BusTopic,
                source_id: caller_id,
                target_id: target.id.clone(),
                weight: Some(0.7),
                cross_repo: false,
                provenance: Some(EdgeProvenance::Heuristic {
                    detector: "message_bus_publisher".to_string(),
                    confidence: 0.7,
                }),
            }])
            .unwrap();

        let mut a = Map::new();
        a.insert(
            "symbol".to_string(),
            Value::String("fire_handler".to_string()),
        );
        let out = assess_change(&graph, &overlay, std::path::Path::new("/"), &a, None)
            .await
            .expect("assess_change on a known symbol must succeed");

        // The static graph is empty (no Calls/Uses edges) but the
        // heuristic caller exists. Pre-fix: verdict was bare `low`
        // and the agent would treat this as safe. Post-fix: verdict
        // must be `low*` and carry a pointer to the heuristic
        // section so the agent knows the static graph is empty by
        // blind-spot, not by absence of callers.
        assert!(
            !out.contains("verdict: **low**\n") && !out.ends_with("verdict: **low**."),
            "heuristic-only assess_change must not say bare risk=low, got:\n{out}"
        );
        assert!(
            out.contains("heuristic-only") || out.contains("heuristic caller(s) included"),
            "expected explicit heuristic marker in the verdict line, got:\n{out}"
        );
    }

    /// Truly-empty blast radius (no static callers AND no heuristic
    /// callers) reports `risk=low` cleanly — no asterisk, no
    /// heuristic marker. This is the third vertex of the risk-tier
    /// contract:
    ///
    ///   1. N static callers → risk tier from count (medium / high)
    ///   2. 0 static + N heuristic → `risk=low* — heuristic-only`
    ///   3. 0 static + 0 heuristic → bare `risk=low`
    ///
    /// Without this third case pinned, the iter-21 `low*` heuristic
    /// could leak into the truly-safe path and confuse an agent into
    /// refusing to edit a leaf function.
    #[tokio::test(flavor = "current_thread")]
    async fn assess_change_truly_empty_blast_radius_says_low() {
        // Fresh graph with a single Function node and no callers.
        // The test fixture must be truly caller-free — make_test_graph
        // wires up a Calls edge, so we build inline.
        let tmp = std::env::temp_dir().join("test_assess_truly_empty");
        let _ = std::fs::remove_dir_all(&tmp);
        let graph = GraphDatabase::new(&tmp).unwrap();
        let overlay = VolatileOverlay::new();
        let leaf = GraphNode::new(
            NodeType::Function,
            "leaf_helper".to_string(),
            "/src/lib.rs".to_string(),
        );
        graph.upsert_node(leaf.clone()).unwrap();

        let mut a = Map::new();
        a.insert(
            "symbol".to_string(),
            Value::String("leaf_helper".to_string()),
        );
        let out = assess_change(&graph, &overlay, std::path::Path::new("/"), &a, None)
            .await
            .expect("assess_change on a known symbol must succeed");

        // Pin the third vertex of the contract: bare `risk=low`,
        // no asterisk, no heuristic-only marker.
        assert!(
            out.contains("verdict: **low**"),
            "truly-empty blast radius must say bare risk=low, got:\n{out}"
        );
        assert!(
            !out.contains("heuristic-only"),
            "truly-empty blast radius must NOT carry the heuristic-only caveat, got:\n{out}"
        );
        assert!(
            !out.contains("heuristic caller(s) included"),
            "truly-empty blast radius must NOT carry the heuristic marker, got:\n{out}"
        );
    }

    /// Mixed static + heuristic callers must produce the regular
    /// risk tier (medium / high) — NOT the bare `low` of the
    /// truly-empty case AND NOT the `low*` of the heuristic-only
    /// case. The heuristic evidence augments an already-existing
    /// caller surface; it doesn't replace the count-based tier.
    /// Pinned here so a future refactor that re-routes the
    /// heuristic-only verdict into the mixed path is a
    /// deliberate decision.
    #[tokio::test(flavor = "current_thread")]
    async fn assess_change_mixed_static_and_heuristic_uses_normal_tier() {
        use crate::schema::{EdgeProvenance, NodeType, RepoNamespace};

        let tmp = std::env::temp_dir().join("test_assess_mixed");
        let _ = std::fs::remove_dir_all(&tmp);
        let graph = GraphDatabase::new(&tmp).unwrap();
        let overlay = VolatileOverlay::new();
        let ns = RepoNamespace::for_test();

        // Target with one Calls edge (static caller) and one
        // BusTopic heuristic edge pointing at it. The static edge
        // alone would put us in the `medium` tier (1 direct
        // caller ≤ 3). The heuristic edge should not flip the
        // verdict to bare low or to `low*`.
        let target = GraphNode::new(
            NodeType::Function,
            "process".to_string(),
            "/src/processor.rs".to_string(),
        );
        graph.upsert_node(target.clone()).unwrap();

        // Static caller: `main` → `process` via Calls.
        let static_caller = GraphNode::new(
            NodeType::Function,
            "main".to_string(),
            "/src/main.rs".to_string(),
        );
        graph.upsert_node(static_caller.clone()).unwrap();
        graph
            .insert_edges_batch(&[GraphEdge::new(
                EdgeType::Calls,
                static_caller.id.clone(),
                target.id.clone(),
            )])
            .unwrap();

        // Heuristic caller: a separate file publishing to a bus
        // that lands on `process`.
        let hub_name = "Hub:message_bus_publisher";
        let hub_id = GraphNode::generate_id(&NodeType::Synthetic, "__hub__", hub_name, None, &ns);
        let heuristic_caller_id =
            GraphNode::generate_id(&NodeType::File, "/src/publisher.rs", "", None, &ns);
        graph
            .upsert_node({
                let mut n = GraphNode::new(
                    NodeType::File,
                    String::new(),
                    "/src/publisher.rs".to_string(),
                );
                n.id = heuristic_caller_id.clone();
                n
            })
            .unwrap();
        graph
            .upsert_node({
                let mut n =
                    GraphNode::new(NodeType::Synthetic, hub_name.to_string(), String::new());
                n.id = hub_id.clone();
                n
            })
            .unwrap();
        graph
            .insert_edges_batch(&[GraphEdge {
                edge_type: EdgeType::BusTopic,
                source_id: heuristic_caller_id,
                target_id: target.id.clone(),
                weight: Some(0.7),
                cross_repo: false,
                provenance: Some(EdgeProvenance::Heuristic {
                    detector: "message_bus_publisher".to_string(),
                    confidence: 0.7,
                }),
            }])
            .unwrap();

        let mut a = Map::new();
        a.insert("symbol".to_string(), Value::String("process".to_string()));
        let out = assess_change(&graph, &overlay, std::path::Path::new("/"), &a, None)
            .await
            .expect("assess_change on a known symbol must succeed");

        // The mixed case must NOT downgrade to either tier-3
        // special verdict. The static caller's presence keeps the
        // risk tier on the regular scale.
        assert!(
            !out.contains("verdict: **low**") && !out.contains("verdict: **low*"),
            "mixed static+heuristic assess_change must NOT downgrade to \
             bare low or low*, got:\n{out}"
        );
        assert!(
            out.contains("verdict: **medium**") || out.contains("verdict: **high**"),
            "mixed static+heuristic must report medium or high, got:\n{out}"
        );
        // And the heuristic evidence should still surface so
        // the agent knows the runtime target has more callers
        // than the static graph shows.
        assert!(
            out.contains("heuristic caller(s) included"),
            "mixed case must still surface the heuristic evidence, got:\n{out}"
        );
    }

    /// The `~ N heuristic caller(s) included` count in
    /// `assess_change`'s body must reflect the actual number of
    /// heuristic edges in the graph — agents rely on that count
    /// to decide whether to follow up with `explain_dispatch`.
    /// Fixture with three BusTopic heuristic edges pointing at
    /// the same target pins `~ 3 heuristic caller(s) included`.
    #[tokio::test(flavor = "current_thread")]
    async fn assess_change_heuristic_caller_count_reflects_graph() {
        use crate::schema::{EdgeProvenance, NodeType, RepoNamespace};

        let tmp = std::env::temp_dir().join("test_assess_heuristic_count");
        let _ = std::fs::remove_dir_all(&tmp);
        let graph = GraphDatabase::new(&tmp).unwrap();
        let overlay = VolatileOverlay::new();
        let ns = RepoNamespace::for_test();

        let target = GraphNode::new(
            NodeType::Function,
            "process".to_string(),
            "/src/processor.py".to_string(),
        );
        graph.upsert_node(target.clone()).unwrap();

        // Three distinct files publish to the same message bus,
        // each landing on `process`.
        for path in [
            "/src/orders.py",
            "/src/payments.py",
            "/src/notifications.py",
        ] {
            let caller_id = GraphNode::generate_id(&NodeType::File, path, "", None, &ns);
            graph
                .upsert_node({
                    let mut n = GraphNode::new(NodeType::File, String::new(), path.to_string());
                    n.id = caller_id.clone();
                    n
                })
                .unwrap();
            graph
                .insert_edges_batch(&[GraphEdge {
                    edge_type: EdgeType::BusTopic,
                    source_id: caller_id,
                    target_id: target.id.clone(),
                    weight: Some(0.7),
                    cross_repo: false,
                    provenance: Some(EdgeProvenance::Heuristic {
                        detector: "message_bus_publisher".to_string(),
                        confidence: 0.7,
                    }),
                }])
                .unwrap();
        }

        let mut a = Map::new();
        a.insert("symbol".to_string(), Value::String("process".to_string()));
        let out = assess_change(&graph, &overlay, std::path::Path::new("/"), &a, None)
            .await
            .expect("assess_change on a known symbol must succeed");

        assert!(
            out.contains("~ 3 heuristic caller(s) included"),
            "the heuristic count must reflect the graph (3 edges), got:\n{out}"
        );
    }

    /// Cross-tool contract: when `assess_change` reports
    /// `risk=low*` (heuristic-only), the same symbol fed to
    /// `explain_dispatch` must return one of the heuristic
    /// verdicts — `heuristic_only` or `runtime_confirmed` (if a
    /// matching runtime span exists). The static-only verdicts
    /// (`static_only`, `mixed`) and `no_callers` / `insufficient_evidence`
    /// are wrong — assess_change said `low*` precisely because the
    /// static graph is empty but the heuristic sensor saw
    /// something. Pinned here so a future refactor that decouples
    /// the two paths (e.g. one stops calling the other) is caught
    /// by CI.
    #[tokio::test(flavor = "current_thread")]
    async fn assess_change_low_star_implies_explain_dispatch_sees_heuristic() {
        use crate::schema::{EdgeProvenance, NodeType, RepoNamespace};

        // Re-use the heuristic-only fixture from iter 21.
        let tmp = std::env::temp_dir().join("test_assess_low_star_to_explain");
        let _ = std::fs::remove_dir_all(&tmp);
        let graph = GraphDatabase::new(&tmp).unwrap();
        let overlay = VolatileOverlay::new();
        let ns = RepoNamespace::for_test();

        let target = GraphNode::new(
            NodeType::Function,
            "fire_handler".to_string(),
            "/src/handler.py".to_string(),
        );
        graph.upsert_node(target.clone()).unwrap();

        let hub_name = "Hub:message_bus_publisher";
        let hub_id = GraphNode::generate_id(&NodeType::Synthetic, "__hub__", hub_name, None, &ns);
        let caller_id = GraphNode::generate_id(&NodeType::File, "/src/caller.py", "", None, &ns);
        graph
            .upsert_node({
                let mut n =
                    GraphNode::new(NodeType::File, String::new(), "/src/caller.py".to_string());
                n.id = caller_id.clone();
                n
            })
            .unwrap();
        graph
            .upsert_node({
                let mut n =
                    GraphNode::new(NodeType::Synthetic, hub_name.to_string(), String::new());
                n.id = hub_id.clone();
                n
            })
            .unwrap();
        graph
            .insert_edges_batch(&[GraphEdge {
                edge_type: EdgeType::BusTopic,
                source_id: caller_id,
                target_id: target.id.clone(),
                weight: Some(0.7),
                cross_repo: false,
                provenance: Some(EdgeProvenance::Heuristic {
                    detector: "message_bus_publisher".to_string(),
                    confidence: 0.7,
                }),
            }])
            .unwrap();

        // 1) assess_change must report risk=low* — heuristic-only.
        let mut a = Map::new();
        a.insert(
            "symbol".to_string(),
            Value::String("fire_handler".to_string()),
        );
        let assess_out = assess_change(&graph, &overlay, std::path::Path::new("/"), &a, None)
            .await
            .expect("assess_change on a known symbol must succeed");
        assert!(
            assess_out.contains("heuristic-only") || assess_out.contains("verdict: **low*"),
            "expected risk=low* — heuristic-only; got:\n{assess_out}"
        );

        // 2) explain_dispatch on the same symbol must surface
        // the heuristic caller (heuristic_only or
        // runtime_confirmed — there are no runtime spans in this
        // fixture so heuristic_only is the right answer).
        let explain_out = crate::server::tools::handlers::explain_dispatch::explain_dispatch(
            &graph,
            &overlay,
            "fire_handler",
        )
        .await
        .expect("explain_dispatch on a known symbol must succeed");

        assert!(
            explain_out.contains("heuristic_only") || explain_out.contains("runtime_confirmed"),
            "explain_dispatch must surface the heuristic caller \
             (verdict heuristic_only or runtime_confirmed); got:\n{explain_out}"
        );
        assert!(
            !explain_out.contains("verdict: no_callers"),
            "explain_dispatch must NOT report no_callers when \
             assess_change reported risk=low*; got:\n{explain_out}"
        );
    }

    /// Helper that wraps `search_code` with `cross_encoder` and
    /// `embedding_cache` defaulted to empty (so the lexical path
    /// is the only one exercised — that's what these unit tests
    /// cover). The full semantic-search wiring is covered by the
    /// existing `tests/semantic_search_*.rs` integration suite.
    fn search_code_lexical_only(
        graph: &GraphDatabase,
        overlay: &VolatileOverlay,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        // Use a stub embedder (the default-constructed one is a
        // stub; we don't need a real model for the lexical path).
        use crate::nlp::{CrossEncoder, NlpEmbedder};
        use crate::tuning::TuningConfig;
        use std::sync::Arc;
        let embedder = NlpEmbedder::new_stub();
        let cross = CrossEncoder::from_dir(std::path::Path::new("/nonexistent"));
        let cache = Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new()));
        let tuning = TuningConfig::default();
        search_code(
            std::path::Path::new("/"),
            graph,
            overlay,
            &embedder,
            &cross,
            &cache,
            &tuning,
            args,
        )
    }
}
