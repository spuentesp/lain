------------------------- MODULE FederationRepoLifecycle -------------------------
\* `FederatedIndex::{add_repo, project_nodes, remove_repo}`
\* (src/server/federation/federated_index.rs).
\*
\* Safety: a repo whose nodes are in the federated backend is registered:
\*     NoResurrection:  r \in backendNodes  =>  r \in registered
\* otherwise a removed repo's symbols keep answering `search_org` forever.
\*
\* Checked   = TRUE : current code. `project_nodes` takes `projection_lock`,
\*                    THEN resolves the repo (`get_repo(id)?`, NotFound if it is
\*                    gone) and writes, all inside the lock; `remove_repo`
\*                    purges and deregisters under the same lock.
\* Checked   = FALSE: the check-then-act variant (resolve the repo before
\*                    taking the lock, write after). Models what a refactor that
\*                    hoisted the lookup out of the lock would do.
EXTENDS Naturals

CONSTANTS Repos, Checked

VARIABLES registered, backendNodes, lockHolder, pending
\* pending[r]: a projector resolved r and has not yet written (unchecked mode)

vars == <<registered, backendNodes, lockHolder, pending>>

NoResurrection == \A r \in Repos : r \in backendNodes => r \in registered

TypeOK ==
    /\ registered \subseteq Repos /\ backendNodes \subseteq Repos
    /\ pending \subseteq Repos

Init == registered = {} /\ backendNodes = {} /\ lockHolder = "none" /\ pending = {}

Add(r) ==
    /\ r \notin registered
    /\ registered' = registered \cup {r}
    /\ UNCHANGED <<backendNodes, lockHolder, pending>>

\* Checked: resolve + write are one critical section.
ProjectChecked(r) ==
    /\ Checked /\ r \in registered
    /\ backendNodes' = backendNodes \cup {r}
    /\ UNCHANGED <<registered, lockHolder, pending>>

\* Unchecked: resolve now, write in a later step.
ResolveRepo(r) ==
    /\ ~Checked /\ r \in registered /\ r \notin pending
    /\ pending' = pending \cup {r}
    /\ UNCHANGED <<registered, backendNodes, lockHolder>>
WriteNodes(r) ==
    /\ ~Checked /\ r \in pending
    /\ backendNodes' = backendNodes \cup {r}
    /\ pending' = pending \ {r}
    /\ UNCHANGED <<registered, lockHolder>>

\* Purge + deregister, atomically under the projection lock.
Remove(r) ==
    /\ r \in registered
    /\ backendNodes' = backendNodes \ {r}
    /\ registered' = registered \ {r}
    /\ UNCHANGED <<lockHolder, pending>>

Next == \E r \in Repos :
    Add(r) \/ ProjectChecked(r) \/ ResolveRepo(r) \/ WriteNodes(r) \/ Remove(r)

Spec == Init /\ [][Next]_vars
=============================================================================
