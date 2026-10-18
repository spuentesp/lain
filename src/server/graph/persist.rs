//! Persistence layer for `GraphDatabase`.
//!
//! Owns the on-disk representation: the bincode codec, the
//! `path_format_version` constant that gates forward compatibility,
//! and the strict read-only inspection path used by the diagnostic
//! tool. The runtime save/load methods on `GraphDatabase`
//! (`save_to_disk`, `load_from_disk`, `export_to_json`) build a
//! `GraphState` snapshot and call [`encode_state`] / [`decode_state`]
//! here.
//!
//! Extracted from the original single-file `src/server/graph.rs` so
//! that bincode, version checks, and disk-IO error categorization
//! live in one place rather than being interleaved with petgraph
//! CRUD.

use super::GraphEdge;
use super::GraphNode;
use petgraph::stable_graph::{NodeIndex, StableGraph};
use petgraph::visit::EdgeRef;
use petgraph::Direction;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Bumped whenever the meaning of `GraphNode.path` changes. Version 2 is
/// the switch from mixed absolute/relative paths to a single
/// workspace-relative form. A graph written by an older lain deserializes
/// fine (the bincode layout is unchanged) but its keys are absolute, so
/// merging it into a v2 graph would double every node instead of updating
/// it. `load_from_disk` therefore discards anything that isn't v2 and lets
/// the caller rebuild from source.
/// 3: `GraphNode::container` added (bincode layout changed).
/// 4: PR 3 schema v3 — `GraphNode.contract`, `GraphNode.entry`,
///    `GraphEdge.site`, `GraphEdge.detail` added (bincode layout
///    changed). Per-repo graphs with the old version are discarded
///    and rebuilt on load, as today.
/// 5: `GraphNode.contract` becomes `Vec<ContractFact>` so one node can
///    record several facts (two subscriptions to different topics on
///    one line used to collapse into one). Bincode layout changed:
///    old graphs are discarded and rebuilt on load, as above.
pub const PATH_FORMAT_VERSION: u32 = 5;

#[derive(Serialize, Deserialize)]
pub(super) struct GraphState {
    pub(super) graph: StableGraph<GraphNode, GraphEdge>,
    pub(super) index_map: HashMap<String, NodeIndex>,
    pub(super) last_commit: Option<String>,
    /// Absent in graphs written before the canonical-path change. Bincode
    /// is positional, so such files fail to decode entirely (see
    /// [`decode_state`]); the loader fail-softs to an empty graph and the
    /// caller rebuilds — the same outcome as a version-check failure.
    #[serde(default)]
    pub(super) path_format_version: u32,
    /// Call-graph publication stamp as of the snapshot. Tri-state:
    ///
    /// - outer `None` — the file predates the stamp ([`decode_state`]'s
    ///   pre-stamp fallback records the absence; see that comment for why
    ///   `#[serde(default)]` alone cannot express it in bincode);
    /// - `Some(None)` — the stamp was written and is **cleared**: the save
    ///   landed mid-pass, with nodes replaced and their `Calls` edges not
    ///   yet re-inserted;
    /// - `Some(Some(commit))` — the pass's call-edge inserts had landed
    ///   for `commit` when the snapshot was written.
    ///
    /// [`super::GraphDatabase::apply_state_loaded`] trusts the inner value
    /// when the field is present and falls back to inferring from
    /// `last_commit` when it is absent.
    #[serde(default)]
    pub(super) calls_published_for: Option<Option<String>>,
}

/// The wire layout this struct had before `calls_published_for` existed
/// (bincode is positional: fields are concatenated, no names, no count).
/// Decoded as the fallback in [`decode_state`] for pre-stamp files.
#[derive(Serialize, Deserialize)]
struct PreStampState {
    graph: StableGraph<GraphNode, GraphEdge>,
    index_map: HashMap<String, NodeIndex>,
    last_commit: Option<String>,
    path_format_version: u32,
}

