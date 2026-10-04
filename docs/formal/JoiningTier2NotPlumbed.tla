---- MODULE JoiningTier2NotPlumbed ----
\* Suspicion 4 — Phase B (tier 2 of the I6 total order) and
\* Phase C (env resolution) are unreachable in the production
\* rejoin path. `FederatedIndex::rejoin_contracts` calls
\* `ContractJoiner::run(&contract_nodes, &contract_edges, &config)`,
\* which defaults to `&ClientRegistry::new()` and
\* `&EnvBindingIndex::default()`. The registry-building and
\* env-binding variants `run_with_registry` and
\* `run_with_registry_and_env` are only used by tests.

EXTENDS Naturals, FiniteSets

CONSTANTS
    Calls,
    Registry

ASSUME Calls # {}

VARIABLES
    registry,
    has_http_clients,
    call_terminal,
    calls_seen

TypeOK ==
    /\ registry \subseteq Registry
    /\ has_http_clients \in BOOLEAN
    /\ call_terminal \in [Calls -> {"None", "Binds", "External", "Unresolved"}]
    /\ calls_seen \subseteq Calls

I2EveryCallTerminal ==
    \A c \in calls_seen : call_terminal[c] /= "None"

Init ==
    /\ registry = {}
    /\ has_http_clients = FALSE
    /\ call_terminal = [c \in Calls |-> "None"]
    /\ calls_seen = {}

RegistryStaysEmpty ==
    /\ registry' = registry
    /\ has_http_clients' = has_http_clients
    /\ call_terminal' = call_terminal
    /\ calls_seen' = calls_seen
    /\ UNCHANGED <<>>

Tier1Confirmed(c) ==
    /\ c \in Calls
    /\ c \notin calls_seen
    /\ call_terminal' = [call_terminal EXCEPT ![c] = "Binds"]
    /\ calls_seen' = calls_seen \cup {c}
    /\ UNCHANGED <<registry, has_http_clients>>

Tier2Registry(c) ==
    /\ c \in Calls
    /\ c \notin calls_seen
    /\ registry # {}
    /\ call_terminal' = [call_terminal EXCEPT ![c] = "Binds"]
    /\ calls_seen' = calls_seen \cup {c}
    /\ UNCHANGED <<registry, has_http_clients>>

Tier3HttpClients(c) ==
    /\ c \in Calls
    /\ c \notin calls_seen
    /\ has_http_clients
    /\ call_terminal' = [call_terminal EXCEPT ![c] = "Binds"]
    /\ calls_seen' = calls_seen \cup {c}
    /\ UNCHANGED <<registry, has_http_clients>>

WrapperDropped(c) ==
    /\ c \in Calls
    /\ c \notin calls_seen
    /\ registry = {}
    /\ ~has_http_clients
    /\ calls_seen' = calls_seen \cup {c}
    /\ call_terminal' = call_terminal
    /\ UNCHANGED <<registry, has_http_clients>>

Next ==
    \/ RegistryStaysEmpty
    \/ \E c \in Calls : Tier1Confirmed(c)
    \/ \E c \in Calls : Tier2Registry(c)
    \/ \E c \in Calls : Tier3HttpClients(c)
    \/ \E c \in Calls : WrapperDropped(c)

vars == <<registry, has_http_clients, call_terminal, calls_seen>>

Spec == Init /\ [][Next]_vars

====
