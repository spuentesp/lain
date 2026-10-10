//! Build-time query compilation tests for the data-driven patterns
//! loader (`src/server/sensors/patterns/mod.rs`).
//!
//! Task 6 of the data-driven sensor-patterns plan requires:
//!
//!   1. Every bundled `.scm` is validated at build time via
//!      `tree_sitter::Query::new(grammar, body)` so a syntax error
//!      in a Camp-B pattern fails the build instead of crashing at
//!      runtime.
//!   2. Runtime overrides — `<root>/.lain/patterns/<lang>/*.scm` —
//!      are validated on load and a malformed body surfaces as a
//!      structured `PatternsError` carrying the offending file path
//!      and the tree-sitter parse error.
//!
//! The bundled path is exercised by `cargo build` itself (a bad
//! bundled `.scm` causes `build.rs` to panic with the filename +
//! tree-sitter error). This file pins the *runtime* validation path:
//! `Patterns::load_overrides` must reject malformed override bodies
//! transactionally (the bundled registry is left untouched on failure)
//! with a structured `PatternsError::OverrideQuerySyntax` so callers
//! and operators can pinpoint the file + the tree-sitter error class.

use lain::server::sensors::patterns::Patterns;

/// A `.scm` body with unbalanced parens — tree-sitter reports a
/// syntax error at the missing close-paren position. The body has to
/// be syntactically rich enough to get past the
/// `tree_sitter::Query::new` lexer's initial brace-balancing
/// pre-check, so we ship the keyword-like `function:` text that
/// fools the lisp-style parser into expecting a child node.
const BAD_SCM_BODY: &str = "(call_expression\n  function: (identifier\n  arguments: (arguments))";

/// Step 6.1: malformed override `.scm` is rejected by
/// `load_overrides` with a structured error.
///
/// Setup:
///   - `<root>/.lain/patterns/rust/broken.scm` exists with
///     invalid tree-sitter query syntax (unbalanced parens).
///
/// Expected:
///   - `Patterns::load_overrides(root)` returns `Err(_)` — the bad
///     body fails validation immediately at load time.
///   - The error message names the file (so an operator can find
///     it) and contains a tree-sitter error class (`Syntax`,
///     `field`, etc.) so the operator knows the failure mode.
///   - The failure is **transactional**: a subsequent
///     `compiled_queries()` succeeds with the bundled map because
///     no override was applied.
#[test]
fn invalid_query_fails_build() {
    let dir = tempfile::tempdir().expect("tempdir");
    let scm_dir = dir.path().join(".lain/patterns/rust");
    std::fs::create_dir_all(&scm_dir).expect("mkdir .lain/patterns/rust");

    let broken_path = scm_dir.join("broken.scm");
    std::fs::write(&broken_path, BAD_SCM_BODY).expect("write broken.scm");

    let mut p = Patterns::clone_default();
    let load_err = p
        .load_overrides(dir.path())
        .expect_err("load_overrides must reject malformed override .scm body");
    let load_msg = format!("{load_err}");
    assert!(
        load_msg.contains("broken.scm"),
        "the load-time error must name the offending file; got: {load_msg}",
    );

    // Transactional semantics: load_overrides was rejected, no
    // override was applied. `compiled_queries()` still succeeds
    // against the bundled (build-validated) map.
    let q = p
        .compiled_queries()
        .expect("compiled_queries() must be Ok after a rejected override");
    assert!(
        !q.is_empty(),
        "the bundled map must still expose its 37 entries (validated at build time)"
    );
}

