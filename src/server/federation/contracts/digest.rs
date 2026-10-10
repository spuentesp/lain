//! Canonical analyzer digest (`§8.3`).
//!
//! `graph.bin` carries a `HashMap` (the per-repo DB's
//! `GraphState::index_map`), so its bytes are not stable across
//! runs — the same graph content serializes to different byte
//! sequences depending on the `HashMap` randomization salt. The
//! digest that pins "the analyzer produced the same graph content
//! from the same fixture" therefore operates on a deterministic
//! shape:
//!
//! - every node sorted by id, each bincode-encoded after clearing
//!   `last_lsp_sync`, `last_git_sync`, `is_hydrated`, and `embedding`;
//! - every edge sorted by `(edge_type, source_id, target_id)`, each
//!   bincode-encoded verbatim (no fields on `GraphEdge` are
//!   recorded when indexing happened — but the `weight`,
//!   `provenance`, `site`, `detail`, `cross_repo` fields are part
//!   of the structural shape and stay);
//! - blake3 over the resulting byte stream.
//!
//! `embedding` is always cleared because snapshot mode never
//! computes one (§8.2); the other three cleared fields are
//! wall-clock-derived per-pass state that would otherwise break the
//! determinism invariant. The function is `pub` and takes a
//! `&GraphDatabase` so the digest test and any future
//! "two snapshots of the same repo produce the same digest" check
//! share a single source of truth.
//!
//! The committed fixture (`tests/fixtures/contracts/analyzer_digest.txt`)
//! stores the digest alongside the `CONTRACT_ANALYZER_REV` it was
//! generated with, so a regeneration run that bumps the counter
//! can tell which (digest, rev) pair belongs to which analyzer
//! version. The line format is `analyzer_rev:<u32>\ndigest:<hex>`.

use crate::graph::GraphDatabase;
use crate::schema::{EdgeType, GraphEdge, GraphNode};
use blake3::Hasher;

/// Compute the canonical analyzer digest for `db` (§8.3).
///
/// The function clones every node and edge out of the graph
/// because it is called once per snapshot or once per digest test
/// — neither path needs to share locks with a hot indexing loop.
/// The whole stream is hashed in one pass so the hasher never
/// observes a half-built byte buffer.
pub fn canonical_digest(db: &GraphDatabase) -> blake3::Hash {
    let mut hasher = Hasher::new();
    let mut nodes: Vec<GraphNode> = db.get_all_nodes();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    for node in &nodes {
        let mut node = node.clone();
        node.last_lsp_sync = None;
        node.last_git_sync = None;
        node.is_hydrated = false;
        node.embedding = None;
        let bytes = bincode::serde::encode_to_vec(&node, bincode::config::legacy())
            .expect("GraphNode bincode encode");
        hasher.update(&bytes);
    }
    let all_edges = db.all_edges();
    let mut edges: Vec<&GraphEdge> = all_edges.iter().collect();
    edges.sort_by(|a, b| {
        edge_sort_key(&a.edge_type, &a.source_id, &a.target_id).cmp(&edge_sort_key(
            &b.edge_type,
            &b.source_id,
            &b.target_id,
        ))
    });
    for edge in edges {
        let bytes = bincode::serde::encode_to_vec(edge, bincode::config::legacy())
            .expect("GraphEdge bincode encode");
        hasher.update(&bytes);
    }
    hasher.finalize()
}

/// Hex-encoded canonical digest, lower-case, no separators. The
/// fixture file uses the same shape so a literal string compare
/// works without a parser.
pub fn canonical_digest_hex(db: &GraphDatabase) -> String {
    canonical_digest(db).to_hex().to_string()
}

/// The same digest as [`canonical_digest_hex`] but restricted to
/// the *first 16 hex characters* — the same prefix length the
/// `snapshot_id` (`§8.4`) uses. Kept here so the snapshot identity
/// computation and the analyzer-digest fixture share the helper.
pub fn canonical_digest_prefix16(db: &GraphDatabase) -> String {
    let hex = canonical_digest_hex(db);
    hex[..32.min(hex.len())].to_string()
}

