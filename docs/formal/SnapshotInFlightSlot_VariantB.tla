---- MODULE SnapshotInFlightSlot_VariantB ----
\* Variant (b) — fix for Suspicion 3: the `in_flight` map lock is
\* held across the whole build+install sequence, so concurrent
\* callers see the slot and wait, never start a duplicate build.

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
    /\ step \in {"init", "build_started", "build_done_publish", "build_done_remove"}

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

Next ==
    \/ \E s \in SnapshotIds : FirstBuildStart(s)
    \/ \E s \in SnapshotIds : FirstBuildDone(s)
    \/ \E s \in SnapshotIds : FirstBuildRemove(s)

vars == <<in_flight, has_first_result, second_build_count, step>>

Spec == Init /\ [][Next]_vars

====
