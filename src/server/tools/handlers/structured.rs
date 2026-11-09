use crate::error::LainError;
use crate::schema::{EdgeType, GraphNode};
use crate::server::tools::registry::{ToolContext, ToolOutput};
use crate::server::tools::utils::{required_str_arg, resolve_node, resolve_node_ambiguous};
use serde_json::{json, Map, Value};
use std::collections::{HashSet, VecDeque};

fn node_json(node: &GraphNode) -> Value {
    json!({
        "id": node.id,
        "type": node.node_type.to_string(),
        "name": node.name,
        "path": node.path,
        "line_start": node.line_start,
        "line_end": node.line_end,
        "commit": node.commit_hash,
        "repo_id": node.repo_id,
        "signature": node.signature,
        "docstring": node.docstring,
        "label": node.label,
        "anchor_score": node.anchor_score,
        "depth_from_main": node.depth_from_main,
        "fan_in": node.fan_in,
        "fan_out": node.fan_out,
    })
}

fn nodes_json(nodes: &[GraphNode]) -> Vec<Value> {
    nodes.iter().map(node_json).collect()
}

fn direct_edges(graph: &crate::graph::GraphDatabase, id: &str, incoming: bool) -> Vec<GraphNode> {
    let edges = if incoming {
        graph.get_edges_to(id)
    } else {
        graph.get_edges_from(id)
    }
    .unwrap_or_default();
    edges
        .into_iter()
        .filter(|edge| edge.edge_type == EdgeType::Calls)
        .filter_map(|edge| {
            let id = if incoming {
                &edge.source_id
            } else {
                &edge.target_id
            };
            graph.get_node(id).ok().flatten()
        })
        .collect()
}

fn source_excerpt(
    ctx: &ToolContext,
    args: &Map<String, Value>,
    node: &GraphNode,
) -> Result<Option<String>, LainError> {
    if let Some((snapshot, repo)) = ctx.snapshot_scope(args) {
        let repo = snapshot
            .repo_for_node(node)
            .unwrap_or_else(|_| repo.to_string());
        let text = snapshot.read_source(&repo, &node.path)?;
        return Ok(crate::server::tools::utils::read_body_summary_from_text(
            node, 2000, &text,
        ));
    }
    Ok(crate::server::tools::utils::read_body_summary(
        node,
        2000,
        &ctx.workspace,
    ))
}

pub fn find_symbol(ctx: &ToolContext, args: &Map<String, Value>) -> Result<Value, LainError> {
    let hits = super::semantic::find_symbol_nodes(&ctx.graph, &ctx.overlay, args)?;
    Ok(json!({
        "symbols": nodes_json(&hits),
        "count": hits.len(),
        "ambiguous": hits.len() > 1,
    }))
}

pub fn get_call_sites(ctx: &ToolContext, args: &Map<String, Value>) -> Result<Value, LainError> {
    let symbol = required_str_arg(args, "symbol")?;
    let (target, other) = resolve_node_ambiguous(&ctx.graph, &ctx.overlay, &symbol)?;
    let callers = direct_edges(&ctx.graph, &target.id, true);
    let mut sites = Vec::new();
    for caller in &callers {
        let lines = super::context::call_lines_in_scoped(
            &ctx.workspace,
            caller,
            &target.name,
            ctx.snapshot_scope(args),
        )?;
        sites.push(json!({
            "caller": node_json(caller),
            "lines": lines,
            "exact": !lines.is_empty(),
        }));
    }
    Ok(json!({
        "target": node_json(&target),
        "sites": sites,
        "count": sites.len(),
        "ambiguous": !other.is_empty(),
    }))
}

pub fn get_cross_runtime_callers(
    ctx: &ToolContext,
    args: &Map<String, Value>,
) -> Result<Value, LainError> {
    let symbol = required_str_arg(args, "node_id")?;
    let target = resolve_node(&ctx.graph, &ctx.overlay, &symbol)?;
    let mut http = Vec::new();
    let mut rpc = Vec::new();
    let mut graphql = Vec::new();
    for edge in ctx.graph.get_edges_to(&target.id)? {
        let Some(source) = ctx.graph.get_node(&edge.source_id)? else {
            continue;
        };
        match edge.edge_type {
            EdgeType::CallsHttp => http.push(node_json(&source)),
            EdgeType::Implements => rpc.push(node_json(&source)),
            EdgeType::Uses | EdgeType::CallsGraphql => graphql.push(node_json(&source)),
            _ => {}
        }
    }
    Ok(json!({
        "target": node_json(&target),
        "http_routes": http,
        "rpc_services": rpc,
        "graphql_resolvers": graphql,
        "availability": {
            "http": EdgeType::CallsHttp.is_indexed(),
            "rpc": EdgeType::Implements.is_indexed(),
            "graphql": EdgeType::CallsGraphql.is_indexed(),
        },
    }))
}

