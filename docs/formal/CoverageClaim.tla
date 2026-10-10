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
\* scope change, a new unresolved consumer appearing (in ANY protocol
\* family — Task 1 of docs/superpowers/plans/2026-10-07-contract-soundness.md
\* extends the could-match clause to the non-HTTP families Topic, Rpc,
\* Graphql, Table). 2 repos × 2 sensors × 4 languages × 6 protocol
\* families — small enough for exhaustive TLC.
\*
\* CouldMatch code mapping: `CouldMatch(repo)` is the §9.7
\* `could_match` predicate of
\* `src/server/federation/contracts/diff.rs` (conservative by
\* direction — a match counts unless it can be positively ruled out),
\* including its `non_http_could_match` extension for the non-HTTP
\* protocols. Within a family, or when the consumer carries no
\* protocol at all (`unknown` — the `UrlExpr` consumer whose key
\* cannot be ruled out from the URL alone), the match counts.
\* Different protocol families can be ruled out (the helper's
\* `_ => FALSE` arm in code). `change_family` is pinned in the .cfg
\* to the protocol of the change under evaluation; any member of
\* FAMILIES can be pinned by editing one line (symmetry).

EXTENDS Naturals, FiniteSets

CONSTANTS
    Repos,          \* Set of repositories
    Sensors,        \* Set of sensor identifiers
    LANGUAGES,      \* Set of all language identifiers
    CONSUMER_LANGS, \* Subset of LANGUAGES that can carry a consumer
    FAMILIES,       \* Protocol families of unresolved consumers
    change_family   \* FAMILIES — family of the change under evaluation

ASSUME
    /\ Repos # {}
    /\ Sensors # {}
    /\ LANGUAGES # {}
    /\ CONSUMER_LANGS \subseteq LANGUAGES
    /\ FAMILIES # {}
    /\ change_family \in FAMILIES

\* ===== State variables =====

VARIABLES
    analyzed,         \* [repo] -> BOOLEAN
    langs_present,    \* [repo] -> SUBSET LANGUAGES
    sensors_ran,      \* [repo] -> SUBSET Sensors
    sensors_failed,   \* [repo] -> SUBSET Sensors
    unresolved,       \* [repo] -> SUBSET LANGUAGES
    unresolved_cons,  \* [repo] -> SUBSET FAMILIES — protocol families of
                      \*   could-match unresolved consumers
    change_in_scope,  \* SUBSET Repos
    claim_fired       \* BOOLEAN — did we report NoKnownImpact in the last step?

\* ===== Helpers =====

\* Per-sensor language coverage. In the model each sensor supports a
\* fixed subset of LANGUAGES; we model two sensors:
\*   - "http"     supports rust, python
\*   - "entry"    supports java, ruby
Supports(s, lang) ==
    CASE s = "http"  -> lang \in {"rust", "python"}
      [] s = "entry" -> lang \in {"java",  "ruby"}
      [] OTHER       -> FALSE

\* Could-match (§9.7, diff.rs::could_match + non_http_could_match):
\* an unresolved consumer of this repo could match the change iff its
\* protocol family equals the change's family, or it carries no
\* protocol at all (`unknown` — the `UrlExpr` consumer, which cannot
\* be ruled out from the URL alone). Different protocol families are
\* ruled out, exactly as `non_http_could_match`'s `_ => FALSE` arm
\* does in code. A `TRUE` here costs a NeedsInvestigation in the
\* product; a missed `TRUE` would wrongly permit NoKnownImpact (I3).
CouldMatch(repo) ==
    \/ change_family \in unresolved_cons[repo]
    \/ "unknown" \in unresolved_cons[repo]

\* A repo is "complete" iff:
\*   (a) its analyzed flag is true;
\*   (b) no sensor that ran on it failed;
\*   (c) every consumer-capable language present has at least one
\*       successful sensor that supports it;
\*   (d) no could-match unresolved consumer exists for it — in ANY
\*       protocol family (Task 1: the pre-fix diff.rs returned
\*       FALSE for Topic/Rpc/Graphql/Table consumers here, which is
\*       the I3 hole this clause now covers).
RepoComplete(repo) ==
    /\ analyzed[repo]
    /\ sensors_failed[repo] = {}
    /\ \A lang \in (langs_present[repo] \cap CONSUMER_LANGS) :
        \E s \in sensors_ran[repo] : Supports(s, lang)
    /\ unresolved[repo] = {}
    /\ ~CouldMatch(repo)

\* ===== Type invariants =====

TypeOK ==
    /\ analyzed         \in [Repos -> BOOLEAN]
    /\ langs_present    \in [Repos -> SUBSET LANGUAGES]
    /\ sensors_ran      \in [Repos -> SUBSET Sensors]
    /\ sensors_failed   \in [Repos -> SUBSET Sensors]
    /\ unresolved       \in [Repos -> SUBSET LANGUAGES]
    /\ unresolved_cons  \in [Repos -> SUBSET FAMILIES]
    /\ change_in_scope  \subseteq Repos
    /\ claim_fired      \in BOOLEAN

