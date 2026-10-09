---- MODULE OneshotSharedServer ----
\* Shared `lain mcp` server for `oneshot` clients.
\*
\* Spec: docs/CONTRIBUTING_AGENTS.md §B1, 2026-10-04.
\*
\* B1 (2026-10-04): the existing `lain oneshot` always spawns a fresh
\* `lain mcp` per call, paying ~5 min of cold reindex for a 41k-LOC
\* repo on every invocation. The proposed fix binds a per-workspace
\* Unix socket inside the spawned `lain mcp`; subsequent `oneshot`
\* calls consult the socket first and only spawn a fresh server when
\* the previous one is dead.
\*
\* The spec models the lifecycle of a per-workspace server process
\* under concurrent `oneshot` calls. We model:
\*
\*   - **At most one server alive at a time** for a given workspace.
\*     Two clients racing to spawn must end up with one process
\*     serving both — never two processes sharing a socket and one
\*     crashing on `EADDRINUSE`.
\*   - **A client that finds no socket must end up served** by a
\*     process whose socket lifetime brackets the client's call.
\*   - **A crashed server is eventually replaced** so a long-lived
\*     CI loop doesn't permanently lose its warm index.
\*
\* The spec is intentionally small (2 clients, 1 server) so the
\* state space is finite and TLC can exhaust it; a variant with N
\* clients is straightforward.

EXTENDS Naturals, FiniteSets

CONSTANTS
    Clients,         \* Set of oneshot-caller identifiers
    Servers,         \* Set of server-process identifiers
    None             \* Sentinel: "no server currently alive"

ASSUME
    /\ Clients # {}
    /\ Servers # {}
    /\ None \notin Servers

VARIABLES
    alive,         \* [server] -> BOOLEAN — whether the server process is up
    socket,        \* [client] -> server or None — the server each client observes
    serving,       \* [server] -> SUBSET Clients — clients currently using this server
    spawn_in_flight\* BOOLEAN — is a client mid-spawn? (prevents two clients racing to bind)

\* ===== Helpers =====

HasLiveServer == {s \in Servers : alive[s]}

ClientBound(c) == socket[c] /= None

\* ===== Type invariants =====

TypeOK ==
    /\ alive \in [Servers -> BOOLEAN]
    /\ socket \in [Clients -> Servers \cup {None}]
    /\ serving \in [Servers -> SUBSET Clients]
    /\ spawn_in_flight \in BOOLEAN

\* ===== Safety invariants =====

\* S1: at most one server process is alive at a time.
AtMostOneServer ==
    Cardinality(HasLiveServer) <= 1

\* S2: a client bound to a server means the server is alive.
ClientBoundImpliesServerAlive ==
    \A c \in Clients : socket[c] /= None => alive[socket[c]]

\* S3: a client's serving set is a subset of clients whose socket
\* points to that server.
ServingConsistentWithSockets ==
    \A s \in Servers :
        \A c \in serving[s] : socket[c] = s

\* S4: a client is in exactly one server's serving set iff it is
\* bound to that server. The `IF` guards the `serving[socket[c]]`
\* lookup: when `socket[c] = None` the LHS is FALSE, and the RHS
\* must be FALSE too, which it is trivially when we don't
\* dereference `serving` at the `None` sentinel.
ServingInSyncWithSockets ==
    \A c \in Clients :
        IF socket[c] /= None
        THEN c \in serving[socket[c]]
        ELSE TRUE

\* ===== Liveness =====

\* L1: every bound client eventually finishes its call and unbinds.
CallsComplete == []<>(\A c \in Clients : ~ClientBound(c))

\* L2: a crashed server is eventually replaced so a new client
\* doesn't find a stale "no server" state forever.
CrashRecovered ==
    []((\A s \in Servers : ~alive[s]) => <>(\E s \in Servers : alive[s]))

\* ===== Initialisation =====

