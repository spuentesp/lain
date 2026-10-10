------------------------------ MODULE ReloadBus ------------------------------
\* Models the hot-reload signal path (src/server/reload.rs, cli/server.rs,
\* cli/signal.rs, server/watcher.rs):
\*   producers  = watcher / unix socket / MCP `request_reload` -> `request_reload()`
\*   channel    = tokio::sync::broadcast, bounded; overflow => Lagged
\*   consumer   = ONE rebuild loop: recv -> run_rebuild (reads repos.yaml *now*)
\*
\* Property (no lost reload): when the loop is idle and the channel drained,
\* every request that reached the channel is covered by a rebuild that started
\* after it, i.e. the live federation reflects repos.yaml as of the last request.
\*
\* LateSubscribe = TRUE models cli/server.rs today: `bus.subscribe()` runs
\* inside the spawned loop task, AFTER producers are live, so a request in that
\* window reaches zero receivers and is dropped (tokio broadcast semantics).
\* LateSubscribe = FALSE models the fix: subscribe before spawning.
EXTENDS Naturals

CONSTANTS MaxReq, Cap, LateSubscribe

VARIABLES
    subscribed, \* the loop's receiver exists
    queue,      \* undelivered signals in the channel (<= Cap)
    lagged,     \* receiver overflowed; next recv yields Lagged
    requested,  \* requests issued
    received,   \* requests that reached the channel (subscribed at send time)
    covered,    \* requests covered by the latest started rebuild
    rebuilding

vars == <<subscribed, queue, lagged, requested, received, covered, rebuilding>>

TypeOK ==
    /\ subscribed \in BOOLEAN
    /\ queue \in 0..Cap
    /\ lagged \in BOOLEAN
    /\ requested \in 0..MaxReq
    /\ received \in 0..MaxReq
    /\ covered \in 0..MaxReq
    /\ rebuilding \in BOOLEAN

Quiescent == ~rebuilding /\ queue = 0 /\ ~lagged

\* Nothing the producers did is silently dropped.
NoLostReload == requested = received

\* A quiescent loop has applied every request that reached it.
Converged == Quiescent => covered = received

Init ==
    /\ subscribed = ~LateSubscribe
    /\ queue = 0 /\ lagged = FALSE
    /\ requested = 0 /\ received = 0 /\ covered = 0
    /\ rebuilding = FALSE

Subscribe ==
    /\ ~subscribed
    /\ subscribed' = TRUE
    /\ UNCHANGED <<queue, lagged, requested, received, covered, rebuilding>>

Request ==
    /\ requested < MaxReq
    /\ requested' = requested + 1
    /\ IF subscribed
         THEN /\ received' = received + 1
              /\ IF queue < Cap
                   THEN /\ queue' = queue + 1 /\ UNCHANGED lagged
                   ELSE /\ lagged' = TRUE /\ UNCHANGED queue
         ELSE UNCHANGED <<received, queue, lagged>>
    /\ UNCHANGED <<subscribed, covered, rebuilding>>

\* try_recv Ok or Lagged -> run_rebuild begins; it reads config now, so it
\* covers every request received so far.
RecvAndStartRebuild ==
    /\ subscribed /\ ~rebuilding
    /\ (queue > 0 \/ lagged)
    /\ IF lagged THEN /\ lagged' = FALSE /\ UNCHANGED queue
                 ELSE /\ queue' = queue - 1 /\ UNCHANGED lagged
    /\ rebuilding' = TRUE
    /\ covered' = received
    /\ UNCHANGED <<subscribed, requested, received>>

FinishRebuild ==
    /\ rebuilding
    /\ rebuilding' = FALSE
    /\ UNCHANGED <<subscribed, queue, lagged, requested, received, covered>>

Next == Subscribe \/ Request \/ RecvAndStartRebuild \/ FinishRebuild

Spec == Init /\ [][Next]_vars
=============================================================================
