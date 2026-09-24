use crate::error::LainError;
use crate::lsp::{HierarchicalSymbol, LspMultiplexer, ReferenceLocation};
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use crate::server::ingest::blocking::offthread;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;
use tracing::debug;

/// A raw call/type-usage reference from tree-sitter, not yet resolved to node IDs.
pub struct StaticFileRef {
    pub file_path: String,
    pub source_line: u32,
    pub target_name: String,
    pub edge_type: EdgeType,
    /// See [`crate::treesitter::StaticRef::foreign_receiver`].
    pub foreign_receiver: bool,
    /// See [`crate::treesitter::StaticRef::self_receiver`].
    pub self_receiver: bool,
    /// See [`crate::treesitter::StaticRef::qualifier`].
    pub qualifier: Option<String>,
}

/// A string literal that could indicate cross-boundary coupling
pub struct PatternRef {
    pub file_path: String,
    pub source_line: u32,
    pub value: String,
}

/// Result of a single file's structural scan
pub struct FileScanResult {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub external_references: Vec<(String, ReferenceLocation)>, // source_node_id, reference
    pub static_refs: Vec<StaticFileRef>,
    pub pattern_refs: Vec<PatternRef>,
}

/// All tree-sitter work for a single file, batched into one
/// offthread call so we amortize the spawn_blocking round-trip
/// across `extract_definitions` / `extract_refs` / `extract_strings`
/// instead of paying it three times. Pure CPU work; no `Send`-hostile
/// references cross the await boundary.
struct TreeSitterFile {
    defs: Vec<crate::treesitter::SymbolDef>,
    static_refs: Vec<crate::treesitter::StaticRef>,
    pattern_refs: Vec<crate::treesitter::StringLiteral>,
}

fn extract_tree_sitter_file(path: &Path, content: &str) -> TreeSitterFile {
    TreeSitterFile {
        defs: crate::treesitter::extract_definitions(path, content),
        static_refs: crate::treesitter::extract_refs(path, content),
        pattern_refs: crate::treesitter::extract_strings(path, content),
    }
}

