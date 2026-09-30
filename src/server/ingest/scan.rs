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
    pub external_references: Vec<(String, ReferenceLocation)>, // (callee_id, reference_use_site)
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

/// B4 — per-scan LSP response cache. Shared across the files in a
/// single `scan_file_batch` so that two files in the same module
/// that resolve to the same LSP path (e.g. a `.c` and its `.h`
/// header) deduplicate the round trip. Keyed on `(path,
/// content_hash)` so a write to either side of the pair invalidates
/// the entry; cleared between passes (the cache lives inside
/// `scan_file_batch` and drops when that function returns).
///
/// `parking_lot::Mutex` because every LSP call inside a single file's
/// scan briefly acquires it. The contention is bounded by the
/// per-file serial scan loop, so a separate coarse lock is fine.
type ByContentCache = parking_lot::Mutex<
    std::collections::HashMap<(PathBuf, [u8; 32]), Vec<crate::lsp::HierarchicalSymbol>>,
>;

#[derive(Default)]
pub struct LspScanCache {
    by_content: ByContentCache,
}

impl LspScanCache {
    /// Look up a cached `HierarchicalSymbol` set for `(path, content_hash)`.
    /// Returns `Some(clone)` on hit, `None` on miss. The caller is
    /// expected to compute the response via LSP, then call
    /// [`Self::put`] with the same key.
    pub fn get(
        &self,
        path: &Path,
        content_hash: [u8; 32],
    ) -> Option<Vec<crate::lsp::HierarchicalSymbol>> {
        self.by_content
            .lock()
            .get(&(path.to_path_buf(), content_hash))
            .cloned()
    }

    /// Insert a freshly-computed `HierarchicalSymbol` set keyed on
    /// `(path, content_hash)`. A later call with the same key
    /// serves from the cache without an LSP round trip.
    pub fn put(
        &self,
        path: &Path,
        content_hash: [u8; 32],
        symbols: &[crate::lsp::HierarchicalSymbol],
    ) {
        self.by_content
            .lock()
            .insert((path.to_path_buf(), content_hash), symbols.to_vec());
    }
}

fn extract_tree_sitter_file(path: &Path, content: &str) -> TreeSitterFile {
    TreeSitterFile {
        defs: crate::treesitter::extract_definitions(path, content),
        static_refs: crate::treesitter::extract_refs(path, content),
        pattern_refs: crate::treesitter::extract_strings(path, content),
    }
}

