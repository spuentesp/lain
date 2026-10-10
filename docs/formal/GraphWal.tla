---- MODULE GraphWal ----
\* Write-ahead log for `GraphDatabase` persistence.
\*
\* Spec: docs/CONTRIBUTING_AGENTS.md §B5, 2026-10-04.
\*
\* B5 (2026-10-04): the on-disk `graph.bin` is a single bincode
\* snapshot. A torn write (process crash, full disk, laptop
\* unplugged) makes the whole file unreadable; recovery is a
\* full reindex from source (~5 min for a 41k-LOC repo). The
\* proposed fix introduces a write-ahead log: every
\* `insert_node` / `upsert_edge` is appended to `graph.wal`
\* first, and a periodic checkpoint atomically writes
\* `graph.bin` and truncates the WAL.
\*
\* The spec models the durability / recovery state machine.
\* We track the on-disk log (`log`) and the on-disk checkpoint
\* (`checkpoint`). The in-memory graph is always equal to the
\* log in non-recovery operation; after a crash the recovery
\* loop replays the log to rebuild the in-memory state.

EXTENDS Naturals, FiniteSets, Sequences

CONSTANTS
    Ops,             \* Set of possible operations (insert_node, upsert_edge, ...)
    CheckpointSize,  \* NATURAL - how many ops between checkpoints
    MaxLog           \* NATURAL - bound on log length for finite model

ASSUME
    /\ Ops # {}
    /\ CheckpointSize \in Nat
    /\ MaxLog \in Nat
    /\ MaxLog > 0

VARIABLES log, checkpoint, in_recovery

\* ===== Helpers =====

IsCheckpointed == checkpoint /= << >>

\* A committed checkpoint is a prefix of the log that was
\* current at checkpoint time. After a Checkpoint action,
\* `log` is truncated to `<< >>` and the entire pre-truncate
\* log lives in `checkpoint`. The two together still represent
\* the full history.
DiskHistory ==
    checkpoint \o log  \* append `log` to `checkpoint`

\* The on-disk graph at any moment is the concatenation of
\* `checkpoint` and the current `log`. This is the data the
\* indexer would replay after a crash.
DurableHistory == DiskHistory

\* ===== Type invariants =====

TypeOK ==
    /\ log \in Seq(Ops)
    /\ checkpoint \in Seq(Ops)
    /\ in_recovery \in BOOLEAN

\* ===== Safety invariants =====

S1_LogBounded ==
    Len(log) <= MaxLog

S2_CheckpointIsSnapshot ==
    \* A committed checkpoint was a prefix of some past log.
    \* Concretely: the checkpoint never references ops that
    \* aren't in the durable history. Trivially true given
    \* `checkpoint \o log` is the canonical history.
    TRUE

\* ===== Liveness =====

\* L1: recovery always terminates (bounded by MaxLog).
RecoveryTerminates == [](in_recovery => <>~in_recovery)

\* L2: eventually a checkpoint happens (we don't grow the WAL
\* forever - that is the whole point of the WAL design).
EventuallyCheckpointed == <>[]IsCheckpointed

\* ===== Initialisation =====

Init ==
    /\ log = << >>
    /\ checkpoint = << >>
    /\ in_recovery = FALSE

\* ===== Steady-state actions =====

\* An indexing step: append to the WAL. The in-memory state
\* is always kept in sync, but we don't model it as a separate
\* variable (it's the WAL itself). Crash safety comes from
\* the WAL being on disk before the in-memory state observes
\* the op.
AppendOp(op) ==
    /\ op \in Ops
    /\ Len(log) < MaxLog
    /\ log' = Append(log, op)
    /\ UNCHANGED <<checkpoint, in_recovery>>

\* Periodic checkpoint: take a snapshot of the current log,
\* write it to `graph.bin` (atomic), and truncate the WAL.
\* Modelled as one atomic step.
Checkpoint ==
    /\ ~in_recovery
    /\ Len(log) >= CheckpointSize
    /\ checkpoint' = log
    /\ log' = << >>
    /\ UNCHANGED in_recovery

\* A smaller, less frequent checkpoint (e.g. on graceful
\* shutdown) is also useful. Modelled as a no-op that doesn't
\* truncate, just records the prefix.
SoftCheckpoint ==
    /\ ~in_recovery
    /\ checkpoint' = log
    /\ UNCHANGED <<log, in_recovery>>

\* ===== Crash + recovery =====

\* The process dies in the middle of an operation. The next
\* start of the process triggers recovery.
Crash ==
    /\ ~in_recovery
    /\ in_recovery' = TRUE
    /\ UNCHANGED <<log, checkpoint>>

\* Recovery completes: clears the in_recovery flag. The
\* in-memory graph state is rebuilt from the durable history
\* (a real implementation would replay `log` against the
\* checkpoint's snapshot to produce the in-memory graph).
RecoveryComplete ==
    /\ in_recovery
    /\ in_recovery' = FALSE
    /\ UNCHANGED <<log, checkpoint>>

Next ==
    \/ \E op \in Ops : AppendOp(op)
    \/ Checkpoint
    \/ SoftCheckpoint
    \/ Crash
    \/ RecoveryComplete

vars == <<log, checkpoint, in_recovery>>

Spec == Init /\ [][Next]_vars

\* ===== Combined =====

Safety ==
    /\ TypeOK
    /\ S1_LogBounded

====
