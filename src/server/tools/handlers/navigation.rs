//! Navigation domain handlers

use crate::error::LainError;
use crate::federation::federated_index::FederatedIndex;
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::schema::{EdgeType, GraphNode, NodeType};
use crate::server::tools::utils::{resolve_node, resolve_node_federation_fallback};
use crate::server::tools::{UiSession, UiSessionData};
use std::collections::{HashMap, HashSet, VecDeque};
use uuid::Uuid;

pub fn trace_dependency(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    symbol: &str,
) -> Result<String, LainError> {
    // 1. Resolve handle
    let start_node = resolve_node(graph, overlay, symbol)?;
    let start_id = start_node.id.clone();

    // Dependencies are what the code uses, not what contains it or merely
    // changes with it: following `Contains` / `CoChangedWith` / `Pattern`
    // pulled in whole files and unrelated modules.
    let is_dependency = |t: &EdgeType| {
        !matches!(
            t,
            EdgeType::Contains | EdgeType::CoChangedWith | EdgeType::Pattern
        )
    };
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    let mut results = Vec::new();
    queue.push_back(start_node);

    while let Some(node) = queue.pop_front() {
        if visited.contains(&node.id) {
            continue;
        }
        visited.insert(node.id.clone());
        // The symbol is not its own dependency.
        if node.id != start_id {
            results.push(node.clone());
        }

        // Get edges from both static and overlay
        let mut targets = HashSet::new();

        // Static edges
        if let Ok(edges) = graph.get_edges_from(&node.id) {
            for e in edges.into_iter().filter(|e| is_dependency(&e.edge_type)) {
                targets.insert(e.target_id);
            }
        }

        // Overlay edges
        let overlay_edges = overlay.get_outgoing_edges(&node.id);
        for (target, edge_type) in overlay_edges {
            if is_dependency(&edge_type) {
                targets.insert(target.id);
            }
        }

        for tid in targets {
            if let Some(target_node) = overlay.get_node(&tid) {
                queue.push_back(target_node);
            } else if let Ok(Some(target_node)) = graph.get_node(&tid) {
                queue.push_back(target_node);
            }
        }
    }

    Ok(format!(
        "Found {} dependency nodes in Merged Brain:\n{}",
        results.len(),
        results
            .iter()
            .map(|n| format!("- {} ({:?})", n.name, n.node_type))
            .collect::<Vec<_>>()
            .join("\n")
    ))
}

