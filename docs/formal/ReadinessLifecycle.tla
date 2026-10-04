------------------------- MODULE ReadinessLifecycle -------------------------
\* Models `ReadinessHandle` (src/server/readiness.rs) as driven by the
\* indexing passes in `build_core_memory` and their callers
\* (mcp/handler.rs startup re-index, ingest/jobs.rs background sync).
\*
\* A *pass* is one `build_core_memory` run:
\*   Begin   = `resume_warming_up()`  (pass starts mutating the graph)
\*   Finish  = the pass returns        (pass stops mutating the graph)
\*   Publish = the CALLER then calls `ready()` / `failed()` / `cancelled()`
\* Finish and Publish are separate steps: the caller runs
\* `sync_volatile_overlay().await` between them, and another pass can
\* begin in that window.
\*
\* Safety property the central gate relies on (readiness.rs `gate_tool_call`):
\*     GateOpenImpliesQuiescent:  state = "ready"  =>  no pass is mutating
\* and cancellation is terminal:
\*     CancelledIsTerminal:       once cancelled, never "ready" again.
\*
\* Guarded = FALSE  models the pre-fix code (`ready()` unconditional).
\* Guarded = TRUE   models the fix (`ready()` is ignored while another pass
\*                  is in flight, and never overrides `index_cancelled`).
EXTENDS Naturals, FiniteSets

CONSTANTS Passes, Guarded

VARIABLES
    state,      \* "warming" | "ready" | "error"
    cancelled,  \* problem.code = "index_cancelled" is currently published
    pass,       \* [Passes -> {"idle","running","finishing","done"}]
    everCancelled

vars == <<state, cancelled, pass, everCancelled>>

Mutating == {p \in Passes : pass[p] = "running"}

TypeOK ==
    /\ state \in {"warming", "ready", "error"}
    /\ cancelled \in BOOLEAN
    /\ pass \in [Passes -> {"idle", "running", "finishing", "done"}]
    /\ everCancelled \in BOOLEAN

GateOpenImpliesQuiescent == state = "ready" => Mutating = {}

CancelledIsTerminal == everCancelled => state # "ready"

\* Once every pass is done, the gate must not be stuck on "warming".
NoStrandedWarming == (\A p \in Passes : pass[p] = "done") => state # "warming"

Init ==
    /\ state = "warming"
    /\ cancelled = FALSE
    /\ pass = [p \in Passes |-> "idle"]
    /\ everCancelled = FALSE

\* A pass may begin after cancellation (the token check in
\* `build_core_memory` is check-then-act). `LifecycleCore::begin_pass`
\* then registers the pass but leaves the cancelled state untouched.
Begin(p) ==
    /\ pass[p] = "idle"
    /\ pass' = [pass EXCEPT ![p] = "running"]
    /\ IF Guarded /\ cancelled
         THEN UNCHANGED <<state, cancelled>>
         ELSE /\ state' = "warming" /\ cancelled' = FALSE
    /\ UNCHANGED everCancelled

Finish(p) ==
    /\ pass[p] = "running"
    /\ pass' = [pass EXCEPT ![p] = "finishing"]
    /\ UNCHANGED <<state, cancelled, everCancelled>>

PublishReady(p) ==
    /\ pass[p] = "finishing"
    /\ pass' = [pass EXCEPT ![p] = "done"]
    /\ IF Guarded /\ (Mutating # {} \/ cancelled)
         THEN UNCHANGED <<state, cancelled>>
         ELSE /\ state' = "ready" /\ cancelled' = FALSE
    /\ UNCHANGED everCancelled

PublishFailed(p) ==
    /\ pass[p] = "finishing"
    /\ pass' = [pass EXCEPT ![p] = "done"]
    /\ state' = "error"
    /\ UNCHANGED <<cancelled, everCancelled>>

PublishCancelled(p) ==
    /\ pass[p] = "finishing"
    /\ pass' = [pass EXCEPT ![p] = "done"]
    /\ state' = "error"
    /\ cancelled' = TRUE
    /\ everCancelled' = TRUE

Next == \E p \in Passes :
    Begin(p) \/ Finish(p) \/ PublishReady(p) \/ PublishFailed(p) \/ PublishCancelled(p)

Spec == Init /\ [][Next]_vars
=============================================================================
