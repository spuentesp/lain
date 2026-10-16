---- MODULE ConsumerBinding ----
\* ConsumerBinding — TLA+ model of the cross-repo consumer→provider
\* resolver at the granularity of `resolve_websocket_consumer` (in
\* `src/server/federation/contracts/joiner/consumer_protocol.rs:428`)
\* plus the confirmed-binding override
\* (`src/server/federation/contracts/joiner/confirmed.rs`,
\* `apply_confirmed_binding` at `:41`, step 6 of the joiner — call
\* site `src/server/federation/contracts/joiner/mod.rs:405`),
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
\* resolution the code produces for c. The branches mirror the
\* code's ORDER: the resolution ladder first (the three cases
\* `resolve_websocket_consumer` handles):
\*   (a) Cardinality 1 → Binds(p) for the unique match
\*   (b) Cardinality 0 → Unresolved (the early `Literal(h) with no
\*       matching service` return at `consumer_protocol.rs:460`, or
\*       `resolve_by_key`'s 0-candidate branch)
\*   (c) Cardinality ≥ 2 → Unresolved (the
\*       `AmbiguityPolicy::GraphqlNoOp` refusal at
\*       `consumer_protocol.rs:115` — spec §8.3: "several
\*       services expose the same root field ⇒ ambiguous, never
\*       single-bound")
\* and then the confirmed override, which REPLACES the ladder's
\* verdict when the binding applies — its pinned provider endpoint
\* exists AND the binding's key pair is well-formed in the
\* `check_binding` sense (§12: same key kind; for HTTP, compatible
\* methods and templates — so a same-kind template bridge whose key
\* strings differ is fine; for GraphQL, equal `(op, field)`).
\* A confirmed binding that does not apply (endpoint missing, or an
\* ill-formed pair) is ignored — a stale binding in the code,
\* recorded with `NoEndpoint` / `IllFormedKeys` — and the ladder's
\* result stands.
\*
\* Abstraction (chosen deliberately): the model's key constants are
\* opaque strings — they carry no kind (http / graphql) and no
\* template structure — so pair well-formedness is a DECLARED
\* premise, `BridgeOK(c)`, not something derivable from the key
\* strings. `BridgeOK(c)` asserts that c's binding pair
\* (`ConsumerKey(c)`, `ProviderKey(ConfirmedProvider(c))`) is
\* well-formed in the code's sense
\* (`federation/contracts/key_compat.rs::keys_compatible`, built
\* from `check_binding`'s `methods_compatible` +
\* `templates_compatible` and the GraphQL `(op, field)` equality).
\* A pair whose strings DIFFER can still be well-formed (c5 — the
\* template-bridge case); an ill-formed cross-kind pair is c6.
\*
\* Invariants:
\*   I1a: every `Binds(p)` edge is backed by a well-formed pair.
\*         For a confirmed consumer: the target is the binding's
\*         declared provider AND the pair is well-formed
\*         (`BridgeOK(c)`). For a NON-confirmed consumer: the
\*         provider's key matches the consumer's key AND (if the
\*         consumer has `Literal(h)` host evidence) some provider
\*         service whose declared hosts match `h`. Confirmed
\*         consumers skip the host clause (the confirmed matcher
\*         has no host axis).
\*   I1b: a NON-confirmed consumer whose `Literal(h)` matches no
\*         configured service's hosts must be `Unresolved`, never
\*         `Binds` (confirmed consumers are excluded — the matcher
\*         ignores host evidence).
\*   I1c: if more than one provider matches and the consumer is NOT
\*         confirmed, the consumer is `Unresolved` (ambiguity
\*         refuses) — not bound to both, not bound to one
\*         arbitrarily. The confirmed override is the sanctioned
\*         exception.
\*   I1d: a confirmed consumer's target, when a `Binds`, equals
\*         `ConfirmedProvider(c)` — the override never picks a third
\*         provider.
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
\*     representative LADDER case because it uses `GraphqlNoOp` (the
\*     most restrictive of the three policies). GraphQL consumers
\*     are NOT out of scope on the confirmed axis: since the GraphQL
\*     config bridge they also flow through `apply_confirmed_binding`
\*     (step 6 matches HTTP and GraphQL consumer nodes alike), and
\*     the model's confirmed axis covers both kinds — the binding
\*     keys are opaque strings either way.
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
\*   - `IsConfirmed(c)`          ↔ a `repos.yaml#bindings[]` entry
\*                                 names c (the node-walk match in
\*                                 `apply_confirmed_binding`)
\*   - `ConfirmedProvider(c)`    ↔ the binding's `(provider.service,
\*                                 provider.key)` resolved to a
\*                                 provider id in the `EndpointTable`
\*   - `BridgeOK(c)`             ↔ the binding's two parsed keys are
\*                                 a well-formed pair —
\*                                 `key_compat::keys_compatible`
\*                                 (`federation/contracts/key_compat.rs:62`,
\*                                 built on `check_binding`'s
\*                                 `methods_compatible` /
\*                                 `templates_compatible` and the
\*                                 GraphQL `(op, field)` equality)
\*   - `ConfirmedApplies(c)`     ↔ the binding is not stale: the
\*                                 endpoint is found AND the pair is
\*                                 well-formed (`BridgeOK(c)`; the
\*                                 `IllFormedKeys` guard in
\*                                 `joiner/confirmed.rs`)
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
    /\ Consumers = {"c1", "c2", "c3", "c4", "c5", "c6"}
    /\ Keys      = {"k1", "k2", "k3"}

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
\* c3: Literal(h3) (unmatched — I1b). c4–c6 are the confirmed axis:
\* c4 key k1, no host check (its binding pins p1 out of a 2-candidate
\* ambiguity — the I1c exception; pair (k1, k1), well-formed);
\* c5 key k2, Literal(h3) (its binding pins p2, whose key is k1 — a
\* WELL-FORMED template bridge whose key strings differ, binding
\* despite the unmatched host: the I1a host-clause skip and the I1b
\* exception); c6 key k3 (its binding pins p1, whose key is k1 —
\* an ILL-FORMED cross-kind pair: the fixed model ignores the
\* binding and the ladder stands).
ConsumerKey(c) ==
    IF c = "c5" THEN "k2"
    ELSE IF c = "c6" THEN "k3"
    ELSE "k1"
ConsumerHost(c) ==
    IF c = "c1" \/ c = "c4" \/ c = "c6" THEN None
    ELSE IF c = "c2" THEN "h1"
    ELSE IF c = "c3" \/ c = "c5" THEN "h3"
    ELSE None

\* Consumers under `repos.yaml#bindings` (§7.6) — the confirmed axis
\* of `apply_confirmed_binding`. The matcher covers HTTP and GraphQL
\* consumer nodes alike (opaque keys either way).
IsConfirmed(c) == c \in {"c4", "c5", "c6"}

\* The provider each confirmed consumer's binding pins —
\* `ConfirmedProvider(c)` is only consulted when `IsConfirmed(c)`;
\* the `"unconfirmed"` sentinel is a don't-care for the rest.
ConfirmedProvider(c) ==
    IF c = "c4" THEN "p1"
    ELSE IF c = "c5" THEN "p2"
    ELSE IF c = "c6" THEN "p1"
    ELSE "unconfirmed"

\* Pair well-formedness (the `BridgeOK` abstraction — see the
\* header): is the pair (ConsumerKey(c), ProviderKey(ConfirmedPro-
\* vider(c))) well-formed in the `check_binding` sense the code
\* enforces (`key_compat::keys_compatible`)? c4: identical strings
\* (trivially well-formed). c5: DIFFERING strings that stand for a
\* same-kind template bridge (well-formed). c6: differing strings
\* that stand for a cross-kind pair (ill-formed). Only consulted
\* when `IsConfirmed(c)`.
BridgeOK(c) == c \in {"c4", "c5"}

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

\* A confirmed binding "applies" to c when it is not stale: the
\* pinned provider endpoint exists (a missing endpoint is the
\* `NoEndpoint` stale case) and the pair is well-formed (`BridgeOK`
\* — an ill-formed pair is the `IllFormedKeys` stale case; the
\* guard in `joiner/confirmed.rs::apply_confirmed_binding` never
\* binds such an entry). When the binding applies, step 6 REPLACES
\* the ladder's verdict with the pinned provider; when it does not,
\* the binding is ignored and the ladder's result stands. c6
\* exercises the ill-formed-pair fall-through; a missing pinned
\* endpoint fails the same guard conjunct and falls through the
\* same way.
ConfirmedApplies(c) ==
    /\ IsConfirmed(c)
    /\ ConfirmedProvider(c) \in Providers
    /\ BridgeOK(c)

\* ===== State variables =====

VARIABLES target  \* [Consumers -> Providers ∪ {NoneUnresolved}]

\* ===== Type invariant =====

TypeOK == target \in [Consumers -> Providers \cup {NoneUnresolved}]

\* ===== Invariants (I1) =====

\* **I1a** every `Binds` edge is backed by a well-formed pair.
\* For a CONFIRMED consumer: the target is the binding's declared
\* provider AND the pair is well-formed (`BridgeOK(c)`) — the
\* unconditional clause. For a NON-confirmed consumer: the
\* provider's key matches the consumer's key AND (if the consumer
\* has `Literal(h)` host evidence) some provider service whose
\* declared hosts match `h`. Confirmed consumers skip the host
\* clause: the confirmed matcher has no host axis.
\*
\* Maps to `consumer_protocol.rs:121` (the `Binds { .. }` branch in
\* `resolve_by_key`) — the resolver only writes a `Binds` edge when
\* the candidate pass produced a provider, i.e. the provider is in
\* the candidate set, which by construction is the set of
\* providers matching the consumer's key + host evidence — and to
\* `joiner/confirmed.rs::apply_confirmed_binding`, which binds only
\* a well-formed pair (`IllFormedKeys` guard on
\* `key_compat::keys_compatible`) and only ever to the binding's
\* own pinned endpoint (no host axis). `config.rs::validate`
\* rejects the ill-formed pairs at load.
I1a == \A c \in Consumers :
    target[c] \in Providers =>
        \/ /\ IsConfirmed(c)
           /\ target[c] = ConfirmedProvider(c)
           /\ BridgeOK(c)
        \/ /\ ~IsConfirmed(c)
           /\ Matches(target[c], c)

\* **I1b** a NON-confirmed consumer whose `Literal(h)` matches no
\* configured service's hosts must be `Unresolved`, never `Binds`.
\* Confirmed consumers are excluded: the confirmed matcher ignores
\* host evidence (and the override is the sanctioned exception).
\*
\* Maps to `consumer_protocol.rs:460` — the early `Literal(h) with
\* no matching service` return. The invariant says the model never
\* reaches a state where a non-confirmed consumer with a literal
\* host that names no service has a Binds edge.
I1b == \A c \in Consumers :
    (~IsConfirmed(c)
     /\ ConsumerHost(c) \in Hosts
     /\ \A s \in Services : ConsumerHost(c) \notin ServiceHosts(s))
    => target[c] = NoneUnresolved

\* **I1c** if more than one provider matches and the consumer is
\* NOT confirmed, the consumer is `Unresolved` (ambiguity refuses)
\* — not bound to both, not bound to one arbitrarily. The confirmed
\* override is the sanctioned exception: step 6 replaces the
\* ladder's ambiguity verdict with the operator's pinned provider.
\*
\* Maps to `consumer_protocol.rs:115` — the
\* `AmbiguityPolicy::GraphqlNoOp` branch (n != 1 → Unresolved). The
\* WebSocket code uses
\* `GraphqlNoOp { route_owner: Some(own_service.clone()) }` at
\* `consumer_protocol.rs:487` precisely so that two services
\* exposing the same key cannot both bind; `confirmed.rs` step 6
\* then applies AFTER that refusal and may replace it.
I1c == \A c \in Consumers :
    (~IsConfirmed(c) /\ Cardinality(MatchingProviders(c)) >= 2)
    => target[c] = NoneUnresolved

\* **I1d** a confirmed consumer's target, when a `Binds`, equals
\* `ConfirmedProvider(c)` — the override never picks a third
\* provider.
\*
\* Maps to `joiner/confirmed.rs::apply_confirmed_binding`: the only
\* provider id step 6 can write is the one the binding's
\* `(provider.service, provider.key)` resolves to in the
\* `EndpointTable`.
I1d == \A c \in Consumers :
    (IsConfirmed(c) /\ target[c] \in Providers)
    => target[c] = ConfirmedProvider(c)

\* ===== Initialisation =====

Init == target = [c \in Consumers |-> NoneUnresolved]

\* ===== Actions =====

\* ResolveConsumer(c) — mirrors the code's ORDER: the resolution
\* ladder (`resolve_websocket_consumer`, or the underlying
\* `resolve_by_key` helper) computes the target for c first —
\* cardinality 1 → Binds(p); cardinality ≠ 1 → Unresolved — and then
\* step 6 (`apply_confirmed_binding`) REPLACES that verdict with the
\* pinned provider when the binding applies (`ConfirmedApplies(c)`:
\* its provider endpoint exists AND the pair is well-formed). A
\* confirmed binding that does not apply is ignored (a stale binding
\* in the code) and the ladder's result stands.
\*
\* The non-determinism in `Next` is the choice of WHICH consumer to
\* resolve; the resolution itself is determined by the matching
\* predicate (the code's actual logic).
ResolveConsumer(c) ==
    \/ /\ ConfirmedApplies(c)
       /\ target' = [target EXCEPT ![c] = ConfirmedProvider(c)]
    \/ /\ ~ConfirmedApplies(c)
       /\ \/ /\ Cardinality(MatchingProviders(c)) = 1
             /\ \E p \in MatchingProviders(c) :
                  target' = [target EXCEPT ![c] = p]
          \/ /\ Cardinality(MatchingProviders(c)) # 1
             /\ target' = [target EXCEPT ![c] = NoneUnresolved]

Next == \E c \in Consumers : ResolveConsumer(c)

vars == <<target>>

Spec == Init /\ [][Next]_vars

====
