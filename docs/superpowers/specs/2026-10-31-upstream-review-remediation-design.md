# Upstream Review Remediation Design

## Intent and success criteria

Parallax and other MCP clients must be able to join contract facts and raw code facts at the same commit, consume raw analysis without parsing Markdown, and distinguish confirmed evidence from heuristic inference. The work is additive wherever an existing client-visible behavior can be retained.

Success means:

- the raw read tools accept a named snapshot and answer entirely from that snapshot;
- the seven Markdown-shaped analysis tools expose typed `structuredContent` and matching `outputSchema` while retaining their text content;
- raw query nodes and edges expose the locations and provenance already stored in the graph;
- chained traversals return complete composed paths and support deterministic cursor pagination;
- the missing ORM, topic, frontend-sink, OTLP-resource, and capability behavior has focused end-to-end tests;
- existing behavior identified as already implemented is not rebuilt.

## Review verdicts

| Item | Verdict | Required action |
|---|---|---|
| 1. Snapshot-scoped raw graph | Confirmed | Route supported raw reads through the existing snapshot federation and cached graph. |
| 2. Structured raw-tool output | Confirmed | Add structured results and output schemas without removing text content. |
| 3. Raw-edge provenance | Partially stale | `GraphEdge` already stores provenance, confidence-bearing weights, source sites, and runtime evidence; expose these on `GraphEdgeRef`. |
| 4. Raw-node locations | Partially stale | `GraphNode` already stores path, lines, commit, and repo; expose these on `GraphNodeRef`. |
| 5. ORM model/table/column chain | Partially confirmed | Django model→table and SQL access exist. Add column/model relations and migration sites, then cover SQLAlchemy, Prisma, Drizzle, TypeORM, and Sequelize. |
| 6. Payload field paths | Partially confirmed | HTTP local dataflow and GraphQL selected-field paths exist. Add operation-scoped topic reads and complete GraphQL local-variable lineage. |
| 7. GraphQL/RPC binding | Stale | `CallsGraphql`, `ContractKey::Rpc`, provider/handler facts, and resolver/registration links already implement the behavior. No duplicate node type is needed. |
| 8. Frontend sinks | Partially confirmed | Generated document→hook→component linking exists. Add rendered/stored/forwarded/discarded dispositions. |
| 9. Composable traversal | Confirmed in part | Multi-seed walks and per-segment paths exist. Compose across `connect` steps and add a cursor. |
| 10. Cross-repo identity | Stale | Federation `GlobalId` is deterministic and parseable; contract `SymbolKey` resolution already joins through it. |
| 11. Runtime ingestion | Partially stale | OTLP HTTP/JSON, trace/span identity, routes/attributes, and `RuntimeCall` exist. Preserve resource-level `service.name` and scope attributes during translation. |
| 12. Capability discovery | Confirmed in part | Indexed commits and semantic readiness exist. Add relation availability, snapshot support, and emitter readiness. |
| 13. Extractor extension point | Stale for the stated protocols | Native GraphQL/RPC/OpenAPI sensors, inventory registration, detector traits, and per-repo pattern overrides already exist. Do not add a dynamic plugin ABI. |

## Phase 1: snapshot-bound raw reads

The supported set is `query_graph`, `find_symbol`, `get_context`, `get_call_sites`, `get_cross_runtime_callers`, `explain_symbol`, `assess_change`, and `explain_dispatch`.

Each accepts an optional `snapshot` string. Omitted or `"live"` preserves current behavior. A `snap_…` value loads the record through `SnapshotManager`, acquires its existing `HoldGuard` for the whole call, and binds a temporary tool context to the snapshot federation's graph. `repo_id` narrows symbol resolution and source lookup to one repo in the record.

A snapshot-bound context carries the snapshot id, record, manager data directory, and graph. Its overlay is empty. Live readiness notes, working-tree freshness warnings, and the process-global runtime store must not leak into the answer. Source excerpts and call-line scans read git blobs from `<data_dir>/mirrors/<repo>.git` at the commit in the snapshot record, reusing the contract tools' path and secret-file safety rules. A tool that cannot honor snapshot semantics must return an explicit unsupported error rather than silently reading live state.

Missing snapshots, non-ready snapshots, excluded repos, and unavailable snapshot support are structured errors. Snapshot acquisition remains bounded by the existing residency and wait mechanisms.

## Phase 2: structured MCP results

Introduce an internal `ToolOutput { text, structured }` result while keeping the existing `ToolHandler::call` behavior as the default adapter. Add a structured dispatch path for the seven analysis tools and `query_graph`; existing callers that only need text continue to receive the same string.

The MCP adapter places the typed value in `CallToolResult.structured_content` and keeps the existing content text, including indexing notices. JSON-shaped tools reuse their existing value rather than parsing Markdown. Markdown-shaped tools build their typed report first and render text from it, preventing the two representations from drifting.

`ToolDefinition` and the inventory handler metadata gain an optional output schema. `tools/list`, HTTP schema output, and `docs/tool-schema.json` expose the same `outputSchema`. Schemas describe the complete `structuredContent` value and are validated by representative success, empty, and ambiguity fixtures.

The typed shapes use stable arrays and explicit counts:

- `find_symbol`: symbols and count;
- `get_context`: definition, callers, callees, source, annotations;
- `get_call_sites`: target, sites with caller/path/line/exactness, and count;
- `get_cross_runtime_callers`: HTTP, RPC, and GraphQL caller arrays plus availability;
- `explain_symbol`: symbol metadata, metrics, annotations, and occupancy;
- `assess_change`: risk verdict, direct/transitive/untested dependents, and coupling;
- `explain_dispatch`: the existing serializable report;
- `query_graph`: the existing query result, enriched as below, plus occupancy.

## Phase 3: raw references and traversal

