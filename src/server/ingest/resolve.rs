//! Resolve phases shared by both ingestion pipelines.
//!
//! The single-workspace pipeline ([`crate::server::ingest::LainServer::build_core_memory`])
//! and the federation pipeline
//! ([`crate::server::ingest::ingestion::index_one_repo`]) used to
//! re-implement the same three resolve loops verbatim: external refs
//! into Calls edges, tree-sitter static refs into Calls/Uses edges,
//! and string-literal pattern refs into Pattern edges. The
//! federation path used tighter constants
//! ([`PatternLimits::FEDERATION`]) while the single-workspace path
//! used the defaults ([`PatternLimits::DEFAULT`]). All three
//! functions in this module are pure: `&self` only on the `db` they
//! mutate through the public `insert_edges_batch`/`upsert_edge` API.

use crate::federation::cross_repo::CrossRepoResolver;
use crate::federation::repo_id::RepoId;
use crate::graph::{graph_path, GraphDatabase};
use crate::schema::{is_type_level_target, EdgeType, GraphEdge};
use crate::lsp::ReferenceLocation;
use super::scan::{PatternRef, StaticFileRef};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Budget knobs for [`resolve_pattern_edges`]. The single-workspace
/// pipeline wants broader coverage; the federation pipeline wants a
/// tighter cap so two repos don't bloat the cross-boundary edge set.
#[derive(Clone, Copy)]
pub struct PatternLimits {
    pub max_files_per_value: usize,
    pub min_dirs: usize,
    pub max_edges: usize,
    pub edges_per_value: usize,
}

impl PatternLimits {
    /// Build limits from the workspace's tuning config.
    ///
    /// `TuningConfig::max_pattern_edges` shipped in `.lain/tuning.toml`
    /// with the same value the constants below hard-code, and nothing
    /// ever read it — raising the cap in config had no effect on the
    /// number of `Pattern` edges produced.
    pub fn from_tuning(tuning: &crate::tuning::TuningConfig, base: PatternLimits) -> PatternLimits {
        PatternLimits {
            max_edges: tuning.max_pattern_edges,
            ..base
        }
    }

    /// The single-workspace defaults — values lifted from the
    /// pre-extraction constants in `ingestion.rs`.
    pub const DEFAULT: PatternLimits = PatternLimits {
        max_files_per_value: 200,
        min_dirs: 2,
        max_edges: 200,
        edges_per_value: 10,
    };

    /// Tighter caps used by the federation pipeline — also lifted
    /// from the pre-extraction `PATTERN_MAX_REF_COUNT` and friends.
    pub const FEDERATION: PatternLimits = PatternLimits {
        max_files_per_value: 20,
        min_dirs: 2,
        max_edges: 200,
        edges_per_value: 10,
    };
}

/// Link external refs (already-resolved source ids + LSP reference
/// locations) to internal nodes. For every ref whose target resolves
/// to a known node at `path:line`, emit a `Calls` edge from the
/// source id to the target id — skipping self-edges.
///
/// `workspace` is the path used by [`graph_path`] to translate an
/// absolute reference path into the same relative key the scanner
/// uses; passing the wrong workspace silently produces 0 edges.
pub fn resolve_call_edges(
    db: &GraphDatabase,
    workspace: &Path,
    refs: &[(String, ReferenceLocation)],
    resolver: Option<&dyn CrossRepoResolver>,
    source_repo: Option<&RepoId>,
) -> Vec<GraphEdge> {
    let mut edges = Vec::with_capacity(refs.len());
    for (source_id, ref_loc) in refs {
        let path_str = graph_path(workspace, &ref_loc.path);
        let mut resolved_target: Option<String> = None;
        if let Some(target) = db.get_node_at_location(&path_str, ref_loc.line) {
            if target.id != *source_id {
                resolved_target = Some(target.id);
            }
        } else if let (Some(resolver), Some(src)) = (resolver, source_repo) {
            if let Some(gid) = resolver.resolve_cross_repo(src, None, Some(&ref_loc.path), Some(ref_loc.line)) {
                let gid_str = gid.as_str().to_string();
                if gid_str != *source_id {
                    resolved_target = Some(gid_str);
                }
            }
        }
        if let Some(target_id) = resolved_target {
            edges.push(GraphEdge::new(EdgeType::Calls, source_id.clone(), target_id));
        }
    }
    edges
}