impl GraphState {
    /// Build a state for a fresh save. `calls_published_for` is the stamp
    /// as of the snapshot (`None` = cleared mid-pass); it is always written
    /// as *present*, so a load can tell a mid-pass save apart from a file
    /// that pre-dates the stamp entirely.
    pub(super) fn new(
        graph: StableGraph<GraphNode, GraphEdge>,
        index_map: HashMap<String, NodeIndex>,
        last_commit: Option<String>,
        calls_published_for: Option<String>,
    ) -> Self {
        Self {
            graph,
            index_map,
            last_commit,
            path_format_version: PATH_FORMAT_VERSION,
            calls_published_for: Some(calls_published_for),
        }
    }
}

/// Encode a `GraphState` snapshot to its on-disk byte form.
///
/// The codec is bincode 2.x with the legacy config (matches the
/// decoder). Returns `EncodeError` on shape/size failures; callers
/// map that into their own `LainError` variant.
pub(super) fn encode_state(state: &GraphState) -> Result<Vec<u8>, bincode::error::EncodeError> {
    bincode::serde::encode_to_vec(state, bincode::config::legacy())
}

/// Upper bound on what one decode may allocate. Without a limit a
/// corrupt length prefix (an interrupted write, a full disk, another
/// version's format) asked for exabytes and panicked with "capacity
/// overflow" before the fail-soft path in `load_from_disk` could run —
/// `lain oneshot`, `query` and `doctor` all crashed until `.lain` was
/// deleted by hand. With it, the decode returns an error instead.
pub(crate) const DECODE_LIMIT: usize = 1 << 32;

/// Decode bytes into a `GraphState`. Mirrors [`encode_state`]; the
/// layout is the same, only allocation is bounded.
///
/// Pre-stamp files (written before `calls_published_for` existed) are
/// accepted through an explicit fallback: bincode is a positional format,
/// so `#[serde(default)]` cannot stand in for a trailing field the writer
/// never emitted — the decoder hits the end of the slice mid-field and
/// errors out. Retry the pre-stamp shape and record the stamp as *absent*
/// (outer `None`) so [`super::GraphDatabase::apply_state_loaded`] can fall
/// back to the pre-stamp inference for those files. The fallback accepts
/// the pre-stamp shape only when it consumes the whole slice: a current
/// file truncated inside the stamp tail must stay a decode error (the
/// loader fail-softs to an empty graph) rather than half-loading as an
/// old graph — see the truncation property in `persist_verification.rs`.
pub(super) fn decode_state(
    data: &[u8],
) -> Result<(GraphState, usize), bincode::error::DecodeError> {
    let config = bincode::config::legacy().with_limit::<DECODE_LIMIT>();
    let current = bincode::serde::decode_from_slice(data, config);
    let Err(err) = current else {
        return current;
    };
    let Ok((pre, used)) = bincode::serde::decode_from_slice::<PreStampState, _>(data, config)
    else {
        return Err(err);
    };
    if used != data.len() {
        // Trailing bytes mean this is not a whole pre-stamp file (a
        // truncated current-shape file lands here): keep the original
        // error so the caller fail-softs instead of half-loading.
        return Err(err);
    }
    Ok((
        GraphState {
            graph: pre.graph,
            index_map: pre.index_map,
            last_commit: pre.last_commit,
            path_format_version: pre.path_format_version,
            calls_published_for: None,
        },
        used,
    ))
}

/// Strict, read-only inspection for diagnostics. Unlike the runtime loader,
/// this preserves the distinction between corrupt and missing graph data.
pub fn inspect_persisted_graph(path: &Path) -> Result<Option<String>, GraphInspectionError> {
    let data = std::fs::read(path).map_err(GraphInspectionError::Io)?;
    let (mut state, _) = decode_state(&data).map_err(GraphInspectionError::Corrupt)?;
    if state.path_format_version != PATH_FORMAT_VERSION {
        return Err(GraphInspectionError::Incompatible(
            state.path_format_version,
        ));
    }
    heal_duplicate_ids(&mut state);
    if state.index_map.len() != state.graph.node_count()
        || state
            .index_map
            .iter()
            .any(|(id, index)| state.graph.node_weight(*index).map(|node| &node.id) != Some(id))
    {
        return Err(GraphInspectionError::InvalidIndex);
    }
    Ok(state.last_commit)
}

