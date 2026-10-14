---------------------------- MODULE PresenceClaims -----------------------------
\* Claim arbitration of `OccupancyMap` / `FileOccupancy` (src/server/presence.rs)
\* for one file. An agent may hold a file-level claim and symbol-level claims on
\* the same file, each with an intent (Read or Edit).
\*
\* Rule: a new claim is granted unless another agent's *recorded* intent
\* conflicts: an Edit conflicts with anything, a Read conflicts with an Edit.
\*
\* Strongest = TRUE  : the code (FileOccupancy::strongest_intent): per agent the
\*                     recorded intent is the strongest of everything it holds.
\* Strongest = FALSE : pre-fix: the recorded intent is the LAST claim made, so a
\*                     file-level Read hid the same agent's symbol-level Edit.
\*                     EXPECTED to violate NoConflictingHolders (found by the
\*                     proptest state machine, finding #3).
\*
\* Safety: NoConflictingHolders - no two distinct agents hold conflicting claims.
EXTENDS Naturals, FiniteSets

CONSTANTS Agents, Scopes, Strongest
Intents == {"Read", "Edit"}

VARIABLES held, last
\* held: set of <<agent, scope, intent>> actually granted (ground truth)
\* last: agent -> intent of its most recent grant (or "None")

vars == <<held, last>>

Conflicts(i, j) == i = "Edit" \/ j = "Edit"

Recorded(a) ==
    IF Strongest
    THEN IF \E c \in held : c[1] = a /\ c[3] = "Edit" THEN "Edit"
         ELSE IF \E c \in held : c[1] = a THEN "Read" ELSE "None"
    ELSE last[a]

NoConflictingHolders ==
    \A c1, c2 \in held : c1[1] # c2[1] => ~Conflicts(c1[3], c2[3])

TypeOK == held \subseteq (Agents \X Scopes \X Intents)

Init == held = {} /\ last = [a \in Agents |-> "None"]

Grantable(a, i) ==
    \A b \in Agents \ {a} : Recorded(b) = "None" \/ ~Conflicts(Recorded(b), i)

Claim(a, s, i) ==
    /\ Grantable(a, i)
    /\ held' = held \cup {<<a, s, i>>}
    /\ last' = [last EXCEPT ![a] = i]

Release(a, s, i) ==
    /\ <<a, s, i>> \in held
    /\ held' = held \ {<<a, s, i>>}
    /\ last' = [last EXCEPT ![a] = IF \E c \in held' : c[1] = a THEN last[a] ELSE "None"]

Next == \E a \in Agents, s \in Scopes, i \in Intents : Claim(a, s, i) \/ Release(a, s, i)

Spec == Init /\ [][Next]_vars
================================================================================