/// The language family a source path belongs to, for the purpose of
/// deciding whether a name reference may link to it.
///
/// Extensions that compile as one language share a group, so a `.ts`
/// caller can still reach a `.tsx` definition and a `.h` declaration
/// pairs with its `.cpp`.
fn language_group(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.').map(|(_, e)| e)?;
    Some(match ext {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => "js",
        "go" => "go",
        "java" => "java",
        "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" => "c",
        "cs" => "csharp",
        "rb" => "ruby",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "scala" => "scala",
        "vue" => "vue",
        "svelte" => "svelte",
        _ => return None,
    })
}

/// Whether a name reference in `from` may resolve to a definition in `to`.
///
/// Name matching is language-blind, so a reference only had to be *unique*
/// to link — and a name that exists once in the whole repo produced a
/// single candidate regardless of what language it was written in. That
/// is how `main` in `tests/e2e/lain_test.py` became a caller of the Rust
/// `run_tests` in `execution.rs`, which put a Python test script inside
/// the blast radius of a Rust function. The same-file preference could
/// not catch it: with one candidate there was nothing to disambiguate.
///
/// Unknown extensions fall back to exact extension equality rather than
/// linking freely.
fn may_link_across(from: &str, to: &str) -> bool {
    match (language_group(from), language_group(to)) {
        (Some(a), Some(b)) => a == b,
        _ => {
            let ext = |p: &str| p.rsplit_once('.').map(|(_, e)| e.to_string());
            ext(from) == ext(to)
        }
    }
}

/// Resolve tree-sitter-derived `Calls`/`Uses` references to internal
/// nodes by name. Self-edges are dropped; `Uses` edges are only kept
/// when the target is a type-level declaration
/// (see [`is_type_level_target`]). Each (source, target) pair is
/// emitted at most once via the local `seen` set.
///
/// `refs` already carries the source file path + line for each entry;
/// no workspace path is needed here.
pub fn resolve_static_edges(
    db: &GraphDatabase,
    refs: &[StaticFileRef],
    resolver: Option<&dyn CrossRepoResolver>,
    source_repo: Option<&RepoId>,
) -> Vec<GraphEdge> {
    // (id, type, path). The path is what lets an ambiguous name be
    // resolved to the definition in the calling file.
    let mut name_index: HashMap<String, Vec<(String, crate::schema::NodeType, String)>> =
        HashMap::new();
    for node in db.get_all_nodes() {
        name_index
            .entry(node.name.clone())
            .or_default()
            .push((node.id.clone(), node.node_type.clone(), node.path.clone()));
    }

    let mut edges: Vec<GraphEdge> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for sr in refs {
        let Some(source_node) = db.get_node_at_location(&sr.file_path, sr.source_line) else {
            continue;
        };
        let Some(candidates) = name_index.get(sr.target_name.as_str()) else {
            if let (Some(resolver), Some(src)) = (resolver, source_repo) {
                if let Some(gid) = resolver.resolve_cross_repo(
                    src,
                    Some(&sr.target_name),
                    None,
                    None,
                ) {
                    let gid_str = gid.as_str().to_string();
                    if gid_str != source_node.id {
                        let key = (source_node.id.clone(), gid_str.clone());
                        if seen.insert(key) {
                            edges.push(GraphEdge::new(
                                sr.edge_type.clone(),
                                source_node.id.clone(),
                                gid_str,
                            ));
                        }
                    }
                }
            }
            continue;
        };
        // A name that several definitions share cannot be resolved by
        // name alone. Emitting an edge to *every* candidate — which is
        // what this did — manufactures callers wholesale: with eleven
        // `fn parse` definitions, `Args::parse()` in main.rs (clap's
        // derive) and `n.parse()` on a `&str` (stdlib) each produced
        // eleven edges, and `get_call_sites parse` answered with 61
        // callers, the same list for every one of the eleven nodes.
        //
        // Prefer a definition in the calling file, which is the case
        // that is actually decidable. Otherwise emit nothing: a missing
        // edge is a gap, N wrong edges are a lie, and the lie also
        // inflates `find_anchors` and `get_blast_radius`.
        // Drop candidates written in another language before counting.
        // A cross-language hit is never a real call here: these refs come
        // from a single-language tree-sitter parse of one file.
        let candidates: Vec<&(String, crate::schema::NodeType, String)> = candidates
            .iter()
            .filter(|(_, _, path)| may_link_across(&sr.file_path, path))
            .collect();

        let resolved: Vec<&(String, crate::schema::NodeType, String)> = if candidates.len() == 1 {
            candidates
        } else {
            candidates
                .into_iter()
                .filter(|(_, _, path)| path == &sr.file_path)
                .collect()
        };
        for (target_id, target_type, _) in resolved {
            if *target_id == source_node.id {
                continue;
            }
            if sr.edge_type == EdgeType::Uses && !is_type_level_target(target_type) {
                continue;
            }
            let key = (source_node.id.clone(), (*target_id).to_string());
            if seen.insert(key) {
                edges.push(GraphEdge::new(
                    sr.edge_type.clone(),
                    source_node.id.clone(),
                    (*target_id).to_string(),
                ));
            }
        }
    }
    edges
}

