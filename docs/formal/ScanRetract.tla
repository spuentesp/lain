---- MODULE ScanRetract ----
\* ScanRetract — TLA+ model of `GraphDatabase::replace_sensor_output`
\* and `sensor_owner_of`, targeting invariant I8 (scan ownership /
\* no peer retraction).
\*
\* Spec: this invariant is new in this branch — to be added to
\* `docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md`
\* as I8 once the fix lands.
\*
\* Code: src/server/graph/mod.rs:130 (`sensor_owner_of`) and
\*       src/server/graph/mod.rs:853 (`replace_sensor_output`).
\*
\* The §6.1 rule is: every node must be assigned to exactly one
\* sensor (its `SensorOwner`), and `replace_sensor_output(s)` retracts
\* only nodes with `owner == s` and then inserts `s`'s current
\* output. The bug the spec targets: when two sensors map to the
\* same owner, `replace_sensor_output(s1)` deletes the other
\* sensor's nodes too (a "peer deletion").
\*
\* This spec models the BUGGY shape — variant (a). The .cfg
\* configures the two sensors to share one owner. TLC will find a
\* peer-deletion counterexample. The fix (variant (b) — per-sensor
\* owners) is in `ScanRetract_VariantB.tla`; the same model with
\* each sensor owning its own output reports no counterexample.
\*
\* State:
\* - Sensors: a set of sensor identifiers.
\* - NodeIds: a set of node identifiers.
\* - Owners: a set of owner values.
\* - sensor_owner: [Sensor -> Owner] — the owner of each sensor's
\*   output. In variant (a) this is a constant function (every
\*   sensor maps to the same owner); in variant (b) it is an
\*   injection (each sensor has its own owner).
\* - nodes: the current set of (NodeId, Owner) pairs in the graph.
\* - latest_output: [Sensor -> SUBSET NodeIds] — the nodes each
\*   sensor has produced in its last scan.
\*
\* Action: `Scan(s)` retracts every node whose owner is
\* `sensor_owner[s]`, then inserts `latest_output[s]` (each
\* inserted node is tagged with `sensor_owner[s]`).
\*
\* Invariant:
\*   **I8**: after any interleaving of `Scan` actions, the node
\*          set equals the union of every sensor's latest output,
\*          with each node tagged with the owner of the sensor
\*          that produced it. Equivalently: no scan ever removes a
\*          node it does not own.
\*
\* Code mapping (TLA+ → Rust):
\*   - `SensorOwner`              ↔ the `SensorOwner` enum in
\*                                 `src/server/graph/mod.rs:84`
\*   - `sensor_owner_of(node)`    ↔ `SensorOwner` derivation
\*                                 (line 130) — the buggy shape
\*                                 maps multiple sensors to the
\*                                 same owner
\*   - `nodes` (in the model)     ↔ the in-memory node set
\*   - `latest_output[s]`         ↔ the nodes the sensor `s` last
\*                                 produced (a logical snapshot of
\*                                 its current scan output)
\*   - `Scan(s)`                  ↔ `replace_sensor_output(s, ...)`
\*                                 (line 853): retracts nodes
\*                                 owned by `s`, inserts
\*                                 `latest_output[s]`

EXTENDS Naturals, FiniteSets

CONSTANTS
    Sensors,
    NodeIds,
    Owners

ASSUME
    /\ Sensors # {}
    /\ NodeIds # {}
    /\ Owners  # {}

\* ===== Scenario (hardcoded — see "Modelling note" below) =====
\*
\* The TLC `.cfg` parser is line-based and accepts only simple
\* set, string, and number literals. The function-bag syntax we
\* would normally use to pass the `sensor_owner` and
\* `initial_latest_output` mappings is therefore written as
\* operator definitions inside the spec. This is the established
\* workaround for this toolchain (`CoverageClaim.tla`,
\* `RejoinProtocol.tla`, `ConsumerBinding.tla` all hardcode
\* their scenario operators the same way; the `.cfg` provides
\* only the SET identifiers).
\*
\* All identifiers in this section are STRING LITERALS so the
\* `.cfg` and the spec agree on them via the set constants
\* declared above. The `ASSUME` block locks the scenario in so a
\* scenario change requires updating both files.

