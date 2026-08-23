//! Metrics and explanation domain handlers

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::nlp::NlpEmbedder;
use crate::overlay::VolatileOverlay;
use crate::schema::NodeType;
use crate::server::presence::OccupancyMap;
use crate::server::tools::utils::read_body_summary;
use crate::server::tools::utils::{build_enriched_text, cosine_similarity, resolve_node};
use std::sync::Arc;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};

pub fn find_anchors(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    limit: usize
) -> Result<String, LainError> {
    let mut anchors = graph.find_anchors(limit)?;

    // Composition fix: callers do `find_anchors` → `get_blast_radius`
    // (or `get_call_sites`). `get_blast_radius` follows `Calls` edges,
    // which enum/struct/trait nodes don't have — so a type at the top
    // of the anchor list is a dead end for the headline workflow.
    // Filter to callable symbols (Function/Method) so the strategy
    // guide's "find_anchors → get_blast_radius" actually works.
    anchors.retain(|n| {
        matches!(
            n.node_type,
            crate::server::schema::NodeType::Function | crate::server::schema::NodeType::Method
        )
    });

    let overlay_anchors = overlay.get_all_nodes().into_iter()
        .filter(|n| n.anchor_score.is_some())
        .filter(|n| {
            matches!(
                n.node_type,
                crate::server::schema::NodeType::Function | crate::server::schema::NodeType::Method
            )
        })
        .collect::<Vec<_>>();

    let mut seen_ids: std::collections::HashSet<String> = anchors.iter().map(|a| a.id.clone()).collect();
    for oa in overlay_anchors {
        if seen_ids.insert(oa.id.clone()) {
            anchors.push(oa);
        }
    }
    anchors.sort_by(|a, b| {
        b.anchor_score
            .unwrap_or(0.0)
            .total_cmp(&a.anchor_score.unwrap_or(0.0))
    });

    if anchors.is_empty() {
        return Ok("No anchors found in Merged Brain.".to_string());
    }

    // Report the path, not just the name. `graph.find_anchors` dedups by
    // name and keeps the best-scoring node, so the name alone is
    // ambiguous whenever a name is defined more than once — and the
    // follow-up call the strategy guide recommends resolves the name
    // independently, landing on a different node. Live, `find_anchors`
    // listed `as_str (score: 100.000)` while `get_anchor_score as_str`
    // answered `0.000`: two tools, same name, different nodes, flatly
    // contradictory answers with nothing on screen to explain it.
    Ok(format!("Top {} anchors (Merged Brain):\n{}",
        anchors.len().min(limit),
        anchors.iter().enumerate().take(limit).map(|(i, n)| {
            let score = n.anchor_score.map(|s| format!("{:.3}", s)).unwrap_or_else(|| "N/A".to_string());
            format!("{}. {} ({:?}) in {} (score: {})", i + 1, n.name, n.node_type, n.path, score)
        }).collect::<Vec<_>>().join("\n")
    ))
}

pub fn get_anchor_score(
    graph: &GraphDatabase, 
    overlay: &VolatileOverlay,
    symbol: &str
) -> Result<String, LainError> {
    let node = resolve_node(graph, overlay, symbol)?;
    // Name which node was scored. A name defined several times resolves
    // to one arbitrary instance here while `find_anchors` reports the
    // best-scoring one, so the bare name made the two tools look like
    // they disagreed (`as_str`: 100.000 there, 0.000 here) when they
    // were describing different symbols.
    match node.anchor_score {
        Some(s) => Ok(format!(
            "Anchor score for '{}' ({:?} in {}): {:.3}",
            symbol, node.node_type, node.path, s
        )),
        None => Ok(format!(
            "Symbol '{}' ({:?} in {}) has no anchor score in Merged Brain.",
            symbol, node.node_type, node.path
        )),
    }
}

pub fn get_context_depth(
    graph: &GraphDatabase, 
    overlay: &VolatileOverlay,
    symbol: &str
) -> Result<String, LainError> {
    let node = resolve_node(graph, overlay, symbol)?;
    match node.depth_from_main {
        Some(d) => Ok(format!("Context depth for '{}': {} layers from entry", symbol, d)),
        None => Ok(format!("Symbol '{}' has no depth score in Merged Brain.", symbol)),
    }
}

