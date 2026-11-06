# Evidence-Based Sensor Extraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace heuristic ORM and frontend extraction with evidence-based parsing while fixing deterministic truncation and snapshot error propagation on PR #390.

**Architecture:** Keep one owning sensor for ORM model/table/column output and one for frontend dispositions. Split ORM extraction into focused internal modules, use the existing tree-sitter parser/query registry where grammars exist, retain a bounded Prisma block parser, and represent uncertainty through coverage records rather than graph guesses.

**Tech Stack:** Rust 2021, tree-sitter 0.25, existing Python/TypeScript/JavaScript grammars, serde YAML, petgraph-backed `GraphDatabase`, Cargo integration tests.

**Spec:** `docs/superpowers/specs/2026-11-02-evidence-based-sensor-extraction-design.md`

## Global Constraints

- Add no Prisma, inflection, SQL-parser, LSP, or other dependency.
- Do not bump package versions or add release headings in this feature PR; every commit message begins `Answer:`.
- Keep one `orm_models` and one `frontend_values` inventory registration and preserve their sensor ownership boundaries.
- Use `util::scan_files`, `util::parse_for_lang`, `Patterns::compiled_queries`, and canonical `GraphNode`/`GraphEdge` DTOs.
- Framework detection shapes live under `src/server/sensors/patterns/` and support `.lain/patterns/` overrides; do not hide new framework matcher tables in Rust.
- Do not add LSP calls to synchronous sensors.
- Implicit Sequelize table names stay unresolved; never approximate pluralization.
- Snapshot-bound reads never fall back to live state or convert source failures into empty results.
- No additional federation graph schema bump is expected; stop and revise the design if serialized graph shapes must change.
- Read and follow `AGENTS.md`, `docs/CONTRIBUTING_AGENTS.md`, `src/server/sensors/AGENTS.md`, `src/server/tools/handlers/AGENTS.md`, and `src/server/federation/AGENTS.md` before editing their respective files.

## Review Focus

- Multiline definitions, nested calls, comments, and strings must not change class/table/column ownership; Tasks 3–7 include focused fixtures for each parser family.
- Migration text mentioning the right column under the wrong table or model must not become evidence; Tasks 4 and 7 include mismatched-table tests.
- React aliases, arbitrary setter names, misleading `setFoo` functions, returns, and assignments must not be guessed or discarded; Task 8 pins each outcome.
- A snapshot mirror or blob that becomes unreadable after snapshot creation must produce an MCP error; Task 2 removes the mirror and checks structured and legacy paths.
- Duplicate display names and different graph insertion orders must not change capped results or ORM identity; Task 1 checks insertion-order independence and Tasks 3–7 retain path-keyed graph tests.

---

### Task 1: Deterministic Default-Cap Truncation and Release-Note Correction

**Files:**
- Modify: `src/server/query/executor.rs:194-234`
- Modify: `src/server/query/executor_tests.rs:1172-1233`
- Modify: `docs/query-language.md:157-168`
- Modify: `CHANGELOG.md:6-20`

**Interfaces:**
- Consumes: existing `Executor::with_default_limit`, `QuerySpec`, and `GraphNodeRef` ordering.
- Produces: automatic default-cap results sorted by `(name, id)`; explicit operation behavior unchanged.

- [ ] **Step 1: Write the failing insertion-order regression test**

Add this helper and test to `executor_tests.rs`, using the existing test imports:

```rust
fn capped_names_in_insertion_order(order: &[&str]) -> Vec<String> {
    let tmp = tempfile::tempdir().unwrap();
    let graph = GraphDatabase::new(&tmp.path().join("graph.bin")).unwrap();
    let namespace = RepoNamespace::for_test();
    let nodes: Vec<GraphNode> = order
        .iter()
        .enumerate()
        .map(|(index, name)| {
            GraphNode::new_in(
                NodeType::Function,
                (*name).into(),
                format!("src/file{index}.rs"),
                &namespace,
            )
        })
        .collect();
    graph.insert_nodes_batch(&nodes).unwrap();
    let (embedder, cache) = make_test_executor_params();
    let mut executor = Executor::with_default_limit(
        &graph,
        &embedder,
        &cache,
        std::path::Path::new(""),
        2,
    );
    let spec = QuerySpec::new(vec![GraphOp::Find(FindOp::new().r#type("Function"))]);
    executor
        .execute(&spec)
        .unwrap()
        .nodes
        .into_iter()
        .map(|node| node.name)
        .collect()
}

#[test]
fn default_cap_sorts_before_truncating_regardless_of_insertion_order() {
    let forward = capped_names_in_insertion_order(&["zulu", "alpha", "mike"]);
    let reverse = capped_names_in_insertion_order(&["mike", "zulu", "alpha"]);
    assert_eq!(forward, vec!["alpha", "mike"]);
    assert_eq!(reverse, forward);
    assert_eq!(
        capped_names_in_insertion_order(&["zulu", "alpha", "alpha"]),
        vec!["alpha", "alpha"]
    );
}
```

- [ ] **Step 2: Run the test and confirm the current order leak**

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test default_cap_sorts_before_truncating_regardless_of_insertion_order --lib`

Expected: FAIL because at least one insertion order returns a node other than `alpha` or `mike`.

- [ ] **Step 3: Sort only before automatic truncation**

Replace the default-cap branch in `executor.rs` with:

```rust
} else if !asked_for_a_limit && self.default_limit > 0 && matched > self.default_limit {
    current_nodes.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    current_nodes.truncate(self.default_limit);
    truncated = true;
}
```

In `docs/query-language.md`, state that the executor orders by name and then id before applying this automatic cap. Do not change explicit `sort` or `limit` operation semantics.

- [ ] **Step 4: Restore the feature-branch changelog shape**

Remove `## [0.14.0] - 2026-10-30` from `CHANGELOG.md`. Keep its existing entries and this PR's new bullets under one `Unreleased` heading, retaining the existing `### Added` subsection for the entries that already had it. Add this bullet to the new remediation group:

