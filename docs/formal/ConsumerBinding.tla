---- MODULE ConsumerBinding ----
\* ConsumerBinding — TLA+ model of the cross-repo consumer→provider
\* resolver at the granularity of `resolve_websocket_consumer` (in
\* `src/server/federation/contracts/joiner/consumer_protocol.rs:365`),
\* targeting invariant I1 ("No invented bindings").
\*
\* Spec: docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md §3 (I1)
\*
\* State:
\* - Providers: a set of provider endpoints, each with a key and a service.
\* - Services: each declares a set of hosts.
\* - Consumers: a set of consumers, each with a key and host evidence.
\* - target: a function from consumers to their resolved target
\*   (a provider id meaning Binds(p), or "Unresolved").
\*
\* Action: `ResolveConsumer(c)` non-deterministically picks the
\* resolution the code produces for c. The action's branches mirror
\* the three cases `resolve_websocket_consumer` handles:
\*   (a) Cardinality 1 → Binds(p) for the unique match
\*   (b) Cardinality 0 → Unresolved (the early `Literal(h) with no
\*       matching service` return at line 397, or `resolve_by_key`'s
\*       0-candidate branch)
\*   (c) Cardinality ≥ 2 → Unresolved (the
\*       `AmbiguityPolicy::GraphqlNoOp` refusal — spec §8.3: "several
\*       services expose the same root field ⇒ ambiguous, never
\*       single-bound")
\*
\* Invariants:
\*   I1a: every `Binds(p)` edge has a corresponding provider whose
\*         `key` matches the consumer's key AND (if the consumer has
\*         `Literal(h)` host evidence) some provider service whose
\*         declared hosts match `h`.
\*   I1b: a consumer whose `Literal(h)` matches no configured
\*         service's hosts must be `Unresolved`, never `Binds`.
\*   I1c: if more than one provider matches, the consumer is
\*         `Unresolved` (ambiguity refuses) — not bound to both, not
\*         bound to one arbitrarily.
\*
\* Out of scope (notes for the report):
\*   - The HTTP ladder (separate tier; spec §7.3, modelled by
\*     `CoverageClaim` and the joiner property tests).
\*   - `resolve_rpc_consumer`'s package-qualified second pass (the
\*     host-evidence step is identical to WebSocket; the second pass
\*     adds a separate matching axis we do not model here).
\*   - `resolve_topic_consumer` and `resolve_graphql_consumer` differ
\*     in ambiguity policy (`NoMatch` / `GraphqlNoOp`) but the host
\*     evidence step is the same as WebSocket's. WebSocket is the
\*     representative case because it uses `GraphqlNoOp` (the most
\*     restrictive of the three policies).
\*
\* Code mapping (TLA+ → Rust):
\*   - `Providers`               ↔ the per-repo provider endpoints in
\*                                 the `EndpointTable`
\*   - `ProviderKey(p)`          ↔ the `ContractKey` of provider p
\*   - `ServiceHosts(s)`         ↔ the `hosts` list of service s
\*                                 (see `host_matches_pattern` in
\*                                 `src/server/federation/contracts/url_resolution.rs:179`)
\*   - `ConsumerKey(c)`          ↔ the `ContractKey` of consumer c
\*   - `ConsumerHost(c)`         ↔ the `HostPart` of consumer c's URL —
\*                                 `Literal(h)` → `h` (∈ Hosts);
\*                                 `None` / `Env` / `Expr` → `None`
\*                                 (the resolver treats these as "no
\*                                 host filter": the `allowed` set
\*                                 stays empty, so any service passes
\*                                 the host gate)
\*   - `target[c]`               ↔ `ConsumerTarget` for consumer c:
\*     Binds(p) → `target[c] = p`; Unresolved → `target[c] = "Unresolved"`

EXTENDS Naturals, FiniteSets

CONSTANTS
    Providers,           \* set of provider ids (strings)
    Services,            \* set of service names (strings)
    Hosts,               \* set of host strings
    Consumers,           \* set of consumer ids (strings)
    Keys                 \* set of contract keys (strings)

\* Sentinel for "no host check". The WebSocket code's `allowed` set
\* is `Vec::new()` for `None` / `Env` / `Expr` (no host filter);
\* `Literal(h)` builds the set from services that declare h. We
\* collapse the three "no filter" cases into one sentinel because the
\* resolver treats them identically at this granularity.
None == "none"
NoneUnresolved == "Unresolved"

ASSUME
    /\ Providers # {}
    /\ Services  # {}
    /\ Hosts     # {}
    /\ Consumers # {}
    /\ Keys      # {}

\* ===== Scenario (hardcoded — see "Modelling note" below) =====
\*
\* The TLC `.cfg` parser is line-based and accepts only simple
\* set, string, and number literals. The function-bag and tuple
\* syntax we would normally use to pass the provider→service map,
\* the service→hosts map, and the consumer→host-evidence map is
\* therefore written as operator definitions inside the spec.
\* This is the established workaround for this toolchain
\* (`CoverageClaim.tla` and `RejoinProtocol.tla` both hardcode
\* their scenario operators the same way; the `.cfg` provides
\* only the SET identifiers).
\*
\* All identifiers in this section are STRING LITERALS so the
\* `.cfg` and the spec agree on them via the set constants
\* declared above. The `ASSUME` block locks the scenario in so a
\* scenario change requires updating both files.

