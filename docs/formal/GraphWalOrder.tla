------------------------------- MODULE GraphWalOrder -------------------------------
\* Is the WAL's order the order mutations take effect?
\* (src/server/graph/mod.rs: upsert_node, insert_nodes_batch, remove_nodes_by_ids,
\* insert_edges_batch, ...)
\*
\* Replay applies the log in order, so crash recovery equals the pre-crash graph
\* only if log order == in-memory apply order. Last-writer-wins per key makes
\* the difference observable.
\*
\* Locked = FALSE : previous code. The WAL append happened BEFORE the graph
\*                  write lock was taken, so two writers could log in one order
\*                  and apply in the other.
\* Locked = TRUE  : fix. Log and apply happen inside one critical section.
EXTENDS Naturals, Sequences

CONSTANTS Writers, Locked

VARIABLES walLog, applied, pc
\* pc[w]: "idle" | "logged" | "done"

vars == <<walLog, applied, pc>>

Last(s) == IF s = <<>> THEN "none" ELSE s[Len(s)]

\* Once every writer finished, the survivor of replay must be the survivor live.
ReplayEqualsLive ==
    (\A w \in Writers : pc[w] = "done") => Last(walLog) = Last(applied)

TypeOK ==
    /\ walLog \in Seq(Writers) /\ applied \in Seq(Writers)
    /\ pc \in [Writers -> {"idle", "logged", "done"}]

Init == walLog = <<>> /\ applied = <<>> /\ pc = [w \in Writers |-> "idle"]

\* Locked: log + apply atomically.
WriteLocked(w) ==
    /\ Locked /\ pc[w] = "idle"
    /\ walLog' = Append(walLog, w) /\ applied' = Append(applied, w)
    /\ pc' = [pc EXCEPT ![w] = "done"]

\* Unlocked: log now, apply later.
Log(w) ==
    /\ ~Locked /\ pc[w] = "idle"
    /\ walLog' = Append(walLog, w)
    /\ pc' = [pc EXCEPT ![w] = "logged"]
    /\ UNCHANGED applied
Apply(w) ==
    /\ ~Locked /\ pc[w] = "logged"
    /\ applied' = Append(applied, w)
    /\ pc' = [pc EXCEPT ![w] = "done"]
    /\ UNCHANGED walLog

Next == \E w \in Writers : WriteLocked(w) \/ Log(w) \/ Apply(w)

Spec == Init /\ [][Next]_vars
=============================================================================