/// Detect cross-boundary coupling via repeated string literals: any
/// value that appears in `>= min_dirs` distinct parent directories and
/// `>= 2` files (and no more than `max_files_per_value`) becomes a
/// `Pattern` edge between each pair of files. The edge budget is
/// capped by `max_edges` overall and `edges_per_value` per value.
pub fn resolve_pattern_edges(
    db: &GraphDatabase,
    refs: &[PatternRef],
    limits: PatternLimits,
) -> Vec<GraphEdge> {
    if refs.is_empty() {
        return Vec::new();
    }

    let nodes: Vec<crate::schema::GraphNode> = db
        .get_all_nodes()
        .into_iter()
        .filter(|n| matches!(n.node_type, crate::schema::NodeType::File))
        .collect();
    let file_nodes: HashMap<&str, &crate::schema::GraphNode> = nodes
        .iter()
        .map(|n| (n.path.as_str(), n))
        .collect();

    let mut value_to_files: HashMap<String, Vec<String>> = HashMap::new();
    for pr in refs {
        let entry = value_to_files.entry(pr.value.clone()).or_default();
        if !entry.contains(&pr.file_path) {
            entry.push(pr.file_path.clone());
        }
    }

    let mut scored: Vec<(usize, String, Vec<String>)> = Vec::new();
    for (value, files) in value_to_files {
        if files.len() < 2 || files.len() > limits.max_files_per_value {
            continue;
        }
        let mut dirs: HashSet<String> = HashSet::new();
        for f in &files {
            if let Some(parent) = std::path::Path::new(f).parent() {
                dirs.insert(parent.to_string_lossy().to_string());
            }
        }
        if dirs.len() < limits.min_dirs {
            continue;
        }
        let pairs = dirs.len() * (dirs.len() - 1) / 2;
        scored.push((pairs, value, files));
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0));

    let max_edges = (scored.len() * limits.edges_per_value).min(limits.max_edges);
    let mut edges: Vec<GraphEdge> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();

    for (_score, _value, files) in scored {
        if edges.len() >= max_edges {
            break;
        }
        let mut dirs: HashMap<String, String> = HashMap::new();
        for f in &files {
            if let Some(parent) = std::path::Path::new(f).parent() {
                let parent_str = parent.to_string_lossy().to_string();
                dirs.entry(parent_str).or_insert_with(|| f.clone());
            }
        }
        let all_dirs: Vec<_> = dirs.into_iter().collect();
        for i in 0..all_dirs.len() {
            if edges.len() >= max_edges {
                break;
            }
            for j in (i + 1)..all_dirs.len() {
                if edges.len() >= max_edges {
                    break;
                }
                let (_dir_a, file_a) = &all_dirs[i];
                let (_dir_b, file_b) = &all_dirs[j];
                let key = (file_a.clone(), file_b.clone());
                if seen.insert(key) {
                    if let (Some(node_a), Some(node_b)) = (
                        file_nodes.get(file_a.as_str()),
                        file_nodes.get(file_b.as_str()),
                    ) {
                        edges.push(GraphEdge::new(
                            EdgeType::Pattern,
                            node_a.id.clone(),
                            node_b.id.clone(),
                        ));
                    }
                }
                if edges.len() >= max_edges {
                    break;
                }
            }
        }
    }
    edges
}

