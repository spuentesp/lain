# Evidence-Based Sensor Extraction Design

## Intent and success criteria

Replace the new ORM sensor's source-text extraction with syntax-aware extraction, remove naming-convention guesses from frontend value dispositions, and close the remaining deterministic-pagination and snapshot-error holes in PR #390. The work must use Lain's existing grammars, sensor registry, graph schema, pattern overrides, and coverage reporting without adding dependencies.

Success means:

- all six supported ORMs extract only structurally identified model, table, and column definitions;
- explicit names and documented convention defaults are correct, while unsupported or dynamic naming rules produce unresolved records instead of guessed graph facts;
- frontend `Stores` and `Forwards` relations come from observed bindings and dataflow, not identifier spelling;
- every tracked frontend value is rendered, stored, forwarded, discarded, or explicitly reported as used but unclassified;
- automatic result truncation and cursor pagination are deterministic;
- a snapshot-bound tool never converts a source-read failure into an apparently empty or absent result;
- sensor rescans remain idempotent and all emitted edges have materialized endpoints.

## Confirmed findings and boundaries

The current ORM implementation uses regular expressions and manual delimiter matching in `src/server/sensors/orm_model_sensor.rs`. This misses Django models without explicit `Meta.db_table`, applies incorrect TypeORM and Sequelize convention defaults, and makes migration evidence dependent on substring matching. The frontend sensor parses TS/TSX but classifies state storage with a setter-name regular expression. Returned or assigned values are neither classified nor reported. The query executor truncates its default-capped result without first establishing deterministic order. Snapshot excerpt paths in structured and legacy handlers discard `read_source` errors.

Capability discovery is not rebuilt. Relation availability already comes from `EdgeType`, emitter availability from sensor inventory, and snapshot support from `SnapshotManager`. Their shared `state` is the owning repository's index lifecycle, not proof that every individual sensor succeeded. Documentation and tests must make that distinction explicit; no per-sensor health signal will be invented.

The premature `0.14.0` changelog heading is removed on this `dev`-based feature branch. Its entries remain under `Unreleased`; release headings and version metadata remain release-PR work.

## Architecture

Retain one `orm_models` sensor and one `frontend_values` sensor registration. Model/table/column extraction is one graph concern, and frontend value disposition is another; framework-specific parsers are internal strategies rather than separately registered graph mutators. This preserves one owner for each sensor's output and avoids cross-sensor retraction.

Split the ORM implementation internally into Python extraction, TypeScript/JavaScript extraction, Prisma extraction, migration extraction, shared model/table resolution, and graph emission. Use `util::scan_files`, `util::parse_for_lang`, the existing parser cache, and the canonical `GraphNode`/`GraphEdge` schema. Framework constructor, decorator, and method vocabularies belong in bundled YAML and tree-sitter query files, with the same per-repository override surface as other syntax-aware sensors. Language node traversal may remain Rust code; framework detection shapes must not become hardcoded matcher tables.

Do not add an LSP dependency to either sensor. Sensor execution is synchronous and language servers are optional. Syntax evidence and explicit unresolved outcomes are the baseline; LSP enrichment can be designed separately if a later use case requires it.

## ORM extraction

### Python: Django and SQLAlchemy

Parse class definitions, assignments, calls, and keyword arguments as Python AST nodes. A field's enclosing class comes from the tree, not from the distance between regular-expression matches. Nested calls and arbitrary argument ordering therefore do not affect literal `db_column` or `name` extraction. Non-literal names produce unresolved records and no column-name guess.

Move Django model/table resolution into a shared AST-backed helper used by both `orm_model_sensor` and the existing `django_orm_sensor`; the latter retains its access-site responsibility. Preserve its established semantics:

- explicit `Meta.db_table` wins;
- otherwise the table is `<app_label>_<model>` lowercased;
- `Meta.app_label` overrides the path-derived app label;
- abstract models have no table;
- proxy models use a uniquely resolved concrete parent's table;
- dynamic table names and ambiguous proxy parents are unresolved.

SQLAlchemy extraction supports declarative `__tablename__` and `Column` forms whose table and column identities are statically literal. It does not invent a convention default when no table name exists.

### TypeScript and JavaScript: TypeORM, Drizzle, and Sequelize

Use the TypeScript, TSX, and JavaScript grammars as appropriate. Decorators, call expressions, object literals, and property assignments provide the structural boundaries.

- TypeORM reads `@Entity` and `@Column`. An omitted entity table name uses the existing shared snake-case conversion; an omitted column name uses the property identifier. Dynamic options are unresolved.
- Drizzle reads `pgTable`, `mysqlTable`, and `sqliteTable` calls with a literal table argument and an object-literal column map. Column source locations come from the property node.
- Sequelize reads `sequelize.define` and `Model.init`. This change supports explicit literal `tableName` and attribute `column` names. Implicit Sequelize table names are always unresolved: appending `s` is not Sequelize's inflection algorithm, and no replacement pluralizer will be approximated.

### Prisma