/// Pure structural scan without side effects (Map)
#[allow(clippy::too_many_arguments)]
pub async fn scan_file_structure(
    path: PathBuf,
    workspace: PathBuf,
    lsp_mux: Arc<AsyncMutex<LspMultiplexer>>,
    lsp_sync: i64,
    git_sync: i64,
    commit_hash: String,
    namespace: &crate::schema::RepoNamespace,
    cancel: CancellationToken,
) -> Result<FileScanResult, LainError> {
    // The canonical graph key for this file. Every node minted below and
    // every ref emitted for the resolve phase uses this exact string — if a
    // producer and a consumer disagree on the form, the resolve phase finds
    // nothing and every Calls edge silently disappears.
    let relative_path = crate::graph::graph_path(&workspace, &path);

    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut external_references = Vec::new();

    // 1. Module hierarchy for directories
    let mut current_parent_id = None;
    if let Some(parent_dir) = Path::new(&relative_path).parent() {
        let mut components = Vec::new();
        for component in parent_dir.components() {
            components.push(component.as_os_str().to_string_lossy().to_string());
            let current_module_path = components.join("/");

            let mut module_node = GraphNode::new_in(
                NodeType::Namespace,
                component.as_os_str().to_string_lossy().to_string(),
                current_module_path.clone(),
                namespace,
            );
            module_node.last_lsp_sync = Some(lsp_sync);
            module_node.last_git_sync = Some(git_sync);
            module_node.commit_hash = Some(commit_hash.clone());

            let node_id = module_node.id.clone();
            nodes.push(module_node);

            if let Some(prev_id) = current_parent_id {
                edges.push(GraphEdge::new(EdgeType::Contains, prev_id, node_id.clone()));
            }
            current_parent_id = Some(node_id);
        }
    }

    // 2. File node
    let mut file_node = GraphNode::new_in(
        NodeType::File,
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string(),
        relative_path.clone(),
        namespace,
    );
    file_node.last_lsp_sync = Some(lsp_sync);
    file_node.last_git_sync = Some(git_sync);
    file_node.commit_hash = Some(commit_hash.clone());

    let file_id = file_node.id.clone();
    nodes.push(file_node);

    if let Some(parent_id) = current_parent_id {
        edges.push(GraphEdge::new(
            EdgeType::Contains,
            parent_id,
            file_id.clone(),
        ));
    }

    // 3. Fetch all references for this file while we hold the
    //    lock (prevents nested-lock deadlock).
    //
    //    AGENT_UX_ROADMAP.md Milestone 4 (PR E follow-up):
    //    lsp-bridge 0.2's `LspMultiplexer::get_references` is an
    //    `async fn` that internally drives the LSP child process
    //    over stdio. Unlike the libgit2 / tree-sitter / ONNX
    //    migrations PR B + E landed, this call can't move onto
    //    `spawn_blocking` from this repo: the only way to expose
    //    sync LSP is upstream in lsp-bridge (sync subprocess entry
    //    point), and `Handle::block_on(inside spawn_blocking)` is
    //    the anti-pattern this whole initiative was designed to
    //    avoid. What we *can* do from here is race each LSP await
    //    against the cancel token — a shutdown that lands mid-scan
    //    aborts the LSP round-trip promptly instead of waiting for
    //    the child to answer. The `tokio::select!` below does that.
    let file_refs: Vec<ReferenceLocation> = {
        let mut lsp = lsp_mux.lock().await;
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                // Cancellation lands between the lock acquire and
                // the LSP round-trip. Drop the lock and bail out
                // before any graph mutation.
                return Ok(FileScanResult {
                    nodes,
                    edges,
                    external_references,
                    static_refs: vec![],
                    pattern_refs: vec![],
                });
            }
            refs = lsp.get_references(&path, 0, 0) => refs.unwrap_or_default(),
        }
    };

    // Collect (node_id, reference) tuples for deferred resolution
    for r in &file_refs {
        external_references.push((file_id.clone(), r.clone()));
    }

    // 4. Recursive symbols (no more per-symbol lock acquisition).
    //    Same cancellation race as the references call above.
    let symbols_result = {
        let mut lsp = lsp_mux.lock().await;
        let ns = crate::schema::RepoNamespace::for_test();
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Ok(FileScanResult {
                    nodes,
                    edges,
                    external_references,
                    static_refs: vec![],
                    pattern_refs: vec![],
                });
            }
            result = lsp.get_document_symbols_hierarchical(
                &path,
                &workspace,
                &ns,
            ) => result,
        }
    };

    match symbols_result {
        Ok(symbols) => {
            if symbols.is_empty() {
                // LSP returned nothing usable (e.g. cold-start, partial parse).
                // Fall back to tree-sitter so the graph isn't empty.
                add_tree_sitter_definitions(
                    &path,
                    ScanContext {
                        graph_key: &relative_path,
                        nodes: &mut nodes,
                        edges: &mut edges,
                        file_id: &file_id,
                        lsp_sync,
                        git_sync,
                        commit_hash: commit_hash.clone(),
                        namespace,
                    },
                    cancel.clone(),
                )
                .await;
            } else {
                for symbol in symbols {
                    process_symbol_recursive_enriched(
                        &mut nodes,
                        &mut edges,
                        &file_id,
                        symbol,
                        lsp_sync,
                        git_sync,
                        commit_hash.clone(),
                    )
                    .await;
                }
            }
        }
        Err(e) => {
            debug!("No LSP symbols for {:?}: {}", path, e);
            // LSP unavailable (binary missing, language unsupported, etc.).
            // Fall back to tree-sitter so `find Function` etc. still works.
            add_tree_sitter_definitions(
                &path,
                ScanContext {
                    graph_key: &relative_path,
                    nodes: &mut nodes,
                    edges: &mut edges,
                    file_id: &file_id,
                    lsp_sync,
                    git_sync,
                    commit_hash: commit_hash.clone(),
                    namespace,
                },
                cancel.clone(),
            )
            .await;
        }
    }

    // Tree-sitter static analysis: extract call, type-usage refs,
    // string literals, and definitions in one offthread call so
    // we amortize the spawn_blocking round-trip. The closure
    // captures only `Path` and `&str` (both Send + 'static-friendly);
    // the result is the same three vectors the inline version
    // produced, just produced off the async runtime.
    let (static_refs, pattern_refs) = if let Ok(content) = tokio::fs::read_to_string(&path).await {
        // Attribute labels, merged onto whatever produced the nodes.
        //
        // The LSP reports names, kinds and ranges but never attributes,
        // so an LSP-indexed `#[test]` function arrives unlabelled and
        // dead-code detection cannot tell it from production code —
        // observed live, ten `#[test]` functions in a top-20 "dead"
        // list. Tree-sitter reads the attribute reliably and we are
        // already parsing this file for refs, so take the labels from
        // there regardless of which path built the nodes.
        //
        // One offthread call covers extract_definitions (for the
        // attribute labels), extract_refs (for static_refs), and
        // extract_strings (for pattern_refs). A cancellation in
        // any of them short-circuits the whole batch.
        // `extract_tree_sitter_file` cannot fail (it just walks the
        // AST), so the inner `Result` is always `Ok`. We still need
        // to return `Result` from the offthread closure (the
        // helper's contract), but the `??` after `await` flattens
        // both layers — outer for cancel, inner for the closure's
        // never-actual error.
        let ts = match offthread(cancel.clone(), move || {
            Ok::<TreeSitterFile, LainError>(extract_tree_sitter_file(&path, &content))
        })
        .await
        {
            Ok(t) => t,
            Err(LainError::Cancelled) => {
                return Ok(FileScanResult {
                    nodes,
                    edges,
                    external_references,
                    static_refs: vec![],
                    pattern_refs: vec![],
                });
            }
            Err(e) => return Err(e),
        };
        apply_attribute_labels(&ts.defs, &mut nodes);
        let path_str = relative_path.clone();
        let static_refs: Vec<StaticFileRef> = ts
            .static_refs
            .into_iter()
            .map(|r| StaticFileRef {
                file_path: path_str.clone(),
                source_line: r.source_line,
                target_name: r.target_name,
                edge_type: r.edge_type,
                foreign_receiver: r.foreign_receiver,
                self_receiver: r.self_receiver,
                qualifier: r.qualifier.clone(),
            })
            .collect();
        let pattern_refs: Vec<PatternRef> = ts
            .pattern_refs
            .into_iter()
            .map(|r| PatternRef {
                file_path: path_str.clone(),
                source_line: r.source_line,
                value: r.value,
            })
            .collect();
        (static_refs, pattern_refs)
    } else {
        (vec![], vec![])
    };

    Ok(FileScanResult {
        nodes,
        edges,
        external_references,
        static_refs,
        pattern_refs,
    })
}

