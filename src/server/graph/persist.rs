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
pub const PATH_FORMAT_VERSION: u32 = 2;

#[derive(Serialize, Deserialize)]
pub(super) struct GraphState {
    pub(super) graph: StableGraph<GraphNode, GraphEdge>,
    pub(super) index_map: HashMap<String, NodeIndex>,
    pub(super) last_commit: Option<String>,
    /// Absent in graphs written before the canonical-path change; serde
    /// defaults it to 0, which fails the version check and forces a rebuild.
    #[serde(default)]
    pub(super) path_format_version: u32,
}

impl GraphState {
    pub(super) fn new(
        graph: StableGraph<GraphNode, GraphEdge>,
        index_map: HashMap<String, NodeIndex>,
        last_commit: Option<String>,
    ) -> Self {
        Self {
            graph,
            index_map,
            last_commit,
            path_format_version: PATH_FORMAT_VERSION,
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
pub(super) fn decode_state(
    data: &[u8],
) -> Result<(GraphState, usize), bincode::error::DecodeError> {
    bincode::serde::decode_from_slice(data, bincode::config::legacy().with_limit::<DECODE_LIMIT>())
}

/// Strict, read-only inspection for diagnostics. Unlike the runtime loader,
/// this preserves the distinction between corrupt and missing graph data.
pub fn inspect_persisted_graph(path: &Path) -> Result<Option<String>, GraphInspectionError> {
    let data = std::fs::read(path).map_err(GraphInspectionError::Io)?;
    let (state, _) = decode_state(&data).map_err(GraphInspectionError::Corrupt)?;
    if state.path_format_version != PATH_FORMAT_VERSION {
        return Err(GraphInspectionError::Incompatible(
            state.path_format_version,
        ));
    }
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
