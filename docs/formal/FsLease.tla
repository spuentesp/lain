------------------------------- MODULE FsLease -------------------------------
\* The advisory filesystem lease in src/server/presence_lock.rs.
\*
\* Mode = "Scan"  : current code. Acquire = scan the lock dir for a live
\*                  holder, THEN create a uniquely named file. Two steps.
\* Mode = "Excl"  : proposed. Acquire = `create_new` (O_EXCL) on ONE fixed
\*                  path: atomic check-and-create. Taking over an expired
\*                  lease is `rename(fixed -> private tombstone)` (atomic;
\*                  exactly one stealer wins), then inspect what was moved and
\*                  put it back with `link` if it turned out to be live.
\*                  Release is the same rename-and-verify, so it can never
\*                  delete another agent's file blindly.
\*
\* Expiry = FALSE : leases never expire (pure acquire/release).
\* Expiry = TRUE  : a Tick may expire the lease; the owner's own clock
\*                  expires its belief at the same instant (lease contract:
\*                  an owner stops relying on a lease once its TTL passed).
\*
\* Safety: at most one agent believes it holds the lease.
EXTENDS Naturals, FiniteSets

CONSTANTS Agents, Mode, Expiry

NONE == "none"

VARIABLES
    files,   \* set of lock files on disk: records [owner, live]
    tomb,    \* tomb[a]: file agent a has renamed away and is inspecting, or NONE
    belief,  \* belief[a]: a thinks it holds the lease
    pc,      \* pc[a]: "idle" | "scanned" | "saw_stale" | "renamed"
    purpose  \* purpose[a]: "steal" | "release" for a pending rename

vars == <<files, tomb, belief, pc, purpose>>

MutualExclusion == Cardinality({a \in Agents : belief[a]}) <= 1

TypeOK ==
    /\ \A f \in files : f.owner \in Agents /\ f.live \in BOOLEAN
    /\ belief \in [Agents -> BOOLEAN]
    /\ pc \in [Agents -> {"idle", "scanned", "saw_stale", "renamed"}]

Init ==
    /\ files = {}
    /\ tomb = [a \in Agents |-> NONE]
    /\ belief = [a \in Agents |-> FALSE]
    /\ pc = [a \in Agents |-> "idle"]
    /\ purpose = [a \in Agents |-> "steal"]

LiveFiles == {f \in files : f.live}

\* ---- Mode "Scan" --------------------------------------------------------
\* Step 1: observe that no live lock file exists.
ScanObserve(a) ==
    /\ Mode = "Scan" /\ pc[a] = "idle" /\ ~belief[a]
    /\ LiveFiles = {}
    /\ pc' = [pc EXCEPT ![a] = "scanned"]
    /\ UNCHANGED <<files, tomb, belief, purpose>>
\* Step 2: create our own uniquely named file (always succeeds).
ScanCreate(a) ==
    /\ Mode = "Scan" /\ pc[a] = "scanned"
    /\ files' = files \cup {[owner |-> a, live |-> TRUE]}
    /\ belief' = [belief EXCEPT ![a] = TRUE]
    /\ pc' = [pc EXCEPT ![a] = "idle"]
    /\ UNCHANGED <<tomb, purpose>>

\* ---- Mode "Excl" --------------------------------------------------------
\* Atomic create-if-absent on the fixed path (at most one file ever exists).
ExclAcquire(a) ==
    /\ Mode = "Excl" /\ pc[a] = "idle" /\ ~belief[a]
    /\ files = {}
    /\ files' = {[owner |-> a, live |-> TRUE]}
    /\ belief' = [belief EXCEPT ![a] = TRUE]
    /\ UNCHANGED <<tomb, pc, purpose>>

\* Observe an expired lease (a stat), remember intent to take it over.
SawStale(a) ==
    /\ Mode = "Excl" /\ pc[a] = "idle" /\ ~belief[a]
    /\ \E f \in files : ~f.live
    /\ pc' = [pc EXCEPT ![a] = "saw_stale"] /\ purpose' = [purpose EXCEPT ![a] = "steal"]
    /\ UNCHANGED <<files, tomb, belief>>

\* A holder (or the CLI `unlock`) decides to release: rename away, then verify.
ReleaseStart(a) ==
    /\ Mode = "Excl" /\ pc[a] = "idle"
    /\ belief[a]   \* owner-initiated only: a non-owner's rename-and-verify can
                   \* move a stranger's live file away (found by TLC)
    /\ pc' = [pc EXCEPT ![a] = "saw_stale"] /\ purpose' = [purpose EXCEPT ![a] = "release"]
    /\ belief' = [belief EXCEPT ![a] = FALSE]   \* stops relying on the lease as it begins to release
    /\ UNCHANGED <<files, tomb>>

\* rename(fixed -> tombstone): moves WHATEVER is there now (the earlier
\* observation may be stale). Atomic: only one renamer gets the file.
Rename(a) ==
    /\ Mode = "Excl" /\ pc[a] = "saw_stale" /\ files # {}
    /\ \E f \in files :
         /\ tomb' = [tomb EXCEPT ![a] = f]
         /\ files' = files \ {f}
    /\ pc' = [pc EXCEPT ![a] = "renamed"]
    /\ UNCHANGED <<belief, purpose>>
\* The rename found nothing (somebody else took it): give up.
RenameMiss(a) ==
    /\ Mode = "Excl" /\ pc[a] = "saw_stale" /\ files = {}
    /\ pc' = [pc EXCEPT ![a] = "idle"]
    /\ UNCHANGED <<files, tomb, belief, purpose>>

\* Inspect the moved file. Expired, or our own on release -> discard it.
\* Otherwise it was somebody else's live lease: put it back with link(2),
\* which fails (EEXIST) if a newer lock appeared in the meantime.
Verify(a) ==
    /\ Mode = "Excl" /\ pc[a] = "renamed"
    /\ LET t == tomb[a] IN
       \/ /\ (~t.live \/ (purpose[a] = "release" /\ t.owner = a))
          /\ UNCHANGED <<files, belief>>
       \/ /\ ~(~t.live \/ (purpose[a] = "release" /\ t.owner = a))
          /\ files' = IF files = {} THEN {t} ELSE files   \* restore, or lose it
          /\ UNCHANGED belief
    /\ tomb' = [tomb EXCEPT ![a] = NONE]
    /\ pc' = [pc EXCEPT ![a] = "idle"]
    /\ UNCHANGED purpose

\* ---- Expiry -------------------------------------------------------------
Tick ==
    /\ Expiry
    /\ \E f \in files : f.live /\
         /\ files' = (files \ {f}) \cup {[owner |-> f.owner, live |-> FALSE]}
         /\ belief' = [belief EXCEPT ![f.owner] = FALSE]
    /\ UNCHANGED <<tomb, pc, purpose>>

\* Under Scan, an agent releases by deleting its own uniquely named file.
ScanRelease(a) ==
    /\ Mode = "Scan" /\ belief[a]
    /\ \E f \in files : f.owner = a /\ files' = files \ {f}
    /\ belief' = [belief EXCEPT ![a] = FALSE]
    /\ UNCHANGED <<tomb, pc, purpose>>

Next == \/ \E a \in Agents :
              ScanObserve(a) \/ ScanCreate(a) \/ ScanRelease(a)
              \/ ExclAcquire(a) \/ SawStale(a) \/ ReleaseStart(a)
              \/ Rename(a) \/ RenameMiss(a) \/ Verify(a)
        \/ Tick

Spec == Init /\ [][Next]_vars
=============================================================================