```markdown
- Use syntax-backed ORM extraction and evidence-based frontend dispositions, fail snapshot source reads explicitly, and sort default-capped query results by name and id before truncation.
```

- [ ] **Step 5: Verify and commit**

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test default_cap --lib && git diff --check`

Expected: PASS, including the pre-existing explicit-limit and small-result tests.

```bash
git add src/server/query/executor.rs src/server/query/executor_tests.rs docs/query-language.md CHANGELOG.md
git commit -m "Answer: Sort default-capped query results deterministically"
```

### Task 2: Propagate Snapshot Source Failures

**Files:**
- Modify: `tests/raw_snapshots.rs:10-111,230-256`
- Modify: `src/server/tools/handlers/structured.rs:53-69,81-104,140-178`
- Modify: `src/server/tools/handlers/metrics.rs:678-725`
- Modify: `src/server/tools/handlers/context.rs:217-228,346-360,440-466`
- Modify: `src/server/tools/handlers/semantic.rs:197-250`

**Interfaces:**
- Consumes: `SnapshotToolContext::read_source(&self, repo: &str, path: &str) -> Result<String, LainError>`.
- Produces: `source_excerpt(...) -> Result<Option<String>, LainError>` and `call_lines_in_scoped(...) -> Result<Vec<usize>, LainError>`; live callers retain their previous empty-on-unreadable behavior.

- [ ] **Step 1: Add the failing unreadable-mirror MCP test**

Add `root: std::path::PathBuf` to `RawSnapshotFixture`, initialize it with `root.clone()`, and add:

```rust
#[tokio::test]
async fn unreadable_snapshot_source_is_an_error_in_structured_and_legacy_tools() {
    let fixture = RawSnapshotFixture::new().await;
    let mirror = fixture.root.join("data/mirrors/orders.git");
    let parked = fixture.root.join("data/mirrors/orders.git.parked");
    std::fs::rename(&mirror, &parked).unwrap();

    for (tool, args) in [
        ("get_context", json!({"symbol": "old_callee"})),
        ("explain_symbol", json!({"symbol": "old_callee"})),
        ("get_call_sites", json!({"symbol": "old_callee"})),
    ] {
        let result = fixture.call(tool, args);
        assert_eq!(
            result.pointer("/result/isError").and_then(Value::as_bool),
            Some(true),
            "{tool} hid the snapshot source failure: {result}"
        );
    }
}
```

- [ ] **Step 2: Run the test and confirm swallowed errors**

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test raw_snapshots unreadable_snapshot_source_is_an_error_in_structured_and_legacy_tools`

Expected: FAIL because at least one tool returns success with an empty source or call-site section.

- [ ] **Step 3: Return `Result` from source helpers**

Change `structured.rs::source_excerpt` to this signature and snapshot branch:

```rust
fn source_excerpt(
    ctx: &ToolContext,
    args: &Map<String, Value>,
    node: &GraphNode,
) -> Result<Option<String>, LainError> {
    if let Some((snapshot, repo)) = ctx.snapshot_scope(args) {
        let repo = snapshot.repo_for_node(node).unwrap_or_else(|_| repo.to_string());
        let text = snapshot.read_source(&repo, &node.path)?;
        return Ok(crate::server::tools::utils::read_body_summary_from_text(
            node, 2000, &text,
        ));
    }
    Ok(crate::server::tools::utils::read_body_summary(
        node,
        2000,
        &ctx.workspace,
    ))
}
```

Update `explain_symbol` and `get_context` to call `source_excerpt(ctx, args, &target)?`. Apply the same `read_source(...)?` propagation in `metrics.rs` and remove the snapshot `.ok()` around scoped source construction in `semantic.rs`.

Change the scoped call-line signature and snapshot branch:

```rust
pub(crate) fn call_lines_in_scoped(
    workspace: &std::path::Path,
    caller: &crate::schema::GraphNode,
    callee: &str,
    snapshot: Option<(&crate::server::tools::registry::SnapshotToolContext, &str)>,
) -> Result<Vec<usize>, LainError> {
    if callee.is_empty() {
        return Ok(Vec::new());
    }
    let text = if let Some((snapshot, repo)) = snapshot {
        let repo = snapshot.repo_for_node(caller).unwrap_or_else(|_| repo.to_string());
        snapshot.read_source(&repo, &caller.path)?
    } else {
        let path = workspace.join(&caller.path);
        if !crate::server::path_util::resolves_inside(workspace, &path) {
            return Ok(Vec::new());
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(_) => return Ok(Vec::new()),
        }
    };
    Ok(scan_call_lines(text, caller, callee))
}
```

Extract the existing line-scanning tail unchanged as `fn scan_call_lines(text: String, caller: &GraphNode, callee: &str) -> Vec<usize>`. Update all callers with `?`; the `#[cfg(test)]` live-only wrapper may use `.expect("live call-line scan")`.

