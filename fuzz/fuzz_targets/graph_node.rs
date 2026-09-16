//! Fuzz target: parse arbitrary bytes as `GraphNode` (JSON).
//!
//! The schema's `GraphNode` is the on-disk JSON shape that the
//! server round-trips through when persisting the in-memory graph
//! to disk and back. A panic or unbounded allocation in the parser
//! would let an attacker who can write to the data dir break the
//! next server start. We fuzz `serde_json::from_slice::<GraphNode>`
//! on arbitrary input.

#![no_main]

use lain::server::schema::GraphNode;

#[libfuzzer_sys::fuzz_target]
fn fuzz_graph_node(data: &[u8]) {
    let _ = serde_json::from_slice::<GraphNode>(data);
}
