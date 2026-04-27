# Toolchain Multiplexer: LSP Diagnostics vs Build Output

## Status: IMPLEMENTED (Phase 1)

The core types and detection logic are now implemented in `src/tools/handlers/decoration.rs`.

## The Two Sources of Errors

### 1. LSP Diagnostics (Editor-Time)
LSP servers emit `textDocument/publishDiagnostics` notifications containing:
```json
{ range, severity, code, source, message }
```

This is the **same shape as `ParsedError`** — language-agnostic and structured.

**The limitation:** LSP diagnostics are editor-time, type-checking errors only. They do NOT give you:
- Test failures (vitest, jest, cargo test)
- Build failures from the actual compiler
- Lint warnings from standalone linters (ruff, eslint)

### 2. Build/Test/Lint Output (Command-Time)
Running `cargo build`, `npm test`, `go build` gives full output including:
- Compile errors with full context
- Test failures with stack traces
- Integration test output

## The Toolchain Multiplexer Model

```
Command output → Parser → Enricher → Report
```

Where `Parser` is the **protocol layer**:

```rust
pub trait ErrorParser: Send + Sync {
    fn parse(&self, output: &str) -> Vec<ParsedError>;
}
```

## Implementation: Top 10 TIOBE Index Support

The `Toolchain` enum now covers the top 10 TIOBE Index languages plus additional JVM/web languages:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toolchain {
    // Top 10 TIOBE
    Python,   // pyproject.toml, setup.py, requirements.txt
    C,        // Makefile, CMakeLists.txt
    Cpp,      // CMakeLists.txt, Makefile
    Java,     // pom.xml, build.gradle
    CSharp,   // .csproj, .sln
    JavaScript, // package.json (Node/JS)
    Go,       // go.mod
    Rust,     // Cargo.toml
    Ruby,     // Gemfile, Rakefile
    Php,      // composer.json
    // Additional
    TypeScript, // tsconfig.json, package.json with typescript
    Swift,    // Package.swift
    Kotlin,   // build.gradle.kts
    Scala,    // build.sbt
}
```

## Project Detection

```rust
pub fn detect_project_profile(cwd: &Path) -> ProjectProfile {
    // Detect by file presence
    // Cargo.toml → Rust
    // package.json → JavaScript or TypeScript (if tsconfig.json exists)
    // go.mod → Go
    // pyproject.toml / setup.py / requirements.txt → Python
    // pom.xml / build.gradle → Java
    // *.csproj / *.sln → C#
    // Gemfile / Rakefile → Ruby
    // composer.json → PHP
    // CMakeLists.txt → C++
    // Makefile → C
    // Package.swift → Swift
    // build.gradle.kts (no pom.xml) → Kotlin
}
```

## The Honest API Design

### Problem: Conflating Detection with Execution

The generic `run_build(cwd)` API is ambiguous in polyglot repos.

### Solution: Parameterized Toolchain Selection

```rust
async fn run_build(
    cwd: &Path,
    toolchain: Option<&str>,  // "rust", "python", "java", etc.
) -> Result<String> {
    let profile = detect_project_profile(cwd)?;
    let tc = profile.resolve(toolchain)
        .ok_or_else(|| LainError::UnrecognizedToolchain)?;

    match tc {
        Toolchain::Rust => run_cargo_build(cwd).await,
        Toolchain::Python => run_python_build(cwd).await,
        Toolchain::Java => run_maven_build(cwd).await,
        Toolchain::Go => run_go_build(cwd).await,
        Toolchain::JavaScript | Toolchain::TypeScript => run_npm_build(cwd).await,
        // ...
    }
}
```

## Toolchain Detection Table

| Language | Detection Files | Test Runner | Build Tool | Parser |
|----------|----------------|-------------|------------|--------|
| Rust | `Cargo.toml` | N/A | cargo | CargoJsonParser |
| Python | `pyproject.toml`, `setup.py`, `requirements.txt` | pytest | pip/poetry | PythonTextParser (TODO) |
| Java | `pom.xml`, `build.gradle` | N/A | Maven/Gradle | JavaTextParser (TODO) |
| Go | `go.mod` | N/A | go | GoTextParser (TODO) |
| JavaScript | `package.json` | jest/vitest/mocha | npm | JestJsonParser (TODO) |
| TypeScript | `package.json` + `tsconfig.json` | jest/vitest | npm | JestJsonParser (TODO) |
| C# | `*.csproj`, `*.sln` | N/A | dotnet | DotNetTextParser (TODO) |
| Ruby | `Gemfile`, `Rakefile` | rspec | rake/bundler | RubyTextParser (TODO) |
| PHP | `composer.json` | phpunit | composer | PhpTextParser (TODO) |
| C/C++ | `CMakeLists.txt`, `Makefile` | N/A | make/cmake | MakeTextParser (TODO) |
| Swift | `Package.swift` | swift test | swift build | SwiftTextParser (TODO) |

## Polyglot Handling

When `toolchain=None` (auto-detect) and multiple toolchains exist:

1. **Single toolchain** → Use that toolchain
2. **Multiple toolchains + explicit** → Use explicit toolchain
3. **Multiple toolchains + no explicit** → Use `profile.primary` (priority heuristic)
4. **No matching toolchain** → Return error listing available toolchains

### Priority Heuristic

When auto-detecting in a polyglot repo, priority order is:
```
Rust > Go > Python > JavaScript/TypeScript > Java > C# > Ruby > PHP > C/C++
```

This reflects the likelihood of being the "primary" project in a monorepo.

## Parsing Strategy by Confidence

### High Confidence (Structured Output)
- Rust: `--message-format=json` ✅ DONE
- JavaScript/TypeScript: `jest --json` (TODO)
- Python: `pytest --json` (TODO)

### Medium Confidence (Text with known format)
- Go: `go build` / `go test` (regex-based) (TODO)
- Java: Maven/Gradle output (regex-based) (TODO)

### Low Confidence (Unstructured Text)
- C/C++: Make output (brittle regex) (TODO)
- Ruby: Rake/Bundler output (TODO)
- PHP: Composer output (TODO)

## Tests

```bash
$ cargo test --test decoration_tests
running 25 tests
  ...
test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

New tests added:
- `test_toolchain_name` - verifies all 14 toolchain names
- `test_project_profile_single_toolchain` - single Rust project
- `test_project_profile_multiple_toolchains` - Rust + JavaScript
- `test_project_profile_empty` - no toolchains
- `test_project_profile_resolve_explicit` - explicit override
- `test_project_profile_resolve_auto` - auto-detection
- `test_node_test_runner_detection_vitest` - vitest detection
- `test_node_test_runner_detection_jest` - jest detection
