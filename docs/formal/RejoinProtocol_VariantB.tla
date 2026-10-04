---- MODULE RejoinProtocol_VariantB ----
\* RejoinProtocol_VariantB — TLA+ model of the variant-(b)
\* clear-before-read rejoin logic. Models the post-Fix 1 Rust
\* semantics:
\*   1. rejoin_contracts_if_dirty acquires projection_lock.
\*   2. Checks dirty; if TRUE, clears dirty BEFORE reading inputs.
\*   3. Calls rejoin_contracts: read config, read inputs,
\*      compute, apply binds, swap index.
\*   4. If a writer lands after the clear (e.g.,
\*      WriterSetConfig, WriterMarkDirty), the writer's mark
\*      re-arms dirty=TRUE. The next call to
\*      rejoin_contracts_if_dirty redoes the work.
\* The NoLostUpdate invariant: `dirty \/ binds =
\* ComputeBinds(config, nodes, edges)` — must hold at every
\* state. With clear-before-read, a writer that lands after
\* the clear sets dirty=TRUE, so the invariant is preserved.
\*
\* Spec: docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md §9.1
\* Code mapping (TLA+ → Rust):
\*   - `dirty: BOOLEAN`           ↔ `contracts_dirty: AtomicBool`
\*   - `config`                   ↔ `contract_config: RwLock<...>`
\*   - `nodes`, `edges`           ↔ the ↔ contract_node_ids + backend reads
\*   - `binds`                    ↔ backend `Binds` edges
\*   - `index`                    ↔ `contract_index: RwLock<...>`
\*   - `rejoin_holds_lock`        ↔ `projection_lock: parking_lot::Mutex<()>`

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

VARIABLES
    dirty,              \* contracts_dirty
    config,             \* contract_config
    nodes,              \* input nodes
    edges,              \* input edges
    binds,              \* published `Binds` set
    index,              \* published generation
    binds_epoch,        \* generation the current `binds` were computed at
    rejoin_step,        \* 0 = idle, 1..6 = mid-rejoin
    rejoin_holds_lock,  \* TRUE while the rejoin holds projection_lock
    reader_index,
    reader_binds_epoch

NextGen(g) == CHOOSE g2 \in Generations : g2 # g

ComputeBinds(c, ns, es) ==
    IF c = CHOOSE x \in Configs : TRUE
    THEN {<<n, e>> : n \in ns, e \in es}
    ELSE {}

TypeOK ==
    /\ dirty             \in BOOLEAN
    /\ config            \in Configs
    /\ nodes             \subseteq Nodes
    /\ edges             \subseteq Edges
    /\ binds             \subseteq {<<n, e>> : n \in Nodes, e \in Edges}
    /\ index             \in Generations
    /\ binds_epoch       \in Generations
    /\ rejoin_step       \in 0..6
    /\ rejoin_holds_lock \in BOOLEAN
    /\ reader_index      \in [Readers -> Generations \cup {Unobserved}]
    /\ reader_binds_epoch \in [Readers -> Generations \cup {Unobserved}]

NoLostUpdate ==
    dirty \/ binds = ComputeBinds(config, nodes, edges)

Convergence ==
    (rejoin_step = 0 /\ ~dirty) =>
        binds = ComputeBinds(config, nodes, edges)

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

\* Writer: takes projection_lock. Cannot run while the rejoin holds it.
WriterUpdateInput(n, e) ==
    /\ ~rejoin_holds_lock
    /\ nodes' = nodes \cup {n}
    /\ edges' = edges \cup {e}
    /\ dirty' = TRUE
    /\ UNCHANGED <<config, binds, index, binds_epoch,
                  rejoin_step, rejoin_holds_lock,
                  reader_index, reader_binds_epoch>>

