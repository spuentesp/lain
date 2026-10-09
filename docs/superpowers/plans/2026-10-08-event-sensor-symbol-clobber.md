# Event-Sensor Symbol Clobber Plan

> **STATUS: DONE (shipped as `adccbf8f..cae035ac`, plus `58b4aa5d`).**
> This plan has been executed. Do **not** implement it again. The
> checkboxes are left unchecked to preserve the plan as written, and
> some step snippets show the *pre-fix* code they were written against —
> in particular Task 2 Step 3 below, which emits a `TopicConsumer` fact
> for every `SiteKind` and de-dupes on `source_id`. Re-applying it would
> reintroduce the producer-emits-consumer defect fixed in `58b4aa5d`
> ("a producer is not a consumer, and one fact per site"). The shipped
> form is in `src/server/sensors/event_sensor.rs::emit_sites`. Task 1's
> claim that the repro test is `#[ignore]`d is also out of date: it is
> not, and it is green.
>
> For agentic workers: this file is now historical reference. If you
> were asked to execute it, stop and report that it is already done.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop `event_sensor`'s rescan from deleting a peer sensor's edges by moving `TopicConsumer` off the shared symbol node, so `ReadsTable` and every other symbol-incident edge survives `run_all`.

**Architecture:** The root cause is not the one first suspected. `replace_sensor_output` step 2 removes an owner's nodes **and their incident edges** (`graph.remove_node`, `src/server/graph/mod.rs:896`). `event_sensor` upserts `TopicConsumer` onto the *enclosing symbol node* (`event_sensor.rs:586`), which makes that symbol `SensorOwner::EventSensor`. On the next `run_all`, `event_sensor`'s rescan retracts the symbol and with it every edge another sensor attached to it — including `sql_sensor`'s `ReadsTable`. The fix is the one already proven in this codebase three times (`rpc-call:`, `graphql-call:`, `sql-read:`): **never put a sensor's fact on a node another sensor owns**. Give `TopicConsumer` its own synthetic node.

**Tech Stack:** Rust 2021, `inventory`, `petgraph`. Oracle: `cargo test`. Mutation verification by hand (apply → run → revert).

**Spec:** `docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` — invariant **I2** (every discovered call lands in exactly one terminal state) and **I3** (never claim absent when unanalyzed). Also `docs/superpowers/plans/2026-10-07-contract-soundness.md` Review Focus #1: "`could_match` returning `false` is the dangerous direction" — its analogue here is "an analysed consumer silently disappearing is the dangerous direction".

## Global Constraints