/// Scan multiple files in a single task (batch processing for reduced task overhead)
#[allow(clippy::too_many_arguments)]
pub async fn scan_file_batch(
    paths: Vec<PathBuf>,
    workspace: PathBuf,
    lsp_mux: Arc<AsyncMutex<LspMultiplexer>>,
    lsp_sync: i64,
    git_sync: i64,
    commit_hash: String,
    namespace: &crate::schema::RepoNamespace,
    cancel: CancellationToken,
) -> Vec<Result<FileScanResult, LainError>> {
    let mut results = Vec::with_capacity(paths.len());
    for path in paths {
        let result = scan_file_structure(
            path,
            workspace.clone(),
            Arc::clone(&lsp_mux),
            lsp_sync,
            git_sync,
            commit_hash.clone(),
            namespace,
            cancel.clone(),
        )
        .await;
        results.push(result);
    }
    results
}

/// Merge tree-sitter attribute labels onto already-built nodes.
///
/// Matched on name plus start line where both are known, falling back
/// to name alone — the LSP's range starts at the doc comment or
/// attribute while tree-sitter's starts at the definition, so the two
/// rarely agree exactly. A node that already carries a label keeps it.
///
/// `defs` is pre-computed by the caller (see
/// [`extract_tree_sitter_file`]) so this function stays sync and
/// trivially callable from tests. The offthread boundary lives at
/// the caller side where the cancel token can be observed.
fn apply_attribute_labels(defs: &[crate::treesitter::SymbolDef], nodes: &mut [GraphNode]) {
    if defs.is_empty() {
        return;
    }
    for node in nodes.iter_mut() {
        if node.label.is_some() {
            continue;
        }
        // Prefer a definition whose span contains the node's start.
        let hit =
            defs.iter()
                .filter(|d| d.name == node.name)
                .min_by_key(|d| match node.line_start {
                    Some(l) => (d.line_start as i64 - l as i64).abs(),
                    None => 0,
                });
        if let Some(def) = hit {
            if def.is_deprecated {
                node.is_deprecated = true;
                node.label = Some("deprecated".to_string());
            } else if let Some(chosen) = def
                .labels
                .iter()
                // `test` wins over whatever else is on the definition:
                // `#[tokio::test]` yields ["tokio", "test"], and taking
                // the first label would file it under "tokio" and lose
                // the only fact any consumer cares about.
                .find(|l| l.as_str() == "test")
                .or_else(|| def.labels.first())
            {
                node.label = Some(chosen.clone());
            }
        }
    }
}