pub fn explain_symbol(ctx: &ToolContext, args: &Map<String, Value>) -> Result<Value, LainError> {
    let symbol = required_str_arg(args, "symbol")?;
    let (target, other) = resolve_node_ambiguous(&ctx.graph, &ctx.overlay, &symbol)?;
    let source = source_excerpt(ctx, args, &target)?;
    let active = ctx.presence.list_active(false);
    let agents: Vec<Value> = active
        .iter()
        .map(|session| {
            json!({
                "agent_id": session.id.as_str(),
                "name": session.name,
                "files": ctx.occupancy.list_for_agent(session.name.as_str())
                    .iter()
                    .map(|claim| claim.path.to_string_lossy().to_string())
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(json!({
        "symbol": node_json(&target),
        "ambiguous": !other.is_empty(),
        "source": source,
        "annotations": super::registry_impl::open_annotations_for_symbol(ctx, &symbol),
        "occupancy": {"active_agents": agents},
    }))
}

pub fn get_context(ctx: &ToolContext, args: &Map<String, Value>) -> Result<Value, LainError> {
    let symbol = required_str_arg(args, "symbol")?;
    let target = resolve_node(&ctx.graph, &ctx.overlay, &symbol)?;
    let callers = direct_edges(&ctx.graph, &target.id, true);
    let callees = direct_edges(&ctx.graph, &target.id, false);
    Ok(json!({
        "definition": node_json(&target),
        "callers": nodes_json(&callers),
        "callees": nodes_json(&callees),
        "source": source_excerpt(ctx, args, &target)?,
        "annotations": super::registry_impl::open_annotations_for_symbol(ctx, &symbol),
    }))
}

pub fn assess_change(ctx: &ToolContext, args: &Map<String, Value>) -> Result<Value, LainError> {
    let symbol = required_str_arg(args, "symbol")?;
    let target = resolve_node(&ctx.graph, &ctx.overlay, &symbol)?;
    let direct_nodes = direct_edges(&ctx.graph, &target.id, true);
    let direct_ids: HashSet<String> = direct_nodes.iter().map(|node| node.id.clone()).collect();
    let mut seen = direct_ids.clone();
    let mut queue: VecDeque<String> = direct_nodes.iter().map(|node| node.id.clone()).collect();
    let mut transitive = Vec::new();
    while let Some(id) = queue.pop_front() {
        for caller in direct_edges(&ctx.graph, &id, true) {
            if seen.insert(caller.id.clone()) {
                queue.push_back(caller.id.clone());
                transitive.push(caller);
            }
        }
    }
    let untested: Vec<Value> = direct_nodes
        .iter()
        .chain(transitive.iter())
        .filter(|node| node.label.as_deref() != Some("test"))
        .map(node_json)
        .collect();
    let risk = if direct_nodes.is_empty() && transitive.is_empty() {
        "low"
    } else if direct_nodes.len() <= 3 && transitive.len() <= 20 {
        "medium"
    } else {
        "high"
    };
    let coupling = ctx
        .graph
        .get_co_change_partners(&target.path)
        .unwrap_or_default()
        .into_iter()
        .map(|(path, count)| json!({"path": path, "count": count}))
        .collect::<Vec<_>>();
    Ok(json!({
        "target": node_json(&target),
        "risk": risk,
        "direct_dependents": nodes_json(&direct_nodes),
        "transitive_dependents": nodes_json(&transitive),
        "untested_dependents": untested,
        "co_change_partners": coupling,
        "counts": {
            "direct": direct_ids.len(),
            "transitive": transitive.len(),
        },
    }))
}

pub fn explain_dispatch(ctx: &ToolContext, args: &Map<String, Value>) -> Result<Value, LainError> {
    let symbol = required_str_arg(args, "symbol")?;
    let target = resolve_node(&ctx.graph, &ctx.overlay, &symbol)?;
    let empty_store;
    let store = if ctx.snapshot.is_some() {
        empty_store = crate::server::runtime_trace::RuntimeTraceStore::new(Default::default());
        &empty_store
    } else {
        crate::server::runtime_trace::RuntimeTraceStore::global()
    };
    let report =
        super::explain_dispatch::build_with_store(&ctx.graph, &ctx.overlay, &target, store);
    serde_json::to_value(report).map_err(LainError::Json)
}

pub async fn search_code_output(
    ctx: &ToolContext,
    args: &Map<String, Value>,
) -> Result<ToolOutput, LainError> {
    let query = required_str_arg(args, "query")?;
    let limit = crate::server::tools::utils::usize_arg(args, "limit").unwrap_or(10);
    let outcome = super::semantic::search_code_detailed(
        &ctx.workspace,
        &ctx.graph,
        &ctx.overlay,
        &ctx.embedder,
        &ctx.cross_encoder,
        &ctx.embedding_cache,
        &ctx.tuning,
        args,
    )?;
    let results: Vec<Value> = outcome
        .hits
        .iter()
        .take(limit)
        .map(|hit| {
            let mut node = node_json(&hit.node);
            node["score"] = json!(hit.score);
            node["score_kind"] = json!(hit.score_kind);
            node
        })
        .collect();
    let structured = json!({
        "query": query,
        "mode": outcome.effective_mode,
        "fell_back": outcome.fell_back,
        "results": results,
        "count": results.len(),
    });
    let mut text = outcome.text;
    if outcome.effective_mode == "semantic" {
        if let Some(e) = ctx.readiness.snapshot().embeddings.filter(|e| e.running) {
            text.push_str(&format!(
                "\n\n⏳ Semantic index still building: {} of {} symbols embedded. \
                 Rankings may change until it finishes; `get_capabilities` shows \
                 semantic_search as warming_up until then.",
                e.embedded, e.total
            ));
        }
    }
    Ok(ToolOutput {
        text,
        structured: Some(structured),
    })
}

pub async fn find_symbol_output(
    ctx: &ToolContext,
    args: &Map<String, Value>,
) -> Result<ToolOutput, LainError> {
    let report = super::semantic::find_symbol_report(&ctx.graph, &ctx.overlay, args)?;
    let structured = json!({
        "symbols": nodes_json(&report.hits),
        "count": report.hits.len(),
        "ambiguous": report.hits.len() > 1,
    });
    Ok(ToolOutput {
        text: report.text,
        structured: Some(structured),
    })
}

pub async fn get_call_sites_output(
    ctx: &ToolContext,
    args: &Map<String, Value>,
) -> Result<ToolOutput, LainError> {
    let symbol = required_str_arg(args, "symbol")?;
    let report = super::context::get_call_sites_report_scoped(
        &ctx.workspace,
        &ctx.graph,
        &ctx.overlay,
        &symbol,
        ctx.snapshot_scope(args),
    )?;
    let sites: Vec<Value> = report
        .sites
        .iter()
        .map(|site| {
            json!({
                "caller": node_json(&site.caller),
                "lines": site.lines,
                "exact": !site.lines.is_empty(),
            })
        })
        .collect();
    let structured = json!({
        "target": node_json(&report.target),
        "sites": sites,
        "count": report.total_sites,
        "ambiguous": !report.other_definitions.is_empty(),
        // `null` means the answer is complete — including an empty
        // answer that is a confirmed absence (nothing in the current
        // index calls this). A reason names what the answer cannot
        // cover.
        "answer_scope": crate::server::tools::answer_scope::scope_value(report.answer_scope),
    });
    Ok(ToolOutput {
        text: report.text,
        structured: Some(structured),
    })
}

pub async fn get_cross_runtime_callers_output(
    ctx: &ToolContext,
    args: &Map<String, Value>,
) -> Result<ToolOutput, LainError> {
    let symbol = required_str_arg(args, "node_id")?;
    let report = super::cross_runtime::get_cross_runtime_report(&ctx.graph, &ctx.overlay, &symbol)?;
    let structured = json!({
        "target": node_json(&report.target),
        "http_routes": nodes_json(&report.http_routes),
        "rpc_services": nodes_json(&report.rpc_services),
        "graphql_resolvers": nodes_json(&report.graphql_resolvers),
        "availability": {
            "http": EdgeType::CallsHttp.is_indexed(),
            "rpc": EdgeType::Implements.is_indexed(),
            "graphql": EdgeType::CallsGraphql.is_indexed(),
        },
    });
    Ok(ToolOutput {
        text: report.text,
        structured: Some(structured),
    })
}

pub async fn explain_symbol_output(
    ctx: &ToolContext,
    args: &Map<String, Value>,
) -> Result<ToolOutput, LainError> {
    let symbol = required_str_arg(args, "symbol")?;
    let mut report = super::metrics::explain_symbol_report_scoped(
        &ctx.workspace,
        &ctx.graph,
        &ctx.overlay,
        &ctx.occupancy,
        &symbol,
        ctx.snapshot_scope(args),
    )?;
    let annotations = super::registry_impl::open_annotations_for_symbol(ctx, &symbol);
    report.text.push_str(&annotations);
    let active = ctx.presence.list_active(false);
    let agents: Vec<Value> = active
        .iter()
        .map(|session| {
            json!({
                "agent_id": session.id.as_str(),
                "name": session.name,
                "files": ctx.occupancy.list_for_agent(session.name.as_str())
                    .iter()
                    .map(|claim| claim.path.to_string_lossy().to_string())
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    report.structured["annotations"] = json!(annotations);
    report.structured["occupancy"] = json!({"active_agents": agents});
    Ok(ToolOutput {
        text: report.text,
        structured: Some(report.structured),
    })
}

pub async fn get_context_output(
    ctx: &ToolContext,
    args: &Map<String, Value>,
) -> Result<ToolOutput, LainError> {
    let mut report = super::semantic::get_context_report_scoped(
        &ctx.workspace,
        &ctx.graph,
        &ctx.overlay,
        &ctx.occupancy,
        args,
        ctx.snapshot_scope(args),
    )
    .await?;
    let symbol = required_str_arg(args, "symbol")?;
    let annotations = super::registry_impl::open_annotations_for_symbol(ctx, &symbol);
    report.text.push_str(&annotations);
    report.structured["annotations"] = json!(annotations);
    Ok(ToolOutput {
        text: report.text,
        structured: Some(report.structured),
    })
}

pub async fn assess_change_output(
    ctx: &ToolContext,
    args: &Map<String, Value>,
) -> Result<ToolOutput, LainError> {
    let report = super::semantic::assess_change_report_scoped(
        &ctx.graph,
        &ctx.overlay,
        &ctx.workspace,
        args,
        super::registry_impl::ui_link(ctx),
        ctx.snapshot_scope(args),
    )
    .await?;
    Ok(ToolOutput {
        text: report.text,
        structured: Some(report.structured),
    })
}

pub async fn explain_dispatch_output(
    ctx: &ToolContext,
    args: &Map<String, Value>,
) -> Result<ToolOutput, LainError> {
    let symbol = required_str_arg(args, "symbol")?;
    let report = super::explain_dispatch::explain_dispatch_report_scoped(
        &ctx.graph,
        &ctx.overlay,
        &ctx.workspace,
        &symbol,
        ctx.snapshot.is_some(),
    )
    .await?;
    Ok(ToolOutput {
        text: report.text,
        structured: Some(serde_json::to_value(report.report).map_err(LainError::Json)?),
    })
}

pub async fn query_graph_output(
    ctx: &ToolContext,
    args: &Map<String, Value>,
) -> Result<ToolOutput, LainError> {
    let text = super::query::query_graph_scoped(
        &ctx.workspace,
        &ctx.graph,
        &ctx.embedder,
        &ctx.embedding_cache,
        &ctx.presence,
        &ctx.occupancy,
        Some(args),
        ctx.tuning.ingestion.default_query_limit,
        ctx.snapshot.is_some(),
    )?;
    let structured = serde_json::from_str(&text).map_err(LainError::Json)?;
    Ok(ToolOutput {
        text,
        structured: Some(structured),
    })
}

pub async fn dispatch_output(
    tool: &str,
    ctx: &ToolContext,
    args: &Map<String, Value>,
) -> Result<ToolOutput, LainError> {
    match tool {
        "find_symbol" => find_symbol_output(ctx, args).await,
        "get_context" => get_context_output(ctx, args).await,
        "get_call_sites" => get_call_sites_output(ctx, args).await,
        "get_cross_runtime_callers" => get_cross_runtime_callers_output(ctx, args).await,
        "explain_symbol" => explain_symbol_output(ctx, args).await,
        "assess_change" => assess_change_output(ctx, args).await,
        "explain_dispatch" => explain_dispatch_output(ctx, args).await,
        "query_graph" => query_graph_output(ctx, args).await,
        "search_code" => search_code_output(ctx, args).await,
        _ => Err(LainError::InvalidArgument(format!(
            "unknown structured tool {tool}"
        ))),
    }
}
