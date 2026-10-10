---- MODULE ScanRetract_VariantB ----
\* ScanRetract_VariantB — variant (b) of `ScanRetract.tla`. The
\* fixed shape: each sensor has its own owner, so
\* `replace_sensor_output` cannot retract another sensor's nodes.
\*
\* Code: this corresponds to the §6.1 fix — per-sensor
\* `SensorOwner` values that distinguish the gRPC/GraphQL
\* contract sensor outputs from the legacy sensor outputs (see
\* `src/server/graph/mod.rs:84` and the long-form commentary at
\* lines 60–125).
\*
\* The state space and the I8 invariant are unchanged from
\* `ScanRetract.tla`; only the `sensor_owner` mapping changes
\* (an injection in variant (b), a constant function in variant
\* (a)). The action `Scan(s)` is the same — it still retracts by
\* owner and inserts by owner. The fix is in WHAT maps to what,
\* not in HOW the action is taken.
\*
\* Spec: docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md
\* (I8, new in this branch).
\*
\* Code mapping (TLA+ → Rust):
\*   - `SensorOwner(s)`   ↔ the per-sensor `SensorOwner` value
\*                         in `src/server/graph/mod.rs:84` —
\*                         each sensor has its own owner
\*   - the rest is the same as in `ScanRetract.tla`

EXTENDS Naturals, FiniteSets

CONSTANTS
    Sensors,
    NodeIds,
    Owners

ASSUME
    /\ Sensors # {}
    /\ NodeIds # {}
    /\ Owners  # {}

\* ===== Scenario (hardcoded — see "Modelling note" in ScanRetract.tla) =====

ASSUME
    /\ Sensors = {"s1", "s2"}
    /\ NodeIds = {"n1", "n2"}
    /\ Owners  = {"own1", "own2"}

\* Variant (b) — FIXED shape. Each sensor maps to its own owner.
\* This is the §6.1 rule: "every node must be assigned to exactly
\* one sensor (its `SensorOwner`)". With the injection, two
\* sensors cannot share an owner, so the retract step in
\* `Scan(s)` is restricted to the nodes `s` itself produced.
SensorOwner(s) ==
    IF s = "s1" THEN "own1"
    ELSE "own2"

\* Each sensor's last-produced output (fixed for the model; see
\* ScanRetract.tla). s1 owns n1; s2 owns n2 — the values differ
\* so the scenario is not vacuous.
InitialLatestOutput(s) ==
    IF s = "s1" THEN {"n1"}
    ELSE {"n2"}

\* ===== State variables =====

VARIABLES
    nodes,         \* SUBSET (NodeId × Owner)
    latest_output  \* [Sensor -> SUBSET NodeIds]

\* ===== Type invariant =====

TypeOK ==
    /\ nodes \subseteq NodeIds \times Owners
    /\ latest_output \in [Sensors -> SUBSET NodeIds]

\* ===== Invariant I8 =====

\* Same invariant as variant (a): after any interleaving of
\* `Scan` actions, the node set equals the union of every
\* sensor's latest output. Holds in this variant because the
\* retract step cannot reach across sensors.
I8 ==
    LET NodesForSensor(s) == {<<n, SensorOwner(s)>> : n \in latest_output[s]}
    IN  nodes = UNION {NodesForSensor(s) : s \in Sensors}

\* ===== Initialisation =====

Init ==
    /\ nodes = UNION { {<<n, SensorOwner(s)>> : n \in InitialLatestOutput(s)} : s \in Sensors }
    /\ latest_output = [s \in Sensors |-> InitialLatestOutput(s)]

\* ===== Actions =====

\* Same action as variant (a). The retract step is restricted to
\* `SensorOwner(s)`; with per-sensor owners that means the retract
\* only removes nodes `s` itself produced, and no peer deletion
\* happens.
Scan(s) ==
    /\ nodes' = (nodes \ {<<n, SensorOwner(s)>> : n \in NodeIds})
                \cup {<<n, SensorOwner(s)>> : n \in latest_output[s]}
    /\ UNCHANGED latest_output

Next == \E s \in Sensors : Scan(s)

vars == <<nodes, latest_output>>

Spec == Init /\ [][Next]_vars

====