#[cfg(test)]
mod ambiguous_name_tests {
    use super::*;
    use crate::graph::GraphDatabase;
    use crate::schema::{GraphNode, NodeType};

    fn fn_node(name: &str, path: &str, lines: (u32, u32)) -> GraphNode {
        let mut n = GraphNode::new(NodeType::Function, name.to_string(), path.to_string());
        n.line_start = Some(lines.0);
        n.line_end = Some(lines.1);
        n
    }

    /// A reference to a name that several definitions share must not
    /// produce an edge to every one of them.
    ///
    /// Live finding: with eleven `fn parse` definitions, every
    /// `.parse()` in the repo — including clap's `Args::parse()` and
    /// stdlib `str::parse` — fanned out to all eleven, so
    /// `get_call_sites parse` reported 61 callers and returned the
    /// identical list for each of the eleven nodes.
    #[test]
    fn an_ambiguous_name_does_not_link_to_every_candidate() {
        let tmp = std::env::temp_dir().join("lain_resolve_ambiguous");
        let _ = std::fs::remove_dir_all(&tmp);
        let db = GraphDatabase::new(&tmp).unwrap();

        // Three definitions of `parse`, in three files.
        for path in ["src/a.rs", "src/b.rs", "src/c.rs"] {
            db.upsert_node(fn_node("parse", path, (1, 5))).unwrap();
        }
        // A caller in a fourth file, which defines no `parse`.
        db.upsert_node(fn_node("caller", "src/d.rs", (1, 20)))
            .unwrap();

        let refs = vec![StaticFileRef {
            file_path: "src/d.rs".to_string(),
            source_line: 10,
            target_name: "parse".to_string(),
            edge_type: EdgeType::Calls,
        }];
        let edges = resolve_static_edges(&db, &refs, None, None);
        assert!(
            edges.is_empty(),
            "a name with three candidates and none in the calling file is \
             unresolvable; emitting {} edge(s) invents callers",
            edges.len()
        );
    }

    /// The decidable case still resolves: a definition in the calling
    /// file wins over same-named definitions elsewhere.
    #[test]
    fn an_ambiguous_name_resolves_to_the_definition_in_the_calling_file() {
        let tmp = std::env::temp_dir().join("lain_resolve_ambiguous_samefile");
        let _ = std::fs::remove_dir_all(&tmp);
        let db = GraphDatabase::new(&tmp).unwrap();

        db.upsert_node(fn_node("parse", "src/other.rs", (1, 5)))
            .unwrap();
        let local = fn_node("parse", "src/d.rs", (1, 5));
        let local_id = local.id.clone();
        db.upsert_node(local).unwrap();
        db.upsert_node(fn_node("caller", "src/d.rs", (10, 20)))
            .unwrap();

        let refs = vec![StaticFileRef {
            file_path: "src/d.rs".to_string(),
            source_line: 15,
            target_name: "parse".to_string(),
            edge_type: EdgeType::Calls,
        }];
        let edges = resolve_static_edges(&db, &refs, None, None);
        assert_eq!(edges.len(), 1, "exactly one edge, to the local definition");
        assert_eq!(edges[0].target_id, local_id);
    }


