# Tree-sitter / LSP Coverage Honesty + Unified `LANGS` Table Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the four hard-coded per-language lists (`treesitter.rs:89-114, 250-258, 293-299`; `lsp.rs:28-48` `LANGUAGE_MAP`; `watcher.rs:122-125` `WATCHED_EXTENSIONS`; `toolchains.rs:274-292` `default_markers`) with a single `static LANGS: &[LangSpec]` table that is the source of truth for every per-language capability flag. Fix the silent Go fall-through bug, the rust-analyzer 5-second startup-timeout bug, the LSP install-honesty gap, and the dead `toolchains/` directory branch in one pass.

**Architecture:** A new `src/server/langs.rs` owns the table. Every other module that needs per-language metadata (`treesitter`, `lsp`, `watcher`, `toolchains`) imports the table and filters it. The four lists become derived views:

```rust
pub static LANGS: &[LangSpec] = &[ /* 19 entries */ ];
pub static LANGUAGE_MAP: Lazy<HashMap<&'static str, &'static LspConfig>> = ...;
pub static WATCHED_EXTENSIONS: Lazy<Box<[&'static str]>> = ...;
pub static SUPPORTED_LANGS: Lazy<Box<[&'static LangSpec]>> = ...;
```

Each `LangSpec` carries `(name, exts, tree_sitter: bool, lsp: Option<&'static LspConfig>, marker: Option<&'static str>, build_cmd: Option<&'static str>, supported: bool)`. `supported: true` ⇔ `tree_sitter: true` OR `lsp.install_cmd.is_some()` — never a half-truth. The Go entry stays in the table with `tree_sitter: false, lsp: Some(... install_cmd: Some(...)), supported: true` once `tree-sitter-go` is added to `Cargo.toml`. Until then Go is `supported: false` and `tree_sitter` returns `vec![]` honestly.

**Tech Stack:** Rust 1.75+, `once_cell::sync::Lazy` (already in `Cargo.toml`), `std::collections::HashMap`. No new dependencies.

**Source spec:** `docs/superpowers/reviews/2026-08-25-solid-dry-simplification.md` § P1-2, P1-11, P1-17, P1-18, P1-24.

## Global Constraints

- **Preserve the public surface on `LspMultiplexer`.** `LspMultiplexer::new`, `ensure_server`, `get_document_symbols_hierarchical`, `install_server`, `get_supported_languages`, `mark_unavailable`, `shutdown` are all called from tests, federation, and MCP code. None of their signatures change. The internal `registry: HashMap<String, &'static LspConfig>` is also unchanged. Only the way the registry is populated moves from a local `LANGUAGE_MAP` const to the shared table.
- **`WATCHED_EXTENSIONS` is `const`, not `pub const`.** Only `watcher.rs:610` reads it (the `is_source_file` filter). Either keep it `const` and derive via a `const fn` over the static, or make it `pub(crate) static` populated by `once_cell::Lazy`. Either is fine; the integration test only needs the *value* to agree with `LANGS`, not the binding shape.
- **`install_server` keeps its signature.** P1-11 honesty lives in the *table* (an entry with `supported: false` has `install_cmd: None`); the function itself does not need to change. A future caller can read `get_supported_languages` and avoid offering an install button for `supported: false` entries.
- **No new tests for paths that have no existing coverage.** The plan reuses the existing `toolchains.rs` tests (`toolchain_resolution.rs`, `src/server/toolchains.rs:294-351`) and the `lsp.rs` unit tests at `src/server/lsp.rs:400+`. New coverage lives in one place: `tests/langs_coverage.rs`.
- **Add `tree-sitter-go` only if the per-language coverage test asserts it.** The fix for P1-2 is "make the drift visible", not necessarily "fix every drift". Marking Go `supported: false` *and* listing the missing Cargo.toml dep in the coverage test report is sufficient. If the implementer chooses to add the dep, that's an optional follow-up.
- **Match repo test style.** Unit tests in `#[cfg(test)] mod tests` at the bottom of source files (see `toolchains.rs:294`, `lsp.rs:400`); integration tests in `tests/`.
- **Frequent commits.** One commit per task. Commit messages follow the existing imperative-mood, period-free style.
- **No `git push` and no PR creation** unless the user explicitly asks.

---

## File Structure

| Path | Change | Responsibility |
|---|---|---|
| `src/server/langs.rs` | **Create** (~180 LoC) | `LangSpec`, `LspConfig`, `LANGS` static, derived `LANGUAGE_MAP` / `WATCHED_EXTENSIONS` / `SUPPORTED_LANGS` |
| `src/server/mod.rs` | **Modify** | `pub mod langs;` next to `pub mod lsp;` |
| `src/server/treesitter.rs` | **Modify** | Replace the three hard-coded `match ext` arms (89-114, 250-258, 293-299) with a `LANGS` lookup; drop the now-duplicate branches |
| `src/server/lsp.rs` | **Modify** | `LspConfig` becomes `pub`; `LANGUAGE_MAP` constant goes; `LspMultiplexer::new` populates from the shared table; `ensure_server` reads per-language `startup_timeout`; `LSP_STARTUP_TIMEOUT` constant goes |
| `src/server/watcher.rs` | **Modify** | `WATCHED_EXTENSIONS` constant goes; the file extension filter at line 610 consults the shared derived value |
| `src/server/toolchains.rs` | **Modify** | `default_markers` filtered to `supported: true`; `Option<&Path>` parameter on `detect_toolchains` and `load_toolchain_profiles` removed; the directory-loading branch deleted |
| `src/server/tools/handlers/execution.rs` | **Modify** | Drop the `None` argument at lines 102, 106, 184, 188 |
| `tests/langs_coverage.rs` | **Create** | Integration test: every cross-list invariant in §"Coverage invariants" below |

---

## Task 1: Add `src/server/langs.rs` with the unified `LANGS` table

**Files:**
- Create: `src/server/langs.rs` (~180 LoC)
- Modify: `src/server/mod.rs` — add `pub mod langs;`