/// Does this symbol name a unit-test container?
///
/// The LSP hands back names, kinds and ranges — never attributes — so a
/// `#[test]` function indexed through the LSP path arrives unlabelled,
/// while the same function indexed through the tree-sitter fallback
/// carries `label = "test"`. That asymmetry is why dead-code reporting
/// had to guess from file and function names. Rust's `mod tests`
/// convention is recoverable from the symbol hierarchy, so propagate it
/// and let consumers read one honest label.
fn is_test_container(node: &GraphNode) -> bool {
    matches!(node.node_type, NodeType::Module | NodeType::Namespace)
        && (node.name == "tests" || node.name == "test")
}

pub async fn process_symbol_recursive_enriched(
    nodes: &mut Vec<GraphNode>,
    edges: &mut Vec<GraphEdge>,
    parent_id: &str,
    symbol: HierarchicalSymbol,
    lsp_sync: i64,
    git_sync: i64,
    commit_hash: String,
) {
    process_symbol_recursive_inner(
        nodes,
        edges,
        parent_id,
        symbol,
        lsp_sync,
        git_sync,
        commit_hash,
        false,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
#[async_recursion::async_recursion]
async fn process_symbol_recursive_inner(
    nodes: &mut Vec<GraphNode>,
    edges: &mut Vec<GraphEdge>,
    parent_id: &str,
    symbol: HierarchicalSymbol,
    lsp_sync: i64,
    git_sync: i64,
    commit_hash: String,
    inside_test_container: bool,
    container: Option<String>,
) {
    let mut node = symbol.node;
    node.container = container;
    node.last_lsp_sync = Some(lsp_sync);
    node.last_git_sync = Some(git_sync);
    node.commit_hash = Some(commit_hash.clone());
    let in_tests = inside_test_container || is_test_container(&node);
    if in_tests && node.label.is_none() {
        node.label = Some("test".to_string());
    }

    let node_id = node.id.clone();

    // NOTE: per-symbol reference matching deferred to resolve phase below
    // file_refs filtering happens there via (source_id, ref_loc) tuples

    nodes.push(node);
    edges.push(GraphEdge::new(
        EdgeType::Contains,
        parent_id.to_string(),
        node_id.clone(),
    ));

    // A type's members belong to it; members of a function (closures,
    // locals) keep the function's own container.
    let child_container = if matches!(
        nodes.last().map(|n| &n.node_type),
        Some(
            NodeType::Class
                | NodeType::Struct
                | NodeType::Interface
                | NodeType::Trait
                | NodeType::Enum
        )
    ) {
        nodes.last().map(|n| n.name.clone())
    } else {
        nodes.last().and_then(|n| n.container.clone())
    };
    for child in symbol.children {
        process_symbol_recursive_inner(
            nodes,
            edges,
            &node_id,
            child,
            lsp_sync,
            git_sync,
            commit_hash.clone(),
            in_tests,
            child_container.clone(),
        )
        .await;
    }
}

/// Tree-sitter fallback: when LSP is unavailable, parse the source directly
/// and create Function/Struct/Trait/Enum/Class nodes with line ranges so that
/// `get_node_at_location(file, line)` can resolve tree-sitter refs back to them.
struct ScanContext<'a> {
    graph_key: &'a str,
    nodes: &'a mut Vec<GraphNode>,
    edges: &'a mut Vec<GraphEdge>,
    file_id: &'a str,
    lsp_sync: i64,
    git_sync: i64,
    commit_hash: String,
    namespace: &'a crate::schema::RepoNamespace,
}

async fn add_tree_sitter_definitions(
    path: &Path,
    context: ScanContext<'_>,
    cancel: CancellationToken,
) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    // Run the tree-sitter extraction on the blocking pool; the
    // graph mutation below happens back on the async runtime where
    // the GraphDatabase's tokio mutex lives. Clone the path so the
    // offthread closure (which requires `Send + 'static`) owns its
    // own PathBuf.
    let path_buf = path.to_path_buf();
    let defs = match offthread(cancel, move || {
        Ok::<Vec<crate::treesitter::SymbolDef>, LainError>(crate::treesitter::extract_definitions(
            &path_buf, &content,
        ))
    })
    .await
    {
        Ok(d) => d,
        Err(_) => return,
    };
    for def in defs {
        let mut node = GraphNode::new_in(
            def.kind,
            def.name.clone(),
            context.graph_key.to_string(),
            context.namespace,
        )
        .with_location_in(def.line_start, def.line_end, context.namespace);
        node.last_lsp_sync = Some(context.lsp_sync);
        node.last_git_sync = Some(context.git_sync);
        node.commit_hash = Some(context.commit_hash.clone());
        node.is_deprecated = def.is_deprecated;
        node.container = def.container.clone();
        // Populate `label` so `find ... | filter label X` works.
        // `is_deprecated` is exposed as the "deprecated" label so users can
        // query with the same syntax docs advertise.
        if def.is_deprecated {
            node.label = Some("deprecated".to_string());
        } else if let Some(first) = def.labels.first() {
            node.label = Some(first.clone());
        }
        let node_id = node.id.clone();
        context.nodes.push(node);
        context.edges.push(GraphEdge::new(
            EdgeType::Contains,
            context.file_id.to_string(),
            node_id,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::Mutex as AsyncMutex;

    /// When LSP is unavailable (no rust-analyzer on PATH, etc.), the scanner must
    /// still produce Function/Struct/etc. nodes — otherwise `find Function`
    /// returns 0 and every downstream tool is empty.
    #[tokio::test]
    async fn scan_produces_symbol_nodes_without_lsp() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("lib.rs");
        std::fs::write(&file, "pub fn hello() {}\npub struct Calc { pub v: i32 }\n")
            .expect("write");

        let lsp = Arc::new(AsyncMutex::new(
            LspMultiplexer::new(tmp.path(), &crate::tuning::RuntimeConfig::default())
                .expect("lsp mux"),
        ));
        // These tests verify the tree-sitter fallback, not a real LSP server.
        // Mark rust-analyzer unavailable so no child process is spawned; the
        // lsp-bridge crate's LspProcess::Drop can hang when cleaning up a
        // defunct or unresponsive LSP process.
        lsp.lock().await.mark_unavailable("rust-analyzer");

        let result = scan_file_structure(
            file,
            tmp.path().to_path_buf(),
            lsp,
            0,
            0,
            "abc".to_string(),
            &crate::schema::RepoNamespace::for_test(),
            CancellationToken::new(),
        )
        .await
        .expect("scan ok");

        let has_function = result
            .nodes
            .iter()
            .any(|n| matches!(n.node_type, NodeType::Function) && n.name == "hello");
        assert!(
            has_function,
            "scan should produce Function node for 'hello' even when LSP is unavailable; got nodes: {:?}",
            result.nodes.iter().map(|n| (&n.node_type, &n.name)).collect::<Vec<_>>()
        );

        let calc = result
            .nodes
            .iter()
            .find(|n| matches!(n.node_type, NodeType::Struct) && n.name == "Calc");
        assert!(
            calc.is_some(),
            "scan should produce Struct node for 'Calc' even when LSP is unavailable"
        );

        // Symbol nodes must carry line ranges so get_node_at_location can find
        // them as the source of static tree-sitter references.
        let hello = result
            .nodes
            .iter()
            .find(|n| matches!(n.node_type, NodeType::Function) && n.name == "hello")
            .unwrap();
        assert!(
            hello.line_start.is_some() && hello.line_end.is_some(),
            "Function node must have line_start/line_end populated, got: {:?}",
            (hello.line_start, hello.line_end)
        );

        // Sanity: File node should still be there.
        assert!(result
            .nodes
            .iter()
            .any(|n| matches!(n.node_type, NodeType::File)));
    }

    #[tokio::test]
    async fn scan_attaches_symbol_to_file_via_contains_edge() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file: PathBuf = tmp.path().join("lib.rs");
        std::fs::write(&file, "pub fn hello() {}\n").expect("write");

        let lsp = Arc::new(AsyncMutex::new(
            LspMultiplexer::new(tmp.path(), &crate::tuning::RuntimeConfig::default())
                .expect("lsp mux"),
        ));
        // These tests verify the tree-sitter fallback, not a real LSP server.
        // Mark rust-analyzer unavailable so no child process is spawned; the
        // lsp-bridge crate's LspProcess::Drop can hang when cleaning up a
        // defunct or unresponsive LSP process.
        lsp.lock().await.mark_unavailable("rust-analyzer");

        let result = scan_file_structure(
            file,
            tmp.path().to_path_buf(),
            lsp,
            0,
            0,
            "abc".to_string(),
            &crate::schema::RepoNamespace::for_test(),
            CancellationToken::new(),
        )
        .await
        .expect("scan ok");

        let file_id = result
            .nodes
            .iter()
            .find(|n| matches!(n.node_type, NodeType::File))
            .map(|n| n.id.clone())
            .expect("file node");

        let hello_id = result
            .nodes
            .iter()
            .find(|n| matches!(n.node_type, NodeType::Function) && n.name == "hello")
            .map(|n| n.id.clone())
            .expect("hello node");

        let attached = result.edges.iter().any(|e| {
            matches!(e.edge_type, EdgeType::Contains)
                && e.source_id == file_id
                && e.target_id == hello_id
        });
        assert!(
            attached,
            "File -> Function Contains edge should exist; edges: {:?}",
            result
                .edges
                .iter()
                .map(|e| (&e.edge_type, &e.source_id, &e.target_id))
                .collect::<Vec<_>>()
        );
    }
}

#[cfg(test)]
mod attribute_label_tests {
    use super::*;

    /// The LSP never reports attributes, so an LSP-indexed `#[test]`
    /// function arrives unlabelled and dead-code detection cannot tell
    /// it from production code. Ten such functions showed up in a
    /// top-20 "dead code" list on this very repo.
    #[test]
    fn test_attribute_is_merged_onto_an_unlabelled_node() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("thing.rs");
        let src = "pub fn prod() -> u32 { 1 }\n\
                   #[cfg(test)]\n\
                   mod tests {\n\
                   #[test]\n\
                   fn checks_a_thing() {}\n\
                   #[tokio::test]\n\
                   async fn checks_async() {}\n\
                   }\n";
        std::fs::write(&f, src).unwrap();

        // Pre-compute defs the way the production path does — via
        // the tree-sitter extractor (the offthread wrapper is
        // exercised in `tests/cancellation_token.rs`; here we just
        // call the sync helper directly).
        let defs = crate::treesitter::extract_definitions(&f, src);

        // Nodes as the LSP would hand them over: no labels at all.
        let mut nodes = vec![
            GraphNode::new(NodeType::Function, "prod".into(), "thing.rs".into()),
            GraphNode::new(
                NodeType::Function,
                "checks_a_thing".into(),
                "thing.rs".into(),
            ),
            GraphNode::new(NodeType::Function, "checks_async".into(), "thing.rs".into()),
        ];
        apply_attribute_labels(&defs, &mut nodes);

        let label = |n: &str| nodes.iter().find(|x| x.name == n).unwrap().label.clone();
        assert_eq!(label("checks_a_thing").as_deref(), Some("test"));
        assert_eq!(
            label("checks_async").as_deref(),
            Some("test"),
            "#[tokio::test] must file as `test`, not `tokio`"
        );
        assert_eq!(label("prod"), None, "production code stays unlabelled");
    }

    #[test]
    fn an_existing_label_is_not_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("thing.rs");
        let src = "#[test]\nfn t() {}\n";
        std::fs::write(&f, src).unwrap();
        let defs = crate::treesitter::extract_definitions(&f, src);
        let mut nodes = vec![GraphNode::new(
            NodeType::Function,
            "t".into(),
            "thing.rs".into(),
        )];
        nodes[0].label = Some("preset".into());
        apply_attribute_labels(&defs, &mut nodes);
        assert_eq!(nodes[0].label.as_deref(), Some("preset"));
    }
}