- [ ] **Step 4: Verify all raw snapshot behavior**

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test raw_snapshots --test raw_structured_output --test raw_snapshot_repo_scope`

Expected: all PASS, including unknown snapshot, excluded repo, arbitrary snapshot values, and the new unreadable-source test.

- [ ] **Step 5: Commit**

```bash
git add tests/raw_snapshots.rs src/server/tools/handlers/structured.rs src/server/tools/handlers/metrics.rs src/server/tools/handlers/context.rs src/server/tools/handlers/semantic.rs
git commit -m "Answer: Propagate snapshot source read failures"
```

### Task 3: AST-Backed Django Models and Shared Table Resolution

**Files:**
- Rename: `src/server/sensors/orm_model_sensor.rs` → `src/server/sensors/orm_model_sensor/mod.rs`
- Create: `src/server/sensors/orm_model_sensor/django.rs`
- Create: `src/server/sensors/orm_model_sensor/python.rs`
- Create: `src/server/sensors/patterns/python/django-orm-model.scm`
- Modify: `src/server/sensors/django_orm_sensor.rs:209-255,347-575`
- Modify: `src/server/sensors/patterns/mod.rs:504-513,1293-1324`
- Modify: `src/server/sensors/patterns/frameworks.yaml:700-779`
- Modify: `tests/orm_models.rs`

**Interfaces:**
- Consumes: existing `DjangoOrmShapes::has_model_base`, `Patterns::compiled_queries`, and `OrmAnalysis` graph emission.
- Produces: shared `django::DjangoModelMap::load(root, shapes)`, `django::DjangoModelMap::table_of(index)`, and `python::analyze_django(analysis, path, content, patterns)` used by `analyze_orm_models` and `django_orm_sensor`.

- [ ] **Step 1: Add failing default, dynamic, and nested-field tests**

Add to `tests/orm_models.rs`:

```rust
#[test]
fn django_defaults_and_dynamic_names_are_ast_resolved_without_guessing() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "shop/models.py",
        "from django.db import models\nclass Product(models.Model):\n    name = models.CharField(max_length=10, default=make_default(), db_column='display_name')\nclass Dynamic(models.Model):\n    class Meta:\n        db_table = TABLE_NAME\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(relation(&analysis, EdgeType::MapsTo, "Product", "shop_product"));
    assert!(relation(&analysis, EdgeType::HasColumn, "shop_product", "display_name"));
    assert!(!analysis.models.iter().any(|model| model.name == "Dynamic"));
    assert!(analysis.unresolved.iter().any(|item| item.contains("Dynamic")));
}
```

Add this override test; the replacement query matches only a deliberately absent class name, so a passing result proves the extractor consumes the override rather than the bundled query:

```rust
#[test]
fn django_ast_query_honors_repository_overrides() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "shop/models.py",
        "from django.db import models\nclass Product(models.Model):\n    name = models.CharField()\n",
    );
    write(
        tmp.path(),
        ".lain/patterns/python/django-orm-model.scm",
        "(class_definition name: (identifier) @model superclasses: (argument_list) @bases body: (block) @body (#eq? @model \"__NoSuchModel__\")) @class",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(analysis.models.is_empty());
}
```

- [ ] **Step 2: Run the new tests against the regex extractor**

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test orm_models django_defaults_and_dynamic_names_are_ast_resolved_without_guessing`

Expected: FAIL because `Product` is dropped without `db_table`.

- [ ] **Step 3: Establish the focused module and query**

Move the existing public types, sensor registration, orchestration, and graph emission into `orm_model_sensor/mod.rs`. Move Python-specific extraction into `python.rs`; leave the other frameworks callable through their existing functions until their own tasks. Add `pub(crate) mod django;` and keep graph-emission helpers `pub(super)` for sibling modules.

Bundle `python/django-orm-model.scm`:

```scheme
(class_definition
  name: (identifier) @model
  superclasses: (argument_list) @bases
  body: (block) @body) @class
```

Replace Django regex keys with typed shape data while retaining unrelated ORM keys until their tasks:

```yaml
orm_models:
  django_field_suffixes: [Field]
```

Add `django_field_suffixes: Vec<String>` to `OrmModelShapes`, merge it additively, and expose it through the existing shapes accessor. Compile the query through `Patterns::compiled_queries`, walk captured class bodies with tree-sitter, and read only literal `string` nodes as names. A `db_column` keyword whose value is not a literal string is unresolved.

Add `source: StaticSource` to `OrmRelation` and a `source: StaticSource` parameter to `add_model` and `add_column`. Graph emission must use `relation.source` instead of hardcoding `StaticSource::Regex`. Django calls pass `TreeSitter`; not-yet-migrated extractors retain `Regex` until their own tasks replace them.

- [ ] **Step 4: Share Django model/table semantics**

Implement this public-within-crate interface in `django.rs`:

```rust
pub(crate) enum DbTable {
    Default,
    Literal(String),
    Dynamic,
}

pub(crate) enum TableResolution {
    Table(String),
    Abstract,
    Unresolved(&'static str),
}

pub(crate) struct DjangoField {
    pub attribute: String,
    pub column: Option<String>,
    pub line: u32,
}

pub(crate) struct DjangoModel {
    pub name: String,
    pub path: String,
    pub line: u32,
    pub end_line: u32,
    pub bases: Vec<String>,
    pub abstract_: bool,
    pub proxy: bool,
    pub db_table: DbTable,
    pub app_label: Option<String>,
    pub fields: Vec<DjangoField>,
}

pub(crate) struct DjangoModelMap {
    pub models: Vec<DjangoModel>,
    by_name: std::collections::BTreeMap<String, Vec<usize>>,
    is_model: Vec<bool>,
}

impl DjangoModelMap {
    pub(crate) fn load(root: &Path, shapes: &DjangoOrmShapes) -> Self {
        let classes = collect_ast_classes(root, shapes);
        Self::build(classes, shapes)
    }

    pub(crate) fn table_of(&self, index: usize) -> TableResolution {
        self.table_of_at_depth(index, 0)
    }

    pub(crate) fn is_model(&self, index: usize) -> bool {
        self.is_model[index]
    }
}
```

Define `collect_ast_classes(root: &Path, shapes: &DjangoOrmShapes) -> Vec<DjangoModel>` in `django.rs`; it performs the shared `scan_files` walk and delegates each file to `parse_for_lang(Lang::Python, content)`. Define `DjangoModelMap::build(classes, shapes) -> Self` using the existing model-base fixpoint and `table_of_at_depth(&self, index, depth) -> TableResolution` using the existing four-level proxy bound, unique-parent rule, explicit table precedence, and `<app_label>_<model>` default. Replace `django_orm_sensor`'s private `ModelMap` with this shared map while leaving access-site analysis unchanged.

Use this conversion for ORM output:

```rust
for (index, model) in map.models.iter().enumerate() {
    if !map.is_model(index) {
        continue;
    }
    match map.table_of(index) {
        TableResolution::Table(table) => add_model(analysis, "django", &model.name, &table, &model.path, model.line, StaticSource::TreeSitter),
        TableResolution::Abstract => continue,
        TableResolution::Unresolved(why) => analysis.unresolved.push(format!("{}:{}: {why}", model.path, model.line)),
    }
}
```

Expose `is_model(&self, index: usize) -> bool` as part of the same interface.

- [ ] **Step 5: Verify both Django consumers and commit**

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test orm_models && CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test django_orm_sensor --lib`

Expected: all PASS, including abstract/proxy/default-table tests and ORM rescan well-formedness.

```bash
git add -A src/server/sensors/orm_model_sensor.rs src/server/sensors/orm_model_sensor src/server/sensors/django_orm_sensor.rs src/server/sensors/patterns tests/orm_models.rs
git commit -m "Answer: Extract Django models with tree-sitter"
```

### Task 4: SQLAlchemy Definitions and Python Migration Operations

**Files:**
- Modify: `src/server/sensors/orm_model_sensor/python.rs`
- Create: `src/server/sensors/orm_model_sensor/migrations.rs`
- Create: `src/server/sensors/patterns/python/sqlalchemy-orm-model.scm`
- Create: `src/server/sensors/patterns/python/django-orm-migration.scm`
- Create: `src/server/sensors/patterns/python/sqlalchemy-orm-migration.scm`
- Modify: `src/server/sensors/orm_model_sensor/mod.rs`
- Modify: `src/server/sensors/patterns/mod.rs`
- Modify: `src/server/sensors/patterns/frameworks.yaml`
- Modify: `tests/orm_models.rs`

**Interfaces:**
- Consumes: Task 3's `OrmAnalysis`, AST literal helpers, and `DjangoModelMap` table evidence.
- Produces: `python::analyze_sqlalchemy(...)` and `migrations::attach_python_migrations(root, &mut OrmAnalysis, &Patterns)`; migration sites attach only on exact operation identity.

- [ ] **Step 1: Add failing nested-argument and wrong-table migration tests**

```rust
#[test]
fn sqlalchemy_and_python_migrations_require_structural_identity() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "models.py",
        "from sqlalchemy import Column, String\nclass Price(Base):\n    __tablename__ = 'prices'\n    amount = Column(String(50), name='price_usd')\n",
    );
    write(
        tmp.path(),
        "alembic/versions/0001.py",
        "def upgrade():\n    op.add_column('orders', sa.Column('price_usd', sa.String()))\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(relation(&analysis, EdgeType::HasColumn, "prices", "price_usd"));
    assert!(analysis.columns.iter().all(|column| column.migration.is_none()));

    write(
        tmp.path(),
        "alembic/versions/0002.py",
        "def upgrade():\n    op.add_column('prices', sa.Column('price_usd', sa.String()))\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(analysis.columns.iter().any(|column| column.migration.is_some()));
}
```

- [ ] **Step 2: Run and confirm the false positive**

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test orm_models sqlalchemy_and_python_migrations_require_structural_identity`

Expected: FAIL because the `orders` operation is attached to `prices.price_usd`.

- [ ] **Step 3: Extract SQLAlchemy through AST calls**

Bundle this class-shape query:

```scheme
(class_definition
  name: (identifier) @model
  body: (block) @body) @class
```

Add `sqlalchemy_column_calls: [Column]` to `OrmModelShapes` and YAML. Walk class-body assignments, require a literal `__tablename__`, and identify configured `Column` call nodes. Resolve a positional string name or `name=` keyword only from a literal string; `String(50)` is a type call and must never be mistaken for a name. Dynamic names append an unresolved sample. Pass `StaticSource::TreeSitter` to `add_model` and `add_column` for these relations.

- [ ] **Step 4: Parse migration calls instead of scanning text**

Bundle the Alembic operation shape:

```scheme
(call
  function: (attribute
    object: (identifier) @receiver
    attribute: (identifier) @method)
  arguments: (argument_list) @arguments
  (#eq? @receiver "op")
  (#eq? @method "add_column")) @operation
```

Bundle the Django operation shape with `@method "AddField"`. In `migrations.rs`, walk each operation's direct arguments and keyword children. Alembic requires its literal table argument and nested `Column` name; Django requires `model_name`, `name`, and a field whose literal `db_column` or attribute name matches. Attach `SourceSite { path, line }` from the operation node. Remove `contains_identifier` and substring-based Python migration matching.

- [ ] **Step 5: Verify and commit**

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test orm_models && CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test sensor_pipeline --lib`

Expected: all PASS with no migration false positives and stable rescans.

```bash
git add src/server/sensors/orm_model_sensor src/server/sensors/patterns tests/orm_models.rs
git commit -m "Answer: Parse SQLAlchemy models and Python migrations"
```

### Task 5: Bounded Prisma Block Parsing

**Files:**
- Create: `src/server/sensors/orm_model_sensor/prisma.rs`
- Modify: `src/server/sensors/orm_model_sensor/mod.rs`
- Modify: `src/server/sensors/patterns/mod.rs`
- Modify: `src/server/sensors/patterns/frameworks.yaml`
- Modify: `tests/orm_models.rs`

**Interfaces:**
- Consumes: `add_model`, `add_column`, and `OrmAnalysis` from `mod.rs`.
- Produces: `prisma::analyze(analysis, path, content)` with comment-aware model blocks and literal map attributes.

- [ ] **Step 1: Add failing comment, nested-attribute, and default-name fixtures**

```rust
#[test]
fn prisma_parser_ignores_comments_and_nested_attribute_parentheses() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "schema.prisma",
        "// model Fake {\n// ignored String\nmodel OrderItem {\n  id Int @id @default(autoincrement())\n  label String @default(db.generated(\"x)y\")) @map(\"display_name\")\n  // fake String @map(\"fake\")\n}\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(relation(&analysis, EdgeType::MapsTo, "OrderItem", "OrderItem"));
    assert!(relation(&analysis, EdgeType::HasColumn, "OrderItem", "display_name"));
    assert!(!analysis.columns.iter().any(|column| column.name == "fake" || column.name == "//"));
}
```

- [ ] **Step 2: Run and identify regex/block failures**

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test orm_models prisma_parser_ignores_comments_and_nested_attribute_parentheses`