- **Never claim absent when unanalyzed.** A consumer that was discovered and then deleted by a rescan is the worst failure in this codebase: `NoKnownImpact` becomes claimable while a real reader exists.
- **`replace_sensor_output(owner, …)` deletes every node with that owner *and its incident edges*.** Any node shared between two sensors is a landmine. The invariant to preserve: *a node whose `contract` is set must be owned by exactly one sensor, and that sensor must be the only one that retracts it.*
- **No destructive git.** Never `git reset --hard`, `git checkout --`, `git stash`, `git clean`, `git restore`. **Never launch a background or detached process** (no `nohup`/`setsid`/`&`) — five agents have corrupted production code that way. Run everything in the **foreground**; revert any hand-applied mutation **by editing the line back**.
- **Never run two `cargo` invocations in parallel** — they contend on the build lock and on `/tmp` and produce false failures.
- T1 ground truth must stay at precision **and** recall 1.0 (`PR13_METRICS_JSON`, all six metrics). A panic there is a soundness regression, not a test to update.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` clean at every commit.

## Review Focus

1. **`upsert_node`'s hydration guard can hide the bug.** `graph/mod.rs:414-418` skips the replace when `!node.is_hydrated && existing_hydrated`. A test that only exercises `upsert_node` will pass while `replace_sensor_output` (which *deletes first*) still destroys the edge. The reproduction must drive `run_all` — the delete-then-insert path.
2. **The fix must not make the symbol sensor-retractable.** If `TopicConsumer` stays on the symbol and `sensor_owner_of` keeps mapping it to `EventSensor`, the symbol is deleted on every rescan. But the *migration guard* matters too: pre-fix graphs carry `TopicConsumer` on real symbol nodes, and a naive owner arm would let `event_sensor`'s rescan **delete real function nodes** from those graphs. Both directions need a test.
3. **Edges are the lossy part, not the fact.** The fact comes back (event re-inserts it); the *edges* attached to the retracted node do not, because they are not in the retractor's `edges` slice. Any test that only asserts "the fact survived" passes while the bug is live.
4. **`enclosing_symbol`'s two-bound requirement is a second, separate trap.** It filters `(Some(s), Some(e))` (`util.rs:466-468`). A synthetic node with `line_end: None` is invisible to it — which is deliberate and load-bearing, so it cannot win the `min_by` tie-break and re-create a shared-id collision. Pin it; it was previously pinned only by a comment.
5. **Other sensors may write onto symbol nodes too.** `websocket_sensor`, `field_access_sensor` and the graphql/grpc families all set `.contract` on nodes. Only some use synthetic ids. Any that share a symbol id have the same bug — Task 3 must audit them rather than assume `event_sensor` is the only one.

---

## File Structure

| File | Responsibility after this plan |
|---|---|
| `src/server/sensors/event_sensor.rs` | Topic detection; emits `TopicConsumerFact` on a **synthetic** node (new), `Produces`/`Consumes` edges still on the enclosing symbol |
| `src/server/graph/mod.rs` | `sensor_owner_of` — name-guarded `TopicConsumer` arm, mirroring `TableConsumer` |
| `src/server/sensors/util.rs` | Shared synthetic-node prefix constants (new) — one source of truth for `sql-read:`, `topic-read:` |
| `tests/sensor_coexistence.rs` | Two-scan and retraction tests; un-ignores `a_rescan_keeps_reads_table_on_the_enclosing_function_second_scan` |

---

### Task 1: Pin the bug — the two-scan test must go RED

The reproduction already exists but is `#[ignore]`d with a **wrong** diagnosis. The comment blames `line_end` destruction; the real cause is incident-edge deletion. Fix the test so it fails for the right reason and pin the two halves of the mechanism separately.

**Files:**
- Modify: `tests/sensor_coexistence.rs` (the `#[ignore]`d test and its doc comment)

**Interfaces:**
- Consumes: `tests/sensor_coexistence.rs`'s existing helpers `workspace`, `write`, `facts`, and the `run_all` import.
- Produces: `a_rescan_keeps_reads_table_on_the_enclosing_function_second_scan` un-ignored and RED; a second test `a_topic_consumer_on_a_symbol_node_makes_it_sensor_retractable` pinning the ownership half.

- [ ] **Step 1: correct the doc comment** on `a_rescan_keeps_reads_table_on_the_enclosing_function_second_scan`. Replace the `line_end` theory with the real mechanism, so the next reader is not sent down the wrong path:

