# Anchor Hub Scoring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `find_anchors` rank orchestration hubs (called by many + calls many + real body) above trivial helpers like `as_str`.

**Architecture:** Single-function change in `GraphDatabase::calculate_anchor_scores` (`src/server/graph.rs`): score only `Function`/`Method` nodes, count only `Calls` edges for the score, multiply by a body-size factor, keep the existing percentile-to-100 normalization. Spec: `docs/superpowers/specs/2026-08-21-anchor-hub-scoring-design.md`.

**Tech Stack:** Rust, petgraph `StableGraph`, existing `GraphDatabase` unit-test idiom (`#[cfg(test)] mod` inside `graph.rs`).

## Global Constraints

- `fan_in`/`fan_out` node fields keep counting ALL edge types (display data, unchanged).
- Score scale stays 0–100 (top of corpus = 100) so `search.rs` composition is unaffected.
- Formula: `raw = calls_in * (2 + calls_out).log2() * min(1, body_lines / 8)`, where `calls_in`/`calls_out` count only `EdgeType::Calls`, `body_lines = line_end - line_start + 1` (missing → 1).
- Only `NodeType::Function` / `NodeType::Method` get nonzero scores.
- cargo is NOT on PATH: prefix every cargo command with `export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH" &&`.

---

### Task 1: Hub scoring in `calculate_anchor_scores`

**Files:**
- Modify: `src/server/graph.rs:833-889` (the `calculate_anchor_scores` function)
- Modify: `src/server/graph.rs` (append new `#[cfg(test)] mod anchor_hub_tests` after the existing test modules, ~line 1350)

**Interfaces:**
- Consumes: `GraphNode { node_type, line_start, line_end, anchor_score, fan_in, fan_out, .. }`, `GraphEdge::new(EdgeType::Calls, source_id, target_id)`, `GraphDatabase::{insert_nodes_batch, upsert_edge, get_node, find_anchors, calculate_anchor_scores}` — all already exist.
- Produces: unchanged signatures. `anchor_score` semantics change: hubs rank above trivial helpers; non-function nodes score 0.

- [ ] **Step 1: Write the failing tests**

Append to `src/server/graph.rs` (after the last `#[cfg(test)]` module):

```rust
#[cfg(test)]
mod anchor_hub_tests {
    use super::*;

    fn db(name: &str) -> GraphDatabase {
        let tmp = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&tmp);
        GraphDatabase::new(&tmp).unwrap()
    }

    fn func(name: &str, path: &str, lines: (u32, u32)) -> GraphNode {
        let mut n = GraphNode::new(NodeType::Function, name.into(), path.into());
        n.line_start = Some(lines.0);
        n.line_end = Some(lines.1);
        n
    }

    /// A trivial 1-line helper with 20 callers must rank BELOW a
    /// 30-line hub with 5 callers and 10 callees. This is the
    /// `as_str` problem: the old fan_in/(fan_out+1) formula put
    /// the helper on top; hub scoring must not.
    #[test]
    fn hub_outranks_trivial_helper() {
        let g = db("lain_test_anchor_hub");
        let helper = func("as_str", "src/util.rs", (10, 10));
        let hub = func("orchestrate", "src/core.rs", (1, 30));
        let mut nodes = vec![helper.clone(), hub.clone()];
        let mut edges = Vec::new();
        for i in 0..20 {
            let caller = func(&format!("caller{i}"), "src/a.rs", (1, 10));
            edges.push(GraphEdge::new(EdgeType::Calls, caller.id.clone(), helper.id.clone()));
            nodes.push(caller);
        }
        for i in 0..5 {
            let caller = func(&format!("hubcaller{i}"), "src/b.rs", (1, 10));
            edges.push(GraphEdge::new(EdgeType::Calls, caller.id.clone(), hub.id.clone()));
            nodes.push(caller);
        }
        for i in 0..10 {
            let callee = func(&format!("callee{i}"), "src/c.rs", (1, 10));
            edges.push(GraphEdge::new(EdgeType::Calls, hub.id.clone(), callee.id.clone()));
            nodes.push(callee);
        }
        g.insert_nodes_batch(&nodes).unwrap();
        for e in edges {
            g.upsert_edge(e).unwrap();
        }

        g.calculate_anchor_scores().unwrap();

        let helper_score = g.get_node(&helper.id).unwrap().unwrap().anchor_score.unwrap();
        let hub_score = g.get_node(&hub.id).unwrap().unwrap().anchor_score.unwrap();
        assert!(
            hub_score > helper_score,
            "hub ({hub_score}) should outrank trivial helper ({helper_score})"
        );
        assert_eq!(hub_score, 100.0, "hub is the corpus max, normalizes to 100");
    }

    /// Types/structs/namespaces never rank as anchors — the handler
    /// filters them out anyway, so the scorer aligns with display.
    #[test]
    fn non_functions_score_zero() {
        let g = db("lain_test_anchor_nonfn");
        let s = GraphNode::new(NodeType::Struct, "Config".into(), "src/cfg.rs".into());
        let caller = func("use_cfg", "src/a.rs", (1, 10));
        let edge = GraphEdge::new(EdgeType::Calls, caller.id.clone(), s.id.clone());
        let sid = s.id.clone();
        g.insert_nodes_batch(&[s, caller]).unwrap();
        g.upsert_edge(edge).unwrap();

        g.calculate_anchor_scores().unwrap();

        let score = g.get_node(&sid).unwrap().unwrap().anchor_score.unwrap();
        assert_eq!(score, 0.0, "struct must score 0");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH" && cargo test --lib anchor_hub_tests 2>&1 | tail -20`
