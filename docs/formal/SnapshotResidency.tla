---- MODULE SnapshotResidency ----
\* SnapshotResidency — TLA+ model of `SnapshotManager`'s residency
\* table (`from_snapshot_with_wait_ms` → `install_resident` →
\* `try_evict_one_lru_unheld`) and the `HoldGuard` token, at fine
\* granularity, targeting the four suspected bugs in spec §9.2.
\*
\* Spec: docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md §9.2
\* Plan: docs/superpowers/plans/2026-10-02-coverage-and-protocols.md
\*
\* Code mapping (TLA+ → Rust):
\*   - `resident: SUBSET SnapshotId`      ↔ `resident: Mutex<BTreeMap<…>>`
\*   - `held_storage: [SnapshotId → BOOLEAN]`
\*       ↔ `held: AtomicBool` per `SnapshotFederation` (line 106)
\*   - `hold_count: [SnapshotId → Nat]`   ↔ the LOGICAL hold count (the
\*       bug is that the implementation only stores a Boolean, so
\*       two concurrent `HoldGuard`s share one slot and the first
\*       `Drop` clears it)
\*   - `build_in_progress: [SnapshotId → Nat]`
\*       ↔ in-flight `build_snapshot_federation` calls (the spec
\*       hypothesises no single-flight; in the current code two
\*       `from_snapshot_with_wait_ms` calls for the same record do
\*       both build — see line 999 where the resident-cache check
\*       is read-then-acted-on under no projection lock)
\*   - `install_checked: SUBSET SnapshotId`
\*       ↔ snapshots that have passed `len() < cap` (line 1100) but
\*       not yet inserted (line 1103). The check and the insert are
\*       under SEPARATE `self.resident.lock()` acquisitions
\*       (line 1100 reads `len()`, drops the guard, then line 1103
\*       acquires a new guard for the `insert`).
\*   - `evict_candidate: SnapshotId ∪ {None}`
\*       ↔ the id picked by `try_evict_one_lru_unheld` between its
\*       selection (lines 1130–1139) and its removal
\*       (line 1142). The two are under separate `self.resident.lock()`
\*       acquisitions.
\*
\* Variant (a) — current code (what this spec models):
\*   `held` is a Boolean, install is check+insert under separate
\*   locks, evict is select+remove under separate locks, and there
\*   is no single-flight on `build_snapshot_federation`.
\*
\* Variant (b) — `held: AtomicUsize` (counter, not bool):
\*   The eviction predicate uses `held.load() == 0` correctly. A
\*   second `Drop` does not clear the flag while the count is > 0.
\*   (To be implemented in the follow-up Rust fix pass.)
\*
\* Variant (c) — single-flight builder per id:
\*   The first `from_snapshot_with_wait_ms` for a record wins the
\*   build; the second waits for the first's federation to land in
\*   `resident` (or returns the same `Arc<SnapshotFederation>`).
\*   (To be implemented in the follow-up Rust fix pass.)
\*
\* Not modeled here (notes for the report):
\*   - The `condvar` lost-wakeup hypothesis from spec §9.2: the
\*     real code uses a 50 ms `wait_timeout` poll (line 1117), so
\*     a lost wakeup is at worst a 50 ms latency blip, not a hang.
\*     Modeling the wakeup race as a TLC liveness property is
\*     possible but is left for the follow-up; the safety
\*     invariants below are the primary deliverable.
\*   - The cap-overrun from `install_resident` ALSO has a "wait"
\*     branch (line 1114) when the cap is full and every slot is
\*     held; that branch is sound (it serialises on the
\*     `residency_notify` condvar). The bug is the non-wait branch.

EXTENDS Naturals, FiniteSets

CONSTANTS
    SnapshotIds,    \* set of snapshot identifiers
    Cap,            \* residency cap
    MaxHolds,       \* upper bound on `hold_count` per id (for finiteness)
    MaxBuilds       \* upper bound on `build_in_progress` per id

ASSUME
    /\ SnapshotIds # {}
    /\ Cap \in Nat
    /\ MaxHolds \in Nat /\ MaxHolds > 0
    /\ MaxBuilds \in Nat /\ MaxBuilds > 0