    /// A tree-sitter name reference must not link across languages. A
    /// name that happens to be unique repo-wide still had exactly one
    /// candidate, so a Python caller linked straight to a Rust
    /// definition.
    #[test]
    fn a_name_reference_does_not_link_across_languages() {
        let tmp = std::env::temp_dir().join("lain_resolve_crosslang");
        let _ = std::fs::remove_dir_all(&tmp);
        let db = GraphDatabase::new(&tmp).unwrap();

        db.upsert_node(fn_node("run_tests", "src/server/execution.rs", (1, 50)))
            .unwrap();
        db.upsert_node(fn_node("main", "tests/e2e/lain_test.py", (1, 30)))
            .unwrap();

        let refs = vec![StaticFileRef {
            file_path: "tests/e2e/lain_test.py".to_string(),
            source_line: 10,
            target_name: "run_tests".to_string(),
            edge_type: EdgeType::Calls,
        }];
        let edges = resolve_static_edges(&db, &refs, None, None);
        assert!(
            edges.is_empty(),
            "a .py caller must not link to a .rs definition; got {} edge(s)",
            edges.len()
        );
    }


    /// The same-language case must keep working, including across the
    /// extensions that belong to one language family.
    #[test]
    fn a_name_reference_still_links_within_a_language_family() {
        let tmp = std::env::temp_dir().join("lain_resolve_same_family");
        let _ = std::fs::remove_dir_all(&tmp);
        let db = GraphDatabase::new(&tmp).unwrap();

        let target = fn_node("renderWidget", "src/ui/widget.tsx", (1, 20));
        let target_id = target.id.clone();
        db.upsert_node(target).unwrap();
        db.upsert_node(fn_node("caller", "src/ui/app.ts", (1, 30)))
            .unwrap();

        let refs = vec![StaticFileRef {
            file_path: "src/ui/app.ts".to_string(),
            source_line: 10,
            target_name: "renderWidget".to_string(),
            edge_type: EdgeType::Calls,
        }];
        let edges = resolve_static_edges(&db, &refs, None, None);
        assert_eq!(edges.len(), 1, ".ts -> .tsx is the same language family");
        assert_eq!(edges[0].target_id, target_id);
    }

    /// A cross-language name collision must not defeat the same-file
    /// preference either: the Rust definition is filtered out first, so
    /// the Python caller resolves to its own file's definition.
    #[test]
    fn a_cross_language_twin_does_not_block_same_file_resolution() {
        let tmp = std::env::temp_dir().join("lain_resolve_crosslang_twin");
        let _ = std::fs::remove_dir_all(&tmp);
        let db = GraphDatabase::new(&tmp).unwrap();

        db.upsert_node(fn_node("run_tests", "src/server/execution.rs", (1, 50)))
            .unwrap();
        let py = fn_node("run_tests", "tests/e2e/lain_test.py", (1, 40));
        let py_id = py.id.clone();
        db.upsert_node(py).unwrap();
        db.upsert_node(fn_node("main", "tests/e2e/lain_test.py", (50, 80)))
            .unwrap();

        let refs = vec![StaticFileRef {
            file_path: "tests/e2e/lain_test.py".to_string(),
            source_line: 60,
            target_name: "run_tests".to_string(),
            edge_type: EdgeType::Calls,
        }];
        let edges = resolve_static_edges(&db, &refs, None, None);
        assert_eq!(edges.len(), 1, "exactly one edge, to the Python definition");
        assert_eq!(edges[0].target_id, py_id);
    }

