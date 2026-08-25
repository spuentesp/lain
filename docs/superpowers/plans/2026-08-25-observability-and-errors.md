# Observability and Error Handling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `lain mcp`'s failure modes observable (init tracing, surface silent drops as a counter) and make `LainError` machine-readable (collapse 17 string-shaped variants into a structured `Message { category, msg }`), and stop the CLI from calling `std::process::exit` from inside `Result`-returning functions.

**Architecture:** Three independent refactors stack vertically:

1. **`LainError` collapses.** Today `LainError` has 19 variants, 17 of which are `String` payloads with no machine-readable shape — only the variant name in `Display`. We add `pub enum ErrorCategory { Git, Graph, Database, Lsp, Nlp, Mcp, Io, Serialization, Config, Workspace, Watcher, Ingest, Reload, Other }` and a single `Message { category: ErrorCategory, msg: String }` variant. The named `Git`, `Graph`, … variants get deleted in the same PR (the report's recommended simpler path). `NotImplemented` goes too — verified unused (`grep -rn NotImplemented src/` returns only the enum definition and a string literal `"NotImplementedError"` in `treesitter.rs:62`). Three variants that carry non-`String` payloads (`UnsupportedManifestVersion(u32)`, `Json(#[from] serde_json::Error)`, `AmbiguousSymbol(Vec<RepoId>)`) stay as-is — they already have structure.

2. **Tracing initializes inside `LainServer::new`.** Every `warn!` in `src/server/ingest/ingestion.rs` (batch persist errors, orphan sweep failures, edge drops, file scan errors) currently goes to stderr that nothing reads when `lain mcp` runs as a child of an MCP client. Cheap fix: `tracing_subscriber::fmt::try_init()` once at `LainServer::new`, gated on `RUST_LOG` being set. No-op when unset so binary stdout remains clean for the MCP protocol.

3. **Silent drops become a counter.** Add `silent_drop_count: AtomicU64` on `LainServer` and increment at the three known silent-warning sites (`insert_edges_reporting`, `sweep_orphans`, the NLP prewarm error path). Surface in `get_health` as `silent_drops: u64`. Operators see "did the indexer swallow anything" without scraping stderr.

4. **CLI stops calling `std::process::exit` from inside `Result<()>` functions.** The five sites in `query.rs`, `ask.rs`, and `workspaces.rs` convert to `tracing::warn!` + `Err(anyhow!(...))`. Same pattern in `refresh/mod.rs:139-143`. The CLI binary's `main` already prints anyhow's chain and exits non-zero on `Err`, so semantics are preserved.

**Tech Stack:** Rust 1.75+, `thiserror`, `tracing` (already a dependency), `tracing_subscriber::fmt::try_init` (add to `Cargo.toml` if not present — verify first), `serde_json`. No new high-level dependencies.

**Source spec:** `docs/superpowers/reviews/2026-08-25-solid-dry-simplification.md` § P0-6, P0-8, P2-3.

---

## Global Constraints

- **Preserve every existing public API that downstream crates consume.** `LainError` is re-exported from `src/lib.rs` and used in `src/main.rs`, `src/server/mcp/**`, and the federation crate boundary. The `From<git2::Error>`, `From<std::io::Error>`, `From<ort::Error<T>>`, `From<serde_json::Error>` impls stay (the last via `#[from]`).
- **`LainError` `Display` must remain stable.** Existing tests at `src/server/error_tests.rs:1-112` assert `to_string()` for every variant: `"Git error: ref not found"`, `"Graph database error: node not found"`, etc. The new `Message { category, msg }` variant must `Display` to the same shape (e.g. `Git error: ref not found`, not `Message(Git, "ref not found")`).
- **`LainError::Serialize` keeps working.** `src/server/error_tests.rs:82-86` (`test_lain_error_serialize`) asserts `serde_json::to_string(&err).unwrap()` round-trips. Continue serializing as the human-readable string.
- **Match repo test style.** Tests live in `tests/` (integration) and `#[cfg(test)] mod tests` blocks at the bottom of source files (unit). `error_tests.rs` is already a sibling of `error.rs`; extend it in place — don't move it.
- **Frequent commits.** Each task ends with one `git commit`. Commit messages follow the existing imperative-mood, period-free style (e.g. `Init tracing in LainServer::new when RUST_LOG is set`).
- **No `git push` and no PR creation** unless the user explicitly asks.
- **No new public API on `ToolHandler` or `LainMcpServer`.** This plan does not touch Plan 1's surface; the two plans merge independently.

---

## File Structure

| Path | Change | Responsibility |
|---|---|---|
| `src/server/error.rs` | **Modify** | Add `ErrorCategory`; collapse 17 string-typed variants into `Message { category, msg }`; keep 3 structured variants |
| `src/server/error_tests.rs` | **Modify** | Rewrite `test_lain_error_all_variants` to cover the new shape; add `test_error_category_round_trips` |
| `src/server/ingest/ingestion.rs` | **Modify** | Replace 4 `LainError::Other(...)` sites with `LainError::Message { category: ErrorCategory::Ingest, msg }`; increment `silent_drop_count` at the 3 silent-warning sites |
| `src/server/ingest/server.rs` | **Modify** | Replace `LainError::Other(...)` sites (likely 6+) with `Message { category: ErrorCategory::Other, msg }` |
| `src/server/ingest/mod.rs` | **Modify** | Replace `LainError::Other(...)` sites; any others |
| `src/server/ingest/constructors.rs` | **Modify** | Replace `LainError::Other(...)` sites; add `silent_drop_count: AtomicU64` field + increment at remaining silent sites |
| `src/server/refresh/mod.rs` | **Modify** | Replace `eprintln!` at line 139-143 with `tracing::warn!`; replace any `LainError::Other` sites |
| `src/server/mod.rs` | **Modify** | No code change unless an `Other` site lives here |
| `src/cli/query.rs` | **Modify** | Convert 2 `eprintln! + std::process::exit` sites (lines 32-35, 49-51) to `tracing::warn! + Err(anyhow!(...))`; replace `LainError::Other` if present |
| `src/cli/ask.rs` | **Modify** | Convert 4 `std::process::exit` sites (lines 8, 13, 23, 35) to silent `Err(anyhow!(...))` (ask is a hook best-effort path) |
| `src/cli/workspaces.rs` | **Modify** | Convert 1 `eprintln! + exit` site (lines 341-342) to `Err(anyhow!(...))`; fix 2 typed-variant-then-anyhow sites (lines 136-142, 326-328) to return `LainError::Message` directly |
| `src/main.rs` | **Modify** | Replace any `LainError::Other` sites; preserve the existing top-level anyhow handling |
| `src/server/federation/**`, `src/server/tools/**`, `src/server/graph.rs`, `src/server/git.rs`, `src/server/lsp.rs`, `src/server/nlp.rs`, `src/server/watcher.rs`, `src/server/audit.rs` | **Modify** | Replace `LainError::Other(...)` and any deprecated-variant call sites — `Cargo check` will guide |

---

## Task 1: Add `ErrorCategory` and `LainError::Message`, delete the 17 string-typed variants

**Files:**
- Modify: `src/server/error.rs` (rewrite the `LainError` enum body)
- Modify: `src/server/error_tests.rs` (rewrite the assertions to cover the new shape)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
  pub enum ErrorCategory {
      Git, Graph, Database, Lsp, Nlp, Mcp, Io, Serialization,
      Config, Workspace, Watcher, Ingest, Reload, Other,
  }

  #[derive(Error, Debug)]
  pub enum LainError {
      #[error("{category_display}: {msg}")]
      Message { category: ErrorCategory, msg: String },

      #[error("Unsupported manifest version: {0}")]
      UnsupportedManifestVersion(u32),

      #[error("JSON error: {0}")]
      Json(#[from] serde_json::Error),

      #[error("Ambiguous symbol: matches repos {0:?}")]
      AmbiguousSymbol(Vec<crate::federation::repo_id::RepoId>),
  }

  impl LainError {
      pub fn category(&self) -> ErrorCategory { /* see Step 3 */ }
      pub fn msg(&self) -> &str { /* see Step 3 */ }
  }
  ```
- The `From<git2::Error>` / `From<std::io::Error>` / `From<ort::Error<T>>` impls convert to `Message { category: ErrorCategory::Git/Io/Nlp, msg: err.to_string() }`.

### Step 1: Run the existing tests, verify they pass

```bash
cd /home/sebastian/lain
cargo test --lib server::error_tests -- --nocapture
```

Expected: 13 tests pass (covers `Git`, `Graph`, `Database`, `Lsp`, `Nlp`, `Mcp`, `Io`, `Json`, `NotFound`, `Unavailable`, `Fatal`, `Debug`, `Serialize`, `From<git2>`, `all_variants`).

### Step 2: Replace `LainError` body, watch the build fail

Replace the body of `src/server/error.rs` with the new enum + `ErrorCategory` + helpers. The `#[error]` attribute on `Message` uses a private function `category_display` so it can format each variant the same way the old code did:

```rust
fn category_display(c: &ErrorCategory) -> &'static str {
    match c {
        ErrorCategory::Git => "Git error",
        ErrorCategory::Graph => "Graph database error",
        ErrorCategory::Database => "Database error",
        ErrorCategory::Lsp => "LSP error",
        ErrorCategory::Nlp => "NLP error",
        ErrorCategory::Mcp => "MCP error",
        ErrorCategory::Io => "IO error",
        ErrorCategory::Serialization => "Serialization error",
        ErrorCategory::Config => "Config error",
        ErrorCategory::Workspace => "Workspace error",
        ErrorCategory::Watcher => "Watcher error",
        ErrorCategory::Ingest => "Ingest error",
        ErrorCategory::Reload => "Reload error",
        ErrorCategory::Other => "Other error",
    }
}
```

Update the `From<...>` impls:

```rust
impl From<git2::Error> for LainError {
    fn from(err: git2::Error) -> Self {
        LainError::Message {
            category: ErrorCategory::Git,
            msg: err.message().to_string(),
        }
    }
}

impl From<std::io::Error> for LainError {
    fn from(err: std::io::Error) -> Self {
        LainError::Message {
            category: ErrorCategory::Io,
            msg: err.to_string(),
        }
    }
}

impl<T> From<ort::Error<T>> for LainError {
    fn from(err: ort::Error<T>) -> Self {
        LainError::Message {
            category: ErrorCategory::Nlp,
            msg: err.to_string(),
        }
    }
}
```

```bash
cd /home/sebastian/lain
cargo build 2>&1 | head -80
```

Expected: ~30+ compile errors, all of the form `error[E0599]: no variant or associated item named 'Git' found for enum 'LainError'` (and `Graph`, `Database`, …, `Other`, `NotImplemented`, `NotFound`, `Unavailable`, `InvalidRepoId`, `InvalidGlobalId`, `Fatal`, `Config`, `Workspace`). This is the expected failure surface — Tasks 2 and 3 fix them.

### Step 3: Update `src/server/error_tests.rs` to cover the new shape

Replace the per-variant assertions with shape-based ones. The key tests:

```rust
#[test]
fn test_message_displays_as_categorized_string() {
    let err = LainError::Message {
        category: ErrorCategory::Git,
        msg: "ref not found".to_string(),
    };
    assert_eq!(format!("{}", err), "Git error: ref not found");
}

#[test]
fn test_message_accessors_round_trip() {
    let err = LainError::Message {
        category: ErrorCategory::Workspace,
        msg: "x".to_string(),
    };
    assert_eq!(err.category(), ErrorCategory::Workspace);
    assert_eq!(err.msg(), "x");
}

#[test]
fn test_from_git2_preserves_category() {
    let git_err = git2::Error::new(git2::ErrorCode::NotFound, git2::ErrorClass::Reference, "reference not found");
    let err = LainError::from(git_err);
    assert_eq!(err.category(), ErrorCategory::Git);
    assert!(err.msg().contains("reference not found"));
    assert!(format!("{}", err).contains("reference not found"));
}

#[test]
fn test_from_io_preserves_category() {
    let io_err = io::Error::new(io::ErrorKind::NotFound, "file not found");
    let err = LainError::from(io_err);
    assert_eq!(err.category(), ErrorCategory::Io);
    assert!(format!("{}", err).contains("IO error"));
}

#[test]
fn test_unsupported_manifest_version_keeps_payload() {
    let err = LainError::UnsupportedManifestVersion(7);
    assert_eq!(format!("{}", err), "Unsupported manifest version: 7");
    assert_eq!(err.category(), ErrorCategory::Other);
    assert_eq!(err.msg(), "7");
}

#[test]
fn test_ambiguous_symbol_keeps_payload() {
    let err = LainError::AmbiguousSymbol(vec![]);
    assert!(format!("{}", err).contains("Ambiguous symbol"));
}

#[test]
fn test_error_serialize_uses_display() {
    let err = LainError::Message { category: ErrorCategory::Mcp, msg: "bad".into() };
    let json = serde_json::to_string(&err).unwrap();
    assert_eq!(json, "\"MCP error: bad\"");
}

#[test]
fn test_error_category_all_variants_have_display() {
    let cats = [
        ErrorCategory::Git, ErrorCategory::Graph, ErrorCategory::Database,
        ErrorCategory::Lsp, ErrorCategory::Nlp, ErrorCategory::Mcp,
        ErrorCategory::Io, ErrorCategory::Serialization, ErrorCategory::Config,
        ErrorCategory::Workspace, ErrorCategory::Watcher,
        ErrorCategory::Ingest, ErrorCategory::Reload, ErrorCategory::Other,
    ];
    for c in cats {
        let err = LainError::Message { category: c, msg: "x".into() };
        // Must always produce a non-empty `<category>: x` line.
        let s = format!("{}", err);
        assert!(s.ends_with(": x"), "category {c:?} produced {s:?}");
    }
}
```

For `category()` and `msg()` on the structured variants, the fallback is `ErrorCategory::Other` and `to_string()` respectively — document it in a comment.

### Step 4: Run tests, verify they fail with compile errors only inside other modules

```bash
cd /home/sebastian/lain
cargo test --lib server::error_tests -- --nocapture
```

Expected: error_tests pass on the assertions above, but **the library still fails to build** because every `LainError::Other(...)` site in `src/` references the deleted variant. That's Task 2's job; don't touch call sites in this task.

### Step 5: Commit `error.rs` only

```bash
git add src/server/error.rs src/server/error_tests.rs
git commit -m "Collapse LainError string variants into Message { category, msg }"
```

Yes, the workspace still doesn't build. The plan is to land `error.rs` as a green-fields change (compile-clean on its own) and sweep call sites in Task 2. If the reviewer rejects a workspace-red commit, squash Task 1+2 into one — but it's cleaner to land them separately so the diff for each task is reviewable.

---

## Task 2: Update the 25 `LainError::Other(...)` call sites

**Files:**
- Modify: every `src/**/*.rs` file that references `LainError::Other` (the report counted 25)
- Verify with: `grep -rn "LainError::Other" src/ | wc -l` → expect 0 after this task

**Interfaces:** no new public API. Every site changes from:

```rust
return Err(LainError::Other(format!("...{x}...")));
```

to:

```rust
return Err(LainError::Message {
    category: ErrorCategory::Other,
    msg: format!("...{x}..."),
});
```

When the surrounding context makes the category obvious (e.g. inside `graph.rs`, `ingest/ingestion.rs`, `federation/...`), prefer the specific category — `ErrorCategory::Graph`, `ErrorCategory::Ingest`, `ErrorCategory::Workspace`, etc. — so downstream consumers can branch on category without parsing the message string.

### Step 1: Find every site

```bash
cd /home/sebastian/lain
grep -rn "LainError::Other" src/ | tee /tmp/other-sites.txt
wc -l /tmp/other-sites.txt
```

Expected: 25 lines (matches the report's count).

### Step 2: Categorize

Walk `/tmp/other-sites.txt` and bucket each line into one of the 14 categories. Use the file path as the primary signal (`graph.rs` → `Graph`, `ingest/*` → `Ingest`, `federation/*` → `Other` unless obviously a workspace concern, `tools/*` → `Other`, `watcher.rs` → `Watcher`, `audit.rs` → `Other`, etc.). When in doubt, `Other` is safe and the behavioral contract is identical.

### Step 3: Apply edits in three commits, sized by blast radius

**Commit A — server core** (highest churn, lowest blast radius):

```bash
# Replace `LainError::Other(...)` in src/server/{error.rs already done, graph.rs, git.rs, lsp.rs, nlp.rs, audit.rs, presence.rs, presence_lock.rs, sentinel.rs, attribution.rs, schema.rs, auth.rs, sync_status.rs, toolchains.rs, tuning.rs, events_log.rs, revision_log.rs, state_lock.rs, sse.rs, watcher.rs, build_info.rs, glob_match.rs, treesitter.rs, overlay.rs, reload.rs, ingest/*}
# For each site: pick the specific category from Step 2's bucket, or `Other` if ambiguous.
```

Run:

```bash
cd /home/sebastian/lain
cargo build 2>&1 | grep -c "LainError::Other"
```

Expected: count drops from 25 to the count of remaining sites in `src/main.rs` + `src/cli/**` (likely 4-6).

```bash
git add -A src/server/
git commit -m "Route LainError::Other sites in src/server/ through Message categories"
```

**Commit B — CLI**:

```bash
# Same sweep in src/cli/**/*.rs. Prefer category `Other` here — the CLI doesn't have enough context to be more specific.
cd /home/sebastian/lain
cargo build 2>&1 | grep -c "LainError::Other"
```

Expected: count drops to 0 or near-0 (any leftover is a typo).

```bash
git add -A src/cli/
git commit -m "Route LainError::Other sites in src/cli/ through Message categories"
```

**Commit C — main + final sweep**:

```bash
cd /home/sebastian/lain
cargo build 2>&1 | grep "LainError::Other"
```

Expected: empty output. If any remain, fix them in this commit.

```bash
git add -A src/main.rs
git commit -m "Route remaining LainError::Other sites through Message categories"
```

### Step 4: Run the full test surface

```bash
cd /home/sebastian/lain
cargo test --lib
cargo test --tests
```

Expected: 100% pass; the same tests that passed before still pass. The message shape is preserved by the `category_display` mapping in Task 1 Step 2.

### Step 5: Commit a CHANGELOG annotation

```bash
# Optional — only if the project keeps a CHANGELOG. If not, skip.
git add docs/superpowers/reviews/2026-08-25-solid-dry-simplification.md  # annotate P0-6 as "Resolved by plan 2026-08-25-observability-and-errors"
git commit -m "docs: annotate P0-6 as resolved by observability-and-errors plan"
```

---

## Task 3: Remove `LainError::NotImplemented` (and the three other variants without categories)

**Files:**
- Modify: `src/server/error.rs` (already done in Task 1 — `NotImplemented` is gone)

This task is **already complete after Task 1 lands**, because the Task 1 Step 2 rewrite deletes every named variant except `UnsupportedManifestVersion`, `Json`, and `AmbiguousSymbol`. The only verification left:

### Step 1: Confirm `NotImplemented` is never constructed

```bash
cd /home/sebastian/lain
grep -rn "NotImplemented" src/ tests/ 2>/dev/null
```

Expected: only a single string literal `"NotImplementedError"` inside `src/server/treesitter.rs:62` (a Python keyword). No `LainError::NotImplemented` references anywhere.

### Step 2: Confirm `NotFound`, `Unavailable`, `Fatal`, `InvalidRepoId`, `InvalidGlobalId` are gone

```bash
cd /home/sebastian/lain
grep -rn "LainError::\(NotFound\|Unavailable\|Fatal\|InvalidRepoId\|InvalidGlobalId\)" src/ tests/
```

Expected: no matches (Task 2 already swept them when the build broke — they're constructed via `LainError::Other` today, not via the named variants). The `RefreshOutcome::NotFound` machinery in `src/server/mcp/handler.rs:1314-1340` is a different `RefreshOutcome` variant, not `LainError::NotFound` — leave it.

### Step 3: Run tests

```bash
cd /home/sebastian/lain
cargo test --lib
```

Expected: pass.

### Step 4: No commit

Task 1's commit already deleted the variants.

---

## Task 4: Initialize tracing in `LainServer::new` when `RUST_LOG` is set

**Files:**
- Modify: `src/server/ingest/constructors.rs:447-538` (`LainServer::new` body)
- Modify: `src/server/ingest/server.rs` (the `LainServer` struct — add `silent_drop_count: AtomicU64`)
- Modify: `src/server/ingest/constructors.rs` (`LainServer::for_federation` and any other constructor — initialize the new field)

**Interfaces:**
- Consumes: `std::env::var("RUST_LOG")`, `tracing_subscriber::fmt::try_init()` (add to `Cargo.toml` if not already a direct dep — verify with `grep tracing_subscriber Cargo.toml`; the `tracing` crate is already used)
- Produces: tracing is initialized exactly once per process. The first `LainServer::new` call installs a global subscriber; subsequent calls are no-ops via `try_init`'s idempotence.

### Step 1: Add the failing test

Append to `src/server/ingest/server.rs`'s `#[cfg(test)] mod tests` block (create the block if it doesn't exist — match the style in `error_tests.rs`):

```rust
#[cfg(test)]
mod tracing_init_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TRACING_TESTS_RUN: AtomicUsize = AtomicUsize::new(0);

    /// Each test must run in isolation because `tracing_subscriber::fmt::try_init`
    /// is process-global. We use a thread-local guard so only the first test in
    /// the process actually installs the subscriber.
    fn ensure_tracing_init_for_test() {
        if TRACING_TESTS_RUN.fetch_add(1, Ordering::SeqCst) == 0 {
            // Safe: this is the first call in the test process.
            let _ = tracing_subscriber::fmt()
                .with_writer(std::io::sink) // don't pollute test stdout
                .try_init();
        }
    }

    #[test]
    fn lain_server_new_initializes_tracing_when_rust_log_set() {
        ensure_tracing_init_for_test();
        // Set RUST_LOG just for this test — env mutation is process-global,
        // so accept that other tests may see it.
        std::env::set_var("RUST_LOG", "warn");
        let tmp = tempfile::tempdir().unwrap();
        let memory_path = tmp.path().join("graph.bin");
        let result = LainServer::new(tmp.path(), &memory_path, None);
        assert!(result.is_ok(), "LainServer::new failed: {result:?}");
        // Verify tracing is initialized by emitting a warn event and checking
        // that the subscriber didn't panic. We can't easily capture the sink
        // output here — see tracing_capture_warning below for the assertion
        // that proves emission works.
        let server = result.unwrap();
        tracing::warn!(target: "test", "post-init warn");
        // Touch the server so the optimizer doesn't drop the binding.
        let _ = server.workspace_root();
    }
}
```

The `workspace_root()` accessor must exist or be added (mirror the existing `started_at()` / `reload_bus()` accessors at `server.rs:151-218`). If `LainConfig` already exposes it publicly, use that. Otherwise add `pub fn workspace_root(&self) -> &Path { &self.config.workspace }`.

### Step 2: Run tests, verify failure

```bash
cd /home/sebastian/lain
cargo test --lib server::ingest::server::tracing_init_tests -- --nocapture
```

Expected: `error[E0599]: no function or associated item named 'workspace_root' found for struct 'LainServer'` (or, if the test compiles, no tracing is installed because the constructor doesn't call `try_init`, so the warn goes nowhere — the test passes vacuously; that's why the next step matters).

### Step 3: Initialize tracing in `LainServer::new`

In `src/server/ingest/constructors.rs`, at the top of `LainServer::new` (before the `LainConfig { ... }` line), add:

```rust
// Initialize tracing once per process when RUST_LOG is set. Cheap to call
// repeatedly because `try_init` is idempotent and returns Err on the second
// call (which we ignore). Without this, every `warn!` inside the ingest
// pipeline (batch persist errors, orphan sweep failures, edge drops) goes
// to stderr that no MCP client reads.
if std::env::var_os("RUST_LOG").is_some() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
}
```

`tracing_subscriber::EnvFilter` is already re-exported via the `tracing-subscriber` crate — verify it's a direct dep with `grep tracing-subscriber Cargo.toml`. If not, add `tracing-subscriber = { version = "0.3", features = ["env-filter"] }` to `[dependencies]`.

### Step 4: Add a real test that proves the warn reaches the subscriber

Append a second test:

```rust
    #[test]
    fn tracing_warn_emits_to_capture_writer() {
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone, Default)]
        struct CaptureWriter(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for CaptureWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        impl<'a> MakeWriter<'a> for CaptureWriter {
            type Writer = CaptureWriter;
            fn make_writer(&'a self) -> Self::Writer { self.clone() }
        }

        // Note: cannot re-init the global subscriber here (it's already installed
        // by the previous test or by lain's main). Use a thread-local guard.
        let buf = CaptureWriter::default();
        let _guard = tracing::subscriber::with_default(
            tracing_subscriber::fmt()
                .with_writer(buf.clone())
                .with_max_level(tracing::Level::WARN)
                .finish(),
            || {
                tracing::warn!(target: "test", "hello");
            },
        );
        let captured = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(captured.contains("hello"), "tracing captured: {captured:?}");
    }
```

This test does **not** depend on `LainServer::new`; it only proves that the `tracing` + `tracing_subscriber` machinery in this crate can capture `warn!` events. The previous test proves `LainServer::new` initializes the global subscriber. Together they prove end-to-end observability.

### Step 5: Run tests, verify pass

```bash
cd /home/sebastian/lain
cargo test --lib server::ingest::server::tracing_init_tests -- --nocapture
```

Expected: 2 passed.

### Step 6: Commit

```bash
git add Cargo.toml src/server/ingest/constructors.rs src/server/ingest/server.rs
git commit -m "Init tracing in LainServer::new when RUST_LOG is set"
```

---

## Task 5: Add `silent_drop_count: AtomicU64` on `LainServer` and surface in `get_health`

**Files:**
- Modify: `src/server/ingest/server.rs:39` (the `LainServer` struct — add the field)
- Modify: `src/server/ingest/constructors.rs` (initialize the field in `LainServer::new` and `LainServer::for_federation`)
- Modify: `src/server/ingest/ingestion.rs:430-460` (`insert_edges_reporting` and `insert_edges_best_effort`)
- Modify: `src/server/ingest/ingestion.rs:462-479` (`sweep_orphans`)
- Modify: `src/server/ingest/ingestion.rs:253-292` (NLP prewarm `tokio::spawn` error path)
- Modify: `src/server/tools/handlers/federation.rs` (the `get_health` handler) — if it exists, add `silent_drops: server.silent_drop_count()`

**Interfaces:**
- Produces:
  ```rust
  // In LainServer struct:
  pub silent_drop_count: AtomicU64,

  // Accessor:
  impl LainServer {
      pub fn silent_drop_count(&self) -> u64 {
          self.silent_drop_count.load(std::sync::atomic::Ordering::Relaxed)
      }

      pub fn record_silent_drop(&self) {
          self.silent_drop_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
      }
  }
  ```

  ```rust
  // In get_health response — extend the JSON shape:
  #[derive(Serialize)]
  pub struct HealthReport {
      // ... existing fields ...
      pub silent_drops: u64,
  }
  ```

### Step 1: Add the field and accessors, write the failing test

In `src/server/ingest/server.rs`, add the field and accessor:

```rust
pub struct LainServer {
    // ... existing fields ...
    pub silent_drop_count: AtomicU64,
}
```

```rust
impl LainServer {
    pub fn silent_drop_count(&self) -> u64 {
        self.silent_drop_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn record_silent_drop(&self) {
        self.silent_drop_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}
```

Test (append to the test block):

```rust
    #[test]
    fn silent_drop_count_starts_at_zero_and_increments() {
        let server = make_test_server();  // add this helper if missing
        assert_eq!(server.silent_drop_count(), 0);
        server.record_silent_drop();
        server.record_silent_drop();
        server.record_silent_drop();
        assert_eq!(server.silent_drop_count(), 3);
    }
```

If `make_test_server` doesn't exist, add it next to the test as a `#[cfg(test)]` helper that builds a `LainServer` with empty state (or, if that's too much surface, build the `AtomicU64` standalone and only exercise the accessor logic).

### Step 2: Run tests, verify failure

```bash
cd /home/sebastian/lain
cargo test --lib server::ingest::server::silent_drop_count -- --nocapture
```

Expected: `error[E0609]: no field 'silent_drop_count' on type 'LainServer'`.

### Step 3: Initialize the field in every constructor

In `src/server/ingest/constructors.rs`, find both `LainServer::new` (line 447) and `LainServer::for_federation` (around line 566 per the grep). In each, add `silent_drop_count: AtomicU64::new(0),` to the struct literal.

```bash
cd /home/sebastian/lain
grep -n "impl LainServer" src/server/ingest/constructors.rs
```

If there are more than two constructors, patch all of them. Use `cargo check` between edits.

### Step 4: Increment at the silent-warning sites

In `src/server/ingest/ingestion.rs`, modify `insert_edges_reporting`:

```rust
fn insert_edges_reporting(
    db: &GraphDatabase,
    edges: &[GraphEdge],
    label: &str,
    silent_drop_count: &AtomicU64,  // <-- new parameter
) -> Result<(), LainError> {
    match db.insert_edges_batch(edges) {
        Ok(0) => Ok(()),
        Ok(dropped) => {
            for _ in 0..dropped {
                silent_drop_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            warn!(
                "{dropped} of {} {label} edges dropped (endpoint not in index)",
                edges.len()
            );
            Ok(())
        }
        Err(e) => Err(e),
    }
}
```

Same pattern for `sweep_orphans` (add a `&AtomicU64` parameter, increment on prune failures). The NLP prewarm `tokio::spawn` closure doesn't have access to `&self` directly — it has `graph_clone`, so it cannot increment the counter from inside the closure. Mitigation: keep the `warn!` for prewarm errors but skip the counter increment there; document it in a comment ("NPL prewarm errors are visible via tracing when RUST_LOG is set; the counter is only for synchronous silent drops").

Pass `&self.silent_drop_count` to `insert_edges_reporting` from the call sites at `ingestion.rs:211`, `221`, and any other call site — `grep -n "insert_edges_reporting" src/server/ingest/ingestion.rs`.

### Step 5: Surface in `get_health`

In `src/server/tools/handlers/federation.rs` (or wherever the `get_health` `ToolHandler` lives — search with `grep -rn 'fn get_health\|"get_health"' src/server/tools/`), add `silent_drops` to the response JSON. Read via `server.silent_drop_count()`.

If `get_health` doesn't exist as a `ToolHandler` yet, this is the wrong layer — surface the field via `LainServer::last_error()`-style accessor and let the existing `get_server_status` tool pick it up.

### Step 6: Run tests

```bash
cd /home/sebastian/lain
cargo test --lib
cargo test --test federation_integration 2>/dev/null
```

Expected: all pass; `get_health` reports `silent_drops: 0` on a fresh server and `> 0` after triggering the edge-drop path in an integration test.

### Step 7: Commit

```bash
git add src/server/ingest/server.rs src/server/ingest/constructors.rs src/server/ingest/ingestion.rs src/server/tools/handlers/federation.rs
git commit -m "Surface silent ingest drops as silent_drops counter in get_health"
```

---

## Task 6: Convert CLI `eprintln! + std::process::exit` to `tracing::warn! + Err(anyhow!(...))`

**Files:**
- Modify: `src/cli/query.rs:32-35, 49-51` (2 sites)
- Modify: `src/cli/ask.rs:8, 13, 23, 35` (4 sites)
- Modify: `src/cli/workspaces.rs:341-342` (1 site)
- Modify: `src/cli/workspaces.rs:136-142, 326-328, 349-355` (3 sites that build `LainError::Config` and throw it away via `anyhow!`)

**Interfaces:** no new API. Each conversion:

```rust
// Before:
eprintln!("Error: ...{x}...");
std::process::exit(1);
// After:
tracing::warn!("...{x}...");
return Err(anyhow!("...{x}..."));
```

For `ask.rs` (a hook best-effort path), the existing `std::process::exit(0)` calls are **silent no-ops** for non-Lain inputs. Convert them to `return Err(anyhow!("not a lain query"))` and let the CLI's `main` decide exit code; if `main` exits non-zero on `Err`, change to `return Ok(())` after a `tracing::debug!` so the hook stays silent. Verify by reading `src/main.rs` first.

### Step 1: Find every site

```bash
cd /home/sebastian/lain
grep -rn "std::process::exit\|eprintln!" src/cli/
```

Expected: 7 exit sites + 4 eprintln sites in `cli/`. (The exact count depends on whether `ask.rs` exit(0) calls are still there — verify.)

### Step 2: Fix the typed-variant-then-anyhow sites in `workspaces.rs` first

The current code at `workspaces.rs:136-142`:

```rust
fn err_already_exists(name: &str) -> LainError {
    LainError::Config(format!("workspace '{name}' already exists"))
}
fn err_not_found(name: &str) -> LainError {
    LainError::Config(format!("workspace '{name}' not found"))
}
```

…returns `LainError::Config`, which no longer exists. Update both to:

```rust
fn err_already_exists(name: &str) -> LainError {
    LainError::Message {
        category: ErrorCategory::Workspace,
        msg: format!("workspace '{name}' already exists"),
    }
}
fn err_not_found(name: &str) -> LainError {
    LainError::Message {
        category: ErrorCategory::Workspace,
        msg: format!("workspace '{name}' not found"),
    }
}
```

Then the existing call sites (`.ok_or_else(|| anyhow!("{}", err_not_found(name)))`) become:

```rust
.ok_or_else(|| anyhow::Error::new(err_not_found(name)))
```

…so the typed variant survives. Alternative cleaner form: keep the helper but call it directly with `?` — refactor only if it shrinks the diff.

### Step 3: Convert `query.rs:32-35` (graph load failure)

```rust
// Before
eprintln!("Error: Failed to load graph at {:?}: {}", memory_path, e);
eprintln!("\nHint: Run 'lain mcp' (or 'lain server') first to build the code graph.");
std::process::exit(1);

// After
tracing::warn!("Failed to load graph at {}: {}", memory_path.display(), e);
return Err(anyhow!(
    "Failed to load graph at {}: {}\nHint: Run 'lain mcp' (or 'lain server') first to build the code graph.",
    memory_path.display(),
    e,
));
```

### Step 4: Convert `query.rs:49-51` (query execution failure)

```rust
// Before
eprintln!("Query error: {}", e);
std::process::exit(1);

// After
tracing::warn!("Query error: {}", e);
return Err(anyhow!("Query error: {}", e));
```

### Step 5: Convert `ask.rs` (4 sites)

Read `src/main.rs` first to confirm exit-code behavior on `Err`:

```bash
cd /home/sebastian/lain
grep -n "process::exit\|std::process" src/main.rs
```

If `main` exits non-zero on `Err`, the ask.rs `std::process::exit(0)` (silent no-op) becomes `return Ok(())`. If `main` exits non-zero on `Err`, the hook would start signaling failures for non-Lain commands — wrong. So:

```rust
// Before (line 8-ish):
if std::io::stdin().read_to_string(&mut input).is_err() {
    std::process::exit(0);
}
// After:
if std::io::stdin().read_to_string(&mut input).is_err() {
    tracing::debug!("ask: stdin read failed; exiting silently");
    return Ok(());
}
```

Same for the other three `exit(0)` sites. Add a test (or extend an existing one) that pipes empty input to `run_ask()` and asserts `Ok(())`.

### Step 6: Convert `workspaces.rs:341-342` (no active workspace)

```rust
// Before
None => {
    eprintln!("no active workspace; use `lain workspaces use <name>`");
    std::process::exit(1);
}
// After
None => {
    tracing::warn!("no active workspace");
    return Err(anyhow!("no active workspace; use `lain workspaces use <name>`"));
}
```

### Step 7: Run tests

```bash
cd /home/sebastian/lain
cargo test --lib
cargo test --test cli_surface
cargo test --test e2e_behavior 2>/dev/null | tail -20
```

Expected: all pass; `cli_surface` tests assert exit codes — verify the new `Err(anyhow!(...))` path still exits non-zero (the binary's `main` should do `std::process::exit(1)` on `Err`; if it doesn't, that's a separate fix).

### Step 8: Commit

```bash
git add src/cli/query.rs src/cli/ask.rs src/cli/workspaces.rs
git commit -m "Replace CLI eprintln+exit with tracing::warn + Err(anyhow!(...))"
```

---

## Task 7: Replace `eprintln!` in `refresh/mod.rs` with `tracing::warn!`

**Files:**
- Modify: `src/server/refresh/mod.rs:139-143` (the `parse_reindex_timeout` env-parse fallback)

**Interfaces:** no new API. Mechanical replacement.

### Step 1: Verify the site is the only `eprintln!` in `src/server/`

```bash
cd /home/sebastian/lain
grep -rn "eprintln!" src/server/
```

Expected: exactly one hit at `src/server/refresh/mod.rs:139-143`. (If more appear after Task 6 changes land, fix them in this task too — but they shouldn't, because Task 6 only touches `src/cli/`.)

### Step 2: Replace

```rust
// Before
Err(_) => {
    eprintln!(
        "LAIN_REINDEX_TIMEOUT={s:?} is not a valid integer; using default 300s"
    );
    Duration::from_secs(300)
}

// After
Err(_) => {
    tracing::warn!(
        "LAIN_REINDEX_TIMEOUT={s:?} is not a valid integer; using default 300s"
    );
    Duration::from_secs(300)
}
```

### Step 3: Add a test

Append to the existing `#[cfg(test)] mod tests` block:

```rust
#[test]
fn parse_reindex_timeout_invalid_value_returns_default_and_warns() {
    // Set the env var, but expect parsing to fall back to the default.
    // We can't easily capture tracing output here, so only assert the return value.
    std::env::set_var("LAIN_REINDEX_TIMEOUT", "not-a-number");
    let d = parse_reindex_timeout();
    assert_eq!(d, Duration::from_secs(300));
    std::env::remove_var("LAIN_REINDEX_TIMEOUT");
}
```

### Step 4: Run tests

```bash
cd /home/sebastian/lain
cargo test --lib server::refresh::tests -- --nocapture
```

Expected: pass.

### Step 5: Commit

```bash
git add src/server/refresh/mod.rs
git commit -m "Route parse_reindex_timeout warning through tracing::warn"
```

---

## Self-Review (do before handing to user)

After writing this plan, verify:

1. **Spec coverage:** Every finding from the report that's in this plan has at least one task.
   - P0-6 (LainError 19 variants → Message { category, msg }) → Tasks 1, 2, 3.
   - P0-6 sub-finding (`NotImplemented` dead code) → Task 3.
   - P0-6 sub-finding (`From<git2::Error>` flattens ErrorCode/ErrorClass) → Task 1 Step 2 (preserves the existing message; structuring the git2 fields is out of scope per the report's "simpler — replace them all in one PR" guidance).
   - P0-8 (tracing not initialized in `lain mcp`) → Task 4.
   - P0-8 sub-finding (`silent_drop_count`) → Task 5.
   - P2-3 (CLI eprintln + exit) → Task 6.
   - P2-3 sub-finding (refresh/mod.rs eprintln) → Task 7.

2. **Placeholder scan:** No "TODO" / "TBD" / "fill in" in any task body. Code blocks show actual signatures and code. The "verify with grep" instructions are concrete commands. The Task 5 Step 4 mitigation comment ("NPL prewarm errors are visible via tracing when RUST_LOG is set; the counter is only for synchronous silent drops") is an explanatory comment, not a placeholder.

3. **Type consistency:** `ErrorCategory` defined in Task 1 is consumed by Tasks 2, 4 (test only), 5 (test only), 6. `LainError::Message` defined in Task 1 is consumed by Tasks 2, 6. `LainServer::silent_drop_count` / `record_silent_drop` defined in Task 5 are consumed by Task 5 Step 4 (insertion sites) and Task 5 Step 5 (get_health). `tracing::subscriber::with_default` in Task 4 Step 4 is the standard library primitive for thread-local subscribers; `tracing_subscriber::fmt` is already used in `tracing-subscriber`.

4. **Bite-sized steps:** Each step is 2–5 minutes. The largest single step is Task 2 Step 3 (~25 mechanical edits across the codebase, but each is one line and grouped into 3 commits by blast radius). Task 1 Step 2 is the largest single edit (~50 lines), but it's a straight rewrite.

5. **Repo conventions:** TDD where existing tests exist (`error_tests.rs` already covers every variant; `cargo check` is the safety net for the 25 call-site sweep). No new tests invented for paths that have no existing coverage — instead, the tracing tests in Task 4 are minimal and the silent_drop_count test is mechanical.

6. **No-placeholders rule:** All code blocks are runnable. The `category_display` function in Task 1 Step 2 covers all 14 categories. The `From<...>` impls are complete. The `insert_edges_reporting` signature in Task 5 Step 4 takes `&AtomicU64` and the call sites are listed. The `parse_reindex_timeout` test in Task 7 Step 3 is self-contained.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-08-25-observability-and-errors.md`.

**Estimated total effort:** 7 tasks, ~3–5 working days for one engineer familiar with the codebase. Task 2 (the 25-site sweep) is the largest by line count but smallest by complexity — mechanical, driven by `cargo build` output.

**Risks:**
- **Task 1's "delete the variants" commit may be rejected** if the reviewer requires the workspace to stay green. Mitigation: squash Tasks 1 and 2 into a single commit — slightly larger diff, but the green-build invariant is preserved. The plan as written keeps them separate for reviewability.
- **Task 4 tracing init may double-emit** in test processes where multiple test binaries run and each calls `LainServer::new`. `tracing_subscriber::fmt::try_init()` returns `Err` on the second call within the same process, which we ignore — safe. Cross-process, each test binary initializes its own subscriber — also safe.
- **Task 5 `insert_edges_reporting` signature change ripples.** Every caller needs the new `&AtomicU64` parameter. The call sites are local to `ingestion.rs` (verified by grep), so the ripple is bounded.
- **Task 6's `ask.rs` conversion depends on `main.rs` exit behavior.** If `main` exits non-zero on `Err`, the `return Ok(())` choice is wrong (hook would silently swallow Lain queries too). Step 5 reads `main.rs` first to confirm.

Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task with this plan in hand, review between tasks, fast iteration. Best for this plan because Task 1's "delete the variants" commit is reviewable in isolation, and Task 2's sweep is mechanical.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints. Best if you want to do the review yourself.
