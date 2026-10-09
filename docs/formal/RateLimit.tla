------------------------------- MODULE RateLimit -------------------------------
\* Token bucket of `Auth::try_consume_at` (src/server/auth.rs), one key.
\*
\* Units are scaled by 60 so everything is an integer: the bucket holds
\* `tokens` in 1/60ths of a request; a call costs 60; one second adds Rpm
\* (= rpm/60 requests); capacity is 60*Rpm (= rpm requests).
\*
\* Clamp = TRUE  : the code (`.min(capacity)` after refilling).
\* Clamp = FALSE : refill without the clamp. EXPECTED to violate the burst /
\*                 rate bound: an idle key banks unlimited credit.
\*
\* Safety:
\*   InRange  - 0 <= tokens <= capacity
\*   BurstBound - however long idle, at most `rpm` calls are admitted at once
\*   RateBound- admitted calls never exceed capacity + refill over the elapsed
\*              time (the documented "rpm per minute" guarantee, burst included)
EXTENDS Naturals

CONSTANTS Rpm, MaxTime, MaxCalls, Clamp

VARIABLES tokens, now, last, admitted, denied, inst
\* inst: admissions at the current instant (reset when time advances)

vars == <<tokens, now, last, admitted, denied, inst>>

Cap == 60 * Rpm
Min(a, b) == IF a < b THEN a ELSE b

TypeOK ==
    /\ tokens \in Nat /\ now \in 0..MaxTime /\ last \in 0..MaxTime
    /\ admitted \in Nat /\ denied \in Nat /\ inst \in Nat

InRange == tokens <= Cap

\* The burst the limiter allows at one instant is at most `rpm` requests,
\* however long the key sat idle.
BurstBound == inst * 60 <= Cap

\* admitted*60 <= Cap + Rpm*elapsed   (elapsed measured from the first call)
RateBound == admitted * 60 <= Cap + Rpm * now

Init == tokens = Cap /\ now = 0 /\ last = 0 /\ admitted = 0 /\ denied = 0 /\ inst = 0

Tick == now < MaxTime /\ now' = now + 1 /\ inst' = 0 /\ UNCHANGED <<tokens, last, admitted, denied>>

Refilled ==
    IF Clamp THEN Min(tokens + (now - last) * Rpm, Cap) ELSE tokens + (now - last) * Rpm

Call ==
    /\ admitted + denied < MaxCalls
    /\ last' = now
    /\ IF Refilled >= 60
         THEN /\ tokens' = Refilled - 60 /\ admitted' = admitted + 1 /\ inst' = inst + 1 /\ UNCHANGED denied
         ELSE /\ tokens' = Refilled /\ denied' = denied + 1 /\ UNCHANGED <<admitted, inst>>
    /\ UNCHANGED now

Finished == now = MaxTime /\ admitted + denied >= MaxCalls /\ UNCHANGED vars

Next == Tick \/ Call \/ Finished

Spec == Init /\ [][Next]_vars
================================================================================