ASSUME
    /\ Sensors = {"s1", "s2"}
    /\ NodeIds = {"n1", "n2"}
    /\ Owners  = {"shared"}

\* Variant (a) — BUGGY shape. Both sensors map to the same owner
\* `"shared"`. This is the §6.1 anti-pattern: the legacy
\* `proto_sensor` and the gRPC `grpc_provider_sensor` share
\* `ProtoSensor` (see the comment on
\* `SensorOwner::ProtoSensor` at mod.rs:99–106); the
\* `field_access` sensor and the GraphQL consumer sensor share
\* `FieldRef` (mod.rs:193–198). Both cases exhibit peer deletion
\* in practice.
SensorOwner(s) == "shared"

\* Each sensor's last-produced output. The values are fixed
\* (the model does not let sensors change their output between
\* scans) and chosen so the two outputs differ — this is the
\* minimum for the bug to be observable. s1 owns n1; s2 owns n2.
InitialLatestOutput(s) ==
    IF s = "s1" THEN {"n1"}
    ELSE {"n2"}

\* ===== State variables =====

VARIABLES
    nodes,         \* SUBSET (NodeId × Owner) — current node set
    latest_output  \* [Sensor -> SUBSET NodeIds] — each sensor's last
                   \* output (fixed in this spec; see above)

\* ===== Type invariant =====

TypeOK ==
    /\ nodes \subseteq NodeIds \times Owners
    /\ latest_output \in [Sensors -> SUBSET NodeIds]

\* ===== Invariant I8 =====

\* **I8 — scan ownership / no peer retraction.** After any
\* interleaving of `Scan` actions, the node set equals the union
\* of every sensor's latest output, with each node tagged with
\* the owner of the sensor that produced it.
\*
\* Equivalently: no scan ever removes a node it does not own.
\*
\* Bug surface (variant (a) — the current spec): two sensors map
\* to the same owner. `Scan(s1)` retracts every node with that
\* shared owner — including the nodes that `s2` produced in its
\* own last scan. The retract step over-deletes. After the
\* insert, the node set is `latest_output[s1]` tagged with the
\* shared owner, which does not include `s2`'s exclusive output.
\* The invariant fails when `latest_output[s2] ⊄ latest_output[s1]`.
I8 ==
    LET NodesForSensor(s) == {<<n, SensorOwner(s)>> : n \in latest_output[s]}
    IN  nodes = UNION {NodesForSensor(s) : s \in Sensors}

\* ===== Initialisation =====

\* Start with every sensor's last output in the graph. In a real
\* run this is true because every sensor has scanned at least
\* once; the init captures the steady-state assumption so the
\* bug is observable on the first Scan.
Init ==
    /\ nodes = UNION { {<<n, SensorOwner(s)>> : n \in InitialLatestOutput(s)} : s \in Sensors }
    /\ latest_output = [s \in Sensors |-> InitialLatestOutput(s)]

\* ===== Actions =====

\* Scan(s) — the join of `replace_sensor_output(s, latest_output[s], …)`
\* (line 853) at our granularity. The real method also takes
\* edges and handles `EntryPointSensor`'s entry-field wipe; those
\* are orthogonal to the I8 invariant.
\*
\* (a) Retract every node with `owner == SensorOwner(s)`.
\* (b) Insert `latest_output[s]`, each tagged with `SensorOwner(s)`.
\*
\* In the buggy shape the retract step removes nodes owned by
\* other sensors too (because they share the owner), which is the
\* peer-deletion the spec targets. The minimum reproduction is
\* s1 owning n1 and s2 owning n2: after `Scan(s1)`, the graph
\* contains only n1, but the union of both sensors' outputs is
\* {n1, n2}. I8 fails.
Scan(s) ==
    /\ nodes' = (nodes \ {<<n, SensorOwner(s)>> : n \in NodeIds})
                \cup {<<n, SensorOwner(s)>> : n \in latest_output[s]}
    /\ UNCHANGED latest_output

Next == \E s \in Sensors : Scan(s)

vars == <<nodes, latest_output>>

Spec == Init /\ [][Next]_vars

====