/// Names that commonly indicate a false positive (trait defaults, constructors, etc.)
const FALSE_POSITIVE_PATTERNS: &[&str] = &[
    "default", "new", "clone", "from", "into", "as_ref", "as_mut",
    "to_string", "to_owned", "debug", "display", "fmt", "format",
    "from_str", "parse", "try_from", "try_into", "borrowed",
];

/// Check if a function name matches known false-positive patterns
fn is_false_positive_name(name: &str) -> bool {
    FALSE_POSITIVE_PATTERNS.iter().any(|p| name == *p || name.ends_with(p))
}

/// Check if function appears in a trait definition (heuristic: path contains "trait")
fn is_trait_context(path: &str) -> bool {
    path.contains("trait") || path.contains("_trait")
}

/// Path conventions that mark a whole file as tests, across the
/// languages lain indexes. Used only where a per-symbol label cannot
/// exist: a JS or Go test file has no attribute for the extractor to
/// read, so the path is the only signal available.
const TEST_FILE_CONVENTIONS: &[&str] = &[
    "_tests.rs",
    "_test.rs",
    "_test.go",
    ".test.js",
    ".test.ts",
    ".spec.js",
    ".spec.ts",
    "_test.py",
];

/// Is this symbol a test?
///
/// A test function is invoked by the harness, never by production code,
/// so "no callers" is its normal state — reporting it as dead is noise,
/// and acting on the report deletes the test suite.
///
/// The authoritative signal is the `test` label, now set on both
/// indexing paths: the tree-sitter extractor reads `#[test]` directly,
/// and the LSP path propagates it down from an enclosing `mod tests`
/// (see `ingest::scan::is_test_container`). Before that, LSP-derived
/// nodes arrived unlabelled and this function had to guess from
/// function names — which is exactly the kind of guessing that makes a
/// tool untrustworthy. Path conventions remain only for languages where
/// no attribute exists to read.
pub(crate) fn is_test_symbol(node: &crate::schema::GraphNode) -> bool {
    if node.label.as_deref() == Some("test") {
        return true;
    }
    let path = node.path.as_str();
    path.contains("/tests/")
        || path.starts_with("tests/")
        || TEST_FILE_CONVENTIONS.iter().any(|c| path.ends_with(c))
}


/// Minimum function count before a file with zero outgoing call edges
/// is treated as unindexed rather than dead.
///
/// One or two edgeless functions in a file is ordinary — a module of
/// small leaf helpers looks exactly like that. Three or more, none of
/// which appear to call anything, does not happen in real code: it
/// means the call extractor produced no `Calls` edges for the file.
const UNINDEXED_FILE_MIN_FUNCTIONS: usize = 3;

/// Files whose call graph is missing entirely.
///
/// This is the correction for the worst wrong answer lain gave: a
/// 1,127-line `watcher.rs` supplied every one of the top 20 "highly
/// confident dead symbols", and all of them were live. Because "no
/// callers *and* no callees" was the top confidence tier, the unindexed
/// bucket and the highest-confidence bucket were the same bucket.
///
/// The cause is in the *index*, not the extractor — worth stating
/// precisely, because the obvious guess is wrong. Running the
/// tree-sitter extractor directly over `watcher.rs` yields 189 call
/// refs, including every symbol that was reported dead; feeding those
/// refs through `resolve_static_edges` against nodes carrying line
/// ranges produces edges. What fails is further downstream: edges are
/// dropped at insert when an endpoint is missing from the index, and
/// two indexing passes over the same commit have been observed
/// producing materially different graphs. Until that is fixed, a file
/// with definitions and no call edges means "we failed to index this",
/// and saying so is the honest answer.
fn unindexed_files(functions: &[crate::schema::GraphNode]) -> HashSet<String> {
    let mut per_file: HashMap<String, (usize, u32)> = HashMap::new();
    // Counts `calls_out`, not `fan_out`: a file whose symbols have
    // structural edges but no call edges is exactly the case this
    // detects, and `fan_out` would hide it.
    for f in functions {
        // Count only non-test definitions toward the threshold. A file
        // that is mostly `#[test]` functions legitimately makes few
        // calls into user code — `glob_match.rs` is one real function
        // plus four tests — and counting the tests pushed it over the
        // bar and mislabelled it an indexing gap.
        if is_test_symbol(f) {
            continue;
        }
        let e = per_file.entry(f.path.clone()).or_insert((0, 0));
        e.0 += 1;
        e.1 += f.calls_out.unwrap_or(0);
    }
    per_file
        .into_iter()
        .filter(|(_, (count, fan_out_total))| {
            *count >= UNINDEXED_FILE_MIN_FUNCTIONS && *fan_out_total == 0
        })
        .map(|(path, _)| path)
        .collect()
}