`GraphNodeRef` gains optional `path`, `line_start`, `line_end`, `commit`, and `repo_id`, populated from the canonical node. `GraphEdgeRef` gains optional `provenance`, `confidence`, `site`, and `detail`. Static Tree-sitter/LSP/regex, heuristic detector, confirmed source, and runtime trace evidence therefore survive raw serialization. No confidence is invented for legacy edges that have none.

Each `connect` receives the incoming path map. A newly selected segment is appended to the prior path by removing the duplicate boundary node and concatenating nodes and edges. Candidate selection still compares the new segment by the documented shortest-path and deterministic tie-break rules. `find` resets paths; filters and limits retain only paths belonging to reported nodes. The top-level edge list remains the deduplicated union of all composed path edges.

Add optional top-level `cursor` and `limit` fields to `QuerySpec`, applied after the existing operation pipeline and independently of the existing `limit` operation. The cursor is an opaque hex-encoded payload containing a query fingerprint and the last returned node id. The fingerprint covers the canonical query with `cursor` removed. A cursor from another query, a malformed cursor, or a cursor whose anchor is no longer in the ordered result is rejected. `meta.next_cursor` is present exactly when more results remain. Ordering must be deterministic before either cursor or default-cap truncation.

Update `docs/query-language.md` to remove the old non-composition statement and document provenance, locations, and pagination.

## Phase 4: ORM relations

Use the canonical graph schema rather than parallel DTOs. Add a `Column` node type and append-only `MapsTo`, `HasColumn`, `ReadsColumn`, and `WritesColumn` edge variants. Logical models use existing class nodes or sensor-owned class nodes for schema-only ORMs. `MapsTo` joins model→table; `HasColumn` joins table→column and carries the defining or migrating `SourceSite`. Access sensors may add column reads/writes where the column is statically known; unknown dynamic columns remain unresolved and are never name-guessed.

Add ORM metadata to contract facts with the ORM name, model/table/column identity, and migration site when known. Bump `FEDERATION_GRAPH_VERSION` in the schema-changing commit and document `lain reindex` in `CHANGELOG.md`.

Extend Django extraction to model fields and `AddField`/migration sites. Add focused extractors for:

- SQLAlchemy declarative `__tablename__` and `Column` definitions, including Alembic `op.add_column`;
- Prisma `model`, `@@map`, field `@map`, and migration SQL;
- Drizzle table declarations and column object keys;
- TypeORM `@Entity`/`@Column` names and migration sites;
- Sequelize `define`/`Model.init` table and attribute mappings.

Definition extraction is the required baseline for all six. Existing table access relations remain intact. Tests cover explicit names, convention defaults, renamed columns, migrations, and dynamic definitions that must stay unresolved.

## Phase 5: payload paths and frontend dispositions

Generalize field-read origins beyond HTTP. A topic consumer seed identifies the broker/topic operation and payload root; a GraphQL consumer seed identifies operation type/root field and selection root. Reuse `JsonPath`, `FieldRef`, `ReadsField`, and `ReadsFrom`, extending origin metadata so a field read cannot join to a different operation with the same variable name. Apply the existing local binding rules to payload decode, aliases, destructuring, and interprocedural pass/return where already supported. Dynamic keys and escapes mark lineage incomplete.

Add append-only `Renders`, `Stores`, `Forwards`, and `Discards` relations for statically observed frontend values. TS/TSX analysis classifies JSX interpolation as rendered, assignments to storage/state as stored, function/props flow as forwarded, and selected values with no use as discarded. Each relation carries a source site and confidence appropriate to its detector. Tests exercise all four dispositions and verify that generated-document linking remains intact.

## Phase 6: OTLP and capabilities

OTLP translation merges resource attributes into each span without overwriting explicit span attributes, and preserves scope name/version under namespaced keys. Resource `service.name`, HTTP route, RPC identity, trace id, and span id are therefore available to the existing resolver and provenance path. Tests cover precedence and multiple resources in one payload.

`get_capabilities` adds:

- per-repo `relations`: every `EdgeType`, its support/indexed status, and ready/warming/unavailable state;
- `snapshots`: whether named snapshot reads are supported and ready;
- `emitters`: registered sensor/emitter names and their current readiness;
- existing semantic state and indexed commits unchanged.

Readiness derives from actual registration and the owning repo's index lifecycle, not from result counts; an emitter with zero detections is still ready. Snapshot metadata is wired from the server's real `SnapshotManager` rather than inferred from the graph.

## Error handling and compatibility

Existing text responses, argument shapes without `snapshot`, live graph behavior, and `GlobalId` formats remain unchanged. New enum variants are append-only. Invalid cursors and snapshot requests fail explicitly; no historical request falls back to live data. Dynamic or ambiguous sensor inputs remain unresolved with evidence rather than producing guessed relations.

## Verification

Each phase begins with failing focused tests and ends with those tests passing. Unit tests cover path composition, cursor rejection, reference serialization, output-schema parity, OTLP precedence, and each extractor. MCP integration tests verify both `structuredContent` and legacy text, plus snapshot results that remain unchanged after the live working tree and live graph change. Snapshot tests prove call-line/source reads use the pinned commit.

Run the relevant sensor, query, registry, MCP, snapshot, and schema-drift suites after each phase. Finish with formatting, clippy, the full Rust test suite, and the repository's standard schema-drift check. Remove generated logs and temporary artifacts; retain only source, tests, schemas, and intentional documentation changes.

## Assumptions

- The requested execution method is native implementation in this session.
- Additive compatibility is preferred over a JSON-only breaking redesign.
- The existing snapshot federation is the authoritative historical raw graph; no second snapshot store will be built.
- Source-level and configuration extension points satisfy item 13; a runtime native-plugin ABI is out of scope.
