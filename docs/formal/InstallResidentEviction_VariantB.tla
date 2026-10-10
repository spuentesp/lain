---- MODULE InstallResidentEviction_VariantB ----
\* Variant (b) — fix for Suspicion 1: `install_resident` does NOT
\* call `try_evict_one_lru_unheld` until `len() == Cap`. The lock
\* is acquired first, the cap check runs, and only on a full
\* resident does the eviction path open.

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

EvictUnheldIfFull ==
    /\ ~install_succeeded
    /\ resident # {}
    /\ Cardinality(resident) = Cap
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
    \/ EvictUnheldIfFull
    \/ InstallCheckInsert
    \/ ResetInstallCounters

vars == <<resident, held_count, next_install, resident_before,
          evictions_during_install, install_succeeded>>

Spec == Init /\ [][Next]_vars

====