/// What a dead-code analysis found, before any of it is turned into
/// prose.
///
/// Separated from rendering because the analysis is the reusable part:
/// `find_dead_code` used to do stub-checking, unindexed detection, test
/// filtering, tiering, semantic filtering *and* formatting in one
/// function that returned a `String`, so any second consumer would have
/// had to parse English to reach the data.
pub struct DeadCodeReport {
    /// Unreferenced and calling nothing — the strong signal.
    pub unreferenced: Vec<crate::schema::GraphNode>,
    /// Unreferenced but still calling out: entry points, callbacks and
    /// trait impls look like this, so it is weaker evidence.
    pub calls_out: Vec<crate::schema::GraphNode>,
    /// Files with definitions but no call edges at all. Their symbols
    /// are excluded rather than reported: we cannot see their callers
    /// because we cannot see anyone's callers there.
    pub unindexed_files: Vec<String>,
    /// How many symbols those files accounted for.
    pub unindexed_symbols: usize,
    /// Tests dropped from consideration — a test has no production
    /// caller by design.
    pub tests_excluded: usize,
    /// Dropped because the name appears again in its own file: a serde
    /// attribute string, a function pointer, or similar reference the
    /// call graph does not model.
    pub name_referenced: usize,
}

/// Classify every function in the graph. Pure with respect to the
/// graph; the optional semantic filter is applied by the caller.
/// Is this symbol's name mentioned anywhere in its own file besides its
/// definition?
///
/// The call graph only records *calls*. A symbol can be referenced in
/// ways that are not calls and that no `Calls` edge will ever capture:
///
/// - `#[serde(default = "default_ref")]` — the reference is a string
///   literal read by a derive macro.
/// - `resolve_in(..., &run_resolver_command)` — a function pointer
///   passed as a value, never invoked at that site.
///
/// Names among `candidates` that appear somewhere in the workspace
/// beyond their own definition.
///
/// The same-file check above only ever caught references a symbol makes
/// to itself. Every miss found in live testing was *cross-file*: a
/// three-agent sweep reported nine dead symbols and seven were called
/// from another file — `edge_counts_by_type` from `tools.rs`,
/// `cosine_similarity` from two query paths, `clone_for_background`
/// from `cli/server.rs`. One of them, `edge_counts_by_type`, produces a
/// section of `get_health`'s own output: the server used the function
/// to answer the agent and then reported that nothing calls it.
///
/// The cause is missing `Calls` edges, not bad logic here — those edges
/// depend on how far LSP indexing got, so the same repo yields
/// different answers warm and cold. That makes the call graph the wrong
/// sole authority for a claim as destructive as "delete this". A
/// textual sweep is weak evidence, but it is weak in the safe
/// direction: a false "referenced" costs a missed cleanup, a false
/// "dead" invites deleting working code.
///
/// One pass over the files, all candidates at once — candidate lists
/// are small (single digits) and re-reading per candidate would be
/// quadratic.
fn names_referenced_anywhere(
    candidates: &[crate::schema::GraphNode],
    workspace: &std::path::Path,
) -> HashSet<String> {
    let mut referenced = HashSet::new();
    if candidates.is_empty() {
        return referenced;
    }
    // Where each name is defined, so the definition itself does not
    // count as a reference to itself.
    let mut own_file: HashMap<&str, &str> = HashMap::new();
    for c in candidates {
        own_file.insert(c.name.as_str(), c.path.as_str());
    }

    let walker = ignore::WalkBuilder::new(workspace).hidden(false).build();
    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let rel = path
            .strip_prefix(workspace)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        for c in candidates {
            let name = c.name.as_str();
            if name.is_empty() || referenced.contains(name) {
                continue;
            }
            // In the defining file the definition is one legitimate
            // occurrence, so a second is needed; elsewhere one is enough.
            let threshold = if own_file.get(name) == Some(&rel.as_str()) { 1 } else { 0 };
            if whole_word_hits(&text, name) > threshold {
                referenced.insert(name.to_string());
            }
        }
    }
    referenced
}

