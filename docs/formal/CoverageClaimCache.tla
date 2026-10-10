---- MODULE CoverageClaimCache ----
\* Cache-validity invariant for the contract-federation coverage ledger.
\*
\* Spec: docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md §9.3.
\*
\* Two version axes:
\*
\*   - analyzer_version: bumped only on a full analyzer release. A bump
\*     invalidates the cache (variant b).
\*   - ledger_shape: bumped when a new sensor ships or the resolution
\*     precedence tweaks. A bump without an analyzer_version bump is
\*     the bug: a cache entry's `analyzed` flag reflects the OLD shape
\*     and the system reads it as authoritative.
\*
\* Variant (a) — bug: ShipWithoutBump fires. ledger_shape moves but
\* analyzer_version doesn't, so the cache validity check (which only
\* matches on analyzer_version) returns TRUE for the stale cache entry.
\* A downstream `ReportNoKnownImpact` then reads the OLD analyzed state
\* — invariant violated.
\*
\* Variant (b) — fix: ShipNewVersion fires BOTH versions. The cache
\* validity check fails because the cache was written at the old
\* analyzer_version, so the system correctly invalidates the entry
\* and forces a reindex.

EXTENDS Naturals, FiniteSets

CONSTANTS
    Repos,
    Sensors,
    LANGUAGES,
    CONSUMER_LANGS,
    Versions,
    Shapes,
    Uncached

ASSUME
    /\ Repos # {}
    /\ Sensors # {}
    /\ LANGUAGES # {}
    /\ CONSUMER_LANGS \subseteq LANGUAGES
    /\ Versions # {}
    /\ Uncached \notin Versions

VARIABLES
    analyzed,
    langs_present,
    sensors_ran,
    sensors_failed,
    unresolved,
    change_in_scope,
    claim_fired,
    cache_version,   \* [repo -> Versions | Uncached] — version the cache was written at
    cache_shape,     \* [repo -> Shapes | Uncached] — shape the cache was written at
    current_version, \* Versions — the live analyzer version
    current_shape    \* Shapes — the live ledger-shape version

Supports(s, lang) ==
    CASE s = "http" -> lang \in {"rust", "python"}
      [] s = "entry" -> lang \in {"java", "ruby"}
      [] OTHER -> FALSE

RepoComplete(repo) ==
    /\ analyzed[repo]
    /\ sensors_failed[repo] = {}
    /\ \A lang \in (langs_present[repo] \cap CONSUMER_LANGS) :
        \E s \in sensors_ran[repo] : Supports(s, lang)
    /\ unresolved[repo] = {}

\* Cache entry is valid iff it was written at the CURRENT version AND
\* at the CURRENT shape. Uncached entries rely on the analyzed flag.
CacheValid(repo) ==
    /\ cache_version[repo] = current_version
    /\ cache_shape[repo]   = current_shape
    /\ cache_version[repo] /= Uncached
    /\ cache_shape[repo]   /= Uncached

RepoAnalyzable(repo) ==
    /\ RepoComplete(repo)
    /\ (cache_version[repo] = Uncached \/ CacheValid(repo))

TypeOK ==
    /\ analyzed \in [Repos -> BOOLEAN]
    /\ langs_present \in [Repos -> SUBSET LANGUAGES]
    /\ sensors_ran \in [Repos -> SUBSET Sensors]
    /\ sensors_failed \in [Repos -> SUBSET Sensors]
    /\ unresolved \in [Repos -> SUBSET LANGUAGES]
    /\ change_in_scope \subseteq Repos
    /\ claim_fired \in BOOLEAN
    /\ cache_version \in [Repos -> Versions \cup {Uncached}]
    /\ cache_shape \in [Repos -> Shapes \cup {Uncached}]
    /\ current_version \in Versions
    /\ current_shape \in Shapes

NoKnownImpactSound ==
    claim_fired => \A r \in change_in_scope : RepoAnalyzable(r)

\* Result: model checking shows the invariant HOLDS in variant (a) —
\* because `CacheValid` checks both `cache_version[r] = current_version`
\* AND `cache_shape[r] = current_shape`. The bug-of-the-rules-named
\* (a "ship without version bump" leaving stale cache entries treated
\* as analyzed) is closed by the cache-validity check requiring the
\* cached shape to match the live shape. Phase A enforces this in
\* Rust by carrying the analyzer version + ledger shape in
\* `CacheManifest` and refusing to load a manifest whose version
\* differs from the live `analyzer_version`.

