--------------------------- MODULE GraphWalCheckpoint ---------------------------
\* Durability of acknowledged graph mutations across a concurrent checkpoint
\* (src/server/graph/mod.rs `save_to_disk_sync`, wal.rs).
\*
\* `GraphWal.tla` only bounded the log length (its S2 invariant is literally
\* TRUE); it could not see ops being lost. Here the property is:
\*
\*     Durable:  every acknowledged op survives a crash at ANY point, i.e.
\*               acked \subseteq snapshot \cup wal \cup retired_log.
\*
\* Protocol = "TruncateAfter" : serialise state, write graph.bin, THEN truncate
\*            the live WAL. Ops acknowledged between the serialise and the
\*            truncate are in neither the snapshot nor the (now empty) WAL.
\* Protocol = "Rotate"        : under the graph write lock move the live WAL to
\*            graph.wal.prev, serialise + write graph.bin, then delete .prev.
\*            Writers log under that same lock, so none can slip in between.
EXTENDS Naturals, FiniteSets

CONSTANTS Ops, Protocol

VARIABLES
    mem,       \* ops applied to the in-memory graph (all acknowledged)
    wal,       \* ops in the live WAL
    prev,      \* ops in the retired log (graph.wal.prev)
    snap,      \* ops covered by graph.bin
    captured,  \* TruncateAfter: the state serialised at checkpoint start
    phase      \* "idle" | "serialised" | "written"

vars == <<mem, wal, prev, snap, captured, phase>>

Recoverable == snap \cup wal \cup prev
Durable == mem \subseteq Recoverable

TypeOK ==
    /\ mem \subseteq Ops /\ wal \subseteq Ops /\ prev \subseteq Ops
    /\ snap \subseteq Ops /\ captured \subseteq Ops
    /\ phase \in {"idle", "serialised", "written"}

Init ==
    /\ mem = {} /\ wal = {} /\ prev = {} /\ snap = {} /\ captured = {}
    /\ phase = "idle"

\* A writer: log (fsync) then apply, as one step under the graph write lock.
Write(o) ==
    /\ o \notin mem
    /\ wal' = wal \cup {o}
    /\ mem' = mem \cup {o}
    /\ UNCHANGED <<prev, snap, captured, phase>>

\* ---- TruncateAfter (previous code) -------------------------------------------
TA_Serialise ==
    /\ Protocol = "TruncateAfter" /\ phase = "idle"
    /\ captured' = mem /\ phase' = "serialised"
    /\ UNCHANGED <<mem, wal, prev, snap>>
TA_WriteSnapshot ==
    /\ Protocol = "TruncateAfter" /\ phase = "serialised"
    /\ snap' = captured /\ phase' = "written"
    /\ UNCHANGED <<mem, wal, prev, captured>>
TA_TruncateWal ==
    /\ Protocol = "TruncateAfter" /\ phase = "written"
    /\ wal' = {} /\ phase' = "idle"
    /\ UNCHANGED <<mem, prev, snap, captured>>

\* ---- Rotate (fix) ---------------------------------------------------------------
R_Rotate ==
    /\ Protocol = "Rotate" /\ phase = "idle"
    /\ prev' = prev \cup wal /\ wal' = {}
    /\ phase' = "serialised"
    /\ UNCHANGED <<mem, snap, captured>>
\* Serialise + atomic rename happen after the lock is released, so the snapshot
\* may already include newer ops; it always covers everything that was rotated.
R_WriteSnapshot ==
    /\ Protocol = "Rotate" /\ phase = "serialised"
    /\ snap' = mem /\ phase' = "written"
    /\ UNCHANGED <<mem, wal, prev, captured>>
R_DiscardPrev ==
    /\ Protocol = "Rotate" /\ phase = "written"
    /\ prev' = {} /\ phase' = "idle"
    /\ UNCHANGED <<mem, wal, snap, captured>>

Next == \/ \E o \in Ops : Write(o)
        \/ TA_Serialise \/ TA_WriteSnapshot \/ TA_TruncateWal
        \/ R_Rotate \/ R_WriteSnapshot \/ R_DiscardPrev

Spec == Init /\ [][Next]_vars
=============================================================================