/// Count whole-word occurrences of `name` in `text`.
fn whole_word_hits(text: &str, name: &str) -> usize {
    let mut hits = 0usize;
    for (i, _) in text.match_indices(name) {
        let before = text[..i].chars().next_back();
        let after = text[i + name.len()..].chars().next();
        let boundary = |c: Option<char>| !c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        if boundary(before) && boundary(after) {
            hits += 1;
        }
    }
    hits
}

pub fn analyze_dead_code(
    graph: &GraphDatabase,
    workspace: &std::path::Path,
) -> Result<DeadCodeReport, LainError> {
    let functions = graph.get_nodes_by_type(NodeType::Function)?;
    let unindexed = unindexed_files(&functions);

    // Primary filter: no incoming *calls*.
    //
    // This read `fan_in` once, which counts every incoming edge — and
    // every symbol has an incoming `Contains` edge from its own file.
    // So `fan_in == 0` was essentially never true and the tool reported
    // nothing at all, silently, while looking healthy.
    let candidates: Vec<_> = functions
        .into_iter()
        .filter(|f| f.calls_in.unwrap_or(0) == 0)
        .collect();

    // Drop names, trait contexts, and tests — all dead-looking by
    // convention rather than by fact.
    let before_tests = candidates.len();
    let named: Vec<_> = candidates
        .into_iter()
        .filter(|f| !is_false_positive_name(&f.name) && !is_trait_context(&f.path))
        .filter(|f| !is_test_symbol(f))
        .collect();
    let tests_excluded = before_tests.saturating_sub(named.len());

    let (unindexed_hits, indexed): (Vec<_>, Vec<_>) =
        named.into_iter().partition(|f| unindexed.contains(&f.path));

    // Drop anything whose name is mentioned again in its own file:
    // serde attribute strings and function pointers are references the
    // call graph cannot see.
    let before_mentions = indexed.len();
    let referenced = names_referenced_anywhere(&indexed, workspace);
    let indexed: Vec<_> = indexed
        .into_iter()
        .filter(|f| !referenced.contains(&f.name))
        .collect();
    let name_referenced = before_mentions.saturating_sub(indexed.len());

    let (unreferenced, calls_out): (Vec<_>, Vec<_>) = indexed
        .into_iter()
        .partition(|f| f.calls_out.unwrap_or(0) == 0);

    let mut unindexed_files: Vec<String> = unindexed.into_iter().collect();
    unindexed_files.sort();

    Ok(DeadCodeReport {
        unreferenced,
        calls_out,
        unindexed_files,
        unindexed_symbols: unindexed_hits.len(),
        tests_excluded,
        name_referenced,
    })
}

