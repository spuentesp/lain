---- MODULE IndexGeneration ----
\* Index-generation-consistency invariant (I7) for the contract-federation
\* rejoin pipeline.
\*
\* Spec: docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md §3, §9.
\*
\* I7: A tool call reads one generation of ContractIndex, never a mix across
\* live rejoin / snapshot swap.
\*
\* The pipeline: writers mark the index dirty; a background "rejoin" reads
\* from the dirty generation and produces a new ContractIndex; tool reads
\* use the snapshot, swapping atomically. Concurrent reads during a swap
\* must observe a single generation end-to-end.
\*
\* We model two readers and one rejoin. Readers always see a coherent
\* snapshot (no torn reads between two generations). Liveness: dirty
\* eventually becomes clean.

EXTENDS Naturals, FiniteSets

CONSTANTS
    Readers,        \* Set of reader identifiers
    Generations,    \* Set of generation identifiers (small — 2 is enough)
    Unloaded        \* Sentinel for "no snapshot loaded"

ASSUME
    /\ Readers # {}
    /\ Generations # {}
    /\ Unloaded \notin Generations

VARIABLES
    dirty,        \* BOOLEAN — index dirty
    snapshot,     \* [reader] -> Generation or Unloaded — the generation a reader last loaded
    current_gen,  \* Generation — the canonical generation rejoin produced
    reader_active \* [reader] -> BOOLEAN — whether the reader currently holds a snapshot

\* ===== Helpers =====

AllReadersCoherent ==
    \A r \in Readers : reader_active[r] => snapshot[r] = current_gen

\* ===== Type invariants =====

TypeOK ==
    /\ dirty \in BOOLEAN
    /\ snapshot \in [Readers -> Generations \cup {Unloaded}]
    /\ current_gen \in Generations
    /\ reader_active \in [Readers -> BOOLEAN]

\* ===== I7 invariant =====

IndexGenerationConsistent == AllReadersCoherent

\* ===== Initialisation =====

Init ==
    /\ dirty        = TRUE
    /\ snapshot     = [r \in Readers |-> Unloaded]
    /\ current_gen  = CHOOSE g \in Generations : TRUE
    /\ reader_active = [r \in Readers |-> FALSE]

\* ===== Actions =====

Rejoin ==
    /\ dirty
    /\ current_gen' = CHOOSE g \in Generations : g /= current_gen
    /\ dirty' = FALSE
    /\ snapshot' = [r \in Readers |-> IF reader_active[r] THEN current_gen' ELSE snapshot[r]]
    /\ UNCHANGED reader_active

LoadSnapshot(r) ==
    /\ ~reader_active[r]
    /\ reader_active' = [reader_active EXCEPT ![r] = TRUE]
    /\ snapshot' = [snapshot EXCEPT ![r] = current_gen]
    /\ UNCHANGED <<dirty, current_gen>>

ReleaseSnapshot(r) ==
    /\ reader_active[r]
    /\ reader_active' = [reader_active EXCEPT ![r] = FALSE]
    /\ snapshot' = [snapshot EXCEPT ![r] = Unloaded]
    /\ UNCHANGED <<dirty, current_gen>>

MarkDirty ==
    /\ ~dirty
    /\ dirty' = TRUE
    /\ UNCHANGED <<snapshot, current_gen, reader_active>>

Next ==
    \/ Rejoin
    \/ \E r \in Readers : LoadSnapshot(r)
    \/ \E r \in Readers : ReleaseSnapshot(r)
    \/ MarkDirty

vars == <<dirty, snapshot, current_gen, reader_active>>

Spec == Init /\ [][Next]_vars

\* ===== Liveness =====

\* Eventually dirty is cleared (a rejoin runs).
Liveness == []<>(~dirty)

====