---- MODULE RejoinProtocolMidRejoinInvariant ----
\* Suspicion 5 — the `NoLostUpdate` invariant in
\* `RejoinProtocol_VariantB.tla` is checked at every reachable
\* state, including mid-rejoin. The clear-before-read fix moves
\* the `dirty := FALSE` to the START of the rejoin.

EXTENDS Naturals, FiniteSets

CONSTANTS
    Generations,
    Nodes,
    Edges,
    Configs

ASSUME
    /\ Generations # {}
    /\ Nodes       # {}
    /\ Edges       # {}
    /\ Configs     # {}

VARIABLES
    dirty,
    config,
    nodes,
    edges,
    binds,
    index,
    binds_epoch,
    rejoin_step,
    rejoin_holds_lock

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

NoLostUpdateOriginal ==
    dirty \/ binds = ComputeBinds(config, nodes, edges)

NoLostUpdateQuiescent ==
    rejoin_step = 0 =>
        (dirty \/ binds = ComputeBinds(config, nodes, edges))

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

RejoinStart ==
    /\ rejoin_step       = 0
    /\ dirty
    /\ ~rejoin_holds_lock
    /\ rejoin_step'       = 1
    /\ rejoin_holds_lock' = TRUE
    /\ dirty'             = FALSE
    /\ UNCHANGED <<config, nodes, edges, binds, index, binds_epoch>>

RejoinApply ==
    /\ rejoin_step = 4
    /\ binds' = ComputeBinds(config, nodes, edges)
    /\ rejoin_step' = 5
    /\ UNCHANGED <<dirty, config, nodes, edges, index, binds_epoch, rejoin_holds_lock>>

RejoinDone ==
    /\ rejoin_step        = 6
    /\ rejoin_holds_lock' = FALSE
    /\ rejoin_step'       = 0
    /\ UNCHANGED <<dirty, config, nodes, edges, binds, index, binds_epoch>>

WriterUpdateInput(n, e) ==
    /\ ~rejoin_holds_lock
    /\ nodes' = nodes \cup {n}
    /\ edges' = edges \cup {e}
    /\ dirty' = TRUE
    /\ UNCHANGED <<config, binds, index, binds_epoch, rejoin_step, rejoin_holds_lock>>

RejoinStep2 ==
    /\ rejoin_step' = 2
    /\ UNCHANGED <<dirty, config, nodes, edges, binds, index, binds_epoch, rejoin_holds_lock>>

RejoinStep3 ==
    /\ rejoin_step = 2
    /\ rejoin_step' = 3
    /\ UNCHANGED <<dirty, config, nodes, edges, binds, index, binds_epoch, rejoin_holds_lock>>

RejoinCompute ==
    /\ rejoin_step = 3
    /\ rejoin_step' = 4
    /\ UNCHANGED <<dirty, config, nodes, edges, binds, index, binds_epoch, rejoin_holds_lock>>

RejoinSwapIndex ==
    /\ rejoin_step  = 5
    /\ index'       = NextGen(index)
    /\ binds_epoch' = index'
    /\ rejoin_step' = 6
    /\ UNCHANGED <<dirty, config, nodes, edges, binds, rejoin_holds_lock>>

Next ==
    \/ \E n \in Nodes, e \in Edges : WriterUpdateInput(n, e)
    \/ RejoinStart
    \/ RejoinStep2
    \/ RejoinStep3
    \/ RejoinCompute
    \/ RejoinApply
    \/ RejoinSwapIndex
    \/ RejoinDone

vars == <<dirty, config, nodes, edges, binds, index, binds_epoch,
          rejoin_step, rejoin_holds_lock>>

Spec == Init /\ [][Next]_vars

====