Expected: FAIL under source-text extraction.

- [ ] **Step 3: Implement the bounded parser**

Define the parser state and block/name results in `prisma.rs`:

```rust
enum PrismaLexState {
    Code,
    LineComment,
    BlockComment,
    String,
}

struct PrismaBlock<'a> {
    name: &'a str,
    body: &'a str,
    body_start: usize,
}

enum MappedName {
    Absent,
    Literal(String),
    Dynamic,
}
```

Implement these exact helpers in the same file:

- `advance_prisma_lexer(source: &[u8], masked: &mut [u8], index: usize, state: PrismaLexState) -> PrismaLexState` recognizes `//`, `/* */`, double quotes, and backslash escapes; it spaces comment/string bytes but preserves newlines and every non-ASCII byte.
- `mask_comments_and_strings(content: &str) -> String` clones the source bytes, drives `advance_prisma_lexer` over every byte, and returns the offset-preserving mask.
- `model_headers(masked: &str) -> Vec<(&str, usize)>` recognizes only `model <identifier> {` in unmasked code and returns the name plus opening-brace offset.
- `matching_brace(masked: &str, open: usize) -> Option<(usize, usize)>` balances braces in the mask and returns the opening and closing offsets.
- `model_blocks<'a>(masked: &'a str, original: &'a str) -> Vec<PrismaBlock<'a>>` combines the preceding two helpers, slices bodies from the original source, and records each body's starting offset for line calculation.
- `find_attribute<'a>(field_line: &'a str, name: &str) -> Option<&'a str>` locates `@map` outside nested default-attribute parentheses and returns its argument text.
- `mapped_name(field_line: &str) -> MappedName` returns `Absent` without `@map`, `Literal` only for a complete double-quoted argument, and `Dynamic` for every other argument form.

Parse field lines only inside a model block, skip block attributes beginning `@@`, and use the original source for names and the mask for delimiters. `Absent` uses the field identifier; `Dynamic` adds an unresolved record and no column. Never slice at a byte offset that is not a UTF-8 character boundary. Pass `StaticSource::Regex` for this non-AST parser rather than claiming tree-sitter provenance. Remove Prisma regex keys from `OrmModelShapes` after this call path is active.

- [ ] **Step 4: Verify and commit**

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test orm_models prisma -- --nocapture`

Expected: all Prisma tests PASS, including idempotent graph emission.

```bash
git add src/server/sensors/orm_model_sensor src/server/sensors/patterns tests/orm_models.rs
git commit -m "Answer: Parse Prisma model blocks without regex drift"
```

### Task 6: TypeORM and Drizzle AST Extraction

**Files:**
- Create: `src/server/sensors/orm_model_sensor/typescript.rs`
- Create: `src/server/sensors/patterns/ts/typeorm-orm-model.scm`
- Create: `src/server/sensors/patterns/tsx/typeorm-orm-model.scm`
- Create: `src/server/sensors/patterns/ts/drizzle-orm-model.scm`
- Create: `src/server/sensors/patterns/tsx/drizzle-orm-model.scm`
- Modify: `src/server/sensors/orm_model_sensor/mod.rs`
- Modify: `src/server/sensors/patterns/mod.rs`
- Modify: `src/server/sensors/patterns/frameworks.yaml`
- Modify: `tests/orm_models.rs`

**Interfaces:**
- Consumes: `Lang::{Ts,Tsx}`, `parse_for_lang`, `to_snake_case`, and Task 3's analysis types.
- Produces: `typescript::analyze_typeorm(...)` and `typescript::analyze_drizzle(...)` with `TreeSitter` edge provenance.

- [ ] **Step 1: Add failing convention, nested-option, and location tests**

```rust
#[test]
fn typeorm_and_drizzle_use_ast_names_and_locations() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/entities.ts",
        "@Entity()\nexport class OrderItem {\n  @Column({ name: 'display_name', transformer: { to: value => value } })\n  name: string;\n}\n",
    );
    write(
        tmp.path(),
        "src/schema.ts",
        "export const orders = pgTable('orders', {\n  total: numeric('total_amount', { precision: 10, scale: 2 }),\n});\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(relation(&analysis, EdgeType::MapsTo, "OrderItem", "order_item"));
    assert!(relation(&analysis, EdgeType::HasColumn, "order_item", "display_name"));
    assert!(relation(&analysis, EdgeType::HasColumn, "orders", "total_amount"));
    assert!(analysis.columns.iter().any(|column| column.name == "total_amount" && column.line == 2));
}
```

- [ ] **Step 2: Run and confirm current default/location errors**

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test orm_models typeorm_and_drizzle_use_ast_names_and_locations`

Expected: FAIL because `OrderItem` maps to `orderitem`.

- [ ] **Step 3: Add framework queries and AST readers**

Bundle the TypeORM decorator shape in both `ts` and `tsx` buckets:

```scheme
(decorator
  (call_expression
    function: (identifier) @decorator
    arguments: (arguments) @options)
  (#any-of? @decorator "Entity" "Column")) @orm_decorator
```

Bundle the Drizzle call shape:

```scheme
(call_expression
  function: (identifier) @table_constructor
  arguments: (arguments
    (string) @table_name
    (object) @columns)
  (#any-of? @table_constructor "pgTable" "mysqlTable" "sqliteTable")) @table
```