\* ===== I3 invariant (verdict soundness) =====

\* At every reachable state, IF a NoKnownImpact claim fired then
\* every in-scope repo is complete. Equivalently, in every reachable
\* state where some in-scope repo is incomplete, the claim has not
\* fired (and `ReportNoKnownImpact` cannot fire next).
NoKnownImpactSound ==
    claim_fired => \A r \in change_in_scope : RepoComplete(r)

\* ===== Initialisation =====

Init ==
    /\ analyzed         = [r \in Repos |-> FALSE]
    /\ langs_present    = [r \in Repos |-> {}]
    /\ sensors_ran      = [r \in Repos |-> {}]
    /\ sensors_failed   = [r \in Repos |-> {}]
    /\ unresolved       = [r \in Repos |-> {}]
    /\ unresolved_cons  = [r \in Repos |-> {}]
    /\ change_in_scope  = {}
    /\ claim_fired      = FALSE

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
    /\ UNCHANGED <<analyzed, langs_present, sensors_ran, sensors_failed,
                   unresolved, unresolved_cons>>

\* Reindex a repo. Makes it complete (modulo consumer-capable coverage
\* and could-match consumers found by the fresh scan). A reindex
\* invalidates any previous claim. It clears `unresolved_cons` the
\* same way it clears `unresolved`: the rebuilt ledger carries only
\* what the new scan observes.
Reindex(repo) ==
    /\ repo \in Repos
    /\ analyzed'         = [analyzed EXCEPT ![repo] = TRUE]
    /\ langs_present'    = [langs_present EXCEPT ![repo] = LANGUAGES]
    /\ sensors_ran'      = [sensors_ran EXCEPT ![repo] = Sensors]
    /\ sensors_failed'   = [sensors_failed EXCEPT ![repo] = {}]
    /\ unresolved'       = [unresolved EXCEPT ![repo] = {}]
    /\ unresolved_cons'  = [unresolved_cons EXCEPT ![repo] = {}]
    /\ claim_fired'      = FALSE
    /\ UNCHANGED <<change_in_scope>>

\* A sensor fails on a repo.
SensorFail(repo, s) ==
    /\ repo \in Repos
    /\ s \in Sensors
    /\ sensors_failed' = [sensors_failed EXCEPT ![repo] = sensors_failed[repo] \cup {s}]
    /\ claim_fired' = FALSE
    /\ UNCHANGED <<analyzed, langs_present, sensors_ran, unresolved,
                   unresolved_cons, change_in_scope>>

\* A new unresolved consumer appears — a topic subscriber, RPC stub,
\* GraphQL selection or table reader (or an un-keyed `unknown` call)
\* the joiner could not resolve. If its family could match the change,
\* RepoComplete(repo) breaks; either way the previously fired claim is
\* no longer sound for THIS consumer and must be forgotten. Task 1:
\* before the fix, a non-HTTP consumer appearing was invisible to the
\* could-match clause (diff.rs returned FALSE for those families) —
\* I3 requires this action to invalidate the claim too.
AddUnresolved(repo, fam) ==
    /\ repo \in Repos
    /\ fam \in FAMILIES
    /\ fam \notin unresolved_cons[repo]
    /\ unresolved_cons' = [unresolved_cons EXCEPT ![repo] = unresolved_cons[repo] \cup {fam}]
    /\ claim_fired' = FALSE
    /\ UNCHANGED <<analyzed, langs_present, sensors_ran, sensors_failed,
                   unresolved, change_in_scope>>

\* Report NoKnownImpact — the soundness claim. Guarded by the invariant.
\* In the real system this is the verdict reported to the operator.
ReportNoKnownImpact ==
    /\ ~claim_fired
    /\ \A r \in change_in_scope : RepoComplete(r)
    /\ claim_fired' = TRUE
    /\ UNCHANGED <<analyzed, langs_present, sensors_ran, sensors_failed,
                   unresolved, unresolved_cons, change_in_scope>>

\* Forget the previous claim — operator moves on.
ClearClaim ==
    /\ claim_fired
    /\ claim_fired' = FALSE
    /\ UNCHANGED <<analyzed, langs_present, sensors_ran, sensors_failed,
                   unresolved, unresolved_cons, change_in_scope>>

Next ==
    \/ \E r \in Repos : AddScopeUnindexed(r)
    \/ \E r \in Repos : Reindex(r)
    \/ \E r \in Repos, s \in Sensors : SensorFail(r, s)
    \/ \E r \in Repos, f \in FAMILIES : AddUnresolved(r, f)
    \/ ReportNoKnownImpact
    \/ ClearClaim

vars == <<analyzed, langs_present, sensors_ran, sensors_failed, unresolved,
          unresolved_cons, change_in_scope, claim_fired>>

\* ===== Spec =====

Spec == Init /\ [][Next]_vars

====