```rust
/// The SECOND-scan form is currently blocked by a real defect:
/// `replace_sensor_output` step 2 removes an owner's nodes **and their
/// incident edges** (`graph/mod.rs:896`, `remove_node`). `event_sensor`
/// upserts `TopicConsumer` onto the enclosing symbol node
/// (`event_sensor.rs:586`), which makes that symbol
/// `SensorOwner::EventSensor`. On the next `run_all`, event's rescan
/// retracts the symbol and with it every edge another sensor attached
/// to it — `sql_sensor`'s `ReadsTable` included. The fact comes back
/// (event re-inserts it); the edges do not, because they are not in
/// event's `edges` slice.
///
/// Not a `line_end` problem: `resolve_function_id`
/// (`event_sensor.rs:125`) returns the existing symbol's id, and
/// `upsert_node`'s hydration guard (`graph/mod.rs:416`) skips the
/// replace for an unhydrated node onto a hydrated one. The destroy path
/// is the delete-then-insert of `replace_sensor_output`, which
/// `upsert_node` alone never exercises.
///
/// Removing the `#[ignore]` is the acceptance criterion for the fix.
```

- [ ] **Step 2: write the ownership test** (new, immediately above it):

```rust
/// The other half of the same bug: a symbol node carrying `TopicConsumer`
/// is owned by `EventSensor`, so `event_sensor`'s rescan is entitled to
/// delete it — symbol record and every incident edge with it.
#[test]
fn a_topic_consumer_on_a_symbol_node_makes_it_sensor_retractable() {
    use lain::graph::sensor_owner_of;
    let mut n = lain::schema::GraphNode::new(NodeType::Function, "job".into(), "src/jobs.py".into());
    n.contract = Some(ContractFact::TopicConsumer(TopicConsumerFact {
        broker: "kafka".into(),
        name: "orders.created".into(),
        kind: TopicConsumerKind::Subscription,
    }));
    assert_eq!(
        sensor_owner_of(&n),
        Some(SensorOwner::EventSensor),
        "BUG: today the symbol is EventSensor-owned and gets deleted with \
         its edges. After Task 2 this must be None (or a synthetic-node \
         owner) — this test is the mechanism half of the reproduction."
    );
}
```

Import `TopicConsumerFact` / `TopicConsumerKind` from `crate`'s contracts model the way `tests/sensor_coexistence.rs` already imports `TableConsumerFact` — read the file's existing `use` block and match it.

- [ ] **Step 3: run both and confirm they behave as designed.**

Run: `cargo test --quiet --test sensor_coexistence -- --nocapture 2>&1 | tail -25`
Expected: `a_topic_consumer_on_a_symbol_node_makes_it_sensor_retractable` **PASSES** (it asserts today's buggy state — it is the mechanism pin, and it must fail after Task 2 flips the mapping); `a_rescan_keeps_reads_table_on_the_enclosing_function_second_scan` **FAILS** with `round 1: no ReadsTable edge`.

- [ ] **Step 4: commit.**

```bash
git add tests/sensor_coexistence.rs
git commit -m "test: pin the event_sensor symbol-clobber mechanism (RED)"
```

---

### Task 2: `TopicConsumer` gets a synthetic node

Mirror `sql-read:` exactly — the pattern this codebase has already used three times and whose trade-offs are understood.

**Files:**
- Modify: `src/server/sensors/event_sensor.rs:570-598` (`emit_sites`'s consumer branch)
- Modify: `src/server/graph/mod.rs:179-188` (the `TopicConsumer` arm of `sensor_owner_of`)
- Modify: `src/server/sensors/util.rs` (shared prefix constant)

**Interfaces:**
- Consumes: `resolve_function_id` (`event_sensor.rs:118`) — keep using it for `source_id`, since the `Produces`/`Consumes` edges still ride the enclosing symbol.
- Produces: `pub const TOPIC_READ_PREFIX: &str = "topic-read:"` in `util.rs`, and `pub const SQL_READ_PREFIX: &str = "sql-read:"` beside it; `sensor_owner_of` maps `TopicConsumer` → `EventSensor` **only** when `node.name.starts_with(TOPIC_READ_PREFIX)`.

- [ ] **Step 1: add the shared constants** in `src/server/sensors/util.rs`, next to the existing shared emitters (`emit_graphql_field_refs`, ~`:507`):

```rust
/// Prefix for synthetic nodes carrying a per-site consumer fact.
///
/// A sensor must never put its `ContractFact` on a node another sensor
/// owns — `replace_sensor_output` step 2 deletes an owner's nodes *and
/// their incident edges* (`graph/mod.rs:896`), so a shared symbol node
/// means one sensor's rescan silently destroys another's edges. Each
/// protocol that annotates a source site gets its own synthetic node
/// named `<prefix><path>:<line>`.
pub const SQL_READ_PREFIX: &str = "sql-read:";
/// See [`SQL_READ_PREFIX`]. Carries `TopicConsumerFact`.
pub const TOPIC_READ_PREFIX: &str = "topic-read:";
```

- [ ] **Step 2: point `sql_sensor` and `event_sensor` at them.** In `sql_sensor.rs` replace the literal `format!("sql-read:{graph_path_str}:{}", site.line)` with `format!("{}{graph_path_str}:{}", crate::server::sensors::util::SQL_READ_PREFIX, site.line)`, and likewise `graph/mod.rs:186`'s `node.name.starts_with("sql-read:")`. Same for `event_sensor`'s new prefix. Six literal sites total (grep `"sql-read:"` and confirm).

- [ ] **Step 3: the emission** in `event_sensor.rs`'s consumer branch. Replace the shared-symbol node with a synthetic one; **keep the `Produces`/`Consumes` edges on `source_id`** so call-chain traversal is unchanged:

> **Do not apply this snippet as written.** It emits a `TopicConsumer`
> fact for *every* `SiteKind` and de-dupes on `source_id`. The shipped
> code guards the fact to `Consumes`/`Scheduled` only, de-dupes per
> site, and emits the synthetic node whenever it is the edge anchor *or*
> the fact carrier. See `emit_sites` in
> `src/server/sensors/event_sensor.rs`. It is kept here only to show
> what was replaced.

```rust
        if emitted_consumer_facts.insert(source_id.clone()) {
            // Synthetic node — NOT `source_id`. `replace_sensor_output`
            // step 2 deletes an owner's nodes and their incident edges;
            // putting `TopicConsumer` on the enclosing symbol made that
            // symbol `EventSensor`-owned and cost every peer sensor its
            // edges on the next scan (see `util::SQL_READ_PREFIX`).
            let id_name = format!(
                "{}{graph_path_str}:{}",
                crate::server::sensors::util::TOPIC_READ_PREFIX,
                site.line
            );
            let id = GraphNode::generate_id(
                &NodeType::Function,
                graph_path_str,
                &id_name,
                Some(site.line),
                namespace,
            );
            let mut consumer_node =
                GraphNode::new(NodeType::Function, id_name, graph_path.to_string());
            consumer_node.id = id;
            consumer_node.line_start = Some(site.line);
            // `line_end` stays `None` deliberately: `util::enclosing_symbol`
            // requires both bounds, so no later scan can resolve *this*
            // node as an enclosing symbol and re-create a shared-id
            // collision. Same rule as `sql-read:`.
            let kind = match site.kind {
                SiteKind::Scheduled => TopicConsumerKind::Scheduled,
                _ => TopicConsumerKind::Subscription,
            };
            consumer_node.contract = Some(CF::TopicConsumer(TopicConsumerFact {
                broker: site.broker.clone(),
                name: topic_name.clone(),
                kind,
            }));
            nodes.push(consumer_node);
        }