    /// The `max_edges` budget must be a hard ceiling, not off by one.
    /// Every `edges.len() >= max_edges` guard survived mutation to `>`,
    /// meaning nothing checked the cap actually holds — and this is the
    /// budget `.lain/tuning.toml`'s `max_pattern_edges` now controls, so
    /// a caller who lowers it is trusting a bound no test verified.
    #[test]
    fn the_pattern_edge_budget_is_a_hard_ceiling() {
        let tmp = std::env::temp_dir().join("lain_pattern_budget");
        let _ = std::fs::remove_dir_all(&tmp);
        let db = GraphDatabase::new(&tmp).unwrap();

        // Enough files across enough directories that the unbounded
        // pairing would produce far more than the cap.
        let mut refs = Vec::new();
        for d in 0..6 {
            for f in 0..4 {
                let path = format!("src/dir{d}/file{f}.rs");
                db.upsert_node(crate::schema::GraphNode::new(
                    crate::schema::NodeType::File,
                    format!("file{f}.rs"),
                    path.clone(),
                ))
                .unwrap();
                refs.push(PatternRef {
                    file_path: path,
                    value: "SHARED_CONSTANT".to_string(),
                    source_line: 1,
                });
            }
        }

        for cap in [1usize, 3, 7] {
            let limits = PatternLimits {
                max_edges: cap,
                ..PatternLimits::DEFAULT
            };
            let edges = resolve_pattern_edges(&db, &refs, limits);
            assert!(
                edges.len() <= cap,
                "budget {cap} exceeded: produced {} edges",
                edges.len()
            );
        }
    }

    /// And the budget must be reachable — a cap that always yields zero
    /// would satisfy the ceiling test while breaking the feature.
    #[test]
    fn a_generous_budget_still_produces_pattern_edges() {
        let tmp = std::env::temp_dir().join("lain_pattern_budget_generous");
        let _ = std::fs::remove_dir_all(&tmp);
        let db = GraphDatabase::new(&tmp).unwrap();

        let mut refs = Vec::new();
        for d in 0..4 {
            let path = format!("src/dir{d}/file.rs");
            db.upsert_node(crate::schema::GraphNode::new(
                crate::schema::NodeType::File,
                "file.rs".to_string(),
                path.clone(),
            ))
            .unwrap();
            refs.push(PatternRef {
                file_path: path,
                value: "SHARED_CONSTANT".to_string(),
                source_line: 1,
            });
        }

        let edges = resolve_pattern_edges(&db, &refs, PatternLimits::DEFAULT);
        assert!(
            !edges.is_empty(),
            "a shared literal across 4 directories should produce Pattern edges"
        );
        assert!(edges.iter().all(|e| e.edge_type == EdgeType::Pattern));
    }

    /// An unambiguous name is unaffected — this is the common case and
    /// must keep working.
    #[test]
    fn a_unique_name_still_links() {
        let tmp = std::env::temp_dir().join("lain_resolve_unique");
        let _ = std::fs::remove_dir_all(&tmp);
        let db = GraphDatabase::new(&tmp).unwrap();

        let target = fn_node("sweep_orphans", "src/a.rs", (1, 5));
        let target_id = target.id.clone();
        db.upsert_node(target).unwrap();
        db.upsert_node(fn_node("caller", "src/d.rs", (10, 20)))
            .unwrap();

        let refs = vec![StaticFileRef {
            file_path: "src/d.rs".to_string(),
            source_line: 15,
            target_name: "sweep_orphans".to_string(),
            edge_type: EdgeType::Calls,
        }];
        let edges = resolve_static_edges(&db, &refs, None, None);
        assert_eq!(edges.len(), 1, "a unique name must still resolve");
        assert_eq!(edges[0].target_id, target_id);
    }
}
