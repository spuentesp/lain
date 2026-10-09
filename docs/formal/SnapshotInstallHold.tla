--------------------------- MODULE SnapshotInstallHold ---------------------------
\* The window between `install_resident` and `HoldGuard::new` in
\* `SnapshotManager::from_snapshot_with_wait_ms`
\* (src/server/federation/contracts/snapshots/manager.rs).
\*
\* The resident-hit path takes its hold under the resident lock, so it is
\* safe. The *build* path inserts the new federation (held = 0), drops the
\* lock, and only then constructs `HoldGuard::new` (a bare `fetch_add`). In
\* between, another install at capacity may legally evict the entry (it looks
\* unheld). The builder then "holds" a snapshot that is no longer resident:
\* the next lookup rebuilds a duplicate and the cap is silently exceeded.
\* `SnapshotResidency*.tla` cannot see this because its Hold(s) requires
\* s \in resident.
\*
\* AtomicHold = FALSE : current code   (insert, unlock, later hold)
\* AtomicHold = TRUE  : fix            (hold taken inside the insert's lock)
EXTENDS Naturals, FiniteSets

CONSTANTS Ids, Cap, Clients, AtomicHold

\* Client "c1" wants snapshot "s1"; every other client wants "s2".
Wants == [c \in Clients |-> IF c = "c1" THEN "s1" ELSE "s2"]

VARIABLES resident, held, pc

vars == <<resident, held, pc>>

\* held[s] > 0  =>  s is resident (a held snapshot is never evicted).
NoEvictionOfHeld == \A s \in Ids : held[s] > 0 => s \in resident
CapBound == Cardinality(resident) <= Cap

TypeOK ==
    /\ resident \subseteq Ids
    /\ held \in [Ids -> 0..Cardinality(Clients)]
    /\ pc \in [Clients -> {"idle", "built", "holding", "done"}]

Init ==
    /\ resident = {}
    /\ held = [s \in Ids |-> 0]
    /\ pc = [c \in Clients |-> "idle"]

\* Resident hit: hold taken under the resident lock (atomic).
Lookup(c) ==
    /\ pc[c] = "idle" /\ Wants[c] \in resident
    /\ held' = [held EXCEPT ![Wants[c]] = @ + 1]
    /\ pc' = [pc EXCEPT ![c] = "holding"]
    /\ UNCHANGED resident

\* Build + install: evicts an unheld entry when at capacity.
Install(c) ==
    /\ pc[c] = "idle" /\ Wants[c] \notin resident
    /\ \/ /\ Cardinality(resident) < Cap
          /\ resident' = resident \cup {Wants[c]}
       \/ /\ Cardinality(resident) >= Cap
          /\ \E v \in resident : held[v] = 0 /\ resident' = (resident \ {v}) \cup {Wants[c]}
    /\ IF AtomicHold
         THEN /\ held' = [held EXCEPT ![Wants[c]] = @ + 1]
              /\ pc' = [pc EXCEPT ![c] = "holding"]
         ELSE /\ UNCHANGED held
              /\ pc' = [pc EXCEPT ![c] = "built"]

\* Pre-fix only: `HoldGuard::new` after the lock was released.
LateHold(c) ==
    /\ pc[c] = "built"
    /\ held' = [held EXCEPT ![Wants[c]] = @ + 1]
    /\ pc' = [pc EXCEPT ![c] = "holding"]
    /\ UNCHANGED resident

Release(c) ==
    /\ pc[c] = "holding"
    /\ held' = [held EXCEPT ![Wants[c]] = @ - 1]
    /\ pc' = [pc EXCEPT ![c] = "done"]
    /\ UNCHANGED resident

Next == \E c \in Clients : Lookup(c) \/ Install(c) \/ LateHold(c) \/ Release(c)

Spec == Init /\ [][Next]_vars
=============================================================================
