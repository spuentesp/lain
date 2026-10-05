------------------------------- MODULE LspPool ---------------------------------
\* `LspPool::next` (src/server/lsp.rs): round-robin over `Size` multiplexers with
\* a counter and `idx = fetch_add(1) % Size`. `LspPool` is `Clone`; every clone
\* must rotate the SAME sequence.
\*
\* Shared = TRUE  : the code: one `Arc<AtomicUsize>` shared by all clones.
\* Shared = FALSE : pre-fix: each clone copied the counter's value and rotated on
\*                  its own, so clones kept landing on the same multiplexers
\*                  while others sat idle. EXPECTED to violate Balanced.
\*
\* `fetch_add` is one atomic step, so interleavings of callers are covered by
\* the step granularity here.
EXTENDS Naturals

CONSTANTS Size, Clones, MaxCalls, Shared

VARIABLES ctr, hits, calls
\* ctr: clone -> counter (all equal to the shared one when Shared)

vars == <<ctr, hits, calls>>

Muxes == 0..(Size - 1)
Max(a, b) == IF a > b THEN a ELSE b
Min(a, b) == IF a < b THEN a ELSE b

TypeOK == hits \in [Muxes -> Nat] /\ calls \in Nat

Init == ctr = [c \in Clones |-> 0] /\ hits = [m \in Muxes |-> 0] /\ calls = 0

Call(c) ==
    /\ calls < MaxCalls
    /\ LET i == ctr[c] % Size IN
       /\ hits' = [hits EXCEPT ![i] = @ + 1]
       /\ ctr' = IF Shared THEN [d \in Clones |-> ctr[c] + 1] ELSE [ctr EXCEPT ![c] = @ + 1]
    /\ calls' = calls + 1

Finished == calls = MaxCalls /\ UNCHANGED vars

Next == (\E c \in Clones : Call(c)) \/ Finished

\* No multiplexer is used more than one call ahead of another.
Balanced ==
    \A m1, m2 \in Muxes : hits[m1] - hits[m2] <= 1 \/ hits[m1] <= hits[m2]

\* The pool is never empty: the modulus is non-zero.
NonEmpty == Size >= 1

Spec == Init /\ [][Next]_vars
================================================================================
