---- MODULE SnapshotResidency_VariantBC ----
\* SnapshotResidency_VariantBC — TLA+ model of the post-fix
\* SnapshotResidency manager. Combines all three variant changes
\* from the spec:
\*   - variant (b): held is a refcount (AtomicUsize), not a bool.
\*     A second Drop does not clear the count while the logical
\*     count is > 0.
\*   - variant (b): install_resident's cap check + insert are
\*     under one lock acquisition (Fix 5).
\*   - variant (b): try_evict's select + remove are under one
\*     lock acquisition (Fix 6).
\*   - variant (c): from_snapshot_with_wait_ms registers a
\*     per-id slot; concurrent callers share the build (Fix 4).
\*
\* The invariants (NoEvictionOfHeld, CapBound, SingleFlight)
\* must hold at every state. Variant (a) violates all three;
\* this variant (b/c) closes them.

EXTENDS Naturals, FiniteSets

CONSTANTS
    SnapshotIds,
    Cap,
    MaxHolds,
    MaxBuilds

ASSUME
    /\ SnapshotIds # {}
    /\ Cap \in Nat
    /\ MaxHolds \in Nat /\ MaxHolds > 0
    /\ MaxBuilds \in Nat /\ MaxBuilds > 0

VARIABLES
    resident,
    held_storage,        \* refcount surface (variant (b))
    hold_count,          \* logical hold count
    build_in_progress,   \* per-id in-flight build count
    install_checked,
    evict_candidate

None == "none"

TypeOK ==
    /\ resident          \subseteq SnapshotIds
    /\ held_storage      \in [SnapshotIds -> 0..MaxHolds]
    /\ hold_count        \in [SnapshotIds -> 0..MaxHolds]
    /\ build_in_progress \in [SnapshotIds -> 0..MaxBuilds]
    /\ install_checked   \subseteq SnapshotIds
    /\ evict_candidate   \in SnapshotIds \cup {None}

NoEvictionOfHeld ==
    \A s \in SnapshotIds :
        hold_count[s] > 0 => s \in resident

CapBound ==
    Cardinality(resident) <= Cap

\* Variant (c) — single-flight. Once a build for `s` is in
\* progress, no other builder can start a build for `s`.
SingleFlight ==
    \A s \in SnapshotIds : build_in_progress[s] <= 1

Init ==
    /\ resident          = {}
    /\ held_storage      = [s \in SnapshotIds |-> 0]
    /\ hold_count        = [s \in SnapshotIds |-> 0]
    /\ build_in_progress = [s \in SnapshotIds |-> 0]
    /\ install_checked   = {}
    /\ evict_candidate   = None

\* Variant (b): Hold increments the refcount. The bool surface
\* is gone; the predicate `held_storage[s] == 0` is the
\* refcount-zero check.
Hold(s) ==
    /\ s \in resident
    /\ hold_count[s] < MaxHolds
    /\ hold_count'   = [hold_count   EXCEPT ![s] = hold_count[s] + 1]
    /\ held_storage' = [held_storage EXCEPT ![s] = held_storage[s] + 1]
    /\ UNCHANGED <<resident, build_in_progress, install_checked,
                  evict_candidate>>

Release(s) ==
    /\ hold_count[s] > 0
    /\ hold_count'   = [hold_count   EXCEPT ![s] = hold_count[s] - 1]
    /\ held_storage' = [held_storage EXCEPT ![s] = held_storage[s] - 1]
    /\ UNCHANGED <<resident, build_in_progress, install_checked,
                  evict_candidate>>

\* Variant (b) — Fix 5: cap check + insert under one lock. We
\* model the combined operation as a single `Install(s)`
\* action that is enabled iff `s` is not resident AND
\* `|resident| < Cap` (the check and insert are atomic).
Install(s) ==
    /\ s \notin resident
    /\ s \notin install_checked
    /\ Cardinality(resident) < Cap
    /\ resident'        = resident \cup {s}
    /\ install_checked' = install_checked \cup {s}
    /\ UNCHANGED <<held_storage, hold_count, build_in_progress,
                  evict_candidate>>

\* Variant (b) — Fix 6: select + remove under one lock. We
\* model the combined operation as `Evict(s)`: pick any
\* resident id whose refcount is 0 and remove it.
Evict ==
    /\ \E s \in resident :
        /\ held_storage[s] = 0
        /\ evict_candidate' = s
    /\ IF evict_candidate # None THEN
            /\ resident'        = resident \ {evict_candidate}
            /\ evict_candidate' = None
       ELSE
            /\ UNCHANGED resident
            /\ UNCHANGED evict_candidate
    /\ UNCHANGED <<held_storage, hold_count, build_in_progress,
                  install_checked>>

\* Variant (c) — single-flight per id. Once a BuildStart(s)
\* is in progress, no other BuildStart(s) can run.
BuildStart(s) ==
    /\ s \notin resident
    /\ build_in_progress[s] < MaxBuilds
    /\ build_in_progress[s] < 1
    /\ build_in_progress' = [build_in_progress EXCEPT ![s] = build_in_progress[s] + 1]
    /\ UNCHANGED <<resident, held_storage, hold_count,
                  install_checked, evict_candidate>>

BuildFinish(s) ==
    /\ build_in_progress[s] > 0
    /\ build_in_progress' = [build_in_progress EXCEPT ![s] = build_in_progress[s] - 1]
    /\ UNCHANGED <<resident, held_storage, hold_count,
                  install_checked, evict_candidate>>

Next ==
    \/ \E s \in SnapshotIds : Hold(s)
    \/ \E s \in SnapshotIds : Release(s)
    \/ \E s \in SnapshotIds : Install(s)
    \/ Evict
    \/ \E s \in SnapshotIds : BuildStart(s)
    \/ \E s \in SnapshotIds : BuildFinish(s)

vars == <<resident, held_storage, hold_count, build_in_progress,
          install_checked, evict_candidate>>

Spec == Init /\ [][Next]_vars

====