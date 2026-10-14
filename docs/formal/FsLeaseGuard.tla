----------------------------- MODULE FsLeaseGuard -----------------------------
\* Proposed fix for the acquire race in `presence_lock::try_lock`.
\*
\* Keep the nonce-named lock files (release/refresh/CLI hooks unchanged) and
\* wrap ONLY the "scan for live holders + create my file" step in a short
\* critical section guarded by `create_new` (O_EXCL) on one fixed path
\* (`<sanitized>.guard`). `create_new` is the atomic primitive the scan lacked.
\*
\* GuardEnabled = FALSE : current code (scan, then create; two bare steps).
\* GuardEnabled = TRUE  : guarded acquire.
\* Stall        = TRUE  : a guard holder may be paused longer than the guard's
\*                        TTL and have its guard taken over mid-section (the
\*                        residual: a process frozen for > GUARD_TTL).
EXTENDS Naturals, FiniteSets

CONSTANTS Agents, GuardEnabled, Stall
NONE == "none"

VARIABLES locks, belief, pc, guard

vars == <<locks, belief, pc, guard>>

MutualExclusion == Cardinality({a \in Agents : belief[a]}) <= 1

TypeOK ==
    /\ locks \subseteq Agents
    /\ belief \in [Agents -> BOOLEAN]
    /\ pc \in [Agents -> {"idle", "guarded", "scanned", "held"}]
    /\ guard \in Agents \cup {NONE}

Init ==
    /\ locks = {} /\ guard = NONE
    /\ belief = [a \in Agents |-> FALSE]
    /\ pc = [a \in Agents |-> "idle"]

\* create_new(<san>.guard): atomic; fails if the guard file exists.
TakeGuard(a) ==
    /\ GuardEnabled /\ pc[a] = "idle" /\ ~belief[a] /\ guard = NONE
    /\ guard' = a
    /\ pc' = [pc EXCEPT ![a] = "guarded"]
    /\ UNCHANGED <<locks, belief>>

\* Scan: no live lock file exists (cleaning stale siblings is not modelled).
Scan(a) ==
    /\ pc[a] = IF GuardEnabled THEN "guarded" ELSE "idle"
    /\ ~belief[a]
    /\ locks = {}
    /\ pc' = [pc EXCEPT ![a] = "scanned"]
    /\ UNCHANGED <<locks, belief, guard>>

\* Scan found a holder: give up (and drop the guard if we had it).
Conflict(a) ==
    /\ pc[a] = IF GuardEnabled THEN "guarded" ELSE "idle"
    /\ (locks # {} \/ belief[a])
    /\ guard' = IF guard = a THEN NONE ELSE guard
    /\ pc' = [pc EXCEPT ![a] = "idle"]
    /\ UNCHANGED <<locks, belief>>

\* Create my nonce-named file, then drop the guard.
Create(a) ==
    /\ pc[a] = "scanned"
    /\ locks' = locks \cup {a}
    /\ belief' = [belief EXCEPT ![a] = TRUE]
    /\ guard' = IF guard = a THEN NONE ELSE guard
    /\ pc' = [pc EXCEPT ![a] = "idle"]

Release(a) ==
    /\ belief[a]
    /\ locks' = locks \ {a}
    /\ belief' = [belief EXCEPT ![a] = FALSE]
    /\ UNCHANGED <<pc, guard>>

\* A paused guard holder: its guard file ages past GUARD_TTL and another
\* acquirer takes it over while the holder is still between Scan and Create.
StallTakeover(b) ==
    /\ Stall /\ GuardEnabled
    /\ guard # NONE /\ guard # b /\ pc[b] = "idle" /\ ~belief[b]
    /\ guard' = b
    /\ pc' = [pc EXCEPT ![b] = "guarded"]
    /\ UNCHANGED <<locks, belief>>

Next == \E a \in Agents :
    TakeGuard(a) \/ Scan(a) \/ Conflict(a) \/ Create(a) \/ Release(a) \/ StallTakeover(a)

Spec == Init /\ [][Next]_vars
=============================================================================