/// Pure structural scan without side effects (Map)
///
/// `lsp_mux` is `None` in snapshot mode (`§8.2`): the per-repo
/// DB is built from tree-sitter symbols + static resolve + sensors
/// alone, with no LSP round trip. The LSP-fallback code path that
/// runs tree-sitter on empty results is the same one snapshot
/// mode uses; the only difference is the absence of an LSP
/// multiplexer to dial in the first place.
#[allow(clippy::too_many_arguments)]
pub async fn scan_file_structure(
    path: PathBuf,
    workspace: PathBuf,
    lsp_mux: Option<Arc<AsyncMutex<LspMultiplexer>>>,
    lsp_sync: i64,
    git_sync: i64,
    commit_hash: String,
    namespace: &crate::schema::RepoNamespace,
    cancel: CancellationToken,
    // B4 — optional per-scan LSP response cache. `None` for the
    // federation's `index_one_repo` path (the cache was added
    // after that path went live and refactoring it is out of
    // scope for PR-B); `Some(&cache)` for `build_core_memory`,
    // which constructs a fresh `LspScanCache` per call and drops
    // it when the batch returns.
    lsp_cache: Option<&LspScanCache>,
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

    // 4. Recursive symbols (no more per-symbol lock acquisition).
    //
    //    B4 — when a per-scan cache is provided and the file's
    //    content hash matches a cached entry, skip the LSP round
    //    trip entirely. The cache is shared across files in this
    //    batch, so a `.h` and `.c` that both point at the same
    //    header get one round trip, not two. The cache lives
    //    inside `scan_file_batch` and drops at the end of the
    //    pass.
    //
    //    Snapshot mode (`§8.2`) skips the LSP step entirely: the
    //    indexer never constructs an `LspMultiplexer`, so `lsp_mux`
    //    is `None`, and the call below is short-circuited to a
    //    tree-sitter-only walk that mirrors the LSP-fallback code
    //    path that runs tree-sitter on empty results.
    let symbols_result = {
        // Compute the file's content hash once. `std::fs::read` is
        // a single syscall; the LSP round trip below is orders of
        // magnitude more expensive, so the hash is free.
        let content_hash: Option<[u8; 32]> = std::fs::read(&path)
            .ok()
            .map(|bytes| *blake3::hash(&bytes).as_bytes());
        if let (Some(cache), Some(hash)) = (lsp_cache, content_hash) {
            if let Some(cached_symbols) = cache.get(&path, hash) {
                // Cache hit. Synthesise the same shape as the LSP
                // round-trip arm so the rest of the function can
                // treat it as if the LSP returned the symbols.
                let _ = hash;
                Ok(cached_symbols)
            } else if let Some(lsp_mux) = lsp_mux.as_ref() {
                let mut lsp = lsp_mux.lock().await;
                let ns = crate::schema::RepoNamespace::for_test();
                let result = tokio::select! {
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
                };
                if let (Ok(ref syms), true) =
                    (&result, !result.as_ref().map(Vec::is_empty).unwrap_or(true))
                {
                    cache.put(&path, hash, syms);
                }
                result
            } else {
                // Snapshot mode + cache hit miss: there is no LSP
                // server to round-trip and the cache did not have
                // this file. Tree-sitter is the fallback by design.
                Ok(Vec::new())
            }
        } else if let Some(lsp_mux) = lsp_mux.as_ref() {
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
        } else {
            // Snapshot mode + no cache: skip LSP, fall through to
            // tree-sitter below.
            Ok(Vec::new())
        }
    };

    let mut selection_positions: Vec<(String, u32, u32)> = Vec::new();

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
                    collect_selection_positions(&mut selection_positions, &symbol);
                    process_symbol_recursive_enriched(
                        &mut nodes,
                        &mut edges,
                        &file_id,
                        symbol,
                        &workspace,
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

    // 5. Per-symbol LSP references — each symbol's selection position
    //    identifies the declaration whose references we want. Pair each
    //    returned reference with the symbol's node id so the resolve
    //    phase can build caller -> callee edges.
    //
    //    Snapshot mode (`§8.2`) skips this step: `lsp_mux` is `None`,
    //    so no LSP round trips and no cross-process references are
    //    added to `external_references`. The resolve phase downstream
    //    runs with whatever tree-sitter produced; that is exactly
    //    what `§8.2` mandates (no cross-repo resolver, no
    //    LSP-derived cross-process `Calls`).
    if let Some(lsp_mux) = lsp_mux.as_ref() {
        for (callee_id, sel_line, sel_col) in selection_positions {
            let refs = {
                let mut lsp = lsp_mux.lock().await;
                lsp.get_references(&path, sel_line, sel_col)
                    .await
                    .unwrap_or_default()
            };
            for r in refs {
                external_references.push((callee_id.clone(), r));
            }
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
        apply_tree_sitter_containers(&ts.defs, &mut nodes);
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
///
/// `lsp_mux` is `Option<…>` so snapshot mode (`§8.2`) can pass
/// `None` and skip LSP entirely. The live federation path always
/// passes `Some(lsp_mux)`.
#[allow(clippy::too_many_arguments)]
pub async fn scan_file_batch(
    paths: Vec<PathBuf>,
    workspace: PathBuf,
    lsp_mux: Option<Arc<AsyncMutex<LspMultiplexer>>>,
    lsp_sync: i64,
    git_sync: i64,
    commit_hash: String,
    namespace: &crate::schema::RepoNamespace,
    cancel: CancellationToken,
    // B4 — optional per-scan LSP response cache. Created by the
    // caller (one fresh cache per `scan_file_batch` call) and
    // dropped when the batch returns. `None` keeps the legacy
    // behaviour (one LSP round trip per file in the batch).
    lsp_cache: Option<&LspScanCache>,
) -> Vec<Result<FileScanResult, LainError>> {
    let mut results = Vec::with_capacity(paths.len());
    for path in paths {
        let result = scan_file_structure(
            path,
            workspace.clone(),
            lsp_mux.as_ref().map(Arc::clone),
            lsp_sync,
            git_sync,
            commit_hash.clone(),
            namespace,
            cancel.clone(),
            lsp_cache,
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
/// The type each definition belongs to, taken from tree-sitter whatever
/// built the nodes. The LSP's symbol tree does not always say: rust-analyzer
/// reports `impl Lexer<'src>` as a non-type symbol, so its methods arrived
/// with no container, read as free functions, and every `x.method()` call
/// from the same file was dropped as "a free function through a receiver".
fn apply_tree_sitter_containers(defs: &[crate::treesitter::SymbolDef], nodes: &mut [GraphNode]) {
    for node in nodes.iter_mut() {
        let Some(line) = node.line_start else {
            continue;
        };
        // LSP ranges can start at a doc comment or attribute above the
        // name, so match the nearest same-named definition a few lines on.
        let hit = defs
            .iter()
            .filter(|d| d.name == node.name && d.container.is_some())
            .map(|d| ((d.line_start as i64 - line as i64).abs(), d))
            .filter(|(dist, _)| *dist <= 8)
            .min_by_key(|(dist, _)| *dist);
        if let Some((_, def)) = hit {
            node.container = def.container.clone();
        }
    }
}

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

/// When the LSP returns an empty `detail`, derive a signature from
/// the symbol's source body so cross-repo matching has a signal to
/// work with.
///
/// Terminator by language:
/// - Rust / TypeScript / JavaScript / Go: the first `{` (block open). Rust
///   parameter lists contain `:` (e.g. `(x: u32)`); a single-character
///   terminator `:` would cut those off, so Rust/TS/JS/Go look for `{` only.
/// - Python / Ruby: the first `:` (header end).
///
/// `line_start` is zero-based (tree-sitter rows and the LSP
/// `range.start.line` are both zero-based). Multi-line signatures — most
/// often Rust `where` clauses — are joined up to and including the line
/// that carries the terminator.
/// Does this signature text carry at least one token the federation
/// matcher can score on? Mirrors `matching`'s tokenisation: split on
/// non-alphanumerics, drop stop words, and require one survivor.
fn signature_names_something(sig: &str) -> bool {
    sig.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .any(|w| !crate::server::federation::matching::SIGNATURE_STOP_WORDS.contains(&w.as_str()))
}

pub fn derive_signature(symbol: &HierarchicalSymbol, workspace: &Path) -> Option<String> {
    let node = &symbol.node;
    // A non-empty LSP `detail` is not automatically a *usable*
    // signature: language servers report fragments like "()", "->",
    // or "pub fn" that carry no non-stop tokens at all, and a matcher
    // fed those refuses to score (correctly) — which silently drops
    // cross-repo peer edges. Fall through to synthesis unless the
    // detail would actually give the matcher something to compare.
    if let Some(sig) = &node.signature {
        if signature_names_something(sig) {
            return Some(sig.clone());
        }
    }
    let line_start = node.line_start?;
    let path = if Path::new(&node.path).is_absolute() {
        PathBuf::from(&node.path)
    } else {
        workspace.join(&node.path)
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "derive_signature: cannot read {} for {} ({e}); workspace={:?}; node will have no signature",
                path.display(),
                node.name,
                workspace
            );
            return None;
        }
    };
    let terminator = terminator_for_path(&node.path);
    let mut lines = content.lines().skip(line_start as usize);
    let head = match lines.next() {
        Some(h) => h,
        None => {
            tracing::warn!(
                "derive_signature: no line at {}:{} in {} ({} lines); node will have no signature",
                node.path,
                line_start,
                path.display(),
                content.lines().count()
            );
            return None;
        }
    };
    let head_trimmed = head.trim();
    if let Some(end) = head_trimmed.find(terminator) {
        return non_empty(head_trimmed[..end].trim());
    }
    let mut joined = head_trimmed.to_string();
    for next in lines {
        let trimmed = next.trim();
        joined.push(' ');
        joined.push_str(trimmed);
        if trimmed.contains(terminator) {
            break;
        }
    }
    let end = joined.find(terminator).unwrap_or(joined.len());
    non_empty(joined[..end].trim())
}

fn non_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn terminator_for_path(path: &str) -> char {
    let ext = path.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "py" | "pyi" | "rb" => ':',
        _ => '{',
    }
}

/// Walk a `HierarchicalSymbol` tree, recording each symbol's id and
/// selection position so the caller can run `get_references` against
/// the LSP using the symbol's own identifier position.
fn collect_selection_positions(out: &mut Vec<(String, u32, u32)>, symbol: &HierarchicalSymbol) {
    out.push((
        symbol.node.id.clone(),
        symbol.selection_line,
        symbol.selection_col,
    ));
    for child in &symbol.children {
        collect_selection_positions(out, child);
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

#[allow(clippy::too_many_arguments)]
pub async fn process_symbol_recursive_enriched(
    nodes: &mut Vec<GraphNode>,
    edges: &mut Vec<GraphEdge>,
    parent_id: &str,
    symbol: HierarchicalSymbol,
    workspace: &Path,
    lsp_sync: i64,
    git_sync: i64,
    commit_hash: String,
) {
    process_symbol_recursive_inner(
        nodes,
        edges,
        parent_id,
        symbol,
        workspace,
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
    workspace: &Path,
    lsp_sync: i64,
    git_sync: i64,
    commit_hash: String,
    inside_test_container: bool,
    container: Option<String>,
) {
    let derived_signature = derive_signature(&symbol, workspace);
    let mut node = symbol.node;
    node.container = container;
    if node.signature.as_deref().map(str::is_empty).unwrap_or(true) {
        if let Some(derived) = derived_signature {
            node.signature = Some(derived);
        }
    }
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
            workspace,
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
            Some(lsp),
            0,
            0,
            "abc".to_string(),
            &crate::schema::RepoNamespace::for_test(),
            CancellationToken::new(),
            None,
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
            Some(lsp),
            0,
            0,
            "abc".to_string(),
            &crate::schema::RepoNamespace::for_test(),
            CancellationToken::new(),
            None,
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

    fn make_symbol(
        name: &str,
        path: &str,
        line_start: Option<u32>,
        signature: Option<&str>,
    ) -> HierarchicalSymbol {
        let mut node = GraphNode::new(NodeType::Function, name.into(), path.into());
        node.line_start = line_start;
        node.signature = signature.map(|s| s.to_string());
        HierarchicalSymbol {
            node,
            selection_line: line_start.unwrap_or(0),
            selection_col: 0,
            children: Vec::new(),
        }
    }

    #[test]
    fn derive_signature_passthrough_when_lsp_provided() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn foo() {}\n").unwrap();
        let sym = make_symbol("foo", "lib.rs", Some(0), Some("pub fn foo()"));

        assert_eq!(
            derive_signature(&sym, dir.path()),
            Some("pub fn foo()".into())
        );
    }

    #[test]
    fn derive_signature_rust_function_with_empty_lsp_signature() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn foo(x: u32) -> Result<(), Error> {\n    todo!()\n}\n",
        )
        .unwrap();
        let sym = make_symbol("foo", "lib.rs", Some(0), None);

        let derived = derive_signature(&sym, dir.path()).unwrap();
        assert!(derived.starts_with("pub fn foo"), "got: {derived}");
        assert!(!derived.contains('{'), "must cut at the brace");
        assert!(
            derived.contains("Result<(), Error>"),
            "must preserve the return type with generics; got: {derived}"
        );
    }

    #[test]
    fn derive_signature_python_function_cuts_at_colon() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("foo.py"),
            "def foo(x: int) -> None:\n    pass\n",
        )
        .unwrap();
        let sym = make_symbol("foo", "foo.py", Some(0), None);

        let derived = derive_signature(&sym, dir.path()).unwrap();
        assert!(derived.starts_with("def foo"), "got: {derived}");
        assert!(!derived.contains(':'), "must cut at the colon");
    }

    #[test]
    fn derive_signature_returns_none_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let sym = make_symbol("foo", "src/lib.rs", Some(0), None);

        assert_eq!(derive_signature(&sym, dir.path()), None);
    }

    /// Codex contract `signature_synthesis_uses_actual_parser_coordinates`.
    /// A definition that follows a blank line or comment must still
    /// produce its own signature; the wave-1 `saturating_sub(1)` in
    /// `derive_signature` read the previous (blank) line, and the
    /// function came back with `signature = None`, so cross-repo
    /// matching had no signal to score on.
    #[test]
    fn signature_synthesis_uses_actual_parser_coordinates() {
        let dir = tempfile::tempdir().unwrap();
        let src = "// a leading doc comment\n\
                   \n\
                   pub fn foo(x: u32) -> Result<u32, Error> {\n    Ok(x)\n}\n";
        std::fs::write(dir.path().join("lib.rs"), src).unwrap();
        let sym = make_symbol("foo", "lib.rs", Some(2), None);

        let derived = derive_signature(&sym, dir.path())
            .expect("signature must synthesize when the def line is the zero-based start");
        assert!(
            derived.starts_with("pub fn foo"),
            "signature must come from the def line, not the prior comment/blank; got: {derived}"
        );
        assert!(
            derived.contains("Result<u32, Error>"),
            "Rust return type with generics must be preserved; got: {derived}"
        );
        assert!(
            !derived.contains('{'),
            "must cut at the brace; got: {derived}"
        );
    }

    /// Codex contract `signature_synthesis_uses_actual_parser_coordinates`,
    /// off-by-zero complement: the parser's row 0 is the file's first
    /// line, not the second.
    #[test]
    fn signature_synthesis_uses_zero_based_first_line() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn first() -> i32 { 0 }\npub fn second() -> i32 { 1 }\n",
        )
        .unwrap();
        let sym = make_symbol("first", "lib.rs", Some(0), None);

        let derived = derive_signature(&sym, dir.path())
            .expect("line_start=0 must synthesize from the very first line");
        assert!(derived.starts_with("pub fn first"), "got: {derived}");
    }

    /// Codex contract `signature_synthesis_uses_actual_parser_coordinates`,
    /// multi-line Rust signature (where clause) — the synthesized
    /// signature must include the `where` clause, not just the head.
    #[test]
    fn signature_synthesis_joins_rust_where_clause_lines() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "fn make<T>(v: T) -> Result<T, Error>\nwhere\n    T: Clone,\n{\n    Ok(v)\n}\n",
        )
        .unwrap();
        let sym = make_symbol("make", "lib.rs", Some(0), None);

        let derived =
            derive_signature(&sym, dir.path()).expect("multi-line Rust signature must synthesize");
        assert!(derived.starts_with("fn make"), "got: {derived}");
        assert!(
            derived.contains("where"),
            "where clause must be included; got: {derived}"
        );
        assert!(
            derived.contains("Clone"),
            "trait bound must be included; got: {derived}"
        );
        assert!(
            !derived.contains('{'),
            "must still cut at the brace; got: {derived}"
        );
    }

    /// The scanner must ask `get_references` for each symbol's
    /// selection position (line + column of the identifier), not
    /// `(0, 0)`. With the pre-fix code, the scanner asked once per
    /// file at `(0, 0)`, so the LSP returned nothing useful and the
    /// resolve phase had zero callees to link — every `Calls` edge
    /// silently disappeared.
    ///
    /// The fixture returns a single symbol `helper` at zero-based
    /// selection `(0, 8)` (the identifier column inside
    /// `pub fn helper`) and one canned reference at that position
    /// pointing at the use site inside `caller`. If the scanner
    /// passes any position other than `(0, 8)` to `get_references`,
    /// the override is not hit and the reference set comes back empty.
    #[tokio::test]
    async fn scanner_calls_get_references_at_each_symbols_selection_position() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("lib.rs");
        std::fs::write(&file, "pub fn helper() {}\npub fn caller() { helper(); }\n")
            .expect("write");

        let helper_path_str = "lib.rs".to_string();
        let mut helper_node = GraphNode::new(
            NodeType::Function,
            "helper".to_string(),
            helper_path_str.clone(),
        );
        helper_node.line_start = Some(0);
        helper_node.line_end = Some(0);
        let helper_symbol = HierarchicalSymbol {
            node: helper_node,
            selection_line: 0,
            selection_col: 8,
            children: Vec::new(),
        };

        let lsp = Arc::new(AsyncMutex::new(
            LspMultiplexer::new(tmp.path(), &crate::tuning::RuntimeConfig::default())
                .expect("lsp mux"),
        ));
        lsp.lock().await.mark_unavailable("rust-analyzer");
        {
            let mut guard = lsp.lock().await;
            let mut symbols = std::collections::HashMap::new();
            symbols.insert(file.clone(), vec![helper_symbol.clone()]);
            guard.set_test_document_symbols(symbols);
            let mut refs = std::collections::HashMap::new();
            refs.insert(
                (file.clone(), 0u32, 8u32),
                vec![ReferenceLocation {
                    path: file.clone(),
                    line: 1,
                    col: 18,
                    context: String::new(),
                }],
            );
            guard.set_test_references(refs);
        }

        let result = scan_file_structure(
            file.clone(),
            tmp.path().to_path_buf(),
            Some(lsp),
            0,
            0,
            "abc".to_string(),
            &crate::schema::RepoNamespace::for_test(),
            CancellationToken::new(),
            None,
        )
        .await
        .expect("scan ok");

        assert_eq!(
            result.external_references.len(),
            1,
            "scanner must emit exactly one external ref for the one canned reference; got {:?}",
            result
                .external_references
                .iter()
                .map(|(id, r)| (id, r.line, r.col))
                .collect::<Vec<_>>()
        );
        let (callee_id, ref_loc) = &result.external_references[0];
        assert_eq!(
            callee_id, &helper_symbol.node.id,
            "external_references must pair the ref with the callee (the symbol we asked about)"
        );
        assert_eq!(ref_loc.line, 1);
        assert_eq!(ref_loc.col, 18);
    }

    /// The scanner must pair each returned reference with the
    /// *callee* (the symbol whose selection position was asked
    /// about), not the file id. The Codex review reproduced
    /// `lsp_reference_location_becomes_caller_not_callee`: with the
    /// pre-fix code the `source_node_id` was always the file id and
    /// the resolve phase then emitted `file -> caller`, which the
    /// caller attribute label tests confirm is the wrong direction.
    #[tokio::test]
    async fn scanner_pairs_references_with_the_callee_symbol_id_not_the_file_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("lib.rs");
        std::fs::write(&file, "pub fn helper() {}\npub fn caller() { helper(); }\n")
            .expect("write");

        let helper_node = {
            let mut n = GraphNode::new(
                NodeType::Function,
                "helper".to_string(),
                "lib.rs".to_string(),
            );
            n.line_start = Some(0);
            n.line_end = Some(0);
            n
        };
        let helper_symbol = HierarchicalSymbol {
            node: helper_node.clone(),
            selection_line: 0,
            selection_col: 8,
            children: Vec::new(),
        };

        let lsp = Arc::new(AsyncMutex::new(
            LspMultiplexer::new(tmp.path(), &crate::tuning::RuntimeConfig::default())
                .expect("lsp mux"),
        ));
        lsp.lock().await.mark_unavailable("rust-analyzer");
        {
            let mut guard = lsp.lock().await;
            let mut symbols = std::collections::HashMap::new();
            symbols.insert(file.clone(), vec![helper_symbol.clone()]);
            guard.set_test_document_symbols(symbols);
            let mut refs = std::collections::HashMap::new();
            refs.insert(
                (file.clone(), 0u32, 8u32),
                vec![ReferenceLocation {
                    path: file.clone(),
                    line: 1,
                    col: 18,
                    context: String::new(),
                }],
            );
            guard.set_test_references(refs);
        }

        let result = scan_file_structure(
            file.clone(),
            tmp.path().to_path_buf(),
            Some(lsp),
            0,
            0,
            "abc".to_string(),
            &crate::schema::RepoNamespace::for_test(),
            CancellationToken::new(),
            None,
        )
        .await
        .expect("scan ok");

        let file_id = result
            .nodes
            .iter()
            .find(|n| matches!(n.node_type, NodeType::File))
            .expect("file node")
            .id
            .clone();

        let (paired_id, _) = &result.external_references[0];
        assert_ne!(
            *paired_id, file_id,
            "the ref must be paired with the callee, not the file; file_id={file_id}"
        );
        assert_eq!(
            *paired_id, helper_node.id,
            "the ref must be paired with the symbol we asked about (helper)"
        );
    }

    /// End-to-end through the deterministic fixture: one declaration
    /// (`helper`) with one use site inside `caller` produces a
    /// `caller -> helper` edge at the expected source position.
    ///
    /// The Codex review's contract
    /// `lsp_reference_location_becomes_caller_not_callee` reproduced
    /// this defect: with the pre-fix code the resolve phase emitted
    /// `helper -> caller` because the source/target interpretation was
    /// reversed and the call to `get_references` was at `(0, 0)`. With
    /// the fix, the scanner asks at the symbol's selection position and
    /// the resolver pairs the use site with the function containing
    /// it, so the edge is `caller -> helper`.
    #[tokio::test]
    async fn fixture_to_edge_pipeline_emits_caller_to_callee() {
        use crate::graph::GraphDatabase;
        use crate::server::ingest::resolve::resolve_call_edges;

        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("lib.rs");
        std::fs::write(&file, "pub fn helper() {}\npub fn caller() { helper(); }\n")
            .expect("write");

        let helper_node = {
            let mut n = GraphNode::new(
                NodeType::Function,
                "helper".to_string(),
                "lib.rs".to_string(),
            );
            n.line_start = Some(0);
            n.line_end = Some(0);
            n
        };
        let caller_node = {
            let mut n = GraphNode::new(
                NodeType::Function,
                "caller".to_string(),
                "lib.rs".to_string(),
            );
            n.line_start = Some(1);
            n.line_end = Some(1);
            n
        };
        let helper_symbol = HierarchicalSymbol {
            node: helper_node.clone(),
            selection_line: 0,
            selection_col: 8,
            children: Vec::new(),
        };

        let lsp = Arc::new(AsyncMutex::new(
            LspMultiplexer::new(tmp.path(), &crate::tuning::RuntimeConfig::default())
                .expect("lsp mux"),
        ));
        lsp.lock().await.mark_unavailable("rust-analyzer");
        {
            let mut guard = lsp.lock().await;
            let mut symbols = std::collections::HashMap::new();
            symbols.insert(file.clone(), vec![helper_symbol.clone()]);
            guard.set_test_document_symbols(symbols);
            let mut refs = std::collections::HashMap::new();
            refs.insert(
                (file.clone(), 0u32, 8u32),
                vec![ReferenceLocation {
                    path: file.clone(),
                    line: 1,
                    col: 18,
                    context: String::new(),
                }],
            );
            guard.set_test_references(refs);
        }

        let result = scan_file_structure(
            file.clone(),
            tmp.path().to_path_buf(),
            Some(lsp),
            0,
            0,
            "abc".to_string(),
            &crate::schema::RepoNamespace::for_test(),
            CancellationToken::new(),
            None,
        )
        .await
        .expect("scan ok");

        let db_tmp = tempfile::tempdir().expect("db tmp");
        let db_path = db_tmp.path().join("graph.bin");
        let db = GraphDatabase::new(&db_path).expect("graph db");
        let helper_id = helper_node.id.clone();
        let caller_id = caller_node.id.clone();
        db.upsert_node(helper_node).expect("upsert helper");
        db.upsert_node(caller_node).expect("upsert caller");

        let edges = resolve_call_edges(&db, tmp.path(), &result.external_references, None, None);
        assert_eq!(
            edges.len(),
            1,
            "the canned use site inside caller must produce exactly one Calls edge; got {:?}",
            edges
                .iter()
                .map(|e| (&e.source_id, &e.target_id))
                .collect::<Vec<_>>()
        );
        let edge = &edges[0];
        assert_eq!(edge.edge_type, EdgeType::Calls);
        assert_eq!(edge.source_id, caller_id, "source must be the caller");
        assert_eq!(
            edge.target_id, helper_id,
            "target must be the callee (helper)"
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
    fn lsp_nodes_take_their_container_from_tree_sitter() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("lexer.rs");
        let src = "struct Lexer;\nimpl<'a> Lexer<'a> {\n  /// doc\n  fn tokenize(self) {}\n}\nfn free() {}\n";
        std::fs::write(&f, src).unwrap();
        let defs = crate::treesitter::extract_definitions(&f, src);
        // As rust-analyzer hands them over: no container, the range
        // starting at the doc comment.
        let mut method = GraphNode::new(NodeType::Function, "tokenize".into(), "lexer.rs".into());
        method.line_start = Some(2);
        let mut free = GraphNode::new(NodeType::Function, "free".into(), "lexer.rs".into());
        free.line_start = Some(5);
        let mut nodes = vec![method, free];
        apply_tree_sitter_containers(&defs, &mut nodes);
        assert_eq!(nodes[0].container.as_deref(), Some("Lexer"));
        assert_eq!(nodes[1].container, None);
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
            Some(lsp),
            0,
            0,
            "abc".to_string(),
            &crate::schema::RepoNamespace::for_test(),
            cancel,
            None,
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

#[cfg(test)]
mod lsp_scan_cache_tests {
    //! B4 — per-scan LSP response cache invariants.

    use super::{scan_file_structure, LspScanCache};
    use crate::lsp::HierarchicalSymbol;
    use crate::schema::{GraphNode, NodeType};
    use std::path::Path;
    use std::sync::Arc;
    use tokio::sync::Mutex as AsyncMutex;
    use tokio_util::sync::CancellationToken;

    fn func_node(name: &str, path: &str) -> GraphNode {
        GraphNode::new(NodeType::Function, name.into(), path.into())
    }

    #[test]
    fn put_then_get_returns_the_same_symbols() {
        let cache = LspScanCache::default();
        let path = Path::new("src/lib.rs");
        let hash = [0x42u8; 32];
        let symbols = vec![HierarchicalSymbol {
            selection_line: 0,
            selection_col: 0,
            node: func_node("hello", "src/lib.rs"),
            children: vec![],
        }];
        cache.put(path, hash, &symbols);

        let cached = cache.get(path, hash).expect("hit");
        assert_eq!(cached.len(), 1);
        assert_eq!(cached[0].node.name, "hello");
    }

    #[test]
    fn cache_misses_on_a_different_path_or_hash() {
        let cache = LspScanCache::default();
        let path = Path::new("src/lib.rs");
        let hash_a = [0x01u8; 32];
        let hash_b = [0x02u8; 32];
        let symbols = vec![HierarchicalSymbol {
            selection_line: 0,
            selection_col: 0,
            node: func_node("hello", "src/lib.rs"),
            children: vec![],
        }];
        cache.put(path, hash_a, &symbols);

        assert!(cache.get(path, hash_b).is_none(), "different hash misses");
        assert!(
            cache.get(Path::new("src/other.rs"), hash_a).is_none(),
            "different path misses"
        );
    }

    /// End-to-end: a second `scan_file_structure` call on the same
    /// path + content (the LSP path is marked unavailable so the
    /// fallback runs and we still hit the cache code path that
    /// hashes the file) sees the cached `HierarchicalSymbol` set
    /// and skips the LSP round trip. The cache's put arm records
    /// only when the LSP returned a non-empty set, so the
    /// fallback's empty result is not cached — this test verifies
    /// the read arm rather than the write arm, which keeps the
    /// test independent of the LSP-fallback interaction.
    #[tokio::test]
    async fn scan_file_structure_uses_cache_when_provided() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("lib.rs");
        std::fs::write(&file, "pub fn hello() {}\n").expect("write");

        // Pre-populate the cache with a synthetic HierarchicalSymbol.
        // `scan_file_structure` will hash the file, look up
        // (path, hash), and on hit return Ok(cached_symbols) without
        // touching LSP. We then assert that the file node is still
        // present (built before the LSP step) and that the cached
        // symbols were used.
        let cache = LspScanCache::default();
        let bytes = std::fs::read(&file).expect("read");
        let hash: [u8; 32] = *blake3::hash(&bytes).as_bytes();
        let cached_symbols = vec![HierarchicalSymbol {
            selection_line: 0,
            selection_col: 0,
            node: GraphNode {
                id: "synthetic::cached".into(),
                ..func_node("cached_fn", "src/lib.rs")
            },
            children: vec![],
        }];
        cache.put(&file, hash, &cached_symbols);

        let lsp = Arc::new(AsyncMutex::new(
            crate::lsp::LspMultiplexer::new(tmp.path(), &crate::tuning::RuntimeConfig::default())
                .expect("lsp mux"),
        ));
        // Mark rust-analyzer unavailable so any actual LSP call would
        // error out — proving the cache hit bypassed the round trip.
        lsp.lock().await.mark_unavailable("rust-analyzer");

        let result = scan_file_structure(
            file,
            tmp.path().to_path_buf(),
            Some(lsp),
            0,
            0,
            "abc".to_string(),
            &crate::schema::RepoNamespace::for_test(),
            CancellationToken::new(),
            Some(&cache),
        )
        .await
        .expect("scan ok");

        // The synthetic cached node must surface as the symbol node.
        let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names.contains(&"cached_fn"),
            "cached symbol must come through; got {names:?}"
        );
    }
}