/// Render a [`DeadCodeReport`] as the tool's text response.
fn render_dead_code(report: &DeadCodeReport, shown: &[crate::schema::GraphNode]) -> String {
    let mut out = format!(
        "Found {} unreferenced symbols (no callers, no callees) in Static Backbone:\n{}",
        shown.len(),
        shown
            .iter()
            .take(20)
            .map(|n| format!("- {} ({}) [no callers, no callees]", n.name, n.path))
            .collect::<Vec<_>>()
            .join("\n")
    );

    if report.tests_excluded > 0 {
        out.push_str(&format!(
            "\n\n{} test symbol(s) were excluded: a test is run by the harness, never \
             called by production code, so \"no callers\" is its normal state.",
            report.tests_excluded
        ));
    }

    if report.name_referenced > 0 {
        out.push_str(&format!(
            "\n\n{} symbol(s) were excluded because their name appears again in \
             their own file — a serde attribute string, a function pointer, or \
             another reference the call graph does not model.",
            report.name_referenced
        ));
    }

    if !report.calls_out.is_empty() {
        out.push_str(&format!(
            "\n\n{} more symbols have no callers but do call out (entry points, \
             callbacks, and trait impls look like this) — weaker evidence, not listed.",
            report.calls_out.len()
        ));
    }

    // Naming the excluded files is the point, not a footnote: it is the
    // most actionable bug report the call extractor will ever get.
    if !report.unindexed_files.is_empty() {
        out.push_str(&format!(
            "\n\n⚠ {} file(s) have definitions but no call edges at all — their call \
             graph could not be extracted, so {} symbol(s) in them were excluded rather \
             than reported as dead. This is an indexing gap, not dead code:\n{}",
            report.unindexed_files.len(),
            report.unindexed_symbols,
            report
                .unindexed_files
                .iter()
                .take(20)
                .map(|p| format!("- {p}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    out
}

pub fn find_dead_code(
    workspace: &std::path::Path,
    graph: &GraphDatabase,
    _overlay: &VolatileOverlay,
    like: Option<&str>,
    embedder: &NlpEmbedder,
    embedding_cache: &Arc<Mutex<HashMap<String, Vec<f32>>>>,
) -> Result<String, LainError> {
    // `like` used to degrade to a silent no-op: with a stub embedder
    // nothing cleared the similarity threshold, the filtered set came
    // back empty, and the code fell back to the *unfiltered* list under
    // a different label. Callers saw an identical result set for every
    // query and had no way to know the filter never ran.
    if like.is_some() && embedder.is_stub() {
        return Err(LainError::Unavailable(
            "`like` needs the NLP model, which is not loaded. Download it with \
             `install.sh --download-model`, point LAIN_EMBEDDING_MODEL at a model \
             directory, or drop one into `.lain/models/`. Call find_dead_code \
             without `like` for the unfiltered list."
                .to_string(),
        ));
    }

    let report = analyze_dead_code(graph, workspace)?;

    let shown: Vec<_> = match like {
        Some(query) => {
            // A user query, so it takes the prefix — this call site
            // used to embed it raw against a corpus embedded plain.
            let query_emb = embedder.embed_query(query)?;
            const SEMANTIC_THRESHOLD: f32 = 0.3;
            report
                .unreferenced
                .iter()
                .filter(|n| {
                    get_embedding(n, embedder, embedding_cache, workspace)
                        .map(|emb| cosine_similarity(&query_emb, &emb) > SEMANTIC_THRESHOLD)
                        .unwrap_or(false)
                })
                .cloned()
                .collect()
        }
        None => report.unreferenced.clone(),
    };

    Ok(render_dead_code(&report, &shown))
}

// Helper to get embedding for a node (cache-first, then on-demand)
fn get_embedding(
    node: &crate::schema::GraphNode,
    embedder: &NlpEmbedder,
    cache: &Arc<Mutex<HashMap<String, Vec<f32>>>>,
    workspace: &std::path::Path,
) -> Option<Vec<f32>> {
    // Check cache
    if let Some(emb) = cache.lock().get(&node.id).cloned() {
        return Some(emb);
    }
    // Check stored embedding
    if let Some(ref e_json) = node.embedding {
        if let Ok(emb) = serde_json::from_str::<Vec<f32>>(e_json) {
            cache.lock().insert(node.id.clone(), emb.clone());
            return Some(emb);
        }
    }
    // On-demand embed
    let text = build_enriched_text(node, workspace);
    embedder.embed(&text).ok()
}

pub fn explain_symbol(
    workspace: &std::path::Path,
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    occupancy: &OccupancyMap,
    symbol: &str
) -> Result<String, LainError> {
    let node = resolve_node(graph, overlay, symbol)?;

    let mut lines = Vec::new();
    lines.push(format!("## Explanation for '{}' ({:?})", symbol, node.node_type));
    // Scoped to the file this answer is about. The index is commit-driven, so
    // a file edited and not yet committed is invisible to it — the reader needs
    // to know that here, not from a separate health call they will not make.
    if let Some(note) = graph.freshness(workspace, &node.path).note(&node.path) {
        lines.push(note);
    }
    lines.push(format!("**Path:** {}", node.path));

    if let Some(sig) = &node.signature {
        lines.push(format!("**Signature:** `{}`", sig));
    }

    if let Some(doc) = &node.docstring {
        lines.push(format!("**Documentation:**\n{}", doc));
    }

    // Show the actual code so the user can see what the symbol looks
    // like, not just metadata about it. Single-user value: a developer
    // asking "what is this?" wants the implementation, not path + score.
    if let Some(excerpt) = read_body_summary(&node, 200, workspace) {
        lines.push(String::new());
        lines.push("### Source".to_string());
        lines.push("```".to_string());
        lines.push(excerpt);
        lines.push("```".to_string());
    }

    lines.push(String::new());
    lines.push("### Structural Context".to_string());

    let depth = node.depth_from_main.map(|d| d.to_string()).unwrap_or_else(|| "N/A".to_string());
    let anchor = node.anchor_score.map(|s| format!("{:.3}", s)).unwrap_or_else(|| "N/A".to_string());

    lines.push(format!("- **Context Depth:** {} (Lower is closer to entry point)", depth));
    lines.push(format!("- **Anchor Score:** {} (Higher means more foundational)", anchor));

    let partners = graph.get_co_change_partners(&node.path)?;
    if !partners.is_empty() {
        lines.push(String::new());
        lines.push("### Frequently Co-Changed With (Git History)".to_string());
        for (p, c) in partners.iter().take(5) {
            lines.push(format!("- {} ({} times)", p, c));
        }
    }

    // Incoming/outgoing call edges: who calls this, what does this call.
    // Resolves edge.target_id to a node name when possible. Most useful
    // section for "what's this used for" / "what depends on this" —
    // the things a developer actually wants to know after "what is this?".
    // Filter to `Calls`. This section is titled "Call Graph" and its
    // lines say "Calls" / "Called by", but it used to render *every*
    // incoming edge — so a `Defines` edge from the enclosing file made
    // `explain_symbol` report `Called by: hooks.rs` (a file, not a
    // caller) for a symbol `get_call_sites` correctly called a leaf.
    // Two tools, one session, opposite answers, and no way for an agent
    // to arbitrate between them.
    let is_call = |e: &crate::schema::GraphEdge| e.edge_type == crate::schema::EdgeType::Calls;
    let incoming: Vec<_> = graph
        .get_edges_to(&node.id)
        .unwrap_or_default()
        .into_iter()
        .filter(is_call)
        .collect();
    let outgoing: Vec<_> = graph
        .get_edges_from(&node.id)
        .unwrap_or_default()
        .into_iter()
        .filter(is_call)
        .collect();
    if !incoming.is_empty() || !outgoing.is_empty() {
        lines.push(String::new());
        lines.push("### Call Graph".to_string());

        // Outgoing: "calls X, Y, Z"
        if !outgoing.is_empty() {
            let mut names: Vec<String> = outgoing
                .iter()
                .take(8)
                .map(|e| {
                    graph.get_node(&e.target_id)
                        .ok()
                        .flatten()
                        .map(|n| n.name.clone())
                        .unwrap_or_else(|| e.target_id.clone())
                })
                .collect();
            if outgoing.len() > 8 {
                names.push(format!("(+{} more)", outgoing.len() - 8));
            }
            lines.push(format!("- **Calls:** {}", names.join(", ")));
        }
        // Incoming: "called by A, B, C"
        if !incoming.is_empty() {
            let mut names: Vec<String> = incoming
                .iter()
                .take(8)
                .map(|e| {
                    graph.get_node(&e.source_id)
                        .ok()
                        .flatten()
                        .map(|n| n.name.clone())
                        .unwrap_or_else(|| e.source_id.clone())
                })
                .collect();
            if incoming.len() > 8 {
                names.push(format!("(+{} more)", incoming.len() - 8));
            }
            lines.push(format!("- **Called by:** {}", names.join(", ")));
        }
    }

    // Multiplayer occupancy: if any agent has claimed the file this
    // symbol lives in, surface the claim so the asking agent knows
    // somebody else is in the same area. Emitted as a fenced `json`
    // block (alongside the Markdown sections) so downstream tooling
    // can pattern-match a stable token ("### Occupancy") rather than
    // parse free-form prose. Only emitted when there's actually a
    // claim — no point appending an empty section.
    if let Some(entry) = occupancy.list_for_path(std::path::Path::new(&node.path)) {
        if !entry.agents.is_empty() {
            // `list_for_agent` returns claims keyed by agent, so we
            // walk it to find this file's intent for each agent on it.
            // An Edit intent overrides Read — if any of the agent's
            // claims on the file is Edit, surface Edit (the
            // collaborator is mutating, so the asker should pause).
            let mut intent = "read";
            for agent_id in &entry.agents {
                let claims = occupancy.list_for_agent(agent_id);
                for c in &claims {
                    if c.path == node.path
                        && matches!(c.intent, crate::server::presence::ClaimIntent::Edit)
                    {
                        intent = "edit";
                        break;
                    }
                }
                if intent == "edit" {
                    break;
                }
            }
            let agents: Vec<String> = entry
                .agents
                .iter()
                .map(|a| a.as_str().to_string())
                .collect();
            let payload = serde_json::json!({
                "agents": agents,
                "intent": intent,
            });
            lines.push(String::new());
            lines.push("### Occupancy".to_string());
            lines.push("```json".to_string());
            lines.push(serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string()));
            lines.push("```".to_string());
        }
    }

    Ok(lines.join("\n"))
}

pub fn suggest_refactor_targets(
    graph: &GraphDatabase,
    _overlay: &VolatileOverlay,
    limit: usize
) -> Result<String, LainError> {
    // `Method` belongs here: in impl-heavy languages most logic lives
    // in methods, and leaving them out meant the real God Objects were
    // invisible while a 69-line shell script and a name-collapsed
    // `default` were flagged instead.
    let node_types = [
        NodeType::File,
        NodeType::Module,
        NodeType::Class,
        NodeType::Struct,
        NodeType::Trait,
        NodeType::Function,
        NodeType::Method,
    ];
    let all_nodes = graph.get_nodes_by_types(&node_types)?;

    if all_nodes.is_empty() {
        return Ok("No nodes found in Static Backbone to analyze. Run enrichment first.".to_string());
    }

    let mut targets: Vec<_> = all_nodes.into_iter().map(|n| {
        let fan_in = n.fan_in.unwrap_or(0);
        let fan_out = n.fan_out.unwrap_or(0);
        let co_change = n.co_change_count.unwrap_or(0);
        let anchor = n.anchor_score.unwrap_or(0.0);

        let debt_score = (fan_in as f32 * fan_out as f32) + (co_change as f32 / (anchor + 0.1));
        
        let mut reasons = Vec::new();
        if fan_in > 10 && fan_out > 10 { reasons.push("Potential 'God Object' (high fan-in/fan-out)"); }
        if co_change > 5 && anchor < 0.2 { reasons.push("Fragile/Spaghetti logic (high coupling, low stability)"); }
        if fan_out > 20 { reasons.push("High complexity/fan-out"); }

        (n, debt_score, reasons)
    })
    .filter(|(_, _, reasons)| !reasons.is_empty())
    .collect();

    targets.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    if targets.is_empty() {
        return Ok("Architecture appears healthy! No high-debt refactor targets identified in Static Backbone.".to_string());
    }

    let mut output = "## Refactor Target Suggestions\n\n".to_string();
    output.push_str("Identified the following areas of high architectural debt:\n\n");

    for (node, _, reasons) in targets.iter().take(limit) {
        output.push_str(&format!("### {} ({:?})\n", node.name, node.node_type));
        output.push_str(&format!("- **Path:** {}\n", node.path));
        for reason in reasons {
            output.push_str(&format!("- **⚠️ {}**\n", reason));
        }
        output.push('\n');
    }

    Ok(output)
}
