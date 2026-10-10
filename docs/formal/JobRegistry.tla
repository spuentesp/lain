------------------------------ MODULE JobRegistry ------------------------------
\* Background-job registry in `ToolExecutor::call` (src/server/tools.rs).
\*
\* A job is started (count Running, insert), its task later finishes or
\* panics, and the registry is snapshotted to jobs.json and restored after a
\* restart. Three independent defects, each switchable:
\*
\*   Atomic      = FALSE : the MAX_CONCURRENT_JOBS count and the insert use two
\*                         separate lock acquisitions (check-then-act race).
\*   PanicSafe   = FALSE : a panicking task never leaves `Running` (leaks a slot).
\*   RestoreSafe = FALSE : jobs persisted as `Running` reload as `Running` after
\*                         a restart even though no task backs them (ghost slots).
\*
\* Safety:
\*   CapBound : never more than Cap jobs Running.
\*   NoGhost  : every Running job has a live task behind it.
EXTENDS Naturals, FiniteSets

CONSTANTS Jobs, Cap, Atomic, PanicSafe, RestoreSafe

VARIABLES state, alive, checked
\* state[j]   : "none" | "running" | "done" | "failed" | "interrupted"
\* alive[j]   : a task is executing job j
\* checked[j] : j passed the cap check but has not inserted yet (racy mode)

vars == <<state, alive, checked>>

Running == {j \in Jobs : state[j] = "running"}

CapBound == Cardinality(Running) <= Cap
NoGhost  == \A j \in Jobs : state[j] = "running" => alive[j]

TypeOK ==
    /\ state \in [Jobs -> {"none", "running", "done", "failed", "interrupted"}]
    /\ alive \in [Jobs -> BOOLEAN]
    /\ checked \in [Jobs -> BOOLEAN]

Init ==
    /\ state = [j \in Jobs |-> "none"]
    /\ alive = [j \in Jobs |-> FALSE]
    /\ checked = [j \in Jobs |-> FALSE]

\* Atomic: check and insert in one critical section.
StartAtomic(j) ==
    /\ Atomic /\ state[j] = "none" /\ Cardinality(Running) < Cap
    /\ state' = [state EXCEPT ![j] = "running"]
    /\ alive' = [alive EXCEPT ![j] = TRUE]
    /\ UNCHANGED checked

\* Racy: the lock is released between the count and the insert.
Check(j) ==
    /\ ~Atomic /\ state[j] = "none" /\ ~checked[j] /\ Cardinality(Running) < Cap
    /\ checked' = [checked EXCEPT ![j] = TRUE]
    /\ UNCHANGED <<state, alive>>
Insert(j) ==
    /\ ~Atomic /\ checked[j]
    /\ state' = [state EXCEPT ![j] = "running"]
    /\ alive' = [alive EXCEPT ![j] = TRUE]
    /\ checked' = [checked EXCEPT ![j] = FALSE]

Finish(j) ==
    /\ alive[j] /\ state[j] = "running"
    /\ state' = [state EXCEPT ![j] = "done"]
    /\ alive' = [alive EXCEPT ![j] = FALSE]
    /\ UNCHANGED checked

Panic(j) ==
    /\ alive[j] /\ state[j] = "running"
    /\ state' = [state EXCEPT ![j] = IF PanicSafe THEN "failed" ELSE "running"]
    /\ alive' = [alive EXCEPT ![j] = FALSE]
    /\ UNCHANGED checked

\* Process restart: every task dies; the registry is rebuilt from jobs.json,
\* which may contain jobs that were Running at snapshot time.
Restart ==
    /\ alive' = [j \in Jobs |-> FALSE]
    /\ state' = [j \in Jobs |-> IF state[j] = "running" /\ RestoreSafe THEN "interrupted" ELSE state[j]]
    /\ checked' = [j \in Jobs |-> FALSE]

Next == \/ \E j \in Jobs : StartAtomic(j) \/ Check(j) \/ Insert(j) \/ Finish(j) \/ Panic(j)
        \/ Restart

Spec == Init /\ [][Next]_vars
=============================================================================