ASSUME
    /\ Providers = {"p1", "p2"}
    /\ Services  = {"svc1", "svc2"}
    /\ Hosts     = {"h1", "h2", "h3"}
    /\ Consumers = {"c1", "c2", "c3"}
    /\ Keys      = {"k1", "k2"}

\* provider p1 belongs to service svc1; p2 belongs to svc2.
ProviderService(p) ==
    IF p = "p1" THEN "svc1"
    ELSE "svc2"

\* Both providers share the key `k1`.
ProviderKey(p) == "k1"

\* svc1 declares h1; svc2 declares h2. h3 is undeclared
\* (the I1b case).
ServiceHosts(s) ==
    IF s = "svc1" THEN {"h1"}
    ELSE IF s = "svc2" THEN {"h2"}
    ELSE {}

\* c1: no host check (None / Env / Expr in the code); c2: Literal(h1);
\* c3: Literal(h3) (unmatched — I1b).
ConsumerKey(c) == "k1"
ConsumerHost(c) ==
    IF c = "c1" THEN None
    ELSE IF c = "c2" THEN "h1"
    ELSE IF c = "c3" THEN "h3"
    ELSE None

\* ===== Helpers =====

\* Provider p "matches" consumer c under the WebSocket resolver's
\* logic. The `target_key` equality check is the primary predicate
\* (provider.key == consumer.key), and the host evidence is the
\* additional axis. The WebSocket code's filter is:
\*
\*     |(svc, k)| k == &key && (allowed.is_empty() || allowed.contains(svc))
\*
\* where `allowed` is empty for `None` / `Env` / `Expr` (no host
\* filter) and contains the services whose hosts match h for
\* `Literal(h)`. The "matches" predicate below captures that
\* exactly: key match AND (no host filter OR the provider's service
\* declares the host).
Matches(p, c) ==
    /\ ProviderKey(p) = ConsumerKey(c)
    /\ \/ ConsumerHost(c) = None
       \/ ConsumerHost(c) \in ServiceHosts(ProviderService(p))

\* The set of providers that match c. Used by the I1c invariant
\* and by the action's branch selector.
MatchingProviders(c) == {p \in Providers : Matches(p, c)}

\* ===== State variables =====

VARIABLES target  \* [Consumers -> Providers ∪ {NoneUnresolved}]

\* ===== Type invariant =====

TypeOK == target \in [Consumers -> Providers \cup {NoneUnresolved}]

\* ===== Invariants (I1) =====

\* **I1a** every `Binds` edge has a corresponding provider whose
\* `key` matches the consumer's key AND (if the consumer has
\* `Literal(h)` host evidence) some provider service whose declared
\* hosts match `h`.
\*
\* Maps to `consumer_protocol.rs:118` (the `Binds { .. }` branch in
\* `resolve_by_key`) — the resolver only writes a `Binds` edge when
\* the candidate pass produced a provider, i.e. the provider is in
\* the candidate set, which by construction is the set of
\* providers matching the consumer's key + host evidence.
I1a == \A c \in Consumers :
    target[c] \in Providers => Matches(target[c], c)

\* **I1b** a consumer whose `Literal(h)` matches no configured
\* service's hosts must be `Unresolved`, never `Binds`.
\*
\* Maps to `consumer_protocol.rs:397` — the early `Literal(h) with
\* no matching service` return. The invariant says the model never
\* reaches a state where a consumer with a literal host that names
\* no service has a Binds edge.
I1b == \A c \in Consumers :
    /\ ConsumerHost(c) \in Hosts
    /\ \A s \in Services : ConsumerHost(c) \notin ServiceHosts(s)
    => target[c] = NoneUnresolved

\* **I1c** if more than one provider matches, the consumer is
\* `Unresolved` (ambiguity refuses) — not bound to both, not bound
\* to one arbitrarily.
\*
\* Maps to `consumer_protocol.rs:112` — the
\* `AmbiguityPolicy::GraphqlNoOp` branch (n != 1 → Unresolved). The
\* WebSocket code uses
\* `GraphqlNoOp { route_owner: Some(own_service.clone()) }` at
\* line 424 precisely so that two services exposing the same key
\* cannot both bind.
I1c == \A c \in Consumers :
    Cardinality(MatchingProviders(c)) >= 2 => target[c] = NoneUnresolved

\* ===== Initialisation =====

Init == target = [c \in Consumers |-> NoneUnresolved]

\* ===== Actions =====

\* ResolveConsumer(c) — `resolve_websocket_consumer` (or the
\* underlying `resolve_by_key` helper) computes the target for c.
\* The action's branches mirror the cases the code handles:
\* cardinality 1 → Binds(p); cardinality ≠ 1 → Unresolved.
\*
\* The non-determinism in `Next` is the choice of WHICH consumer to
\* resolve; the resolution itself is determined by the matching
\* predicate (the code's actual logic).
ResolveConsumer(c) ==
    \/ /\ Cardinality(MatchingProviders(c)) = 1
       /\ \E p \in MatchingProviders(c) :
            target' = [target EXCEPT ![c] = p]
    \/ /\ Cardinality(MatchingProviders(c)) # 1
       /\ target' = [target EXCEPT ![c] = NoneUnresolved]

Next == \E c \in Consumers : ResolveConsumer(c)

vars == <<target>>

Spec == Init /\ [][Next]_vars

====