fn edge_sort_key(
    edge_type: &EdgeType,
    source_id: &str,
    target_id: &str,
) -> (String, String, String) {
    (
        format!("{edge_type:?}"),
        source_id.to_string(),
        target_id.to_string(),
    )
}

/// Render the digest fixture content.
///
/// Format:
/// ```text
/// analyzer_rev: <u32>
/// digest: <blake3 hex>
/// ```
///
/// The committed fixture stores both so a regenerator that
/// forgot to bump the constant can be told apart from one that
/// remembered: the analyzer_rev row in the file is the
/// `CONTRACT_ANALYZER_REV` the digest was generated under. The
/// integration test (`tests/contracts_analyzer_digest.rs`) compares
/// the parsed `digest:` field against a fresh recomputation AND
/// the parsed `analyzer_rev:` field against the current
/// `CONTRACT_ANALYZER_REV`; mismatch on either fails with a
/// regenerate command.
pub fn render_digest_fixture(db: &GraphDatabase) -> String {
    let rev = super::CONTRACT_ANALYZER_REV;
    let digest = canonical_digest_hex(db);
    format!("analyzer_rev: {rev}\ndigest: {digest}\n")
}

/// Parsed view of a committed digest fixture. The integration test
/// uses this to compare the file's two fields against the
/// recomputed values and emit a regenerate command on mismatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDigestFixture {
    pub analyzer_rev: u32,
    pub digest: String,
}