pub async fn get_call_chain(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    federation: Option<&FederatedIndex>,
    from: &str,
    to: &str,
    ui_sessions: crate::server::tools::UiLink<'_>,
) -> Result<String, LainError> {
    // Federation fallback: if `from`/`to` don't resolve through the
    // active repo's graph + the shared overlay, search the
    // federation's other repos before declaring NotFound. Without
    // this, get_call_chain is the most frequent victim of the
    // post-boot indexer race: the per-repo graph is still empty for
    // a few hundred ms after the HTTP listener comes up, and any
    // call in that window returns isError=true with "Node not
    // found". Empirically ~40% of test runs on a cold-boot fixture
    // hit this before the fallback; with it, the race is
    // recoverable.
    let resolve = |handle: &str| -> Result<GraphNode, LainError> {
        match resolve_node(graph, overlay, handle) {
            Ok(n) => Ok(n),
            Err(_) => match federation {
                Some(fed) => resolve_node_federation_fallback(fed, handle).ok_or_else(|| {
                    LainError::NotFound(format!("Node not found for handle: {handle}"))
                }),
                None => Err(LainError::NotFound(format!(
                    "Node not found for handle: {handle}"
                ))),
            },
        }
    };
    // A bare name can name several definitions — `request` is both
    // `requests.api.request` and `Session.request`. Picking one of them
    // (the first by path) answered "no path" whenever the chain ran
    // through another, so search from and to every definition of it.
    if from.trim().is_empty() || to.trim().is_empty() {
        return Err(LainError::NotFound(
            "get_call_chain needs non-empty `from` and `to`".to_string(),
        ));
    }
    let all_named = |handle: &str| -> Result<Vec<GraphNode>, LainError> {
        if overlay.get_node(handle).is_none() && !matches!(graph.get_node(handle), Ok(Some(_))) {
            let mut named = graph.find_all_nodes_by_name(handle);
            for n in overlay.find_nodes_by_name(handle) {
                if n.name == handle && !named.iter().any(|m| m.id == n.id) {
                    named.push(n);
                }
            }
            if !named.is_empty() {
                return Ok(named);
            }
        }
        Ok(vec![resolve(handle)?])
    };
    let starts = all_named(from)?;
    let end_nodes = all_named(to)?;
    // An endpoint found only through the federation fallback lives in
    // another repository, where this graph's call edges cannot reach.
    // Searching anyway answered "No call path found", which reads as
    // "they are unrelated".
    let local = |n: &GraphNode| {
        overlay.get_node(&n.id).is_some() || matches!(graph.get_node(&n.id), Ok(Some(_)))
    };
    for (handle, nodes) in [(from, &starts), (to, &end_nodes)] {
        if !nodes.is_empty() && !nodes.iter().any(local) {
            return Err(LainError::NotFound(format!(
                "'{handle}' is not in this repository (found in {}); call chains are traced \
                 within one repository — pass the repo_id that holds both ends",
                nodes[0].path
            )));
        }
    }
    let ends: HashSet<String> = end_nodes.into_iter().map(|n| n.id).collect();

    let mut queue = VecDeque::new();
    let mut parents = HashMap::new();

    for start in &starts {
        queue.push_back(start.id.clone());
        parents.insert(start.id.clone(), None);
    }

    let mut found = None;
    while let Some(current_id) = queue.pop_front() {
        if ends.contains(&current_id) {
            found = Some(current_id);
            break;
        }

        // Calls only: following Contains / CoChangedWith / Pattern edges
        // reported "app.py → helper" (a file containing a function) as a
        // call chain.
        let is_call = |t: &EdgeType| matches!(t, EdgeType::Calls | EdgeType::CallsHttp);
        let mut targets = HashSet::new();
        if let Ok(edges) = graph.get_edges_from(&current_id) {
            for e in edges.into_iter().filter(|e| is_call(&e.edge_type)) {
                targets.insert(e.target_id);
            }
        }
        let overlay_edges = overlay.get_outgoing_edges(&current_id);
        for (target, edge_type) in overlay_edges {
            if is_call(&edge_type) {
                targets.insert(target.id);
            }
        }

        for tid in targets {
            if !parents.contains_key(&tid) {
                parents.insert(tid.clone(), Some(current_id.clone()));
                queue.push_back(tid);
            }
        }
    }

    let Some(end_id) = found else {
        return Ok(format!(
            "No call path found from '{}' to '{}' in this repository.",
            from, to
        ));
    };

    let mut path = Vec::new();
    let mut current = Some(end_id);
    while let Some(id) = current {
        let node = if let Some(n) = overlay.get_node(&id) {
            Some(n)
        } else {
            graph.get_node(&id)?
        };
        if let Some(n) = node {
            // Say which definition an ambiguous endpoint turned out to be.
            let at_start = parents.get(&id).is_some_and(|p| p.is_none());
            let at_end = path.is_empty();
            if (at_start && starts.len() > 1) || (at_end && ends.len() > 1) {
                path.push(format!("{} ({})", n.name, n.path));
            } else {
                path.push(n.name);
            }
        }
        current = parents.get(&id).cloned().flatten();
    }
    path.reverse();

    let mut output = format!("## Call Chain: {} -> {}\n\n{}", from, to, path.join(" → "));

    // Store UI session if rich format requested
    if let Some((sessions, port, ttl)) = ui_sessions {
        let session_id = Uuid::new_v4().to_string();
        let session = UiSession {
            id: session_id.clone(),
            session_type: "call-chain".to_string(),
            created_at: std::time::SystemTime::now(),
            expires_at: std::time::SystemTime::now() + ttl,
            data: UiSessionData::CallChain {
                from: from.to_string(),
                to: to.to_string(),
                path: path.clone(),
            },
        };

        // Insert the session synchronously before returning. Spawning it
        // would race the agent's immediate follow-up fetch of the URL in
        // our response — the session would not exist yet.
        {
            let mut guard = sessions.lock().await;
            // Clean up expired sessions before insert (bounded memory)
            let now = std::time::SystemTime::now();
            guard.retain(|_, s| s.expires_at > now);
            guard.insert(session_id.clone(), session);
        }

        output.push_str(&format!(
            "\n\n[Interactive call chain: http://localhost:{}/ui/call-chain/{}]",
            port, session_id
        ));
        output.push_str("\nClick nodes to explore, then describe your selection to the agent.");
    }

    Ok(output)
}