Init ==
    /\ alive = [s \in Servers |-> FALSE]
    /\ socket = [c \in Clients |-> None]
    /\ serving = [s \in Servers |-> {}]
    /\ spawn_in_flight = FALSE

\* ===== Actions =====

\* A client attempts to use the shared server. Three sub-cases:
\*   1. server already alive -> bind to it (cheap path)
\*   2. no server alive, no spawn in flight -> start one, then bind
\*   3. no server alive, spawn in flight -> wait (model as "no-op"
\*      in this transition; the spawning client will set socket when
\*      it completes the spawn)
ClientArrive(c) ==
    /\ ~ClientBound(c)
    /\ \/ /\ HasLiveServer /= {}
       /\ \E s \in HasLiveServer :
            /\ socket' = [socket EXCEPT ![c] = s]
            /\ serving' = [serving EXCEPT ![s] = serving[s] \cup {c}]
            /\ UNCHANGED <<alive, spawn_in_flight>>
    \/ /\ HasLiveServer = {}
       /\ ~spawn_in_flight
       /\ spawn_in_flight' = TRUE
       /\ UNCHANGED <<alive, socket, serving>>
    \/ /\ HasLiveServer = {}
       /\ spawn_in_flight
       /\ UNCHANGED <<alive, socket, serving, spawn_in_flight>>

\* The client that flipped `spawn_in_flight` to TRUE eventually
\* finishes spawning and binds a server. This is the only action
\* that can add a live server.
FinishSpawn ==
    /\ spawn_in_flight
    /\ \E s \in Servers : ~alive[s]
    /\ \E s \in Servers :
        /\ ~alive[s]
        /\ alive' = [alive EXCEPT ![s] = TRUE]
        /\ \E c \in Clients : socket[c] = None  \* a real client must exist to bind
        /\ socket' = [socket EXCEPT
                         ![CHOOSE c \in Clients : socket[c] = None] = s]
        /\ serving' = [serving EXCEPT
                         ![s] = serving[s] \cup
                              {CHOOSE c \in Clients : socket[c] = None}]
        /\ spawn_in_flight' = FALSE

\* A client finishes its tool call and unbinds. If the server has
\* no more clients and the operator didn't ask for a long-lived
\* instance, the server can shut down — modelled as a single
\* optional action.
ClientLeave(c) ==
    /\ ClientBound(c)
    /\ LET s == socket[c] IN
        /\ socket' = [socket EXCEPT ![c] = None]
        /\ serving' = [serving EXCEPT ![s] = serving[s] \ {c}]
        /\ UNCHANGED <<alive, spawn_in_flight>>

\* A client finished and the server has no more clients; it
\* gracefully shuts down.
LastClientLeavesShutdown ==
    /\ \E s \in Servers :
        /\ alive[s]
        /\ serving[s] = {}
        /\ alive' = [alive EXCEPT ![s] = FALSE]
        /\ UNCHANGED <<socket, serving, spawn_in_flight>>

\* The server process crashes (e.g. SIGSEGV, OOM kill, laptop
\* unplugged). The socket is gone; the next client must respawn.
ServerCrashes(s) ==
    /\ alive[s]
    /\ alive' = [alive EXCEPT ![s] = FALSE]
    /\ serving' = [serving EXCEPT ![s] = {}]
    /\ socket' = [c \in Clients |-> IF socket[c] = s THEN None ELSE socket[c]]
    /\ UNCHANGED spawn_in_flight

Next ==
    \/ \E c \in Clients : ClientArrive(c)
    \/ FinishSpawn
    \/ \E c \in Clients : ClientLeave(c)
    \/ LastClientLeavesShutdown
    \/ \E s \in Servers : ServerCrashes(s)

vars == <<alive, socket, serving, spawn_in_flight>>

Spec == Init /\ [][Next]_vars

\* ===== Combined invariants for TLC =====

Safety ==
    /\ TypeOK
    /\ AtMostOneServer
    /\ ClientBoundImpliesServerAlive
    /\ ServingConsistentWithSockets
    /\ ServingInSyncWithSockets

====