/// Fold nodes the id index does not point at into the node it does.
///
/// Before `replace_nodes_for_paths` refreshed kept Namespace nodes in place,
/// every re-index added a second node under a directory's id. Those graphs
/// still sit in users' `.lain/graph.bin`, and `lain doctor` rejects them.
/// Moving the stray copy's edges onto the indexed node and dropping it
/// repairs them on load instead of asking for a manual rebuild, and
/// [`inspect_persisted_graph`] judges a graph by what loading it yields.
pub(super) fn heal_duplicate_ids(state: &mut GraphState) {
    let strays: Vec<NodeIndex> = state
        .graph
        .node_indices()
        .filter(|i| state.index_map.get(&state.graph[*i].id) != Some(i))
        .collect();
    for stray in strays {
        let id = state.graph[stray].id.clone();
        let Some(keep) = state
            .index_map
            .get(&id)
            .copied()
            .filter(|k| state.graph.node_weight(*k).is_some_and(|n| n.id == id))
        else {
            // Nothing indexed under this id: the stray is the only copy.
            state.index_map.insert(id, stray);
            continue;
        };
        let moved: Vec<(NodeIndex, NodeIndex, GraphEdge)> = state
            .graph
            .edges_directed(stray, Direction::Outgoing)
            .map(|e| (keep, e.target(), e.weight().clone()))
            .chain(
                state
                    .graph
                    .edges_directed(stray, Direction::Incoming)
                    .map(|e| (e.source(), keep, e.weight().clone())),
            )
            .collect();
        for (from, to, edge) in moved {
            if from == stray || to == stray {
                continue;
            }
            let already = state
                .graph
                .edges_connecting(from, to)
                .any(|e| e.weight().edge_type == edge.edge_type);
            if !already {
                state.graph.add_edge(from, to, edge);
            }
        }
        state.graph.remove_node(stray);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GraphInspectionError {
    #[error("cannot read graph: {0}")]
    Io(std::io::Error),
    #[error("cannot decode graph: {0}")]
    Corrupt(bincode::error::DecodeError),
    #[error("graph path format {0} is incompatible with this binary")]
    Incompatible(u32),
    #[error("graph index does not match its nodes")]
    InvalidIndex,
}

#[cfg(test)]
mod decode_limit_tests {
    use super::*;

    /// A clobbered length prefix is an error, not a panic.
    #[test]
    fn a_corrupt_length_prefix_is_an_error_not_a_panic() {
        // Every length prefix reads as u64::MAX.
        let bytes = vec![0xffu8; 64];
        assert!(decode_state(&bytes).is_err());
    }
}

#[cfg(test)]
mod stamp_persist_tests {
    //! The `calls_published_for` stamp is tri-state on the wire (absent /
    //! cleared / published) and every state of it must round-trip exactly,
    //! because `apply_state_loaded` turns it into the `call_graph`
    //! capability gate on warm boot.

    use super::*;
    use crate::schema::{GraphEdge, GraphNode, NodeType};

    fn one_node() -> (
        StableGraph<GraphNode, GraphEdge>,
        HashMap<String, NodeIndex>,
    ) {
        let mut graph = StableGraph::new();
        let node = GraphNode::new(NodeType::Function, "callee".into(), "src/b.rs".into());
        let mut index_map = HashMap::new();
        index_map.insert(node.id.clone(), graph.add_node(node));
        (graph, index_map)
    }

    /// `Some(Some(commit))` survives encode/decode unchanged.
    #[test]
    fn a_published_stamp_survives_encode_decode() {
        let (graph, index_map) = one_node();
        let state = GraphState::new(graph, index_map, Some("abc".into()), Some("abc".into()));
        let bytes = encode_state(&state).expect("encode");
        let (decoded, used) = decode_state(&bytes).expect("decode");
        assert_eq!(used, bytes.len());
        assert_eq!(decoded.calls_published_for, Some(Some("abc".into())));
        assert_eq!(decoded.last_commit.as_deref(), Some("abc"));
    }

    /// `Some(None)` — a save that landed mid-pass, stamp cleared — must
    /// not be confused with a file that predates the stamp (outer `None`).
    /// Confusing them would publish `ready` over the torn graph.
    #[test]
    fn a_cleared_stamp_survives_encode_decode_as_present() {
        let (graph, index_map) = one_node();
        let state = GraphState::new(graph, index_map, Some("abc".into()), None);
        let bytes = encode_state(&state).expect("encode");
        let (decoded, _) = decode_state(&bytes).expect("decode");
        assert_eq!(
            decoded.calls_published_for,
            Some(None),
            "a cleared stamp is present-and-None, never absent"
        );
    }

    /// A file written before the stamp existed (4-field layout) decodes
    /// and records the stamp as *absent*, so the loader can fall back to
    /// the pre-stamp inference. `#[serde(default)]` cannot do this in
    /// bincode — the format is positional and the decoder errors at the
    /// missing tail instead of yielding `None` — so `decode_state` retries
    /// the pre-stamp shape explicitly.
    #[test]
    fn a_pre_stamp_state_loads_and_records_the_stamp_absent() {
        let (graph, index_map) = one_node();
        let bytes = encode_pre_stamp_state_for_tests(graph, index_map, Some("old".into()))
            .expect("encode pre-stamp");
        let (decoded, used) = decode_state(&bytes).expect("pre-stamp file must load");
        assert_eq!(used, bytes.len());
        assert_eq!(
            decoded.calls_published_for, None,
            "a pre-stamp file records the stamp absent, not cleared"
        );
        assert_eq!(decoded.last_commit.as_deref(), Some("old"));
        assert_eq!(decoded.path_format_version, PATH_FORMAT_VERSION);
        assert_eq!(decoded.graph.node_count(), 1);
    }

    /// The pre-stamp fallback must not half-load a current-shape file
    /// truncated inside the stamp tail: that stays a whole-file decode
    /// error (the loader fail-softs to empty), preserving the truncation
    /// property pinned in `persist_verification.rs`.
    #[test]
    fn a_state_truncated_in_the_stamp_is_not_half_loaded_as_pre_stamp() {
        let (graph, index_map) = one_node();
        let state = GraphState::new(graph, index_map, Some("abc".into()), Some("abc".into()));
        let bytes = encode_state(&state).expect("encode");
        // Cut inside the trailing stamp (`1u8 1u8` + string).
        let cut = bytes.len() - 3;
        assert!(
            decode_state(&bytes[..cut]).is_err(),
            "a truncated stamp tail must not decode as a pre-stamp file"
        );
    }
}

/// Encode a pre-stamp (4-field) state — the bytes an older Lain wrote —
/// so load tests can prove those files still load (see
/// [`stamp_persist_tests`] and the warm-boot tests in
/// [`super::GraphDatabase`]'s test modules).
#[cfg(test)]
pub(super) fn encode_pre_stamp_state_for_tests(
    graph: StableGraph<GraphNode, GraphEdge>,
    index_map: HashMap<String, NodeIndex>,
    last_commit: Option<String>,
) -> Result<Vec<u8>, bincode::error::EncodeError> {
    bincode::serde::encode_to_vec(
        &PreStampState {
            graph,
            index_map,
            last_commit,
            path_format_version: PATH_FORMAT_VERSION,
        },
        bincode::config::legacy(),
    )
}

#[cfg(test)]
#[path = "persist_verification.rs"]
mod verification;