#[cfg(test)]
mod lsp_cancel_tests {
    //! AGENT_UX_ROADMAP.md M4 follow-up: the LSP subprocess awaits
    //! inside `scan_file_structure` are raced against the cancel
    //! token via `tokio::select!`. When the token fires mid-scan
    //! the LSP round-trip is abandoned promptly (rather than waiting
    //! for the child to answer) and the per-file result carries
    //! empty `static_refs` / `pattern_refs` / `external_references`
    //! — a clean "cancelled before LSP" signal.
    //!
    //! We can't simulate a hung LSP round-trip from inside the
    //! `current_thread` test runtime — `LspMultiplexer` runs the
    //! LSP child in a real subprocess. Instead we exercise the
    //! cancel-arm of the `tokio::select!` by pre-cancelling the
    //! token: the LSP `await` is never awaited, so the cancel
    //! branch always wins. That branch's contract (return a
    //! `FileScanResult` with empty refs and no error) is what we
    //! pin here. The "LSP round-trip is the loser" arm is the
    //! production path; the test confirms the cancel arm fires
    //! when the token is set up-front.
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex as AsyncMutex;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn scan_returns_empty_refs_when_cancel_pre_cancels_lsp() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("lib.rs");
        std::fs::write(&file, "pub fn hello() {}\n").expect("write");