\* ===== State variables =====

VARIABLES
    resident,            \* SUBSET SnapshotIds
    held_storage,        \* [SnapshotId → BOOLEAN] — the buggy AtomicBool
    hold_count,          \* [SnapshotId → Nat] — logical hold count
    build_in_progress,   \* [SnapshotId → Nat] — in-flight builds
    install_checked,     \* SUBSET SnapshotIds — passed the cap check
    evict_candidate      \* SnapshotId ∪ {None} — id selected for eviction

\* ===== Helpers =====

\* Sentinel for "no eviction candidate selected". We use the
\* string "none" — distinct from every `SnapshotIds` value by the
\* ASSUME that `SnapshotIds` contains only identifiers, not
\* strings. (If a test ever passed a string as a snapshot id this
\* would need to be revisited; in practice `SnapshotIds` is a
\* model set of opaque names.)
None == "none"

\* ===== Type invariant =====

TypeOK ==
    /\ resident          \subseteq SnapshotIds
    /\ held_storage      \in [SnapshotIds -> BOOLEAN]
    /\ hold_count        \in [SnapshotIds -> 0..MaxHolds]
    /\ build_in_progress \in [SnapshotIds -> 0..MaxBuilds]
    /\ install_checked   \subseteq SnapshotIds
    /\ evict_candidate   \in SnapshotIds \cup {None}

\* ===== Invariants (the four things §9.2 asks us to check) =====

\* **No-eviction-of-held**. A snapshot that is still held by any
\* caller must remain in `resident`. The bug surface: the `held`
\* `AtomicBool` is shared by all `HoldGuard` instances on a given
\* `SnapshotFederation`; the first `Drop` clears it, and the
\* eviction predicate sees "unheld" while another holder is still
\* using the federation. A second surface: the eviction select
\* and the eviction remove are under separate lock acquisitions;
\* a `Hold` can land on the selected id between them.
NoEvictionOfHeld ==
    \A s \in SnapshotIds :
        hold_count[s] > 0 => s \in resident

\* **Cap-bound**. The resident set never grows past the cap. The
\* bug surface: `install_resident` checks `len() < cap` and inserts
\* under SEPARATE `self.resident.lock()` acquisitions; two
\* concurrent installs can both pass the check before either
\* inserts.
CapBound ==
    Cardinality(resident) <= Cap

\* **Single-flight**. At most one builder per snapshot id. The
\* bug surface: two `from_snapshot_with_wait_ms` calls for the
\* same record both miss the resident cache and both call
\* `build_snapshot_federation`; the second build's output
\* overwrites the first's when both try to install.
SingleFlight ==
    \A s \in SnapshotIds : build_in_progress[s] <= 1

\* ===== Initialisation =====

Init ==
    /\ resident          = {}
    /\ held_storage      = [s \in SnapshotIds |-> FALSE]
    /\ hold_count        = [s \in SnapshotIds |-> 0]
    /\ build_in_progress = [s \in SnapshotIds |-> 0]
    /\ install_checked   = {}
    /\ evict_candidate   = None

\* ===== Actions =====

\* Hold: acquire a hold token on `s`. Requires `s` to be resident
\* (an unheld federation cannot be held — `from_snapshot_with_wait_ms`
\* installs first, then returns a `HoldGuard`).
Hold(s) ==
    /\ s \in resident
    /\ hold_count[s] < MaxHolds
    /\ hold_count'   = [hold_count   EXCEPT ![s] = hold_count[s] + 1]
    /\ held_storage' = [held_storage EXCEPT ![s] = TRUE]
    /\ UNCHANGED <<resident, build_in_progress, install_checked,
                  evict_candidate>>

\* Release: drop a hold token on `s`.
\*
\* BUG: the real `Drop` does `held.store(false)` unconditionally
\* (line 1197). Two holders share the Boolean; the first Drop
\* clears it while the second is still using the federation. We
\* model the buggy semantics here — the storage flips to FALSE
\* regardless of the remaining logical count.
Release(s) ==
    /\ hold_count[s] > 0
    /\ hold_count'   = [hold_count   EXCEPT ![s] = hold_count[s] - 1]
    /\ held_storage' = [held_storage EXCEPT ![s] = FALSE]
    /\ UNCHANGED <<resident, build_in_progress, install_checked,
                  evict_candidate>>