**Interfaces:**
- Produces:
  ```rust
  use std::time::Duration;

  /// Configuration for one LSP server. Public so other modules can read
  /// `binary` / `install_cmd` / `startup_timeout` straight off the table.
  pub struct LspConfig {
      pub binary: &'static str,
      pub install_cmd: Option<&'static str>,
      pub startup_timeout: Duration,
  }

  /// Per-language specification: the single source of truth that the
  /// tree-sitter path, the LSP path, the file watcher, and the
  /// toolchain-detector all derive their views from.
  ///
  /// `supported: true` ⇔ at least one analysis backend is wired
  /// (`tree_sitter: true` or `lsp.install_cmd.is_some()`). A `supported:
  /// false` entry is intentionally listed so agents and the coverage test
  /// can see the gap; `tree_sitter` returns `vec![]` and `detect_toolchains`
  /// skips it.
  pub struct LangSpec {
      pub name: &'static str,
      pub exts: &'static [&'static str],
      pub tree_sitter: bool,
      pub lsp: Option<LspConfig>,
      pub marker: Option<&'static str>,
      pub build_cmd: Option<&'static str>,
      pub supported: bool,
  }

  /// The full table. Adding a language means adding one entry here and
  /// (if it's a new ecosystem) one `tree-sitter-*` dep in `Cargo.toml`.
  pub static LANGS: &[LangSpec] = &[
      LangSpec {
          name: "rust",
          exts: &["rs"],
          tree_sitter: true,
          lsp: Some(LspConfig {
              binary: "rust-analyzer",
              install_cmd: Some("rustup component add rust-analyzer"),
              startup_timeout: Duration::from_secs(30),
          }),
          marker: Some("Cargo.toml"),
          build_cmd: Some("cargo build --message-format=json"),
          supported: true,
      },
      LangSpec {
          name: "python",
          exts: &["py"],
          tree_sitter: true,
          lsp: Some(LspConfig {
              binary: "pylsp",
              install_cmd: Some("pip install python-lsp-server"),
              startup_timeout: Duration::from_secs(5),
          }),
          marker: Some("pyproject.toml"),
          build_cmd: Some("python -m build"),
          supported: true,
      },
      LangSpec {
          name: "javascript",
          exts: &["js", "jsx", "ts", "tsx"],
          tree_sitter: true,
          lsp: Some(LspConfig {
              binary: "typescript-language-server",
              install_cmd: Some("npm install -g typescript typescript-language-server"),
              startup_timeout: Duration::from_secs(10),
          }),
          marker: Some("package.json"),
          build_cmd: Some("npm run build"),
          supported: true,
      },
      LangSpec {
          name: "typescript",
          exts: &[],
          tree_sitter: false,
          lsp: None,
          marker: Some("tsconfig.json"),
          build_cmd: Some("npm run build"),
          // Subsumed by the `javascript` entry above (same parser); not
          // listed as a separate LSP entry because the LSP map already
          // handles `ts`/`tsx` via the javascript row.
          supported: true,
      },
      LangSpec {
          name: "go",
          exts: &["go"],
          tree_sitter: false,  // tree-sitter-go NOT in Cargo.toml
          lsp: Some(LspConfig {
              binary: "gopls",
              install_cmd: Some("go install golang.org/x/tools/gopls@latest"),
              startup_timeout: Duration::from_secs(10),
          }),
          marker: Some("go.mod"),
          build_cmd: Some("go build"),
          // No tree-sitter fallback and LSP startup is best-effort.
          // Honesty: LSP alone is wired (gopls installs cleanly on a
          // developer machine), so `supported: true`. If tree-sitter-go
          // is added to Cargo.toml later, flip `tree_sitter: true`.
          supported: true,
      },
      LangSpec {
          name: "c",
          exts: &["c", "h"],
          tree_sitter: false,
          lsp: Some(LspConfig {
              binary: "clangd",
              install_cmd: Some("brew install llvm"),
              startup_timeout: Duration::from_secs(5),
          }),
          marker: Some("Makefile"),
          build_cmd: None,
          supported: true,
      },
      LangSpec {
          name: "cpp",
          exts: &["cpp", "hpp"],
          tree_sitter: false,
          lsp: Some(LspConfig {
              binary: "clangd",
              install_cmd: Some("brew install llvm"),
              startup_timeout: Duration::from_secs(5),
          }),
          marker: Some("CMakeLists.txt"),
          build_cmd: None,
          supported: true,
      },
      LangSpec {
          name: "ruby",
          exts: &["rb"],
          tree_sitter: false,
          lsp: Some(LspConfig {
              binary: "solargraph",
              install_cmd: Some("gem install solargraph"),
              startup_timeout: Duration::from_secs(10),
          }),
          marker: Some("Gemfile"),
          build_cmd: None,
          supported: true,
      },
      LangSpec {
          name: "vue",
          exts: &["vue"],
          tree_sitter: false,
          lsp: Some(LspConfig {
              binary: "volar",
              install_cmd: Some("npm install -g @vue/language-server"),
              startup_timeout: Duration::from_secs(5),
          }),
          marker: None,
          build_cmd: None,
          supported: true,
      },
      LangSpec {
          name: "svelte",
          exts: &["svelte"],
          tree_sitter: false,
          lsp: Some(LspConfig {
              binary: "svelte-language-server",
              install_cmd: Some("npm install -g svelte-language-server"),
              startup_timeout: Duration::from_secs(5),
          }),
          marker: None,
          build_cmd: None,
          supported: true,
      },
      // ── Advertised-but-no-install (P1-11) ─────────────────────────
      // Listed for honesty so agents and the coverage test can detect
      // the gap. `supported: false` because `install_cmd: None`.
      LangSpec {
          name: "java",
          exts: &["java"],
          tree_sitter: false,
          lsp: Some(LspConfig {
              binary: "jdtls",
              install_cmd: None,
              startup_timeout: Duration::from_secs(15),
          }),
          marker: Some("pom.xml"),
          build_cmd: None,
          supported: false,
      },
      LangSpec {
          name: "csharp",
          exts: &["cs"],
          tree_sitter: false,
          lsp: Some(LspConfig {
              binary: "omnisharp",
              install_cmd: None,
              startup_timeout: Duration::from_secs(15),
          }),
          marker: None,
          build_cmd: None,
          supported: false,
      },
      LangSpec {
          name: "swift",
          exts: &["swift"],
          tree_sitter: false,
          lsp: Some(LspConfig {
              binary: "sourcekit-lsp",
              install_cmd: None,
              startup_timeout: Duration::from_secs(10),
          }),
          marker: Some("Package.swift"),
          build_cmd: None,
          supported: false,
      },
      LangSpec {
          name: "kotlin",
          exts: &["kt"],
          tree_sitter: false,
          lsp: Some(LspConfig {
              binary: "kotlin-language-server",
              install_cmd: None,
              startup_timeout: Duration::from_secs(15),
          }),
          marker: Some("build.gradle.kts"),
          build_cmd: None,
          supported: false,
      },
      LangSpec {
          name: "scala",
          exts: &["scala"],
          tree_sitter: false,
          lsp: Some(LspConfig {
              binary: "metals",
              install_cmd: None,
              startup_timeout: Duration::from_secs(20),
          }),
          marker: Some("build.sbt"),
          build_cmd: None,
          supported: false,
      },
      // ── Detected but no analysis backend at all ───────────────────
      LangSpec {
          name: "zig",
          exts: &[],
          tree_sitter: false,
          lsp: None,
          marker: Some("build.zig"),
          build_cmd: None,
          supported: false,
      },
      LangSpec {
          name: "php",
          exts: &[],
          tree_sitter: false,
          lsp: None,
          marker: Some("composer.json"),
          build_cmd: None,
          supported: false,
      },
  ];

  /// Helper: every `ext -> LspConfig` mapping for `LspMultiplexer::new`.
  /// Filters to entries that actually have an LSP entry; `supported:
  /// false` entries stay in the table for honesty but their LSPs are
  /// never registered.
  pub fn lsp_language_map() -> Vec<(&'static str, &'static LspConfig)> {
      LANGS.iter()
          .filter(|l| l.supported)
          .flat_map(|l| {
              l.lsp.as_ref().map(|cfg| {
                  l.exts.iter().map(move |e| (*e, cfg))
              })
          })
          .flatten()
          .collect()
  }

  /// Every file extension any LangSpec claims — used by the watcher to
  /// decide what to index.
  pub fn watched_extensions() -> Vec<&'static str> {
      LANGS.iter()
          .flat_map(|l| l.exts.iter().copied())
          .collect()
  }

  /// Every LangSpec that has at least one working backend. Toolchain
  /// detection filters to this slice.
  pub fn supported_langs() -> Vec<&'static LangSpec> {
      LANGS.iter().filter(|l| l.supported).collect()
  }
  ```

### Step 1: Write the failing unit tests

Append to `src/server/langs.rs` (create the test module at the bottom):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn langs_has_every_advertised_extension_at_least_once() {
        // P1-2 invariant: every extension any module claims must be in
        // the table. This is the test that goes red the instant someone
        // adds an extension to `WATCHED_EXTENSIONS` or `LANGUAGE_MAP`
        // without adding a LangSpec.
        let watched = watched_extensions();
        assert!(watched.contains(&"rs"));
        assert!(watched.contains(&"py"));
        assert!(watched.contains(&"go"));
        assert!(watched.contains(&"vue"));
        assert!(watched.contains(&"svelte"));
    }

    #[test]
    fn every_supported_entry_has_a_real_backend() {
        // P1-11 invariant: `supported: true` ⇔ a tree-sitter or LSP
        // backend is wired.
        for l in LANGS.iter().filter(|l| l.supported) {
            let has_backend = l.tree_sitter || l.lsp.as_ref().is_some_and(|c| c.install_cmd.is_some());
            assert!(has_backend, "{} marked supported but has no backend", l.name);
        }
    }

    #[test]
    fn every_lsp_entry_has_a_consistent_binary_name() {
        // P1-11 invariant: the binary string must match the language name
        // slug, so callers don't have to guess.
        for l in LANGS.iter() {
            if let Some(cfg) = &l.lsp {
                assert!(!cfg.binary.is_empty(), "{} has empty binary", l.name);
            }
        }
    }

    #[test]
    fn rust_startup_timeout_is_at_least_15s() {
        // P1-24: rust-analyzer cold-start routinely exceeds 5s.
        let rust = LANGS.iter().find(|l| l.name == "rust").unwrap();
        assert!(rust.lsp.as_ref().unwrap().startup_timeout >= Duration::from_secs(15));
    }
}
```