pub fn navigate_to_anchor(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    symbol: &str,
) -> Result<String, LainError> {
    let start = resolve_node(graph, overlay, symbol)?;

    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();
    let mut best_anchor: Option<GraphNode> = None;

    queue.push_back(start);

    while let Some(current) = queue.pop_front() {
        if visited.contains(&current.id) {
            continue;
        }
        visited.insert(current.id.clone());

        let score = current.anchor_score.unwrap_or(0.0);
        // `if let` so the comparison guard is one expression and a
        // future refactor can't strip the `is_none()` short-circuit
        // and turn the inner `unwrap()` into a panic on the first
        // iteration. The trailing `else` keeps the original
        // short-circuit semantics: take the candidate only when its
        // score strictly beats the current best.
        let replace = match &best_anchor {
            None => true,
            Some(b) => score > b.anchor_score.unwrap_or(0.0),
        };
        if replace {
            best_anchor = Some(current.clone());
        }

        // Neighbors from both
        let mut targets = HashSet::new();
        if let Ok(edges) = graph.get_edges_from(&current.id) {
            for edge in edges {
                targets.insert(edge.target_id);
            }
        }
        for (target, _) in overlay.get_outgoing_edges(&current.id) {
            targets.insert(target.id);
        }

        for tid in targets {
            if let Some(target) = overlay.get_node(&tid) {
                queue.push_back(target);
            } else if let Ok(Some(target)) = graph.get_node(&tid) {
                queue.push_back(target);
            }
        }
    }

    match best_anchor {
        Some(anchor) if anchor.name != symbol => {
            Ok(format!("The foundational anchor for '{}' is **{}** (score: {:.3}, path: {}).\n\nThis node is more foundational because it has a higher fan-in/fan-out ratio.",
                symbol, anchor.name, anchor.anchor_score.unwrap_or(0.0), anchor.path))
        },
        _ => {
            // Leaf case: no more-foundational anchor is reachable. Rather
            // than dead-end with "appears to be foundational already" (which
            // gives the user no actionable next step), point them at the
            // corpus's overall top anchor — that's the most foundational
            // symbol the project has, and it's the most useful answer to
            // "where should I go from here?".
            let top = graph.find_anchors(1).ok().and_then(|mut v| v.pop());
            match top {
                Some(t) if t.name != symbol => Ok(format!(
                    "'{}' has no more-foundational anchor reachable from it. The corpus's top anchor is **{}** (score: {:.3}, path: {}).",
                    symbol, t.name, t.anchor_score.unwrap_or(0.0), t.path)),
                _ => Ok(format!("'{}' is itself a top anchor in this codebase (score: {:.3}, path: {}).",
                    symbol, best_anchor.as_ref().and_then(|a| a.anchor_score).unwrap_or(0.0),
                    best_anchor.as_ref().map(|a| a.path.as_str()).unwrap_or("?")))
            }
        }
    }
}

pub fn get_layered_map(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    layer: usize,
    granularity: &str,
) -> Result<String, LainError> {
    let mut all_nodes = Vec::new();
    // Includes `Method` — a layered map that omits impl blocks is a
    // map with most of the code missing.
    for node_type in [
        NodeType::File,
        NodeType::Namespace,
        NodeType::Module,
        NodeType::Class,
        NodeType::Function,
        NodeType::Method,
    ] {
        all_nodes.extend(graph.get_nodes_by_type(node_type)?);
    }

    // Merge overlay using HashSet for O(N)
    let mut seen_ids: HashSet<String> = all_nodes.iter().map(|n| n.id.clone()).collect();
    for on in overlay.get_all_nodes() {
        if seen_ids.insert(on.id.clone()) {
            all_nodes.push(on);
        }
    }

    let filtered: Vec<_> = all_nodes
        .into_iter()
        .filter(|n| n.depth_from_main.unwrap_or(u32::MAX) as usize == layer)
        .collect();

    if filtered.is_empty() {
        return Ok(format!(
            "No nodes found at Layer {}. Ensure core memory is built.",
            layer
        ));
    }

    let mut output = format!("## Architectural Map: Layer {}\n\n", layer);

    match granularity {
        "module" => {
            let mut modules = HashSet::new();
            for n in filtered {
                if n.node_type == NodeType::File {
                    if let Some(parent_path) = std::path::Path::new(&n.path).parent() {
                        modules.insert(parent_path.to_string_lossy().to_string());
                    }
                } else if n.node_type == NodeType::Namespace {
                    modules.insert(n.path.clone());
                }
            }
            output.push_str("### Modules involved in this layer:\n");
            for m in modules {
                output.push_str(&format!("- **{}**\n", m));
            }
        }
        "file" => {
            output.push_str("### Files involved in this layer:\n");
            let files: HashSet<_> = filtered.into_iter().map(|n| n.path.clone()).collect();
            for f in files {
                output.push_str(&format!("- {}\n", f));
            }
        }
        _ => {
            output.push_str("### Symbols at this layer:\n");
            for n in filtered {
                output.push_str(&format!("- {} ({:?}) in {}\n", n.name, n.node_type, n.path));
            }
        }
    }

    output.push_str(&format!(
        "\n*Use `get_layered_map(layer: {})` to see what these components depend on.*",
        layer + 1
    ));

    Ok(output)
}