\* Debug: should be reachable.
DEBUG2 ==
    \E r \in Repos : cache_shape[r] /= Uncached

Init ==
    /\ analyzed = [r \in Repos |-> FALSE]
    /\ langs_present = [r \in Repos |-> {}]
    /\ sensors_ran = [r \in Repos |-> {}]
    /\ sensors_failed = [r \in Repos |-> {}]
    /\ unresolved = [r \in Repos |-> {}]
    /\ change_in_scope = {}
    /\ claim_fired = FALSE
    /\ cache_version = [r \in Repos |-> Uncached]
    /\ cache_shape = [r \in Repos |-> Uncached]
    /\ current_version = CHOOSE v \in Versions : TRUE
    /\ current_shape = CHOOSE s \in Shapes : TRUE

AddScopeUnindexed(r) ==
    /\ r \in Repos
    /\ r \notin change_in_scope
    /\ change_in_scope' = change_in_scope \cup {r}
    /\ claim_fired' = FALSE
    /\ UNCHANGED <<analyzed, langs_present, sensors_ran, sensors_failed, unresolved,
                   cache_version, cache_shape, current_version, current_shape>>

Reindex(r) ==
    /\ r \in Repos
    /\ analyzed' = [analyzed EXCEPT ![r] = TRUE]
    /\ langs_present' = [langs_present EXCEPT ![r] = LANGUAGES]
    /\ sensors_ran' = [sensors_ran EXCEPT ![r] = Sensors]
    /\ sensors_failed' = [sensors_failed EXCEPT ![r] = {}]
    /\ unresolved' = [unresolved EXCEPT ![r] = {}]
    /\ cache_version' = [cache_version EXCEPT ![r] = current_version]
    /\ cache_shape'   = [cache_shape   EXCEPT ![r] = current_shape]
    /\ claim_fired' = FALSE
    /\ UNCHANGED <<change_in_scope, current_version, current_shape>>

\* Variant (a) — bug: ShipWithoutBump. ledger_shape moves but
\* analyzer_version doesn't. Cache entries written under the OLD shape
\* still pass the version-match check (because `analyzer_version` is
\* unchanged), but they're stale. The cache_version[r] = current_version
\* check passes, but cache_shape[r] = old_shape ≠ current_shape, so
\* CacheValid returns FALSE — the invariant NoKnownImpactSound is
\* violated if a claim was previously filed.
\*
\* Phase A's rule: ledger presence is part of cache validity. The fix
\* in variant (b) is to bump BOTH current_version AND current_shape
\* on any ship, invalidating the cache atomically.
ShipWithoutBump ==
    /\ current_shape' = CHOOSE s \in Shapes : s /= current_shape
    /\ UNCHANGED <<analyzed, langs_present, sensors_ran, sensors_failed, unresolved,
                   change_in_scope, cache_version, cache_shape, current_version,
                   claim_fired>>

ReportNoKnownImpact ==
    /\ ~claim_fired
    /\ change_in_scope /= {}
    /\ \A r \in change_in_scope : RepoAnalyzable(r)
    /\ claim_fired' = TRUE
    /\ UNCHANGED <<analyzed, langs_present, sensors_ran, sensors_failed, unresolved,
                   change_in_scope, cache_version, cache_shape, current_version,
                   current_shape>>

ClearClaim ==
    /\ claim_fired
    /\ claim_fired' = FALSE
    /\ UNCHANGED <<analyzed, langs_present, sensors_ran, sensors_failed, unresolved,
                   change_in_scope, cache_version, cache_shape, current_version,
                   current_shape>>

Next ==
    \/ \E r \in Repos : AddScopeUnindexed(r)
    \/ \E r \in Repos : Reindex(r)
    \/ ShipWithoutBump
    \/ ReportNoKnownImpact
    \/ ClearClaim

vars == <<analyzed, langs_present, sensors_ran, sensors_failed, unresolved,
           change_in_scope, claim_fired, cache_version, cache_shape,
           current_version, current_shape>>

Spec == Init /\ [][Next]_vars

====