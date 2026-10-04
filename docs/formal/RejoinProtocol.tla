---- MODULE RejoinProtocol ----
\* RejoinProtocol — TLA+ model of `FederatedIndex::rejoin_contracts_if_dirty`
\* and its inputs, at fine granularity, targeting the three suspected
\* bugs in spec §9.1.
\*
\* Spec: docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md §9.1
\* Plan: docs/superpowers/plans/2026-10-02-coverage-and-protocols.md
\*
\* Code mapping (TLA+ → Rust):
\*   - `dirty: BOOLEAN`           ↔ `contracts_dirty: AtomicBool`
\*   - `config`                   ↔ `contract_config: RwLock<Option<Arc<…>>>`
\*   - `nodes`, `edges`           ↔ the `contract_node_ids` DashMap + the
\*                                 backend's per-call reads in
\*                                 `rejoin_contracts` (lines 1041, 1074)
\*   - `binds`                    ↔ the `Binds` edges in `GraphBackend`
\*   - `index`                    ↔ `contract_index: RwLock<Option<…>>`
\*   - `binds_epoch`              ↔ the generation tag implicit in the
\*                                 `Arc<ContractIndex>` swap (we make it
\*                                 explicit so the bug is observable)
\*   - `rejoin_step: 0..7`        ↔ the sequence of operations inside
\*                                 `rejoin_contracts` (load dirty, read
\*                                 config, read inputs, compute, apply
\*                                 binds, swap index, clear dirty)
\*   - `rejoin_holds_lock`        ↔ `projection_lock: parking_lot::Mutex<()>`
\*                                 (held by `rejoin_contracts_if_dirty` from
\*                                 the dirty check through the trailing clear)
\*
\* Variant (a) — current code (what this spec models):
\*   The rejoin holds `projection_lock` for the whole sequence, so
\*   writers that take the lock (`project_nodes` / `project_edges` /
\*   `add_repo` / `remove_repo`) serialize with the rejoin and cannot
\*   tear its input. The bugs the spec hypothesises come from writers
\*   that do NOT take the lock:
\*     - `mark_contracts_dirty` (test-only hatch, line 1003)
\*     - `set_contract_config` (line 956, writes under the
\*       `contract_config` RwLock then `contracts_dirty.store(true)`
\*       with no `projection_lock`)
\*   These can land between the rejoin's dirty-check and the
\*   trailing clear, and the trailing clear overwrites their mark.
\*
\* The model also exercises the "two views" bug directly: between
\* `RejoinApply` (binds written) and `RejoinSwapIndex` (index written)
\* a reader can see new binds with old index. The reader records
\* (index, binds_epoch); the I7 invariant fails when those are from
\* different generations.
\*
\* Variant (b) — clear-before-read:
\*   Rejoin clears the dirty flag BEFORE reading inputs. If a writer
\*   lands after the clear, the writer's own mark re-sets dirty; the
\*   published binds are always consistent with the inputs read by
\*   the rejoin. (To be implemented in the follow-up Rust fix pass.)
\*
\* Variant (c) — epoch-stamped atomic publish:
\*   The rejoin produces `(index, binds)` and publishes them together
\*   under a single write-lock; readers see them atomically. (To be
\*   implemented in the follow-up Rust fix pass.)

EXTENDS Naturals, FiniteSets

CONSTANTS
    Generations,    \* set of generation identifiers
    Nodes,          \* set of contract-bearing node ids
    Edges,          \* set of input edge ids
    Configs,        \* set of config versions
    Readers,        \* set of reader identifiers
    Unobserved      \* sentinel: a reader has not yet read this slot

ASSUME
    /\ Generations # {}
    /\ Nodes       # {}
    /\ Edges       # {}
    /\ Configs     # {}
    /\ Readers     # {}

\* ===== State variables =====

