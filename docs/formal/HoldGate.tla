-------------------------------- MODULE HoldGate --------------------------------
\* `RepoIndex::mark_ready` vs `RepoIndex::hold_ready(false)`
\* (src/server/federation/repo_index.rs).
\*
\* At startup the CLI holds every repo's readiness (`hold_ready(true)`) until
\* the federation is projected, then releases it (`hold_ready(false)`), which
\* promotes a repo whose indexing pass already finished. The indexing pass
\* ends with `last_indexed = now; mark_ready()`, where mark_ready reads the
\* hold flag and publishes `Indexing` (held) or `Ready`.
\*
\* Locked = FALSE : current code. mark_ready is read-hold then write-health;
\*                  hold_ready is write-hold, read-health, write-health. Each a
\*                  separate step, so they interleave.
\* Locked = TRUE  : fix. The decision is atomic under one lock (HealthGate).
\*
\* Safety (checked on quiescent states): once the pass finished and the hold is
\* released, the repo is Ready - never stuck Indexing.
EXTENDS Naturals

CONSTANTS Locked

VARIABLES hold, health, indexed, ipc, rpc, seen
\* ipc: indexer program counter: "run" | "marked" (last_indexed set) | "read" | "done"
\* rpc: releaser program counter: "idle" | "released" | "done"
\* seen: value of `hold` the indexer read (racy mode)

vars == <<hold, health, indexed, ipc, rpc, seen>>

Quiescent == ipc = "done" /\ rpc = "done"
NotStuck == Quiescent => health = "ready"

TypeOK ==
    /\ hold \in BOOLEAN /\ health \in {"indexing", "ready"} /\ indexed \in BOOLEAN
    /\ ipc \in {"run", "marked", "read", "done"} /\ rpc \in {"idle", "released", "done"}
    /\ seen \in BOOLEAN

Init ==
    /\ hold = TRUE            \* startup hold is on
    /\ health = "indexing"
    /\ indexed = FALSE
    /\ ipc = "run" /\ rpc = "idle" /\ seen = FALSE

\* ---- indexer ---------------------------------------------------------------
MarkIndexed ==
    /\ ipc = "run"
    /\ indexed' = TRUE /\ ipc' = "marked"
    /\ UNCHANGED <<hold, health, rpc, seen>>

\* Locked: read hold and write health in ONE step.
MarkReadyAtomic ==
    /\ Locked /\ ipc = "marked"
    /\ health' = IF hold THEN "indexing" ELSE "ready"
    /\ ipc' = "done"
    /\ UNCHANGED <<hold, indexed, rpc, seen>>

\* Racy: read the hold flag now, publish later.
MarkReadRead ==
    /\ ~Locked /\ ipc = "marked"
    /\ seen' = hold /\ ipc' = "read"
    /\ UNCHANGED <<hold, health, indexed, rpc>>
MarkReadWrite ==
    /\ ~Locked /\ ipc = "read"
    /\ health' = IF seen THEN "indexing" ELSE "ready"
    /\ ipc' = "done"
    /\ UNCHANGED <<hold, indexed, rpc, seen>>

\* ---- releaser --------------------------------------------------------------
\* Locked: clear the flag and promote in ONE step.
ReleaseAtomic ==
    /\ Locked /\ rpc = "idle"
    /\ hold' = FALSE
    /\ health' = IF health = "indexing" /\ indexed THEN "ready" ELSE health
    /\ rpc' = "done"
    /\ UNCHANGED <<indexed, ipc, seen>>

\* Racy: store(false), then separately read health/last_indexed and promote.
ReleaseStore ==
    /\ ~Locked /\ rpc = "idle"
    /\ hold' = FALSE /\ rpc' = "released"
    /\ UNCHANGED <<health, indexed, ipc, seen>>
ReleasePromote ==
    /\ ~Locked /\ rpc = "released"
    /\ health' = IF health = "indexing" /\ indexed THEN "ready" ELSE health
    /\ rpc' = "done"
    /\ UNCHANGED <<hold, indexed, ipc, seen>>

Next == MarkIndexed \/ MarkReadyAtomic \/ MarkReadRead \/ MarkReadWrite
        \/ ReleaseAtomic \/ ReleaseStore \/ ReleasePromote

Spec == Init /\ [][Next]_vars
=============================================================================
