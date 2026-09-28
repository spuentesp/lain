# Follow-ups

Current work deliberately deferred from changes already merged to `dev`.
Completed plans, audits, and implementation notes live in Git history rather
than this file.

Last verified against `dev` on 2026-09-21.

## Distribution: Windows clean-room install

- **Status:** fix landed in tree; awaiting the next release tag.
- **Evidence:** `release.yml::build-windows` packages every
  `*.dll` from `target/x86_64-pc-windows-msvc/release/` alongside
  `lain.exe`, and the build fails loudly if `DirectML.dll` is
  missing. `npm-shim/scripts/install.test.js` has the
  `Windows install fails clearly when DirectML.dll is absent`
  regression fixture. The published artifact is still the
  pre-fix `lain.exe`-only archive until the next tag carries the
  corrected packaging.
- **Work:** ship the corrected archive in the next release and
  verify it from a clean Windows runner.
- **Acceptance:** all six scheduled user/automation lanes pass and
  the release gate remains green for all three release targets.

## Indexing: LSP subprocess isolation

- **Status:** blocked on the upstream `lsp-bridge` API.
- **Background:** libgit2, tree-sitter, and ONNX work has moved off Tokio
  worker threads. LSP round trips remain async-only but are cancellation-aware.
- **Work:** migrate local call sites once upstream `lsp-bridge` exposes
  blocking entry points or raw stdio transport.
- **Acceptance:** hot LSP calls run outside Tokio worker threads while retaining
  cancellation and timeout behavior.

## Planned capability expansions

- **Hybrid LSP expansion:** LSP implementation, type-definition, and call-hierarchy edges.
- **OTLP gRPC ingest:** optional OTLP gRPC/protobuf ingest alongside the existing lightweight HTTP/JSON path.

## Trust and release work

- **Dependency advisories:** keep the current actions in
  [`VULNS.md`](VULNS.md).
- **OpenSSF gaps:** keep the current measurements and process decisions in
  [`SCORECARD.md`](SCORECARD.md).
- **Agent-contract badge:** optional polish. The commit status is live and used
  by branch protection, but there is no distinct badge endpoint. Only build one
  if the README needs a separate signal from the ordinary CI badge.
- **Next release:** `dev` is ahead of `main`; choose the next version, create a
  `release/v0.x.y` branch from `dev`, update all release metadata, and open the
  release PR against `main` as described in [`BRANCHING.md`](BRANCHING.md).

## Maintenance rule

Add only concrete unfinished work with evidence and acceptance criteria. Remove
an entry when it lands; the PR and Git history are the archive.
