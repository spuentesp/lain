---- MODULE InstallResidentEviction ----
\* Suspicion 1 — `install_resident` evicts an LRU entry unnecessarily
\* when the resident set has space available.
\*
\* Spec: docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md §8.5
\* Code: src/server/federation/contracts/snapshots/manager.rs lines 1185-1208
\*       (the combined-lock fix, post-`fix(snapshots): install_resident checks
\*        cap under the insert lock`).
\*
\* The real code:
\*
\*     loop {
\*         let evicted = self.try_evict_one_lru_unheld();   // <-- runs FIRST
\*         let mut resident = self.resident.lock();
\*         if resident.len() < cap || evicted {
\*             resident.insert(fed.snapshot_id.clone(), fed.clone());
\*             return Ok(());
\*         }
\*         // ... wait or busy
\*     }
\*
\* The bug: `try_evict_one_lru_unheld()` is called BEFORE checking
\* `len() < cap`. When the resident has space (say 2 entries, cap 4),
\* the call evicts an unheld LRU entry, reducing `len()` to 1. The
\* subsequent insert brings `len()` back to 2. Net effect: one
\* cache hit lost per install.
\*
\* The fix: the lock should be acquired FIRST; eviction should be
\* tried ONLY when `len() == cap`. Variant (b) below models the
\* fixed ordering.

EXTENDS Naturals, FiniteSets

CONSTANTS
    SnapshotIds,
    Cap

ASSUME
    /\ SnapshotIds # {}
    /\ Cap \in Nat
    /\ Cap > 0
    /\ Cardinality(SnapshotIds) > Cap

VARIABLES
    resident,
    held_count,
    next_install,
    resident_before,
    evictions_during_install,
    install_succeeded

TypeOK ==
    /\ resident \subseteq SnapshotIds
    /\ held_count \in [SnapshotIds -> Nat]
    /\ next_install \in SnapshotIds
    /\ resident_before \in Nat
    /\ evictions_during_install \in Nat
    /\ install_succeeded \in BOOLEAN

NoUnnecessaryEviction ==
    install_succeeded =>
        ~(evictions_during_install > 0 /\ resident_before < Cap)

Init ==
    /\ resident = {}
    /\ held_count = [s \in SnapshotIds |-> 0]
    /\ next_install = CHOOSE s \in SnapshotIds : TRUE
    /\ resident_before = 0
    /\ evictions_during_install = 0
    /\ install_succeeded = FALSE

EvictUnheld ==
    /\ ~install_succeeded
    /\ resident # {}
    /\ \E s \in resident :
        /\ held_count[s] = 0
    /\ \E s \in resident :
        /\ held_count[s] = 0
        /\ resident' = resident \ {s}
        /\ evictions_during_install' = evictions_during_install + 1
    /\ UNCHANGED <<held_count, next_install, resident_before, install_succeeded>>

InstallCheckInsert ==
    /\ ~install_succeeded
    /\ Cardinality(resident) < Cap \/ evictions_during_install > 0
    /\ resident_before' = Cardinality(resident)
    /\ install_succeeded' = TRUE
    /\ resident' = resident \cup {next_install}
    /\ held_count' = [held_count EXCEPT ![next_install] = 0]
    /\ UNCHANGED <<evictions_during_install, next_install>>

ResetInstallCounters ==
    /\ install_succeeded
    /\ install_succeeded' = FALSE
    /\ evictions_during_install' = 0
    /\ resident_before' = 0
    /\ UNCHANGED <<resident, held_count, next_install>>

Next ==
    \/ EvictUnheld
    \/ InstallCheckInsert
    \/ ResetInstallCounters

vars == <<resident, held_count, next_install, resident_before,
          evictions_during_install, install_succeeded>>

Spec == Init /\ [][Next]_vars

====