\* Writer: set_contract_config. Does NOT take projection_lock.
WriterSetConfig(c) ==
    /\ c # config
    /\ config' = c
    /\ dirty' = TRUE
    /\ UNCHANGED <<nodes, edges, binds, index, binds_epoch,
                  rejoin_step, rejoin_holds_lock,
                  reader_index, reader_binds_epoch>>

\* Writer: mark_contracts_dirty. Does NOT take projection_lock.
WriterMarkDirty ==
    /\ ~dirty
    /\ dirty' = TRUE
    /\ UNCHANGED <<config, nodes, edges, binds, index, binds_epoch,
                  rejoin_step, rejoin_holds_lock,
                  reader_index, reader_binds_epoch>>

\* Rejoin step 0 → 1: take the lock, CLEAR dirty (variant (b)).
\* The clear-before-read shape: dirty is FALSE for the rest of
\* the sequence. A writer that lands after this clear sets
\* dirty=TRUE again, re-arming the dirty bit for the next call.
RejoinStart ==
    /\ rejoin_step       = 0
    /\ dirty
    /\ ~rejoin_holds_lock
    /\ rejoin_step'       = 1
    /\ rejoin_holds_lock' = TRUE
    /\ dirty'             = FALSE
    /\ UNCHANGED <<config, nodes, edges, binds, index,
                  binds_epoch, reader_index, reader_binds_epoch>>

RejoinReadConfig ==
    /\ rejoin_step = 1
    /\ rejoin_step' = 2
    /\ UNCHANGED <<dirty, config, nodes, edges, binds, index,
                  binds_epoch, rejoin_holds_lock,
                  reader_index, reader_binds_epoch>>

RejoinReadInputs ==
    /\ rejoin_step = 2
    /\ rejoin_step' = 3
    /\ UNCHANGED <<dirty, config, nodes, edges, binds, index,
                  binds_epoch, rejoin_holds_lock,
                  reader_index, reader_binds_epoch>>

RejoinCompute ==
    /\ rejoin_step = 3
    /\ rejoin_step' = 4
    /\ UNCHANGED <<dirty, config, nodes, edges, binds, index,
                  binds_epoch, rejoin_holds_lock,
                  reader_index, reader_binds_epoch>>

RejoinApply ==
    /\ rejoin_step = 4
    /\ binds' = ComputeBinds(config, nodes, edges)
    /\ rejoin_step' = 5
    /\ UNCHANGED <<dirty, config, nodes, edges, index,
                  binds_epoch, rejoin_holds_lock,
                  reader_index, reader_binds_epoch>>

RejoinSwapIndex ==
    /\ rejoin_step  = 5
    /\ index'       = NextGen(index)
    /\ binds_epoch' = index'
    /\ rejoin_step' = 6
    /\ UNCHANGED <<dirty, config, nodes, edges, binds,
                  rejoin_holds_lock,
                  reader_index, reader_binds_epoch>>

\* Rejoin step 6 → 0: release the lock. dirty is NOT cleared
\* here (variant (b) — it was cleared at the start).
RejoinDone ==
    /\ rejoin_step        = 6
    /\ rejoin_holds_lock' = FALSE
    /\ rejoin_step'       = 0
    /\ UNCHANGED <<dirty, config, nodes, edges, binds, index,
                  binds_epoch, reader_index, reader_binds_epoch>>

ReaderReadIndex(r) ==
    /\ reader_index' = [reader_index EXCEPT ![r] = index]
    /\ UNCHANGED <<dirty, config, nodes, edges, binds, index,
                  binds_epoch, rejoin_step, rejoin_holds_lock,
                  reader_binds_epoch>>

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
    \/ RejoinDone
    \/ \E r \in Readers : ReaderReadIndex(r)
    \/ \E r \in Readers : ReaderReadBinds(r)

vars == <<dirty, config, nodes, edges, binds, index, binds_epoch,
          rejoin_step, rejoin_holds_lock,
          reader_index, reader_binds_epoch>>

Spec == Init /\ [][Next]_vars

====