        let lsp = Arc::new(AsyncMutex::new(
            LspMultiplexer::new(tmp.path(), &crate::tuning::RuntimeConfig::default())
                .expect("lsp mux"),
        ));
        // Mark rust-analyzer unavailable — same pattern as the
        // pre-cancel tests above; the LSP fallback path is what
        // we'd otherwise exercise, but here we pre-cancel the
        // token so the LSP await never wins the race.
        lsp.lock().await.mark_unavailable("rust-analyzer");

        let cancel = CancellationToken::new();
        cancel.cancel();

        let result = scan_file_structure(
            file,
            tmp.path().to_path_buf(),
            lsp,
            0,
            0,
            "abc".to_string(),
            &crate::schema::RepoNamespace::for_test(),
            cancel,
        )
        .await
        .expect("scan returns Ok(empty refs) on cancel");

        // Pre-cancel short-circuits before any LSP round-trip
        // and before the tree-sitter extract phase. The
        // `FileScanResult` carries the namespace/file nodes that
        // were built before the LSP step, but the LSP-derived
        // vectors (`external_references`, `static_refs`,
        // `pattern_refs`) are empty.
        assert!(result.external_references.is_empty());
        assert!(result.static_refs.is_empty());
        assert!(result.pattern_refs.is_empty());
        // The File and Namespace/Module nodes that were built
        // before the LSP step still appear.
        assert!(result
            .nodes
            .iter()
            .any(|n| matches!(n.node_type, NodeType::File)));
    }
}