Walk the decorated class/property parents to associate captures. Accept `@Entity('orders')` and `@Entity({ name: 'orders' })`; use `to_snake_case` only when no explicit table name exists. Read `@Column` object `name` only from a string literal. Drizzle columns come from direct `pair` nodes in the captured object, and their line is the key/value property's 1-based start row. Dynamic values are unresolved.

- [ ] **Step 4: Remove obsolete regex keys, verify, and commit**

Delete TypeORM and Drizzle entries from `OrmModelShapes.patterns` and YAML after the AST path is active. Pass `StaticSource::TreeSitter` to `add_model` and `add_column` for their relations.

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test orm_models typeorm -- --nocapture && CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test orm_models drizzle -- --nocapture`

Expected: all PASS.

```bash
git add src/server/sensors/orm_model_sensor src/server/sensors/patterns tests/orm_models.rs
git commit -m "Answer: Extract TypeORM and Drizzle schemas from AST"
```

### Task 7: Explicit Sequelize Identity and Token-Aware SQL Migrations

**Files:**
- Create: `src/server/sensors/orm_model_sensor/javascript.rs`
- Create: `src/server/sensors/patterns/tsjs/sequelize-orm-model.scm`
- Modify: `src/server/sensors/orm_model_sensor/migrations.rs`
- Modify: `src/server/sensors/orm_model_sensor/mod.rs`
- Modify: `src/server/sensors/patterns/mod.rs`
- Modify: `src/server/sensors/patterns/frameworks.yaml`
- Modify: `tests/orm_models.rs`

**Interfaces:**
- Consumes: TypeScript/JavaScript AST literal/object helpers and `OrmAnalysis`.
- Produces: `javascript::analyze_sequelize(...)`, explicit-only table defaults, and `migrations::attach_sql_migrations(...)` for supported `ALTER TABLE ... ADD COLUMN` statements.

- [ ] **Step 1: Replace the incorrect pluralization expectation with failing honesty tests**

```rust
#[test]
fn sequelize_implicit_tables_are_unresolved_and_explicit_names_survive() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/models.js",
        "const OrderItem = sequelize.define('OrderItem', { name: { type: DataTypes.STRING } }, {});\nclass Product extends Model {}\nProduct.init({ name: { type: DataTypes.STRING, column: 'display_name' } }, { sequelize, tableName: 'products' });\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(!analysis.models.iter().any(|model| model.name == "OrderItem"));
    assert!(analysis.unresolved.iter().any(|item| item.contains("OrderItem")));
    assert!(relation(&analysis, EdgeType::MapsTo, "Product", "products"));
    assert!(relation(&analysis, EdgeType::HasColumn, "products", "display_name"));
}
```

Add this SQL identity regression as a separate test:

```rust
#[test]
fn sql_migration_requires_matching_table_and_column_tokens() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "schema.prisma",
        "model Product {\n  name String @map(\"display_name\")\n  @@map(\"products\")\n}\n",
    );
    write(
        tmp.path(),
        "migrations/0001.sql",
        "ALTER TABLE orders ADD COLUMN display_name text;\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(analysis.columns.iter().all(|column| column.migration.is_none()));

    write(
        tmp.path(),
        "migrations/0002.sql",
        "ALTER TABLE products ADD COLUMN display_name text;\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    let column = analysis.columns.iter().find(|column| column.name == "display_name").unwrap();
    assert_eq!(column.migration.as_ref().map(|site| site.line), Some(1));
}
```

- [ ] **Step 2: Run and confirm the old `OrderItems` guess**

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test orm_models sequelize_implicit_tables_are_unresolved_and_explicit_names_survive`

Expected: FAIL because the current extractor invents `OrderItems`.

- [ ] **Step 3: Extract `define` and `Model.init` calls structurally**

Bundle this JavaScript call shape:

```scheme
(call_expression
  function: (member_expression) @callee
  arguments: (arguments) @arguments) @definition
```

In `javascript.rs`, accept a callee only when its text is exactly `sequelize.define` or its property is `init` on a class-shaped receiver. For `define`, the model name is its first literal argument and attributes/options are the second/third object literals. For `init`, the model is the receiver identifier and attributes/options are the first/second object literals. Require literal `tableName`; if absent, append an unresolved record and emit no model/table/column relations for that definition. Read attribute `column` only from literal strings. Pass `StaticSource::TreeSitter` to `add_model` and `add_column`. Once this path is active, remove the now-empty `OrmModelShapes.patterns` map and the old regex `pattern` helper so no legacy ORM source-text extractor remains callable.

- [ ] **Step 4: Tokenize supported SQL migration statements**

Implement a bounded lexer yielding identifiers, quoted identifiers, punctuation, and other tokens with byte offsets. Parse only this token sequence, allowing a trailing semicolon and optional column type tokens:

```text
ALTER TABLE <table> ADD COLUMN [IF NOT EXISTS] <column>
```

Compare unquoted identifiers case-insensitively and quoted identifiers exactly after removing their matching quotes. Never search the whole statement for a column substring. Attach the statement's 1-based source line only when both table and column match; leave all other SQL forms unmatched.

- [ ] **Step 5: Verify and commit**

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test orm_models`

Expected: all 13 existing tests plus all added regressions PASS; no `OrderItems` convention guess remains.

```bash
git add src/server/sensors/orm_model_sensor src/server/sensors/patterns tests/orm_models.rs
git commit -m "Answer: Require explicit Sequelize identity and exact SQL evidence"
```

### Task 8: Evidence-Based Frontend Dispositions and Unclassified Coverage

**Files:**
- Modify: `src/server/sensors/frontend_value_sensor.rs`
- Modify: `src/server/sensors/patterns/mod.rs:515-525,1293-1324`
- Modify: `src/server/sensors/patterns/frameworks.yaml:776-779`
- Modify: `src/server/federation/contracts/model.rs:1089-1162`
- Modify: `tests/frontend_value_sinks.rs`

**Interfaces:**
- Consumes: existing exact `FieldRef` nodes, lexical `Binding`, `emit`, and frontend edge replacement.
- Produces: `scan_workspace_frontend_values_with_report(...) -> Result<ScanReport, LainError>`, React binding-aware `Stores`, and `UnresolvedReason::FrontendValueUnclassified` coverage records.

- [ ] **Step 1: Add failing setter, misleading-name, and unclassified tests**

Extend the test fixture helper to return `ScanReport` and add:

```rust
#[test]
fn frontend_storage_uses_bindings_and_unclassified_use_is_not_discarded() {
    let source = r#"import { useState as useReactState } from "react";
export function Orders({ data }) {
  const [ignored, refresh] = useReactState(null);
  const stored = data.customer.id;
  const returned = data.total;
  const unused = data.internal;
  refresh(stored);
  setCustomer(returned);
  let assigned;
  assigned = returned;
  return returned;
}
"#;
    let fixture = FrontendFixture::new(source);
    let report = fixture.scan();
    assert!(fixture.has(EdgeType::Stores, "customer.id"));
    assert!(!fixture.has(EdgeType::Stores, "total"));
    assert!(fixture.has(EdgeType::Forwards, "total"));
    assert!(fixture.has(EdgeType::Discards, "internal"));
    assert!(!fixture.has(EdgeType::Discards, "total"));
    assert!(report.unresolved.iter().any(|record| {
        record.reason == UnresolvedReason::FrontendValueUnclassified
            && record.sample_ids.iter().any(|site| site.contains("Orders.tsx"))
    }));
}
```

Define the test helper in `tests/frontend_value_sinks.rs` with this interface:

```rust
struct FrontendFixture {
    _temp: tempfile::TempDir,
    graph: GraphDatabase,
    component: GraphNode,
    fields: std::collections::BTreeMap<String, GraphNode>,
}

impl FrontendFixture {
    fn new(source: &str) -> Self {
        let (_temp, graph, component, fields) = build_exact_frontend_graph(source);
        Self { _temp, graph, component, fields }
    }

    fn scan(&self) -> ScanReport {
        scan_workspace_frontend_values_with_report(
            &self.graph,
            self._temp.path(),
            &RepoNamespace::for_test(),
        )
        .unwrap()
    }

    fn has(&self, edge_type: EdgeType, field: &str) -> bool {
        let field = &self.fields[field];
        self.graph.all_edges().iter().any(|edge| {
            edge.edge_type == edge_type
                && edge.source_id == self.component.id
                && edge.target_id == field.id
        })
    }
}
```

Implement `build_exact_frontend_graph(source: &str) -> (tempfile::TempDir, GraphDatabase, GraphNode, BTreeMap<String, GraphNode>)` by extracting the existing tempdir/component/`add_field` setup and registering `customer.id`, `total`, and `internal` on their actual declaration lines. The `setCustomer(returned)` argument is proven forwarding, while the subsequent assignment and `return returned` are used but unclassified.

- [ ] **Step 2: Run and confirm name-based classification**

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test frontend_value_sinks frontend_storage_uses_bindings_and_unclassified_use_is_not_discarded`

Expected: FAIL because no report exists and the current setter logic is name-based.

- [ ] **Step 3: Replace name regexes with storage configuration and React bindings**

Change `FrontendValueShapes` to:

```rust
#[derive(Debug, Deserialize, Clone, Default, PartialEq, Eq)]
pub struct FrontendValueShapes {
    #[serde(default)]
    pub storage_calls: std::collections::BTreeSet<String>,
}
```

Merge `storage_calls` additively and replace YAML with:

```yaml
frontend_values:
  storage_calls:
    - localStorage.setItem
    - sessionStorage.setItem
```

Collect imported React hook aliases and array-pattern declarations. The essential binding rule is:

```rust
#[derive(Clone, Copy)]
enum ReactHook {
    UseState,
    UseReducer,
}

struct ReactImports {
    by_local_name: std::collections::BTreeMap<String, ReactHook>,
}

impl ReactImports {
    fn kind_of(&self, local_name: &str) -> Option<ReactHook> {
        self.by_local_name.get(local_name).copied()
    }
}

fn react_setter(node: Node, src: &[u8], react_hooks: &ReactImports) -> Option<String> {
    let declarator = (node.kind() == "variable_declarator").then_some(node)?;
    let name = declarator.child_by_field_name("name")?;
    let value = declarator.child_by_field_name("value")?;
    let hook = called_hook_name(value, src)?;
    let slot = match react_hooks.kind_of(hook)? {
        ReactHook::UseState => 1,
        ReactHook::UseReducer => 1,
    };
    array_pattern_element(name, slot, src)
}
```

Populate `ReactImports` only from imports whose source is `react`, including aliases and the `React` namespace used by `React.useState`/`React.useReducer`. Define `called_hook_name(value: Node, src: &[u8]) -> Option<&str>` to return the local hook identifier from a direct or React-namespace call. Define `array_pattern_element(name: Node, slot: usize, src: &[u8]) -> Option<String>` to return only an identifier at the requested array-pattern slot. Store the resulting setter identifier in `Binding` scope data and classify calls through that binding, never `^set[A-Z_]`.

- [ ] **Step 4: Emit rich reports for used-but-unclassified values**

Append `FrontendValueUnclassified` to `UnresolvedReason`. Replace `register_sensor!` for `FrontendValueSensor` with the equivalent explicit `Sensor` implementation and exactly one `inventory::submit!(SensorEntry(&FrontendValueSensor))`; override `scan_with_report` to call the new report function.

Track every non-declaration use before disposition matching. When a use produces no `Renders`, `Stores`, or `Forwards` edge, record its `path:line`; do not insert an uncertainty edge. Return:

```rust
Ok(ScanReport {
    emitted: edges.len(),
    error: None,
    unresolved: grouped_unresolved(&unclassified_sites),
})
```

Implement `fn grouped_unresolved(sites: &[String]) -> Vec<UnresolvedRecord>` as a local helper returning an empty vector for no sites and otherwise one `UnresolvedRecord { reason: FrontendValueUnclassified, count, sample_ids }` with at most five sorted sites.

- [ ] **Step 5: Verify sensor architecture and behavior**

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test frontend_value_sinks && python3 scripts/check-no-duplicate-sensors.py && CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test sensor_pipeline --lib`

Expected: all PASS; generated-document links survive and two scans remain idempotent.

- [ ] **Step 6: Commit**

```bash
git add src/server/sensors/frontend_value_sensor.rs src/server/sensors/patterns src/server/federation/contracts/model.rs tests/frontend_value_sinks.rs
git commit -m "Answer: Classify frontend values from evidence"
```

### Task 9: Capability Semantics and Full Delivery Verification

**Files:**
- Modify: `tests/capability_discovery.rs`
- Modify: `src/server/tools.rs:1603-1660`
- Modify: `docs/FEDERATION.md`
- Modify: `docs/superpowers/plans/2026-11-02-evidence-based-sensor-extraction.md` only to check completed steps during execution

**Interfaces:**
- Consumes: existing `get_capabilities` registry and lifecycle projection.
- Produces: documented lifecycle semantics and final verified PR head; no capability payload redesign.

- [ ] **Step 1: Strengthen capability lifecycle tests**

Extend the federation readiness tests so warming and unavailable repositories assert the same state on `relations[]`, `emitters[]`, and `snapshots`, while `indexed` and registration names remain present. Retain the zero-detection integration assertion:

```rust
let django = emitters.iter().find(|emitter| emitter["name"] == "django_orm").unwrap();
assert_eq!(django["state"], "ready");
assert!(relations.iter().all(|relation| relation["indexed"].is_boolean()));
assert_eq!(value["snapshots"]["supported"], true);
```

Add this helper to the `tools.rs` test module and use it for `repos[1]` while beta is indexing:

```rust
fn assert_engine_lifecycle(repo: &serde_json::Value, state: &str) {
    assert!(repo["relations"].as_array().unwrap().iter().all(|relation| {
        relation["state"] == state && relation["indexed"].is_boolean()
    }));
    assert!(repo["emitters"].as_array().unwrap().iter().all(|emitter| {
        emitter["state"] == state && emitter["name"].is_string()
    }));
}
```

After the warming assertions, transition beta through the existing federation handle and verify unavailable state without changing registration data:

```rust
executor
    .ctx
    .federation
    .as_ref()
    .unwrap()
    .get_repo(&RepoId::new("beta").unwrap())
    .unwrap()
    .set_health(RepoHealth::Unavailable);
let unavailable: serde_json::Value =
    serde_json::from_str(&executor.get_capabilities().unwrap()).unwrap();
let beta = &unavailable["repositories"][1];
assert_engine_lifecycle(beta, "unavailable_error");
assert!(beta["relations"].as_array().unwrap().iter().all(|relation| relation["indexed"].is_boolean()));
assert_eq!(unavailable["snapshots"]["state"], "unavailable_error");
```

Do not infer readiness from node or edge counts.

- [ ] **Step 2: Document the meaning of availability and state**

Add a `get_capabilities` subsection to `docs/FEDERATION.md` with this contract:

```markdown
### Relation, emitter, and snapshot capabilities

`relations[].indexed` and `emitters[]` describe compiled-in support from the edge registry and sensor inventory. Their `state`, and `snapshots.state`, describe the owning repository's index lifecycle (`ready`, `warming_up`, or `unavailable_error`). They do not assert that a relation occurred in source or that every sensor completed without an extraction gap. Zero detections therefore leave a registered emitter ready. Use contract coverage and unresolved records for per-sensor extraction gaps.
```

- [ ] **Step 3: Run focused tests and schema/architecture guards**

Run:

```bash
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test capability_discovery --test orm_models --test frontend_value_sinks --test raw_snapshots --test raw_snapshot_repo_scope --test raw_structured_output
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test query:: --lib
make schema
python3 scripts/check-no-mirror-dtos.py
python3 scripts/check-mcp-dispatch-shape.py
python3 scripts/check-no-duplicate-sensors.py
git diff --check
```

Expected: all PASS. No schema diff is expected; if `make schema` updates `docs/tool-schema.json` for the new unresolved-reason variant, inspect and retain that intentional generated change, then rerun the schema command to confirm idempotence.

- [ ] **Step 4: Run the complete final workspace verification**

Run:

```bash
cargo fmt --all -- --check
LAIN_EMBEDDING_MODEL=/tmp/lain-no-such-model CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --workspace
```

Expected: zero failures and no unexpected skips. Record the exact passed/ignored totals printed by Cargo for the PR body. If formatting fails, run `cargo fmt --all`, inspect the diff, and repeat the focused tests before the full suite.

- [ ] **Step 5: Review the delivered diff and workspace hygiene**

Run:

```bash
git status --short
git diff --stat origin/dev...HEAD
git diff --check
df -h /data
```

Confirm no probe files, logs, test databases, or generated schema drift remain. Do not run `cargo clean` while further verification is needed; if cleanup is desired afterward, remove only this worktree's regenerable `target/` output.

- [ ] **Step 6: Commit remaining documentation, push, and update PR #390**

```bash
git add tests/capability_discovery.rs src/server/tools.rs docs/FEDERATION.md docs/tool-schema.json docs/superpowers/plans/2026-11-02-evidence-based-sensor-extraction.md
git commit -m "Answer: Document and verify evidence-based extraction"
git push origin spuentesp/booby
```

Update the PR body's verification section with the exact final Cargo totals and commands from Steps 3–4; do not reuse the earlier `3,980 passed` claim. Confirm `gh pr view 390 --json headRefOid,statusCheckRollup,reviewDecision,mergeStateStatus` points at the pushed head. Report green checks separately from `REVIEW_REQUIRED`; do not merge or delete the worktree unless the user explicitly authorizes it after review.