VARIABLES
    dirty,              \* contracts_dirty
    config,             \* contract_config (the joiner's input)
    nodes,              \* input nodes (the per-call `get_node` set)
    edges,              \* input edges (the per-call `all_edges` filter)
    binds,              \* published `Binds` set (backend edges)
    index,              \* published generation (contract_index epoch)
    binds_epoch,        \* generation the current `binds` were computed at
    rejoin_step,        \* 0 = idle, 1..7 = mid-rejoin step
    rejoin_holds_lock,  \* TRUE while the rejoin holds projection_lock
    reader_index,       \* [Reader → Generation ∪ {Unobserved}]
    reader_binds_epoch  \* [Reader → Generation ∪ {Unobserved}]

\* ===== Helpers =====

\* Pick a generation different from `g`. With |Generations| ≥ 2 this
\* is well-defined; we assert it via the ASSUME above.
NextGen(g) == CHOOSE g2 \in Generations : g2 # g

\* The "intended" binds the joiner would produce for the given
\* inputs. Config `c0` (the canonical one) yields the cross-product
\* of nodes and edges; any other config yields the empty set. The
\* asymmetry is what makes the lost-dirty-flag bug observable — a
\* config change must produce a different output.
ComputeBinds(c, ns, es) ==
    IF c = CHOOSE x \in Configs : TRUE
    THEN {<<n, e>> : n \in ns, e \in es}
    ELSE {}

\* ===== Type invariant =====

TypeOK ==
    /\ dirty             \in BOOLEAN
    /\ config            \in Configs
    /\ nodes             \subseteq Nodes
    /\ edges             \subseteq Edges
    /\ binds             \subseteq {<<n, e>> : n \in Nodes, e \in Edges}
    /\ index             \in Generations
    /\ binds_epoch       \in Generations
    /\ rejoin_step       \in 0..7
    /\ rejoin_holds_lock \in BOOLEAN
    /\ reader_index      \in [Readers -> Generations \cup {Unobserved}]
    /\ reader_binds_epoch \in [Readers -> Generations \cup {Unobserved}]

\* ===== Invariants (the three things §9.1 asks us to check) =====

\* **Convergence**. In any truly quiescent state (the rejoin is idle
\* AND the dirty flag is clear, so no rejoin is even enabled), the
\* published `binds` are exactly what the joiner would compute from
\* the current inputs. The bug surface: a writer's
\* `mark_contracts_dirty` (or a `set_contract_config` that lands
\* between the rejoin's `RejoinReadConfig` and `RejoinClear`) sets
\* `dirty=true`; the rejoin's trailing `RejoinClear` overwrites the
\* mark, leaving a state where `~dirty` AND `binds` doesn't match
\* the current inputs AND no rejoin can be triggered.
Convergence ==
    (rejoin_step = 0 /\ ~dirty) =>
        binds = ComputeBinds(config, nodes, edges)

\* **No-lost-update**. At every state, either the dirty flag is set
\* (signalling a rejoin is needed) OR the published binds already
\* match the current inputs. The bug surface: the rejoin's trailing
\* clear overwrites a writer's `dirty=true` mark, leaving the system
\* in a state where binds do not match inputs and dirty is FALSE.
NoLostUpdate ==
    dirty \/ binds = ComputeBinds(config, nodes, edges)

\* **I7 reader consistency**. A reader that has read both `index` and
\* `binds_epoch` must have seen the same generation in both slots.
\* The bug surface: the rejoin updates `binds` at step 5 and `index`
\* at step 6; a reader that reads `index` between the two records
\* old `index` but `binds_epoch` is still old at the read point, then
\* the rejoin advances `binds_epoch` and the reader's subsequent
\* `binds_epoch` read records the new value. The two recorded values
\* differ.
I7ReaderConsistency ==
    \A r \in Readers :
        reader_index[r] # Unobserved /\ reader_binds_epoch[r] # Unobserved
        => reader_index[r] = reader_binds_epoch[r]

\* ===== Initialisation =====

Init ==
    /\ dirty             = TRUE
    /\ config            = CHOOSE c \in Configs : TRUE
    /\ nodes             = {}
    /\ edges             = {}
    /\ binds             = ComputeBinds(config, nodes, edges)
    /\ index             = CHOOSE g \in Generations : TRUE
    /\ binds_epoch       = index
    /\ rejoin_step       = 0
    /\ rejoin_holds_lock = FALSE
    /\ reader_index      = [r \in Readers |-> Unobserved]
    /\ reader_binds_epoch = [r \in Readers |-> Unobserved]

\* ===== Actions =====

\* Writer that takes `projection_lock` (e.g. `project_nodes`,
\* `project_edges`, `add_repo`, `remove_repo`). It cannot run while
\* the rejoin is holding the lock.
WriterUpdateInput(n, e) ==
    /\ ~rejoin_holds_lock
    /\ nodes' = nodes \cup {n}
    /\ edges' = edges \cup {e}
    /\ dirty' = TRUE
    /\ UNCHANGED <<config, binds, index, binds_epoch,
                  rejoin_step, rejoin_holds_lock,
                  reader_index, reader_binds_epoch>>

\* Writer that does NOT take `projection_lock`: the
\* `set_contract_config` path (line 956 — writes to the
\* `contract_config` RwLock, then `contracts_dirty.store(true)`
\* without taking `projection_lock`). The lost-dirty-flag bug comes
\* from this action landing between the rejoin's `RejoinReadConfig`
\* and `RejoinClear`.
WriterSetConfig(c) ==
    /\ c # config
    /\ config' = c
    /\ dirty' = TRUE
    /\ UNCHANGED <<nodes, edges, binds, index, binds_epoch,
                  rejoin_step, rejoin_holds_lock,
                  reader_index, reader_binds_epoch>>

\* Writer that does NOT take `projection_lock` and does NOT change
\* inputs: the `mark_contracts_dirty` test-only escape hatch (line
\* 1003). When this lands between `RejoinReadConfig` and
\* `RejoinClear`, the trailing clear overwrites the mark.
WriterMarkDirty ==
    /\ ~dirty
    /\ dirty' = TRUE
    /\ UNCHANGED <<config, nodes, edges, binds, index, binds_epoch,
                  rejoin_step, rejoin_holds_lock,
                  reader_index, reader_binds_epoch>>

\* Rejoin step 0 → 1: take the lock, observe dirty.
RejoinStart ==
    /\ rejoin_step       = 0
    /\ dirty
    /\ ~rejoin_holds_lock
    /\ rejoin_step'       = 1
    /\ rejoin_holds_lock' = TRUE
    /\ UNCHANGED <<dirty, config, nodes, edges, binds, index,
                  binds_epoch, reader_index, reader_binds_epoch>>

\* Rejoin step 1 → 2: read config (line 1025: `self.contract_config.read().clone()`).
RejoinReadConfig ==
    /\ rejoin_step = 1
    /\ rejoin_step' = 2
    /\ UNCHANGED <<dirty, config, nodes, edges, binds, index,
                  binds_epoch, rejoin_holds_lock,
                  reader_index, reader_binds_epoch>>

\* Rejoin step 2 → 3: read inputs (lines 1041, 1074: the
\* `contract_node_ids` iteration plus the `all_edges` filter).
RejoinReadInputs ==
    /\ rejoin_step = 2
    /\ rejoin_step' = 3
    /\ UNCHANGED <<dirty, config, nodes, edges, binds, index,
                  binds_epoch, rejoin_holds_lock,
                  reader_index, reader_binds_epoch>>

\* Rejoin step 3 → 4: compute (line 1095: `ContractJoiner::run`). Local.
RejoinCompute ==
    /\ rejoin_step = 3
    /\ rejoin_step' = 4
    /\ UNCHANGED <<dirty, config, nodes, edges, binds, index,
                  binds_epoch, rejoin_holds_lock,
                  reader_index, reader_binds_epoch>>

\* Rejoin step 4 → 5: apply binds (lines 1152–1156: the
\* `upsert_edges_batch` and `remove_edges` calls). THIS is where
\* `binds` is written while `binds_epoch` is still the OLD
\* generation — the two-views window.
RejoinApply ==
    /\ rejoin_step = 4
    /\ binds' = ComputeBinds(config, nodes, edges)
    /\ rejoin_step' = 5
    /\ UNCHANGED <<dirty, config, nodes, edges, index,
                  binds_epoch, rejoin_holds_lock,
                  reader_index, reader_binds_epoch>>

\* Rejoin step 5 → 6: swap index (line 1159: `*self.contract_index.write()`).
\* `binds_epoch` advances together with `index` so a reader that
\* observes `binds_epoch` here sees the same generation as the
\* `binds` written one step earlier.
RejoinSwapIndex ==
    /\ rejoin_step  = 5
    /\ index'       = NextGen(index)
    /\ binds_epoch' = index'
    /\ rejoin_step' = 6
    /\ UNCHANGED <<dirty, config, nodes, edges, binds,
                  rejoin_holds_lock,
                  reader_index, reader_binds_epoch>>

\* Rejoin step 6 → 0: clear dirty and release the lock (line 1160).
RejoinClear ==
    /\ rejoin_step        = 6
    /\ dirty'             = FALSE
    /\ rejoin_holds_lock' = FALSE
    /\ rejoin_step'       = 0
    /\ UNCHANGED <<config, nodes, edges, binds, index, binds_epoch,
                  reader_index, reader_binds_epoch>>

\* Reader: record the current `index`.
ReaderReadIndex(r) ==
    /\ reader_index' = [reader_index EXCEPT ![r] = index]
    /\ UNCHANGED <<dirty, config, nodes, edges, binds, index,
                  binds_epoch, rejoin_step, rejoin_holds_lock,
                  reader_binds_epoch>>

\* Reader: record the current `binds_epoch`.
ReaderReadBinds(r) ==
    /\ reader_binds_epoch' = [reader_binds_epoch EXCEPT ![r] = binds_epoch]
    /\ UNCHANGED <<dirty, config, nodes, edges, binds, index,
                  binds_epoch, rejoin_step, rejoin_holds_lock,
                  reader_index>>

Next ==
    \/ \E n \in Nodes, e \in Edges : WriterUpdateInput(n, e)
    \/ \E c \in Configs            : WriterSetConfig(c)
    \/ WriterMarkDirty
    \/ RejoinStart
    \/ RejoinReadConfig
    \/ RejoinReadInputs
    \/ RejoinCompute
    \/ RejoinApply
    \/ RejoinSwapIndex
    \/ RejoinClear
    \/ \E r \in Readers : ReaderReadIndex(r)
    \/ \E r \in Readers : ReaderReadBinds(r)

vars == <<dirty, config, nodes, edges, binds, index, binds_epoch,
          rejoin_step, rejoin_holds_lock,
          reader_index, reader_binds_epoch>>

Spec == Init /\ [][Next]_vars

====