\* Install — phase 1 (check). The real code reads
\* `self.resident.lock().len()` and drops the guard (line 1100).
\* We model the check as recording the snapshot in
\* `install_checked` and not yet inserting.
InstallCheck(s) ==
    /\ s \notin resident
    /\ s \notin install_checked
    /\ Cardinality(resident) < Cap
    /\ install_checked' = install_checked \cup {s}
    /\ UNCHANGED <<resident, held_storage, hold_count,
                  build_in_progress, evict_candidate>>

\* Install — phase 2 (insert). The real code calls
\* `self.resident.lock().insert(...)` after re-acquiring the
\* guard (line 1103).
InstallInsert(s) ==
    /\ s \in install_checked
    /\ s \notin resident
    /\ resident'        = resident \cup {s}
    /\ install_checked' = install_checked \ {s}
    /\ UNCHANGED <<held_storage, hold_count, build_in_progress,
                  evict_candidate>>

\* Evict — phase 1 (select). Find an unheld LRU id; record it in
\* `evict_candidate`; the real code holds the lock for the
\* selection loop (lines 1130–1139) but the lock is released
\* before the remove on line 1142.
\*
\* The "held" predicate uses `held_storage` (the Boolean) — that
\* is what `try_evict_one_lru_unheld` actually checks
\* (line 1132: `!f.held.load(Ordering::Acquire)`). The bug is
\* right there: a second holder's drop has not happened yet, but
\* the first drop already cleared the bool.
EvictSelect ==
    /\ evict_candidate = None
    /\ \E s \in resident :
        /\ ~held_storage[s]
        /\ (evict_candidate = None
            \/ TRUE)  \* the LRU rule is irrelevant to the bug; we
                     \* pick any unheld resident id.
    /\ evict_candidate' = CHOOSE s \in resident :
                            ~held_storage[s]
    /\ UNCHANGED <<resident, held_storage, hold_count,
                  build_in_progress, install_checked>>

\* Evict — phase 2 (remove). The real code calls
\* `self.resident.lock().remove(&id)` (line 1142) under a fresh
\* lock acquisition. By the time this lands, a `Hold` may have
\* incremented `hold_count[s]` above zero.
EvictRemove ==
    /\ evict_candidate # None
    /\ resident'        = resident \ {evict_candidate}
    /\ evict_candidate' = None
    /\ UNCHANGED <<held_storage, hold_count, build_in_progress,
                  install_checked>>

\* Build — start. `from_snapshot_with_wait_ms` calls
\* `build_snapshot_federation` after the resident-cache miss
\* (line 1007). Two concurrent calls for the same record both
\* build; the second's output is the one that ends up in
\* `resident` (line 1103) and overwrites the first.
BuildStart(s) ==
    /\ s \notin resident
    /\ build_in_progress[s] < MaxBuilds
    /\ build_in_progress' = [build_in_progress EXCEPT ![s] = build_in_progress[s] + 1]
    /\ UNCHANGED <<resident, held_storage, hold_count,
                  install_checked, evict_candidate>>

\* Build — finish. The real code calls `install_resident` next
\* (line 1009). We model the finish as "build done, now eligible
\* to install".
BuildFinish(s) ==
    /\ build_in_progress[s] > 0
    /\ build_in_progress' = [build_in_progress EXCEPT ![s] = build_in_progress[s] - 1]
    /\ UNCHANGED <<resident, held_storage, hold_count,
                  install_checked, evict_candidate>>

Next ==
    \/ \E s \in SnapshotIds : Hold(s)
    \/ \E s \in SnapshotIds : Release(s)
    \/ \E s \in SnapshotIds : InstallCheck(s)
    \/ \E s \in SnapshotIds : InstallInsert(s)
    \/ EvictSelect
    \/ EvictRemove
    \/ \E s \in SnapshotIds : BuildStart(s)
    \/ \E s \in SnapshotIds : BuildFinish(s)

vars == <<resident, held_storage, hold_count, build_in_progress,
          install_checked, evict_candidate>>

Spec == Init /\ [][Next]_vars

====