Keep a small hand-written parser for Prisma's block structure because no Prisma grammar is available in the repository. It must handle nested attribute parentheses, comments, `model` blocks, `@@map`, and field-level `@map` without treating comments as fields. The default table and column names are the Prisma model and field identifiers. Do not invoke Prisma tooling or add a DMMF dependency.

### Migrations and provenance

Parse Django `AddField` and Alembic `op.add_column` operations as Python calls. Associate evidence only when the operation's model/table identity and field/column identity match the definition. A matching column name in another table is not evidence. Source sites point to the operation or keyword node that establishes the match.

SQL migration evidence uses token-aware table and column matching rather than arbitrary substring containment; unsupported SQL forms remain unmatched. No general SQL parser is introduced.

AST-derived relations carry `StaticSource::TreeSitter`. Prisma and SQL token extraction retain the existing non-AST provenance representation rather than claiming tree-sitter evidence. Dynamic or ambiguous definitions contribute `OrmDynamicQuery` coverage records with precise source samples.

## Frontend value dispositions

Continue tracking only exact `FieldRef` bindings and their lexically scoped local-variable uses. Strengthen the callee and disposition analysis as follows:

- `Renders`: the bound value appears in a JSX expression container.
- `Forwards`: the bound value is passed as a JSX prop or as an argument in an observed call. This describes value flow and does not claim the callee's internal behavior.
- `Stores`: the bound value is passed to an exact configured storage receiver such as `localStorage.setItem` or `sessionStorage.setItem`, or to a setter/dispatch binding structurally obtained from an imported React `useState`/`useReducer` call. Import aliases and arbitrary local setter names are supported. A local function merely named `setFoo` has no special meaning.
- `Discards`: the binding has no non-declaration use in its lexical scope. This remains heuristic evidence with its existing confidence.
- Used but unclassified: any other non-declaration use, including return and ordinary assignment, suppresses `Discards` and emits a dedicated frontend-value-unclassified coverage record with the use site. No new graph edge is created for uncertainty.

Storage receivers remain configurable. React setter recognition is based on imports and binding structure, not configurable name regexes. Custom hooks and wrapped storage libraries remain unclassified unless a later evidence-based resolver supports them.

Each scan replaces only frontend disposition edges, preserves generated-document links, and remains idempotent.

## Query, snapshot, capability, and release-note corrections

Immediately before automatic default-cap truncation, sort results by `(name, id)`. Explicit query sort/limit operations retain their documented behavior, and cursor pages continue to use the stable ordering and query fingerprint already implemented.

Change snapshot source-excerpt helpers to return `Result<Option<_>>`. Propagate snapshot repository, blob, decoding, and read failures through both structured and legacy raw-tool paths. An absent optional excerpt is representable only when the source was read successfully but no applicable body/range exists. Audit all eight snapshot-capable raw tools so unreadable source cannot masquerade as no source, no call sites, or an empty section.

Keep the existing capability payload and registry-derived availability. Document that `relations[].state`, `emitters[].state`, and `snapshots.state` describe repository/index lifecycle. Individual extraction gaps remain visible through coverage reporting; zero detections do not make a registered emitter unavailable. Tests cover ready, warming, unavailable, and zero-detection semantics.

Update the PR body's verification counts only after testing the final head.

## Error handling and compatibility

Existing live-tool behavior, graph node/edge shapes, sensor registration names, and cursor formats remain unchanged. This remediation adds no dependency and no release version bump. Dynamic names, unsupported framework conventions, unresolved frontend flows, invalid snapshots, and snapshot source failures are explicit outcomes rather than silent omissions. No additional federation graph schema bump is expected because this design changes extraction and reporting, not the serialized graph shape introduced by PR #390.

## Verification

Write focused failing regression tests before each implementation change:

- Django default tables, app-label overrides, abstract/proxy models, and dynamic names;
- SQLAlchemy nested arguments and explicit column names;
- TypeORM snake-case defaults and nested decorator options;
- Drizzle property source lines;
- explicit Sequelize names and unresolved implicit names;
- Prisma comments, maps, nested attributes, and defaults;
- migration table/model mismatches and exact evidence sites;
- arbitrary React setter names, misleading `setFoo` callees, forwarded arguments, returned/assigned unclassified values, and true unused values;
- identical default-capped results across graph insertion orders;
- snapshot source failures in structured and legacy responses;
- capability lifecycle semantics without relying on detection counts.

Re-run ORM and frontend rescan tests, graph well-formedness and sensor-idempotency properties, query/cursor properties, raw snapshot suites, schema and architecture guards, formatting, and the full workspace suite. Compare test counts against the final head and remove generated logs or temporary artifacts before reporting completion.

## Assumptions and non-goals

- The approved implementation target is PR #390 on `spuentesp/booby`, based on `dev`.
- No Prisma, inflection, SQL-parser, or LSP dependency is added.
- Full type resolution, custom-hook behavior, Sequelize-compatible pluralization, and per-sensor historical health are non-goals.
- Existing `django_orm_sensor` access-site extraction is retained; only its model/table resolution is shared and made AST-backed.
- The implementation plan and code changes wait for review of this written spec.