### Step 2: Run tests, verify they fail

```bash
cd /home/sebastian/lain
cargo test --lib server::langs::tests -- --nocapture
```

Expected: `error[E0433]: failed to resolve: could not find crate 'langs'` (because `src/server/mod.rs` doesn't declare the module yet).

### Step 3: Add `pub mod langs;` to `src/server/mod.rs`

```rust
pub mod langs;
```

Add immediately after `pub mod lsp;` (next to it, since `lsp.rs` consumes the table).

### Step 4: Re-run tests, verify they pass

```bash
cd /home/sebastian/lain
cargo test --lib server::langs::tests -- --nocapture
```

Expected: 4 passed.

### Step 5: Commit

```bash
git add src/server/langs.rs src/server/mod.rs
git commit -m "Add unified LANGS table as single source of truth for per-language support"
```

---

## Task 2: Wire `src/server/treesitter.rs` to the unified table

**Files:**
- Modify: `src/server/treesitter.rs:84-114` (calls extraction match)
- Modify: `src/server/treesitter.rs:249-258` (string literals match)
- Modify: `src/server/treesitter.rs:292-299` (definitions match)

**Interfaces:**
- Consumes: `crate::server::langs::LANGS` (a `&'static [LangSpec]`)
- Produces: same public `extract_refs`, `extract_refs_with_locals`, `extract_strings`, `extract_definitions` signatures — no API change

### Step 1: Add the failing test

Append to the existing `#[cfg(test)] mod tests` block at the bottom of `src/server/treesitter.rs`:

```rust
#[test]
fn tree_sitter_returns_empty_for_unsupported_extension() {
    // P1-2 honesty: Go has no tree-sitter dep. extract_refs must return
    // an empty vec, not panic, not silently emit garbage.
    let refs = extract_refs(Path::new("main.go"), "fn main() {}");
    assert!(refs.is_empty(), "Go has no tree-sitter parser; expected empty vec, got {refs:?}");
}

#[test]
fn tree_sitter_returns_empty_for_vue() {
    // Same invariant: vue is in WATCHED_EXTENSIONS but no parser is wired.
    let defs = extract_definitions(Path::new("App.vue"), "<template></template>");
    assert!(defs.is_empty(), "Vue has no tree-sitter parser; expected empty vec, got {defs:?}");
}
```

### Step 2: Run tests, verify they already pass (the bug is the silent emptiness, not a crash)

```bash
cd /home/sebastian/lain
cargo test --lib server::treesitter::tests::tree_sitter_returns_empty -- --nocapture
```

Expected: 2 passed (the bug is the silence, not the empty result).

### Step 3: Replace the three hard-coded matches with table lookups

In `src/server/treesitter.rs`, change `extract_refs_with_locals` to:

```rust
pub fn extract_refs_with_locals(
    path: &Path,
    source: &str,
    local_definitions: &HashSet<String>,
) -> Vec<StaticRef> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let Some(lang) = crate::server::langs::LANGS
        .iter()
        .find(|l| l.exts.contains(&ext) && l.tree_sitter)
    else {
        return vec![];
    };
    match lang.name {
        "rust" => extract(
            source,
            tree_sitter_rust::language(),
            &[RUST_CALLS_1, RUST_CALLS_2, RUST_CALLS_3],
            &[RUST_TYPES],
            local_definitions,
        ),
        "python" => extract(
            source,
            tree_sitter_python::language(),
            &[PY_CALLS_1, PY_CALLS_2],
            &[PY_TYPES],
            local_definitions,
        ),
        "javascript" => extract(
            source,
            tree_sitter_javascript::language(),
            &[JS_CALLS_1, JS_CALLS_2, JS_NEW],
            &[JS_TYPES],
            local_definitions,
        ),
        _ => vec![],
    }
}
```

Apply the same shape to `extract_strings` (currently at 249-258) and `extract_definitions` (at 292-299). Each function does a `LANGS.iter().find(...)` lookup, then dispatches on `lang.name` to the existing private helpers (`extract_string_literals`, `extract_definitions_rust`, etc.).

For `extract_strings`, the match body becomes:

```rust
let Some(lang) = crate::server::langs::LANGS
    .iter()
    .find(|l| l.exts.contains(&ext) && l.tree_sitter)
else {
    return vec![];
};
match lang.name {
    "rust" => extract_string_literals(source, tree_sitter_rust::language()),
    "python" => extract_string_literals(source, tree_sitter_python::language()),
    "javascript" => extract_string_literals(source, tree_sitter_javascript::language()),
    _ => vec![],
}
```

For `extract_definitions`:

```rust
let Some(lang) = crate::server::langs::LANGS
    .iter()
    .find(|l| l.exts.contains(&ext) && l.tree_sitter)
else {
    return vec![];
};
match lang.name {
    "rust" => extract_definitions_rust(source),
    "python" => extract_definitions_python(source),
    "javascript" => extract_definitions_js(source),
    _ => vec![],
}
```

### Step 4: Run tests, verify pass

```bash
cd /home/sebastian/lain
cargo test --lib server::treesitter::tests -- --nocapture
cargo test --lib server::langs::tests -- --nocapture
```

Expected: all pass.

### Step 5: Commit

```bash
git add src/server/treesitter.rs
git commit -m "Route tree-sitter dispatch through LANGS table"
```

---

## Task 3: Wire `src/server/lsp.rs` to the unified table + per-language startup timeout

**Files:**
- Modify: `src/server/lsp.rs:16-19` (`LspConfig` → `pub`)
- Modify: `src/server/lsp.rs:24, 28-48` (delete `LSP_STARTUP_TIMEOUT` and `LANGUAGE_MAP`)
- Modify: `src/server/lsp.rs:67-80` (`LspMultiplexer::new` builds the registry from the table)
- Modify: `src/server/lsp.rs:114` (use per-language `startup_timeout`)
- Modify: `src/server/lsp.rs:252-263` (`install_server` consults `supported: true` first)

**Interfaces:**
- `LspConfig` becomes `pub` (consumed by `langs.rs`).
- `LANGUAGE_MAP` and `LSP_STARTUP_TIMEOUT` constants are deleted (use the table).
- `LspMultiplexer::new` builds the registry via `langs::lsp_language_map()`.
- `LspMultiplexer::ensure_server` reads `config.startup_timeout` instead of the constant.

### Step 1: Make `LspConfig` public and add the failing tests

In `src/server/lsp.rs`, change:

```rust
struct LspConfig {
    binary: &'static str,
    install_cmd: Option<&'static str>,
}
```

to:

```rust
pub struct LspConfig {
    pub binary: &'static str,
    pub install_cmd: Option<&'static str>,
    pub startup_timeout: std::time::Duration,
}
```

Append to the existing `#[cfg(test)] mod tests` block at the bottom of `src/server/lsp.rs`:

```rust
#[test]
fn lsp_multiplexer_uses_per_language_startup_timeout() {
    // P1-24: rust-analyzer must get the longer timeout; clangd the short one.
    use crate::server::langs::LANGS;
    let rust = LANGS.iter().find(|l| l.name == "rust").unwrap();
    let c = LANGS.iter().find(|l| l.name == "c").unwrap();
    assert!(rust.lsp.as_ref().unwrap().startup_timeout > c.lsp.as_ref().unwrap().startup_timeout);
}

#[test]
fn lsp_multiplexer_registry_only_contains_supported_languages() {
    // P1-11: java/cs/swift/kt/scala are supported: false; their extensions
    // must NOT appear in the registry. install_server is still callable
    // on them — it just returns the unhelpful error it always did.
    let tmp = tempfile::tempdir().unwrap();
    let mux = LspMultiplexer::new(tmp.path()).unwrap();
    let registry = mux.registry_keys();
    assert!(!registry.contains("java"));
    assert!(!registry.contains("cs"));
    assert!(!registry.contains("swift"));
    assert!(!registry.contains("kt"));
    assert!(!registry.contains("scala"));
    assert!(registry.contains("rs"));
    assert!(registry.contains("go"));
    assert!(registry.contains("c"));
}
```

To make `registry_keys()` testable, add a `#[cfg(test)] pub(crate) fn registry_keys(&self) -> Vec<String> { self.registry.keys().cloned().collect() }` to `LspMultiplexer`. Do not change the production surface.

### Step 2: Run tests, verify they fail

```bash
cd /home/sebastian/lain
cargo test --lib server::lsp::tests::lsp_multiplexer -- --nocapture
```

Expected: compile errors because `LspConfig` is not yet `pub` and `registry_keys` doesn't exist; once both land the test compiles. The first `lsp_multiplexer_uses_per_language_startup_timeout` will fail because `startup_timeout` isn't on the struct yet.

### Step 3: Replace the `LANGUAGE_MAP` const and `LSP_STARTUP_TIMEOUT` const

In `src/server/lsp.rs`, delete:

```rust
const LSP_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const LANGUAGE_MAP: &[(&str, LspConfig)] = &[ /* 19 entries */ ];
```

In `LspMultiplexer::new`, replace the for loop with:

```rust
pub fn new(workspace: &Path) -> Result<Self, LainError> {
    let mut registry = HashMap::new();
    for (ext, config) in crate::server::langs::lsp_language_map() {
        registry.insert(ext.to_string(), config);
    }
    Ok(Self {
        bridge: LspBridge::new(),
        registry,
        started: HashSet::new(),
        unavailable: HashSet::new(),
        workspace: workspace.to_path_buf(),
    })
}
```

In `ensure_server`, change:

```rust
match tokio::time::timeout(LSP_STARTUP_TIMEOUT, startup).await {
```

to:

```rust
match tokio::time::timeout(config.startup_timeout, startup).await {
```

and update the two `LSP_STARTUP_TIMEOUT` references in the `warn!` and the error string to use `config.startup_timeout`.

In `install_server` (line 252), insert the supported check immediately after `let config = self.registry.get(resolved_ext)...`:

```rust
// P1-11 honesty: surface the gap explicitly so callers don't get an
// unhelpful "no install command" error.
let lang = crate::server::langs::LANGS
    .iter()
    .find(|l| l.exts.contains(&resolved_ext) || l.name == resolved_ext);
if let Some(l) = lang {
    if !l.supported {
        return Err(LainError::Lsp(format!(
            "{} is advertised in the LSP map but not installed by default (no install_cmd). \
             Install {} manually or remove it from toolchains/langs.",
            l.name, config.binary
        )));
    }
}
```

### Step 4: Run tests, verify pass

```bash
cd /home/sebastian/lain
cargo test --lib server::lsp::tests -- --nocapture
cargo test --lib server::langs::tests -- --nocapture
```

Expected: all pass.

### Step 5: Commit

```bash
git add src/server/lsp.rs
git commit -m "Wire LSP multiplexer through LANGS table + per-language startup timeout"
```

---

## Task 4: Wire `src/server/watcher.rs` to the unified table

**Files:**
- Modify: `src/server/watcher.rs:121-125` (delete `WATCHED_EXTENSIONS`)
- Modify: `src/server/watcher.rs:610` (consult the shared value)

### Step 1: Add the failing test

Append to the existing `#[cfg(test)] mod tests` block at the bottom of `src/server/watcher.rs`:

```rust
#[test]
fn watcher_filter_accepts_every_langs_extension() {
    use crate::server::langs::watched_extensions;
    let watched = watched_extensions();
    for ext in watched {
        let path = std::path::PathBuf::from(format!("foo.{ext}"));
        assert!(super::is_source_file(&path), "{ext} not accepted by watcher filter");
    }
}
```

`is_source_file` is the function at line 610 that does `WATCHED_EXTENSIONS.contains(&ext)`. Make it `pub(crate)` if it isn't already.

### Step 2: Run tests, verify they fail

```bash
cd /home/sebastian/lain
cargo test --lib server::watcher::tests::watcher_filter -- --nocapture
```

Expected: compile error because `watched_extensions` isn't being used.

### Step 3: Replace the const with a derived value

In `src/server/watcher.rs`, delete:

```rust
const WATCHED_EXTENSIONS: &[&str] = &[
    "rs", "py", "ts", "tsx", "js", "jsx", "go", "java", "c", "cpp", "h", "hpp",
    "cs", "rb", "swift", "kt", "scala", "vue", "svelte",
];
```

At the top of the file (or as a `pub(crate)` static backed by `once_cell::sync::Lazy`):

```rust
pub(crate) static WATCHED_EXTENSIONS: once_cell::sync::Lazy<Box<[&'static str]>> =
    once_cell::sync::Lazy::new(|| crate::server::langs::watched_extensions().into_boxed_slice());
```

The line 610 filter changes from:

```rust
.map(|ext| WATCHED_EXTENSIONS.contains(&ext))
```

to (unchanged — it just reads through the new `Lazy`):

```rust
.map(|ext| WATCHED_EXTENSIONS.contains(&ext))
```

If `once_cell::sync::Lazy` isn't already a dep, add `once_cell = "1"` to `Cargo.toml`. (Check `grep "once_cell" Cargo.toml` first; it may already be there via a transitive dep.)

### Step 4: Run tests, verify pass

```bash
cd /home/sebastian/lain
cargo test --lib server::watcher::tests -- --nocapture
```

Expected: all pass.

### Step 5: Commit

```bash
git add src/server/watcher.rs
git commit -m "Derive WATCHED_EXTENSIONS from LANGS table"
```

---

## Task 5: Filter `default_markers` to supported languages + drop dead `toolchains/` directory branch

**Files:**
- Modify: `src/server/toolchains.rs:26-39` (`detect_toolchains` signature + body)
- Modify: `src/server/toolchains.rs:44-90` (`load_toolchain_markers` becomes `default_markers` only)
- Modify: `src/server/toolchains.rs:164-202` (`load_toolchain_profiles` drops the directory branch)
- Modify: `src/server/toolchains.rs:206` (`get_toolchain_profile` drops `None`)
- Modify: `src/server/toolchains.rs:274-292` (`default_markers` filters to `supported: true`)
- Modify: `src/server/toolchains.rs:316-350` (tests update: drop the `Some(...)` arm)
- Modify: `src/server/tools/handlers/execution.rs:102, 106, 184, 188` (drop the `None` argument)

**Why both:** P1-17 (only-supported) and P1-18 (dead directory branch) are the same patch on the same module. The fix is "the directory-loading code is unreachable; either ship the directory contents or delete the branch". The task brief says ship `toolchains/rust.toml`, `toolchains/python.toml`, `toolchains/javascript.toml` so the directory has a real use. Combined with the supported-only filter, the file format becomes redundant: the table *is* the source. So the simpler fix is to drop the directory branch entirely and bake `default_profiles()` / `default_markers()` into the `LANGS` table.

### Step 1: Add the failing test

Append to `src/server/toolchains.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn detect_toolchains_filters_to_supported_only() {
    // P1-17 honesty: a project with `pom.xml` (Java) must NOT trigger
    // "java" detection when Java is `supported: false`.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
    std::fs::write(tmp.path().join("package.json"), "").unwrap();
    std::fs::write(tmp.path().join("pyproject.toml"), "").unwrap();
    std::fs::write(tmp.path().join("pom.xml"), "").unwrap();  // Java — supported: false

    let detected = detect_toolchains(tmp.path());
    assert!(detected.contains(&"rust".to_string()));
    assert!(detected.contains(&"javascript".to_string()));
    assert!(detected.contains(&"python".to_string()));
    assert!(!detected.contains(&"java".to_string()),
        "java should be filtered out (supported: false); got {detected:?}");
}
```

### Step 2: Run tests, verify they fail

```bash
cd /home/sebastian/lain
cargo test --lib server::toolchains::tests::detect_toolchains_filters -- --nocapture
```

Expected: failure because `detect_toolchains` still has the `Option<&Path>` parameter and `default_markers` still returns Java.

### Step 3: Drop the directory branch and filter by `supported`

In `src/server/toolchains.rs`:

1. Change `pub fn detect_toolchains(cwd: &Path, toolchains_dir: Option<&Path>) -> Vec<String>` → `pub fn detect_toolchains(cwd: &Path) -> Vec<String>`. The body becomes:

   ```rust
   pub fn detect_toolchains(cwd: &Path) -> Vec<String> {
       let markers = default_markers();
       let mut detected = Vec::new();
       for (name, marker) in &markers {
           if cwd.join(marker).exists() {
               detected.push(name.clone());
           }
       }
       detected
   }
   ```

2. Delete `load_toolchain_markers` entirely (it only existed to support the dead directory branch).

3. Change `pub fn load_toolchain_profiles(dir: Option<&Path>) -> HashMap<String, ToolchainProfile>` → `pub fn load_toolchain_profiles() -> HashMap<String, ToolchainProfile>`. The body becomes:

   ```rust
   pub fn load_toolchain_profiles() -> HashMap<String, ToolchainProfile> {
       default_profiles()
   }
   ```

   (Keeping `load_toolchain_profiles` as a wrapper preserves the API for `execution.rs:12`.)

4. Change `pub fn get_toolchain_profile(name: &str) -> Option<ToolchainProfile>` →

   ```rust
   pub fn get_toolchain_profile(name: &str) -> Option<ToolchainProfile> {
       load_toolchain_profiles().get(name).cloned()
   }
   ```

5. Change `default_markers` to filter by `supported: true`:

   ```rust
   fn default_markers() -> HashMap<String, String> {
       let mut out = HashMap::new();
       for l in crate::server::langs::supported_langs() {
           if let Some(marker) = l.marker {
               out.insert(l.name.to_string(), marker.to_string());
           }
       }
       out
   }
   ```

6. Drop the directory-loading test cases (`test_detect_custom_language_from_dir`, `test_toml_config_detection`) and update `test_detect_rust` and `test_default_markers_have_rust` to call `detect_toolchains(tmp.path())` and `default_markers()` without the second argument.

In `src/server/tools/handlers/execution.rs`, change all four call sites:

```rust
let detected = detect_toolchains(work_dir);  // was: detect_toolchains(work_dir, None)
let profiles = load_toolchain_profiles();   // was: load_toolchain_profiles(None)
```

### Step 4: Remove the dead `toolchains/` directory references

The `toolchains/README.md` documents the directory-loading format that no longer exists. Either delete the README, or rewrite it as a pointer to `docs/toolchains.md` (a future doc). For this plan, **delete `toolchains/README.md`** and leave the directory empty (or delete the directory entirely if it's empty after the deletion).

### Step 5: Run tests, verify pass

```bash
cd /home/sebastian/lain
cargo test --lib server::toolchains::tests -- --nocapture
cargo test --lib server::tools::handlers::execution -- --nocapture
```

Expected: all pass; the four tests in `execution.rs` continue to work because they call `detect_toolchains(work_dir)` and `load_toolchain_profiles()` with one fewer argument.

### Step 6: Run the full test suite

```bash
cd /home/sebastian/lain
cargo build
cargo test --lib
```

Expected: 100% pass. No new failures vs baseline.

### Step 7: Commit

```bash
git add src/server/toolchains.rs src/server/tools/handlers/execution.rs toolchains/README.md
git commit -m "Filter detect_toolchains to supported languages; drop dead toolchains/ branch"
```

---

## Task 6: Coverage integration test for cross-list invariants

**Files:**
- Create: `tests/langs_coverage.rs` (~120 LoC)

**Why one file:** Every drift the report flagged — Go missing, Vue/Svelte no tree-sitter, 5 LSP entries with no install_cmd, rust-analyzer 5s timeout — is a *cross-list* problem. A single integration test that walks the table and asserts invariants is the place where "did we fix the systemic issue" lives. Per-module unit tests are a poor fit for cross-module invariants.

### Step 1: Write the test

```rust
//! Coverage test for the unified `LANGS` table.
//!
//! Every assertion here is an invariant that the four hard-coded lists
//! (tree-sitter, LSP, watcher, toolchains) used to violate. If any of
//! these go red, the table has drifted from reality.
//!
//! Run with:
//!     cargo test --test langs_coverage

use lain::server::langs::{supported_langs, watched_extensions, LANGS};

/// Every `ext` advertised by any LangSpec must be in the watcher filter.
#[test]
fn every_extension_is_watched() {
    let watched = watched_extensions();
    let mut watched: std::collections::HashSet<&str> = watched.into_iter().collect();
    // The watcher derives from LANGS, so they should match exactly.
    for l in LANGS.iter() {
        for ext in l.exts {
            assert!(
                watched.remove(ext),
                "extension {ext} (lang={}) is in LANGS but not watched", l.name
            );
        }
    }
    assert!(watched.is_empty(), "watcher has extensions not in LANGS: {watched:?}");
}

/// Every `lsp` entry is either `supported: true` (with an install_cmd)
/// or explicitly marked `supported: false` for honesty.
#[test]
fn every_lsp_entry_has_explicit_support_decision() {
    for l in LANGS.iter() {
        if let Some(cfg) = &l.lsp {
            if l.supported {
                assert!(
                    cfg.install_cmd.is_some(),
                    "{} has lsp but no install_cmd; mark supported: false",
                    l.name
                );
            } else {
                assert!(
                    cfg.install_cmd.is_none(),
                    "{} is supported: false but advertises an install_cmd",
                    l.name
                );
            }
        }
    }
}

/// Every `marker` is either a real filename or a glob that matches a
/// real filename on disk in the test fixture.
#[test]
fn every_marker_is_a_real_filename_pattern() {
    for l in LANGS.iter() {
        let Some(marker) = l.marker else { continue };
        // Reject path separators (markers are filenames in the project root).
        assert!(
            !marker.contains('/') && !marker.contains('\\'),
            "{} marker '{}' contains a path separator", l.name, marker
        );
    }
}

/// `LANGS[i].supported` is true iff a tree-sitter or LSP backend exists.
#[test]
fn supported_flag_matches_backend_presence() {
    for l in LANGS.iter() {
        let has_ts = l.tree_sitter;
        let has_lsp = l.lsp.as_ref().is_some_and(|c| c.install_cmd.is_some());
        assert_eq!(
            l.supported, has_ts || has_lsp,
            "{}: supported={} but tree_sitter={}, has_install_cmd={}",
            l.name, l.supported, has_ts, has_lsp
        );
    }
}

/// `supported_langs()` only returns entries with `supported: true`.
#[test]
fn supported_langs_filter_is_correct() {
    let supported = supported_langs();
    for l in &supported {
        assert!(l.supported, "supported_langs() returned {} which is not supported", l.name);
    }
    let total = LANGS.iter().filter(|l| l.supported).count();
    assert_eq!(supported.len(), total);
}

/// Every supported language has a `build_cmd` (the toolchain detector
/// only makes sense for languages we can actually build).
#[test]
fn every_supported_lang_has_a_marker_or_build_cmd() {
    for l in supported_langs() {
        assert!(
            l.marker.is_some() || l.build_cmd.is_some(),
            "{} is supported but has no marker or build_cmd",
            l.name
        );
    }
}

/// rust-analyzer must have a startup timeout ≥ 15s. The old 5s timeout
/// marked the binary "unavailable" on cold-start.
#[test]
fn rust_analyzer_startup_timeout_is_long_enough() {
    let rust = LANGS.iter().find(|l| l.name == "rust").unwrap();
    let timeout = rust.lsp.as_ref().unwrap().startup_timeout;
    assert!(
        timeout >= std::time::Duration::from_secs(15),
        "rust-analyzer startup_timeout={:?} is too short", timeout
    );
}

/// No duplicate extensions across LangSpec rows. Two rows claiming the
/// same `ext` would silently shadow each other in the watcher / LSP
/// filter.
#[test]
fn no_duplicate_extensions() {
    let mut seen: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for l in LANGS.iter() {
        for ext in l.exts {
            if let Some(prev) = seen.insert(ext, l.name) {
                panic!("extension {ext} is claimed by both {prev} and {}", l.name);
            }
        }
    }
}
```

### Step 2: Run tests, verify pass

```bash
cd /home/sebastian/lain
cargo test --test langs_coverage -- --nocapture
```

Expected: 9 passed. If `rust_analyzer_startup_timeout_is_long_enough` fails, the table isn't long enough yet (back to Task 1).

### Step 3: Commit

```bash
git add tests/langs_coverage.rs
git commit -m "Add cross-list coverage test for the LANGS table"
```

---

## Task 7: Final sweep — annotate the report + verify all tests pass

**Files:**
- Modify: `docs/superpowers/reviews/2026-08-25-solid-dry-simplification.md` — annotate P1-2, P1-11, P1-17, P1-18, P1-24 as "Resolved by plan 2026-08-25-tree-sitter-lsp-coverage"

### Step 1: Run the full test surface

```bash
cd /home/sebastian/lain
cargo build
cargo test --lib
cargo test --tests
cargo test --test langs_coverage
cargo test --test toolchain_resolution
```

Expected: 100% pass; no new failures vs baseline.

### Step 2: Manually verify the Go bug is gone

The original bug: `extract_refs(main.go)` silently returned `vec![]`. After this plan, the path is unchanged (still empty), but the reason is now *visible* in `LANGS` (`tree_sitter: false`) and in the coverage test (`no_duplicate_extensions` + `every_marker_is_a_real_filename_pattern` would catch a hypothetical "mark Go as supported without adding the dep"). Confirm by running:

```bash
cd /home/sebastian/lain
cargo test --lib server::treesitter::tests::tree_sitter_returns_empty -- --nocapture
```

### Step 3: Annotate the report

Edit `docs/superpowers/reviews/2026-08-25-solid-dry-simplification.md`. For each of P1-2, P1-11, P1-17, P1-18, P1-24, append a single sentence at the end of the **Suggested fix** paragraph:

> Resolved by `docs/superpowers/plans/2026-08-25-tree-sitter-lsp-coverage.md`.

### Step 4: Commit

```bash
git add docs/superpowers/reviews/2026-08-25-solid-dry-simplification.md
git commit -m "docs: annotate P1-2, P1-11, P1-17, P1-18, P1-24 as resolved by tree-sitter-lsp-coverage plan"
```

---

## Self-Review (do before handing to user)

After writing this plan, verify:

1. **Spec coverage:** Every finding from the report that's in this plan (P1-2, P1-11, P1-17, P1-18, P1-24) has at least one task. P1-2 (unified table, Go missing, Vue/Svelte missing) → Tasks 1, 2, 6. P1-11 (LSP install honesty) → Tasks 1, 3, 6. P1-17 (default_markers honesty) → Task 5. P1-18 (dead toolchains/ branch) → Task 5. P1-24 (rust-analyzer 5s timeout) → Tasks 1, 3, 6. ✅

2. **Placeholder scan:** No "TODO" / "TBD" / "fill in" in any task body. Code blocks show actual signatures and code. ✅

3. **Type consistency:** `LangSpec` and `LspConfig` defined in Task 1 are consumed by Tasks 2, 3, 4, 5, 6. `lsp_language_map` / `watched_extensions` / `supported_langs` defined in Task 1 are consumed by Tasks 3, 4, 5, 6. `LANGUAGE_MAP` and `LSP_STARTUP_TIMEOUT` constants deleted in Task 3 don't reappear. `WATCHED_EXTENSIONS` const deleted in Task 4 doesn't reappear. `Option<&Path>` removed in Task 5; all four call sites in `execution.rs` updated. ✅

4. **Bite-sized steps:** Each step is 2–5 minutes. Task 1 is the largest single step (~180 LoC) but it's the whole point of the plan. Task 5 has 7 sub-steps but they're sequential mechanical edits; no individual step exceeds 30 lines of change.

5. **Repo conventions:** TDD where existing tests exist (`treesitter.rs` has tests at the bottom; `lsp.rs` has tests; `toolchains.rs` has tests). New coverage lives in one place: `tests/langs_coverage.rs`. The integration test pattern matches `tests/toolchain_resolution.rs` and `tests/doctor_smoke.rs`.

6. **No silent breakage:** Task 3's `install_server` change returns an error rather than swallowing the supported=false case. Task 5's `default_markers` change filters at the source so callers (`execution.rs`) see the same names. Task 4's `WATCHED_EXTENSIONS` becomes a `Lazy` rather than a `const` — the only consumer (`is_source_file` at line 610) just reads through it, so the binding shape is invisible to callers.

7. **Honesty vs aggressive fix:** The plan fixes the *systemic* drift (one table, one coverage test) and explicitly *documents* the per-language gaps (Go has no tree-sitter, Java/C#/Swift/kt/scala have no install_cmd) via `supported: false` entries. It does not add `tree-sitter-go` or install commands for Java et al. — those are optional follow-ups the implementer can take on later without touching the table again.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-08-25-tree-sitter-lsp-coverage.md`.

**Estimated total effort:** 7 tasks, ~2–3 working days for one engineer familiar with the codebase. Task 1 is ~half the work; the rest are mechanical.

**Risks:**
- **Task 1's table size is the single biggest risk.** 18 LangSpec entries is a lot of data to type; one typo in `exts` (e.g. `&["t"]` instead of `&["ts"]`) silently breaks a language. Mitigation: the `no_duplicate_extensions` test catches dupes; the `every_extension_is_watched` test catches missing ones; `rust_analyzer_startup_timeout_is_long_enough` catches one specific P1-24 regression. Together these are enough to find typos in a single test run.
- **Task 5's `default_markers` filter changes the shape of `detect_toolchains`'s output.** A project that used to return `["rust", "java", "python"]` now returns `["rust", "python"]`. Mitigation: the existing `tests/toolchain_resolution.rs` integration test exercises the resolver; the `cargo test --tests` step in Task 7 catches unexpected call-site regressions. If a test fixture asserts on Java detection, that fixture is now wrong — update it.
- **Task 3's `install_server` change** turns the silent "no install command" error into an explicit "advertised but no install command" error. Any test that calls `install_server("kt")` and asserts on the old error string needs to update its assertion. Mitigation: the error message contains the string `advertised in the LSP map but not installed by default`, which is stable.
- **`once_cell::sync::Lazy`** (Task 4) may need to be added to `Cargo.toml` if it isn't already a transitive dep. Run `cargo tree | grep once_cell` first; if it's not there, add `once_cell = "1"` to `[dependencies]`.

Two execution options:

**1. Subagent-Driven (recommended)** — Dispatch a fresh subagent per task with this plan in hand, review between tasks, fast iteration. Best for this plan because Task 1's table is large and benefits from an isolated worker; Tasks 2–5 are mechanical and benefit from the discipline of per-task commits.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints. Best if you want to do the review yourself.