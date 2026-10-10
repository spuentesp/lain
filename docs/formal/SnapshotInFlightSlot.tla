---- MODULE SnapshotInFlightSlot ----
\* Suspicion 3 — single-flight slot is removed AFTER the outcome is
\* published, opening a window where a third caller can race past
\* the publish but before the remove and start a duplicate build.

EXTENDS Naturals, FiniteSets

CONSTANTS
    SnapshotIds

ASSUME SnapshotIds # {}

VARIABLES
    in_flight,
    has_first_result,
    second_build_count,
    step

TypeOK ==
    /\ in_flight \subseteq SnapshotIds
    /\ has_first_result \in [SnapshotIds -> BOOLEAN]
    /\ second_build_count \in [SnapshotIds -> Nat]
    /\ step \in {"init", "build_started", "build_done_publish", "build_done_remove", "second_build_started"}

NoDuplicateBuildWhileLive ==
    \A s \in SnapshotIds :
        has_first_result[s] = TRUE => second_build_count[s] = 0

Init ==
    /\ in_flight = {}
    /\ has_first_result = [s \in SnapshotIds |-> FALSE]
    /\ second_build_count = [s \in SnapshotIds |-> 0]
    /\ step = "init"

FirstBuildStart(s) ==
    /\ s \notin in_flight
    /\ has_first_result[s] = FALSE
    /\ in_flight' = in_flight \cup {s}
    /\ step' = "build_started"
    /\ UNCHANGED <<has_first_result, second_build_count>>

FirstBuildDone(s) ==
    /\ s \in in_flight
    /\ step = "build_started"
    /\ has_first_result' = [has_first_result EXCEPT ![s] = TRUE]
    /\ step' = "build_done_publish"
    /\ UNCHANGED <<in_flight, second_build_count>>

FirstBuildRemove(s) ==
    /\ step = "build_done_publish"
    /\ in_flight' = in_flight \ {s}
    /\ step' = "build_done_remove"
    /\ UNCHANGED <<has_first_result, second_build_count>>

SecondBuildAfterRemove(s) ==
    /\ step = "build_done_remove"
    /\ s \notin in_flight
    /\ has_first_result[s] = TRUE
    /\ second_build_count' = [second_build_count EXCEPT ![s] = second_build_count[s] + 1]
    /\ step' = "second_build_started"
    /\ UNCHANGED <<in_flight, has_first_result>>

Next ==
    \/ \E s \in SnapshotIds : FirstBuildStart(s)
    \/ \E s \in SnapshotIds : FirstBuildDone(s)
    \/ \E s \in SnapshotIds : FirstBuildRemove(s)
    \/ \E s \in SnapshotIds : SecondBuildAfterRemove(s)

vars == <<in_flight, has_first_result, second_build_count, step>>

Spec == Init /\ [][Next]_vars

====