impl ParsedDigestFixture {
    /// Parse the fixture text. Tolerates trailing whitespace and
    /// blank lines; rejects anything that does not look like the
    /// `analyzer_rev:` / `digest:` two-line shape so a
    /// hand-corrupted fixture surfaces as a clear error rather
    /// than a silent zero.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut analyzer_rev: Option<u32> = None;
        let mut digest: Option<String> = None;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("analyzer_rev:") {
                analyzer_rev = Some(
                    rest.trim()
                        .parse::<u32>()
                        .map_err(|e| format!("analyzer_rev not a u32: {e}"))?,
                );
            } else if let Some(rest) = line.strip_prefix("digest:") {
                digest = Some(rest.trim().to_string());
            } else {
                return Err(format!("unrecognized fixture line: {line:?}"));
            }
        }
        Ok(ParsedDigestFixture {
            analyzer_rev: analyzer_rev.ok_or_else(|| "missing analyzer_rev:".to_string())?,
            digest: digest.ok_or_else(|| "missing digest:".to_string())?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::GraphDatabase;
    use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};

    fn empty_db() -> GraphDatabase {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("graph.bin");
        GraphDatabase::new(&path).unwrap()
    }

    #[test]
    fn empty_graph_digest_is_stable() {
        let db = empty_db();
        let a = canonical_digest_hex(&db);
        let b = canonical_digest_hex(&db);
        assert_eq!(a, b, "digest of an empty graph must be deterministic");
    }

    #[test]
    fn determinism_holds_with_cleared_fields_set() {
        // A node whose `last_lsp_sync`, `last_git_sync`,
        // `is_hydrated`, and `embedding` are populated must digest
        // the same as the same node with those fields cleared —
        // because the digest clears them before hashing.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("graph.bin");
        let db = GraphDatabase::new(&path).unwrap();
        let mut n = GraphNode::new(NodeType::Function, "alpha".into(), "src/a.rs".into());
        n.last_lsp_sync = Some(1_700_000_000);
        n.last_git_sync = Some(1_700_000_001);
        n.is_hydrated = true;
        n.embedding = Some("non-empty-embedding-bytes".into());
        db.upsert_node(n.clone()).unwrap();

        let mut cleared = n.clone();
        cleared.last_lsp_sync = None;
        cleared.last_git_sync = None;
        cleared.is_hydrated = false;
        cleared.embedding = None;
        let db_cleared_path = tempfile::tempdir().unwrap().path().join("graph.bin");
        let db_cleared = GraphDatabase::new(&db_cleared_path).unwrap();
        db_cleared.upsert_node(cleared).unwrap();

        assert_eq!(
            canonical_digest_hex(&db),
            canonical_digest_hex(&db_cleared),
            "the four cleared fields must not influence the digest"
        );
    }

    #[test]
    fn digest_is_order_independent() {
        // The graph stores nodes in a HashMap; the digest must
        // arrive at the same byte stream regardless of insertion
        // order, because it sorts by id before hashing.
        let tmp = tempfile::tempdir().unwrap();
        let db_a_path = tmp.path().join("graph_a.bin");
        let db_a = GraphDatabase::new(&db_a_path).unwrap();
        for name in ["zeta", "alpha", "mu"] {
            let n = GraphNode::new(NodeType::Function, name.into(), format!("src/{name}.rs"));
            db_a.upsert_node(n).unwrap();
        }

        let db_b_path = tempfile::tempdir().unwrap().path().join("graph_b.bin");
        let db_b = GraphDatabase::new(&db_b_path).unwrap();
        for name in ["alpha", "mu", "zeta"] {
            let n = GraphNode::new(NodeType::Function, name.into(), format!("src/{name}.rs"));
            db_b.upsert_node(n).unwrap();
        }
        assert_eq!(
            canonical_digest_hex(&db_a),
            canonical_digest_hex(&db_b),
            "digest must be independent of node insertion order"
        );
    }

    #[test]
    fn edge_sort_is_deterministic() {
        // Same node set, different edge insertion order → same digest.
        let tmp = tempfile::tempdir().unwrap();
        let a = GraphDatabase::new(&tmp.path().join("a.bin")).unwrap();
        let alpha = GraphNode::new(NodeType::Function, "alpha".into(), "src/a.rs".into());
        let beta = GraphNode::new(NodeType::Function, "beta".into(), "src/b.rs".into());
        let gamma = GraphNode::new(NodeType::Function, "gamma".into(), "src/c.rs".into());
        a.upsert_node(alpha.clone()).unwrap();
        a.upsert_node(beta.clone()).unwrap();
        a.upsert_node(gamma.clone()).unwrap();
        a.insert_edge(&GraphEdge::new(
            EdgeType::Calls,
            alpha.id.clone(),
            beta.id.clone(),
        ))
        .unwrap();
        a.insert_edge(&GraphEdge::new(
            EdgeType::Calls,
            beta.id.clone(),
            gamma.id.clone(),
        ))
        .unwrap();

        let b_path = tempfile::tempdir().unwrap().path().join("b.bin");
        let b = GraphDatabase::new(&b_path).unwrap();
        b.upsert_node(alpha.clone()).unwrap();
        b.upsert_node(beta.clone()).unwrap();
        b.upsert_node(gamma.clone()).unwrap();
        b.insert_edge(&GraphEdge::new(
            EdgeType::Calls,
            beta.id.clone(),
            gamma.id.clone(),
        ))
        .unwrap();
        b.insert_edge(&GraphEdge::new(
            EdgeType::Calls,
            alpha.id.clone(),
            beta.id.clone(),
        ))
        .unwrap();

        assert_eq!(
            canonical_digest_hex(&a),
            canonical_digest_hex(&b),
            "edge insertion order must not affect the digest"
        );
    }

    #[test]
    fn fixture_parser_round_trip() {
        let db = empty_db();
        let text = render_digest_fixture(&db);
        let parsed = ParsedDigestFixture::parse(&text).unwrap();
        assert_eq!(parsed.analyzer_rev, super::super::CONTRACT_ANALYZER_REV);
        assert_eq!(parsed.digest, canonical_digest_hex(&db));
    }

    #[test]
    fn fixture_parser_rejects_garbage() {
        let err = ParsedDigestFixture::parse("nope\n").unwrap_err();
        assert!(err.contains("unrecognized"), "got {err}");
        let err = ParsedDigestFixture::parse("analyzer_rev: 1\n").unwrap_err();
        assert!(err.contains("missing digest"), "got {err}");
        let err = ParsedDigestFixture::parse("digest: deadbeef\n").unwrap_err();
        assert!(err.contains("missing analyzer_rev"), "got {err}");
    }

    #[test]
    fn prefix16_is_first_32_hex() {
        let db = empty_db();
        let full = canonical_digest_hex(&db);
        let prefix = canonical_digest_prefix16(&db);
        assert_eq!(prefix, &full[..32]);
    }
}
