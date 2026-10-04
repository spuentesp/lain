---- MODULE CoverageClaim ----
\* Verdict-soundness invariant (I3) for the contract-federation
\* coverage ledger.
\*
\* Spec: docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md §3, §4.
\*
\* I3: NoKnownImpact(change) ⇒ every repo in scope is Analyzed for
\* every consumer-capable language present, AND no unresolved consumer
\* could match.
\*
\* We model the claim `ReportNoKnownImpact` as a guarded action: it can
\* fire only when every in-scope repo is "complete". The invariant
\* `NoKnownImpactSound` asserts that the guard holds whenever the
\* claim fires, plus that the model never lets the guard be violated
\* by an action that puts a repo in scope without indexing it.
\*
\* Action space: reindex (which clears prior failure), sensor failure,
\* scope change. 2 repos × 2 sensors × 4 languages — small enough for
\* exhaustive TLC.

EXTENDS Naturals, FiniteSets

CONSTANTS
    Repos,         \* Set of repositories
    Sensors,       \* Set of sensor identifiers
    LANGUAGES,     \* Set of all language identifiers
    CONSUMER_LANGS \* Subset of LANGUAGES that can carry a consumer

ASSUME
    /\ Repos # {}
    /\ Sensors # {}
    /\ LANGUAGES # {}
    /\ CONSUMER_LANGS \subseteq LANGUAGES

\* ===== State variables =====

VARIABLES
    analyzed,        \* [repo] -> BOOLEAN
    langs_present,   \* [repo] -> SUBSET LANGUAGES
    sensors_ran,     \* [repo] -> SUBSET Sensors
    sensors_failed,  \* [repo] -> SUBSET Sensors
    unresolved,      \* [repo] -> SUBSET LANGUAGES
    change_in_scope, \* SUBSET Repos
    claim_fired      \* BOOLEAN — did we report NoKnownImpact in the last step?

\* ===== Helpers =====

\* Per-sensor language coverage. In the model each sensor supports a
\* fixed subset of LANGUAGES; we model two sensors:
\*   - "http"     supports rust, python
\*   - "entry"    supports java, ruby
Supports(s, lang) ==
    CASE s = "http"  -> lang \in {"rust", "python"}
      [] s = "entry" -> lang \in {"java",  "ruby"}
      [] OTHER       -> FALSE

\* A repo is "complete" iff:
\*   (a) its analyzed flag is true;
\*   (b) no sensor that ran on it failed;
\*   (c) every consumer-capable language present has at least one
\*       successful sensor that supports it;
\*   (d) no could-match unresolved consumer exists for it.
RepoComplete(repo) ==
    /\ analyzed[repo]
    /\ sensors_failed[repo] = {}
    /\ \A lang \in (langs_present[repo] \cap CONSUMER_LANGS) :
        \E s \in sensors_ran[repo] : Supports(s, lang)
    /\ unresolved[repo] = {}

\* ===== Type invariants =====

TypeOK ==
    /\ analyzed       \in [Repos -> BOOLEAN]
    /\ langs_present  \in [Repos -> SUBSET LANGUAGES]
    /\ sensors_ran    \in [Repos -> SUBSET Sensors]
    /\ sensors_failed \in [Repos -> SUBSET Sensors]
    /\ unresolved     \in [Repos -> SUBSET LANGUAGES]
    /\ change_in_scope \subseteq Repos
    /\ claim_fired     \in BOOLEAN

\* ===== I3 invariant (verdict soundness) =====

\* At every reachable state, IF a NoKnownImpact claim fired then
\* every in-scope repo is complete. Equivalently, in every reachable
\* state where some in-scope repo is incomplete, the claim has not
\* fired (and `ReportNoKnownImpact` cannot fire next).
NoKnownImpactSound ==
    claim_fired => \A r \in change_in_scope : RepoComplete(r)

\* ===== Initialisation =====

Init ==
    /\ analyzed        = [r \in Repos |-> FALSE]
    /\ langs_present   = [r \in Repos |-> {}]
    /\ sensors_ran     = [r \in Repos |-> {}]
    /\ sensors_failed  = [r \in Repos |-> {}]
    /\ unresolved      = [r \in Repos |-> {}]
    /\ change_in_scope = {}
    /\ claim_fired     = FALSE

\* ===== Actions =====

\* Add a repo to the change set WITHOUT reindexing. In the real system
\* this would happen if a downstream tool requested NoKnownImpact
\* before LAIN finished indexing the new repo. In the model this is
\* exactly the unsafe transition — invariant I3 must forbid the claim
\* from firing while a repo is in this state.
AddScopeUnindexed(repo) ==
    /\ repo \in Repos
    /\ repo \notin change_in_scope
    /\ change_in_scope' = change_in_scope \cup {repo}
    /\ claim_fired' = FALSE
    /\ UNCHANGED <<analyzed, langs_present, sensors_ran, sensors_failed, unresolved>>

\* Reindex a repo. Makes it complete (modulo consumer-capable coverage).
\* A reindex invalidates any previous claim.
Reindex(repo) ==
    /\ repo \in Repos
    /\ analyzed'       = [analyzed EXCEPT ![repo] = TRUE]
    /\ langs_present'  = [langs_present EXCEPT ![repo] = LANGUAGES]
    /\ sensors_ran'    = [sensors_ran EXCEPT ![repo] = Sensors]
    /\ sensors_failed' = [sensors_failed EXCEPT ![repo] = {}]
    /\ unresolved'     = [unresolved EXCEPT ![repo] = {}]
    /\ claim_fired'    = FALSE
    /\ UNCHANGED change_in_scope

\* A sensor fails on a repo.
SensorFail(repo, s) ==
    /\ repo \in Repos
    /\ s \in Sensors
    /\ sensors_failed' = [sensors_failed EXCEPT ![repo] = sensors_failed[repo] \cup {s}]
    /\ claim_fired' = FALSE
    /\ UNCHANGED <<analyzed, langs_present, sensors_ran, unresolved, change_in_scope>>

\* Report NoKnownImpact — the soundness claim. Guarded by the invariant.
\* In the real system this is the verdict reported to the operator.
ReportNoKnownImpact ==
    /\ ~claim_fired
    /\ \A r \in change_in_scope : RepoComplete(r)
    /\ claim_fired' = TRUE
    /\ UNCHANGED <<analyzed, langs_present, sensors_ran, sensors_failed, unresolved, change_in_scope>>

\* Forget the previous claim — operator moves on.
ClearClaim ==
    /\ claim_fired
    /\ claim_fired' = FALSE
    /\ UNCHANGED <<analyzed, langs_present, sensors_ran, sensors_failed, unresolved, change_in_scope>>

Next ==
    \/ \E r \in Repos : AddScopeUnindexed(r)
    \/ \E r \in Repos : Reindex(r)
    \/ \E r \in Repos, s \in Sensors : SensorFail(r, s)
    \/ ReportNoKnownImpact
    \/ ClearClaim

vars == <<analyzed, langs_present, sensors_ran, sensors_failed, unresolved, change_in_scope, claim_fired>>

\* ===== Spec =====

Spec == Init /\ [][Next]_vars

====