```

Note `emitted_consumer_facts` still keys on `source_id` (one consumer fact per enclosing source) while the node id is per-site — if you prefer one fact per site like `sql-read:` does, key the set on `id_name` instead and say so in your report. Either is defensible; be consistent and state the choice.

- [ ] **Step 4: the owner guard** in `graph/mod.rs`. Replace the unguarded `TopicConsumer` arm (added in `9efcfd06`) with the name-guarded shape `TableConsumer` already uses:

```rust
        // The event sensor owns its synthetic `topic-read:` consumer
        // nodes. The name guard is the migration arm: pre-fix graphs
        // carry `TopicConsumer` on the real symbol node, and retracting
        // those would delete real function nodes (and their edges) from
        // an operator's graph.
        (_, Some(ContractFact::TopicConsumer(_)))
            if node.name.starts_with(crate::server::sensors::util::TOPIC_READ_PREFIX) =>
        {
            Some(SensorOwner::EventSensor)
        }
```

- [ ] **Step 5: run and verify GREEN.**

Run: `cargo test --quiet --test sensor_coexistence -- --nocapture 2>&1 | tail -20`
Expected: **all pass**, including the two from Task 1. `a_topic_consumer_on_a_symbol_node_makes_it_sensor_retractable` must now FAIL if you left its assertion as written — **update it to assert the fixed state** (`sensor_owner_of(&n) == None`) in this step, and note the flip in your report. That flip is the proof the mapping changed.

- [ ] **Step 6: prove both halves kill their mutant.** Hand-apply each and watch the test fail, then revert **by editing the line back**:
  1. Set `consumer_node.id = source_id.clone();` again → `a_rescan_keeps_reads_table_on_the_enclosing_function_second_scan` must FAIL.
  2. Remove the `if node.name.starts_with(TOPIC_READ_PREFIX)` guard → `a_topic_consumer_on_a_symbol_node_makes_it_sensor_retractable` must FAIL.

  Report both observed failures. A test that cannot fail is not evidence.

- [ ] **Step 7: full verification.**

Run, **sequentially**:
`cargo test --quiet --lib 'federation::contracts'` · `cargo test --quiet --test sensor_coexistence` · `cargo test --quiet --test contract_tool_e2e_non_http` · `cargo test --quiet --test sql_tables` · `cargo test --quiet --test federation_contracts_e2e 2>&1 | tail -8` (T1 all six metrics 1.0) · `cargo fmt --check` · `cargo clippy --all-targets -- -D warnings`

- [ ] **Step 8: commit.**

```bash
git add src/server/sensors/event_sensor.rs src/server/sensors/sql_sensor.rs src/server/sensors/util.rs src/server/graph/mod.rs tests/sensor_coexistence.rs
git commit -m "fix(sensors): TopicConsumer moves to a synthetic node; peer edges survive rescan"
```

---

### Task 3: Audit every remaining shared-symbol writer

Review Focus #5. The bug class is "a sensor sets `contract` on a node it does not exclusively own". `event_sensor` was found by accident; find the rest deliberately.

**Files:**
- Modify: `tests/sensor_coexistence.rs` (one test per collision pair found)
- Possibly: the offending sensors — **only if a collision is confirmed**

**Interfaces:**
- Consumes: `sensor_owner_of` (`graph/mod.rs:118-207`) and the per-sensor `replace_sensor_output` call sites.
- Produces: a coexistence test per confirmed collision pair; a written list of pairs checked and found clean.

- [ ] **Step 1: enumerate the writers.** The grep below lists every site that sets a `ContractFact` on a node. For each, answer two questions and write the answers in your report as a table: (a) is the node id **synthetic** (a prefix like `rpc-call:` / `sql-read:` / `topic-read:`) or is it a symbol id from `util::enclosing_symbol`? (b) if a symbol, does any *other* sensor also write to or attach edges at that id?

```bash
rg -n "\.contract = Some\(ContractFact::" src/server/sensors/*.rs
rg -n "replace_sensor_output" src/server/sensors/*.rs
```

Candidates to check explicitly (from the earlier grep): `grpc_provider_sensor.rs:200,218,246`, `websocket_sensor.rs:173,203,227`, `grpc_handler_link_sensor.rs:110`, `util.rs:772` (the shared `emit_graphql_field_refs`), `grpc_consumer_sensor.rs:119`, `graphql_resolver_link_sensor.rs:121`, `graphql_provider_sensor.rs:113,133`, `sql_sensor.rs:1038,1062`, `field_access_sensor.rs` (grep for it).

- [ ] **Step 2: write one coexistence test per confirmed collision**, in the shape of `a_topic_and_sql_reader_keeps_both_consumer_facts`. The likeliest one — `websocket_sensor` putting `WebSocketConsumer` on an `HttpClientCall` node that `http_client_sensor` also emits — is written out below; do the same for each pair Step 1 finds:

```rust
/// `websocket_sensor` writes `WebSocketConsumer` onto a `HttpClientCall`
/// node; `http_client_sensor` emits `HttpClientCall` nodes too. If they
/// share a node id, whichever runs later costs the other its edges.
#[test]
fn websocket_and_http_client_both_survive_run_all() {
    let ws = workspace("ws_and_http_client");
    write(&ws, "src/api.ts", "fetch(\"https://x.test/a\").then(r => r.json());\n");
    write(&ws, "src/feed.ts", "const s = new WebSocket(\"ws://x.test/feed\");\n");
    let graph = scan(&ws);

    let ws_facts = facts(&graph, |c| matches!(c, ContractFact::WebSocketConsumer(_))).len();
    let http_nodes = nodes_of(&graph, NodeType::HttpClientCall).len();
    assert!(ws_facts > 0, "the WebSocketConsumer fact was clobbered");
    assert!(
        http_nodes >= 2,
        "expected the plain HTTP call to survive alongside the WS dial, got {http_nodes} nodes"
    );
    // …and both their edges survive, which is the lossy half.
    let sends = graph.all_edges().into_iter()
        .filter(|e| e.edge_type == lain::schema::EdgeType::SendsHttp).count();
    assert!(sends >= 2, "SendsHttp edges were lost to a peer retraction: {sends}");
}
```

- [ ] **Step 3: run each and confirm it FAILS before touching production code.** A test that passes immediately is not pinning the collision — say so explicitly in your report if a suspected pair turns out not to collide, and record it as clean.

- [ ] **Step 4: fix each confirmed collision with a synthetic node**, the same shape as Task 2 — one prefix per protocol, declared in `util.rs` next to `SQL_READ_PREFIX` / `TOPIC_READ_PREFIX`, and a name-guarded `sensor_owner_of` arm with a migration guard. If a "collision" turns out to be two sensors writing to *different* node ids, record it as clean and move on.

- [ ] **Step 5: verify** the same command list as Task 2 Step 7, plus `cargo test --quiet --lib 'graph'` and `cargo test --quiet --lib 'sensors'`.

- [ ] **Step 6: commit.**

```bash
git add src/server/sensors/ src/server/graph/mod.rs tests/sensor_coexistence.rs
git commit -m "fix(sensors): no sensor shares a symbol node with another (audit + fixes)"
```

---

### Task 4: Document the invariant

The rule that prevented this is unwritten. Five agents have now shipped variants of the same bug (fact clobber, edge clobber, ownership landmine). Put it where the next person will read it.

**Files:**
- Modify: `src/server/sensors/AGENTS.md`
- Modify: `docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md`

- [ ] **Step 1: `src/server/sensors/AGENTS.md`** — add a section after the existing "The short version" bullets:

```markdown
## Never put a `ContractFact` on a node another sensor owns

`replace_sensor_output(owner, …)` removes every node whose
`sensor_owner_of` matches `owner` **and its incident edges**
(`graph/mod.rs:896`). So a node shared between two sensors means one
sensor's rescan silently deletes the other's edges — the fact comes
back, the edges do not.

Rule: a node carrying a `ContractFact` must be owned by exactly one
sensor, and that sensor must be the only one that retracts it.

- Per-site consumer facts ride a **synthetic node** named
  `<prefix><path>:<line>` — see `SQL_READ_PREFIX` / `TOPIC_READ_PREFIX`
  in `util.rs`. Never reuse the enclosing symbol's id.
- Synthetic nodes keep `line_end: None`. `util::enclosing_symbol`
  requires both bounds, so this is what stops a later scan from
  resolving the synthetic node as "the enclosing symbol" and re-creating
  the collision.
- `sensor_owner_of` arms for these facts are **name-guarded**, so a
  pre-fix graph whose symbol node carries the fact is not retracted —
  deleting it would take real function nodes and their edges with it.
```

- [ ] **Step 2: the spec** — append a clause to invariant **I2** in the invariant list (the block at `:42-52`):

```
   A node carrying a `ContractFact` is owned by exactly one sensor, and
   a rescan never removes a node or edge another sensor produced.
```

- [ ] **Step 3: verify** — `cargo test --quiet --test sensor_coexistence` still green (doc-only change must not move anything).

- [ ] **Step 4: commit.**

```bash
git add src/server/sensors/AGENTS.md docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md
git commit -m "docs: the one-fact-one-owner rule that keeps peer edges alive"
```

---

## Out of scope

- **`upsert_node`'s hydration guard** (`graph/mod.rs:414-418`) — it silently skips a replace and is its own source of confusion, but it is not the destroy path here and changing shared insert semantics is a different risk class.
- **Multi-fact nodes** (letting one node carry several `ContractFact`s). Attractive, but it is a `schema.rs` model change touching every consumer of `GraphNode.contract` and every bincode payload. Synthetic nodes solve this without it.
- **`tests/real_federation/*` fragility** (the fixture tracks upstream `master` tips) and the mutation harness's remaining `gap` survivors — separate concerns, tracked in `2026-10-07-mutation-credibility.md`.