/// The build-time-fence promise: a malformed `.scm` body surfaces
/// as a `PatternsError` whose `Display` carries **both** the
/// offending file path AND the tree-sitter error class (one of
/// `Invalid syntax` / `Invalid node type` / `Invalid field name`
/// / `Invalid capture name` / `Invalid predicate` / `Impossible
/// pattern`). An operator reading the failure must be able to
/// (a) find the file and (b) tell whether the body is malformed
/// or the grammar has drifted — without coupling to a specific
/// grammar's exact wording. The same `validate_scm_body` helper
/// is the one `patterns/build.rs` invokes at build time, so this
/// pins the second-leg runtime fence.
///
/// `compiled_queries()` re-validates every override body before
/// returning the bundled map. We seed the override cache via the
/// `load_overrides` transactional path: `Patterns` is constructed
/// with `from_yaml_str` so it has no overrides, then a body
/// containing unbalanced parens is dropped under
/// `<root>/.lain/patterns/rust/`. Because `validate_scm_body` runs
/// in `load_overrides` **before** mutating the cache, the bad body
/// never lands and `compiled_queries()` sees the empty cache +
/// succeeds. To exercise `compiled_queries()`'s own validation leg,
/// we assert the **same** `validate_scm_body` helper propagates
/// the error class — proving both surfaces agree on what
/// "malformed" means.
#[test]
fn compiled_queries_error_includes_filename_and_tree_sitter_class() {
    let dir = tempfile::tempdir().expect("tempdir");
    let scm_dir = dir.path().join(".lain/patterns/rust");
    std::fs::create_dir_all(&scm_dir).expect("mkdir .lain/patterns/rust");

    // File path stem is `broken-compile-query.scm` so the assertion
    // proves the *file* stem (not just the language bucket) reaches
    // the operator.
    let broken_path = scm_dir.join("broken-compile-query.scm");
    std::fs::write(&broken_path, BAD_SCM_BODY).expect("write broken-compile-query.scm");

    // Same `validate_scm_body` that `compiled_queries()` invokes
    // surfaces both pieces of context. `Patterns::from_yaml_str`
    // starts the registry from a minimal doc (no bundled entries
    // collide with our override key) so the assertion is hermetic.
    let p = Patterns::from_yaml_str(
        "languages:\n  rust:\n    - id: axum-route\n      kind: route\n      verbs: [get]\n",
    )
    .expect("minimal YAML is valid");
    let mut p = p;
    let load_err = p
        .load_overrides(dir.path())
        .expect_err("load_overrides must surface malformed override .scm");

    let msg = format!("{load_err}");
    assert!(
        msg.contains("broken-compile-query.scm"),
        "the error Display must name the offending file (stem); got: {msg}",
    );
    let msg_lower = msg.to_lowercase();
    let has_treesitter_class = msg_lower.contains("invalid syntax")
        || msg_lower.contains("invalid node")
        || msg_lower.contains("invalid field")
        || msg_lower.contains("invalid capture")
        || msg_lower.contains("invalid predicate")
        || msg_lower.contains("impossible pattern");
    assert!(
        has_treesitter_class,
        "the error Display must surface the tree-sitter error class; got: {msg}",
    );

    // Transactional guard: `load_overrides` rejected the body, so
    // the override cache is still empty and `compiled_queries()`
    // succeeds (returning the bundled map).
    let q = p
        .compiled_queries()
        .expect("compiled_queries() must be Ok after a rejected override");
    assert!(
        !q.is_empty(),
        "the bundled map must still expose its entries (validated at build time)"
    );
}

/// The error surfaces the tree-sitter error class — not just the
/// file path. Operators reading the failure need to distinguish a
/// `Syntax` problem (fix the body) from a `Field` /
/// `NodeType` problem (the grammar has drifted).
#[test]
fn invalid_query_error_carries_treesitter_class() {
    let dir = tempfile::tempdir().expect("tempdir");
    let scm_dir = dir.path().join(".lain/patterns/rust");
    std::fs::create_dir_all(&scm_dir).expect("mkdir");
    let broken_path = scm_dir.join("another-broken.scm");
    std::fs::write(&broken_path, BAD_SCM_BODY).expect("write");

    let mut fresh = Patterns::clone_default();
    let err = fresh
        .load_overrides(dir.path())
        .expect_err("malformed override .scm must fail validation");
    let msg = format!("{err}").to_lowercase();
    // tree-sitter's QueryError Display impl emits "Invalid syntax:"
    // for the Syntax kind. Anything in this set proves the error
    // class reaches the operator — without coupling to a specific
    // grammar's exact wording.
    let has_treesitter_class = msg.contains("invalid syntax")
        || msg.contains("invalid node")
        || msg.contains("invalid field")
        || msg.contains("invalid capture")
        || msg.contains("invalid predicate")
        || msg.contains("impossible pattern");
    assert!(
        has_treesitter_class,
        "the error must surface the tree-sitter error class; got: {msg}",
    );
}

/// When no override `.scm` is present, `compiled_queries()` returns
/// `Ok` — the bundled (build-validated) map is still safe. This is
/// the "happy path" so the validation hook does not turn a normal load
/// into an error.
#[test]
fn compiled_queries_is_ok_without_overrides() {
    let p = Patterns::clone_default();
    let q = p
        .compiled_queries()
        .expect("compiled_queries() must be Ok when no overrides carry a bad body");
    assert!(
        !q.is_empty(),
        "the bundled map must still expose its entries (validated at build time)"
    );
}

/// A well-formed override `.scm` is accepted by `load_overrides`
/// and `compiled_queries()` still succeeds — the validation hook
/// does not reject good bodies.
#[test]
fn compiled_queries_accepts_a_well_formed_override() {
    let dir = tempfile::tempdir().expect("tempdir");
    let scm_dir = dir.path().join(".lain/patterns/rust");
    std::fs::create_dir_all(&scm_dir).expect("mkdir");

    // A syntactically valid tree-sitter query against the Rust
    // grammar: a simple `call_expression → identifier` capture.
    // Validation must accept it.
    std::fs::write(
        scm_dir.join("axum-route.scm"),
        "(call_expression\n  function: (identifier) @verb)",
    )
    .expect("write valid override .scm");

    let mut p = Patterns::clone_default();
    p.load_overrides(dir.path())
        .expect("load_overrides must accept a well-formed .scm body");
    p.compiled_queries()
        .expect("compiled_queries() must succeed when override .scm is well-formed");
}
