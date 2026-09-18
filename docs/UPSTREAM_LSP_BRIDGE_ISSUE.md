# Upstream issue draft: sync entry points on `lsp-bridge`

This file is the issue text to file against `ciresnave/lsp-bridge`
on GitHub. The upstream repo is at
<https://github.com/ciresnave/lsp-bridge>; the version of `lsp-bridge`
we currently depend on is `0.2` (pinned in `Cargo.toml`).

The blocker is structural: the consumer side of `lsp-bridge` (the
`lain` MCP server at <https://github.com/spuentesp/lain>) has migrated
its other indexer calls — libgit2, tree-sitter, ONNX — onto a
`spawn_blocking` thread pool so the Tokio runtime stays free of
subprocess work. The LSP calls are the only ones left on the runtime,
because `lsp-bridge` exposes the LSP round-trip only as `pub async
fn` returning a future, with no sync entry point.

This file is **not committed to the upstream repo** — it is a draft
to copy into the upstream issue tracker. We have already landed
the cancel-aware `tokio::select!` race on our side (PR
`feat/m4-lsp-cancel-aware`); the only thing that closes this
followup is a sync API upstream.

---

## Title

`LspBridge`: add sync `_blocking` entry points for the hot LSP
round-trips, or expose the underlying stdio handle so a
`spawn_blocking` closure can drive it

## Problem

`lsp-bridge` 0.2 exposes every LSP round-trip as `pub async fn` on
`LspBridge`. The two hot ones in our consumer are:

- `LspBridge::find_references` (`bridge.rs:224`)
- `LspBridge::get_document_symbols` (`bridge.rs:717`)
- `LspBridge::open_document` (`bridge.rs:159`) — write side, called
  before each `get_document_symbols` round-trip.

We have migrated the rest of our indexer off the Tokio runtime and
onto `tokio::task::spawn_blocking` so the runtime stays free of
subprocess work — libgit2 calls in PR `feat/m4-spawn-blocking`,
tree-sitter + ONNX in `feat/m4-spawn-blocking-followup`. The LSP
calls are the only ones left on the runtime because there is no
sync entry point on `LspBridge`.

## What we want

A sync variant of each of the three methods above that can be
called from inside `tokio::task::spawn_blocking` without
`Handle::block_on`. Two shapes work:

### Option A — direct `_blocking` methods on `LspBridge`

```rust
impl LspBridge {
    pub fn find_references_blocking(
        &self,
        server_id: &str,
        uri: &str,
        position: lsp_types::Position,
    ) -> Result<Vec<lsp_types::Location>>;

    pub fn get_document_symbols_blocking(
        &self,
        server_id: &str,
        uri: &str,
    ) -> Result<Vec<lsp_types::DocumentSymbol>>;

    pub fn open_document_blocking(
        &self,
        server_id: &str,
        uri: &str,
        content: &str,
    ) -> Result<()>;
}
```

Internally these would drive the LSP child process over stdio
from inside a private, single-threaded `tokio::runtime::Builder::new_current_thread()`
that is `block_on`'d inside the `_blocking` body. **This
`block_on` is acceptable here** because (a) the calling thread is
a `spawn_blocking` worker, not the Tokio runtime, and (b) the
runtime is scoped to one LSP round-trip and dropped at the end of
the call.

### Option B — expose the stdio handle so the consumer drives it

```rust
impl LspBridge {
    pub fn try_take_stdio_handle(
        &mut self,
        server_id: &str,
    ) -> Result<ChildStdio>;

    pub fn return_stdio_handle(
        &mut self,
        server_id: &str,
        handle: ChildStdio,
    ) -> Result<()>;
}
```

`ChildStdio` would carry the child's stdin/stdout, a write
buffer, and a parsed-message receiver. The consumer drives the
JSON-RPC framing inside its own `spawn_blocking` closure. This
shape is closer to the libgit2 migration we did on our side, but
requires re-implementing the LSP wire protocol framing on the
consumer side, which is more invasive.

We prefer **Option A** — minimal new API, the runtime-internal
`Handle::block_on` is contained inside the bridge, the consumer
stays high-level. Option B is a fallback if there is a reason A
isn't feasible (e.g. bridge state is shared across Tokio tasks
that can't move to a blocking thread).

## Why we can't `Handle::block_on` from our side

`tokio::task::spawn_blocking` runs the closure on a thread that is
not part of the Tokio worker pool. If the closure does
`Handle::block_on(future)`, the future runs to completion on the
blocking thread but the work it does (e.g. reading from the LSP
child's stdout via `tokio::process::Child::wait`) is driven by the
Tokio runtime's IO driver. With a single-threaded runtime that's
deadlock under load: the blocking thread holds the IO driver's
event loop, and any IO the runtime wants to do elsewhere blocks.

So we cannot wrap `bridge.find_references(...).await` from inside
our `spawn_blocking` closure — that's the anti-pattern this whole
initiative is designed to avoid. The runtime must live on the
blocking thread that owns it for the duration of the LSP
round-trip, which is what Option A does.

## Worked example for our consumer

Before:

```rust
// src/server/lsp.rs (current)
let locations = tokio::time::timeout(
    LSP_REQUEST_TIMEOUT,
    self.bridge.find_references(&server_id, &uri, position),
)
.await
```

After (with Option A):

```rust
// src/server/lsp.rs (after upstream ships)
let locations = tokio::task::spawn_blocking({
    let bridge = self.bridge.clone(); // Arc<LspBridge>
    let server_id = server_id.clone();
    let uri = uri.clone();
    move || bridge.find_references_blocking(&server_id, &uri, position)
})
.await
.map_err(|e| LainError::Lsp(e.to_string()))??;
```

The cancel-aware `tokio::select!` race we already have on the
`tokio::time::timeout(...) .await` moves up to the `spawn_blocking`
JoinHandle site — `tokio::select! { _ = cancel.cancelled() => ...,
result = join_handle => ... }` — so a shutdown that lands mid-scan
still aborts the LSP round-trip promptly instead of waiting for
the child to answer.

## Acceptance criteria

- [ ] `LspBridge::find_references_blocking`, `get_document_symbols_blocking`,
      `open_document_blocking` exist with the same semantics as
      their async counterparts, returning the same `Result<_>`
      type on success and error.
- [ ] Internally each `_blocking` method drives its LSP round-trip
      on a private, scoped, single-threaded Tokio runtime — **not**
      the caller's runtime.
- [ ] The `_blocking` methods are safe to call from inside
      `tokio::task::spawn_blocking` on the consumer side without
      `Handle::block_on`.
- [ ] Existing async methods are unchanged (no behaviour drift).
- [ ] A `cargo doc` build on the new public surface and a smoke
      test that calls each `_blocking` method from inside
      `std::thread::spawn` against a fixture language server pass.

## What we already have on our side (consumer perspective)

- **PR `feat/m4-cancellation-token` (#88):** server-owned
  `CancellationToken` plumbed through the whole indexer.
- **PR `feat/m4-spawn-blocking` (#90):** libgit2 calls onto
  `spawn_blocking` via `src/server/ingest/blocking.rs::offthread`.
- **PR `feat/m4-spawn-blocking-followup` (#98):** tree-sitter +
  ONNX NLP onto the same blocking pool.
- **PR `feat/m4-lsp-cancel-aware` (#101):** `tokio::select!`
  race between LSP `await` and cancel token. Cancellation
  latency is fixed; the migration onto `spawn_blocking` is
  still blocked on the upstream sync entry points.

The only consumer-side piece left is the call-site swap once
upstream ships the `_blocking` methods.

## Consumer reference

For the full call-site list on our side, see
[`docs/FOLLOWUPS.md` §"LSP subprocess calls (deferred — upstream
blocker)"](FOLLOWUPS.md) in the `spuentesp/lain` repo. The
`scan.rs:161`, `scan.rs:186`, `ingestion.rs:826` sites are the
three hot ones, plus the inner `bridge.find_references` /
`bridge.get_document_symbols` calls in
`src/server/lsp.rs:399` and `src/server/lsp.rs:318`.
