# Anchor scoring: hub semantics for `find_anchors`

Date: 2026-08-21
Status: approved (design), pending implementation

## Problem

`anchor_score = fan_in / (fan_out + 1)` (percentile-normalized to 100)
rewards tiny utility functions with many callers. On the lain repo the
top anchors were `as_str`, `get`, `default`, `parse` — trivial helpers
that are dead ends for the headline workflow `find_anchors →
get_blast_radius`. Two compounding flaws:

1. The ratio penalizes fan_out, which is backwards for orchestration:
   real hubs call many things.
2. fan_in/fan_out count ALL edge types (`neighbors_directed`
   unfiltered), including `Contains` from the parent file.

## Goal

The top of `find_anchors` should list **orchestration hubs**: functions
that are called by many AND that coordinate many others, with a real
body. Approved direction (2026-08-21): option A — reweighted formula
over existing fields, no new indexing.

## Design

Single-function change in `GraphDatabase::calculate_anchor_scores`
(`src/server/graph.rs`), keeping the existing two-pass
compute-then-normalize structure:

- Only `NodeType::Function` / `NodeType::Method` get a nonzero score
  (aligns scoring with the handler-side filter in
  `tools/handlers/metrics.rs`).
- Symbols under test paths score 0 — test fixtures are hub-shaped but
  anchors are entry points into the product. Detection by path
  convention: a `tests/` directory component, or the `*_tests.rs` /
  `*_test.rs` / `tests.rs` file stems used for `#[cfg(test)]` modules
  under `src/` (live check: `make_test_graph` lives in
  `src/server/graph_tests.rs`). Calls edges with a test-path endpoint
  don't count toward fan-in/out either — fifty `test_*` callers don't
  make `Default::default` an orchestration hub. Inline `#[cfg(test)]`
  modules in regular src files are not detectable by path.
- `calls_in` / `calls_out`: incoming/outgoing edges restricted to
  `EdgeType::Calls`.
- `body_lines = line_end - line_start + 1`; missing line info → 1.
- `size_factor = min(1.0, body_lines / 8.0)`.
- `raw = calls_in * log2(1 + calls_out) * size_factor`
  - `log2(1 + calls_out)`: a leaf that calls nothing scores 0 — live
    verification showed `2 +` left pure utilities (`as_str`, 91
    callers, 0 callees) in the top 3.
- Percentile normalization to 100 over the corpus max, unchanged.

Worked example: `as_str` (calls_in 40, calls_out 1, 1 line) →
40 × 1.0 × 0.125 = 5. A pure leaf (calls_in 91, calls_out 0) → 0.
A hub (calls_in 8, calls_out 12, 40 lines) → 8 × 3.70 × 1.0 ≈ 29.6.
The hub wins.

## Explicitly unchanged

- `fan_in` / `fan_out` node fields keep counting all edge types
  (they are display/diagnostic data).
- Dedup by `(name, kind)` in `find_anchors`.
- The 0–100 scale and its composition with `search.rs` ranking.
- `VolatileOverlay` anchor merging in the handler.

## Edge cases

- Nodes without line ranges (LSP-only symbols) get body=1 → damped
  8×. Acceptable: they are a minority and symbols without bodies
  should not dominate a "where do I start reading" list.
- Empty graph → `max_raw = 0` → all scores 0 (same as today).

## Testing

- Unit test (synthetic graph): trivial helper (calls_in 20,
  calls_out 1, 1 line) must score BELOW a hub (calls_in 5,
  calls_out 10, 30 lines). Non-function nodes score 0.
- Live verification: `lain oneshot find_anchors` on the lain repo no
  longer shows `as_str` / `default` / `parse` in the top; top entries
  are functions with real bodies and outbound calls.