Expected: FAIL — `hub_outranks_trivial_helper` panics because the old formula ranks the helper first (raw 20/(1+1)=10 vs hub 5/(10+1)=0.45), and `non_functions_score_zero` fails because the struct gets a nonzero score.

- [ ] **Step 3: Replace the raw-score computation in pass 1**

In `src/server/graph.rs`, inside `calculate_anchor_scores`, replace pass 1 (the loop that computes `raw = fan_in / (fan_out + 1.0)`) with:

```rust
        // Pass 1: compute raw hub scores, find max.
        //
        // Hub semantics (spec 2026-08-21-anchor-hub-scoring-design):
        // an anchor is an ORCHESTRATION hub — called by many
        // (calls_in), coordinating many (calls_out), with a real
        // body (size_factor). Only Calls edges count: the old
        // fan_in/(fan_out+1) counted every edge type (including
        // Contains from the parent file) and actively punished
        // fan_out, which is backwards for hubs — it put 1-line
        // helpers like `as_str` at the top of find_anchors.
        let mut max_raw: f32 = 0.0;
        let mut raws: Vec<(petgraph::graph::NodeIndex, f32)> = Vec::with_capacity(indices.len());
        for idx in &indices {
            let node = &graph[*idx];
            let raw = match node.node_type {
                NodeType::Function | NodeType::Method => {
                    let calls_in = graph
                        .edges_directed(*idx, Direction::Incoming)
                        .filter(|e| e.weight().edge_type == EdgeType::Calls)
                        .count() as f32;
                    let calls_out = graph
                        .edges_directed(*idx, Direction::Outgoing)
                        .filter(|e| e.weight().edge_type == EdgeType::Calls)
                        .count() as f32;
                    let body_lines = match (node.line_start, node.line_end) {
                        (Some(s), Some(e)) => e.saturating_sub(s) as f32 + 1.0,
                        _ => 1.0,
                    };
                    let size_factor = (body_lines / 8.0).min(1.0);
                    // `2 +` inside the log so calls_out=0 keeps factor 1
                    // instead of zeroing the score.
                    calls_in * (2.0 + calls_out).log2() * size_factor
                }
                _ => 0.0,
            };
            if raw > max_raw {
                max_raw = raw;
            }
            raws.push((*idx, raw));
        }
```

Also update the stale comment block above pass 1 (lines ~837-854) that describes the old `fan_in/(fan_out+1)` formula: keep the normalization rationale (0–100 scale, stable rankings, composes with `search.rs`), drop the description of the old raw formula, and point at the spec for the hub semantics.

Pass 2 (recompute all-edge fan_in/fan_out for display, normalize `raw / max_raw * 100.0`, write back) is UNCHANGED.

- [ ] **Step 4: Run tests to verify they pass**

Run: `export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH" && cargo test --lib anchor_hub_tests 2>&1 | tail -10`
Expected: PASS — 2 tests ok. (hub raw = 5 × log2(12) × 1.0 ≈ 17.9 → 100.0; helper raw = 20 × log2(3) × 0.125 ≈ 3.96 → ≈22.1.)

- [ ] **Step 5: Full suite**

Run: `export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH" && cargo test 2>&1 | grep -cE "test result: ok"; echo "exit: ${PIPESTATUS[0]}"`
Expected: every line `test result: ok`, exit 0. Watch for tests that assert on specific anchor values/order — fix only if the assertion encoded the OLD formula's degenerate behavior (e.g. expecting `as_str` on top); such assertions are the bug, not the spec.

- [ ] **Step 6: Live verification on this repo**

```bash
export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo build --release 2>&1 | tail -1
rm -rf /tmp/lain-clone2 && git clone -q /home/sebastian/lain /tmp/lain-clone2
cd /tmp/lain-clone2 && git -C /tmp/lain-clone2 checkout -q HEAD
XDG_STATE_HOME=/tmp/lain-hub-state /home/sebastian/lain/target/release/lain oneshot find_anchors
# then force staleness so the re-index recomputes scores, and query again:
echo "// reindex trigger" >> README.md && git add -A && git commit -qm trigger
XDG_STATE_HOME=/tmp/lain-hub-state /home/sebastian/lain/target/release/lain oneshot find_anchors
```

Expected: after the re-index completes (may need a second invocation a few seconds later, since re-index runs in background), the top anchors are hub-like functions (real bodies, outbound calls) — NOT `as_str` / `default` / `parse` one-liners dominating. If the first call still shows the old ranking, wait ~10s for the background re-index and call again.

- [ ] **Step 7: Commit**

```bash
git add src/server/graph.rs
git commit -m "Anchor scoring: rank orchestration hubs above trivial helpers

raw = calls_in * log2(2 + calls_out) * min(1, body_lines/8) over Calls
edges only, Functions/Methods only. The old fan_in/(fan_out+1) counted
every edge type and punished fan_out, putting 1-line helpers (as_str,
default, parse) at the top of find_anchors — dead ends for the
find_anchors → get_blast_radius workflow. Spec: docs/superpowers/specs/
2026-08-21-anchor-hub-scoring-design.md"
git push
```
