# Multi-Instance Owner + Sidecar Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let one `lain --mode owner` and N `lain --mode sidecar` processes coexist on the same workspace, with the sidecar connecting to the live HTTP singleton and receiving overlay updates over a JSON-RPC server stream.

**Architecture:** Replace the `pid:port` text-file lock at `<workspace>/.lain/server.lock` with an OS-level `flock(2)` guard. Add a `--mode owner|sidecar` flag. Owner keeps today's behavior; sidecar opens the graph read-only and subscribes to the owner's overlay stream via a new MCP method. The install loop writes `url: http://localhost:9999/mcp` for every agent that supports it, and `command: lain --mode sidecar` for the rest.

**Tech Stack:** Rust 2021, `nix` (for `flock(2)`) or `fs2` (for portable flock), `rust-mcp-sdk 1.0.1` server-stream API, `serde_json` for the overlay diff schema, `tokio` (already a dep), `tempfile` (dev dep).

## Global Constraints

- `<workspace>/.lain/server.lock` is a real `flock(F_SETLK)` guard; the old `kill -0` text-only check is removed.
- `--mode owner` keeps today's behavior; no test that passes today breaks.
- `--mode sidecar` skips the workspace write paths; opens the graph read-only; starts the HTTP transport; subscribes to the owner's overlay stream.
- Two `lain --mode owner` on the same workspace: the second fails (the `flock` blocks it).
- One `lain --mode owner` plus N `lain --mode sidecar` on the same workspace: all `Operational`; sidecars see overlay updates within 1 second.
- `lain agents install --scope user` writes `url: http://localhost:9999/mcp` for agents that support it, and `command: lain --mode sidecar` for the rest.
- Existing tests in `cargo test --all-targets` remain green; new tests for `WorkspaceLock`, `open_read_only`, and `dual_instance` pass.
- All work lands on `main`. This is a host-managed linked worktree already on `main`.
- No git commit, push, reset, or other mutation without explicit user authorization.
- No new dependencies beyond `nix = "0.29"` (or the existing `fs2` if already present) and any dev-only crate used for the new tests.

---

## File Map

- **Create:** `src/lock.rs` — `WorkspaceLock` newtype with `acquire_exclusive()` and `acquire_shared()`.
- **Modify:** `src/main.rs` — parse `--mode owner|sidecar`; gate writer paths; add `SidelessSidecarRunner`.
- **Create:** `src/sidecar.rs` — read-only runtime with overlay subscription client.
- **Modify:** `src/graph.rs` — add `GraphDatabase::open_read_only` and a `mode: LainMode` field; gate every insert/update path.
- **Modify:** `src/server/ingestion.rs`, `src/server/jobs.rs`, `src/sensors/*.rs` — gate every `graph.insert_*` call on `mode == owner`.
- **Modify:** `src/mcp/handler.rs` — add `overlay/subscribe` server-stream method and `overlay/get_snapshot` polling fallback.
- **Create:** `src/overlay/stream.rs` — `OverlayDiff` schema and a broadcast channel.
- **Modify:** `src/server/mod.rs` — add overlay broadcast hook on insert.
- **Create:** `tests/dual_instance.rs` — integration test that spawns one owner and one sidecar on the same workspace.
- **Modify:** `agents/manifest.toml` and the seven adapter files in `src/cmds/agents/adapters/` — write `url: http://localhost:9999/mcp` by default; fall back to `command: lain --mode sidecar` for the rest.
- **Modify:** `docs/agent-installation.md` and `README.md` — describe the new mode.
- **Modify:** `src/cmds/agents/install.rs` — accept the new HTTP transport.

---

### Task 1: WorkspaceLock with `flock(2)` and a one-place test

**Files:**
- Create: `src/lock.rs`
- Create: `src/lock_tests.rs` (unit tests; not a new file because the tests live in the same module)
- Modify: `src/lib.rs` to add `pub mod lock;`

**Interfaces:**
- Consumes: nothing yet.
- Produces:
  - `pub struct WorkspaceLock(PathBuf);`
  - `impl WorkspaceLock { pub fn path(&self) -> &Path; pub fn acquire_exclusive(&self) -> Result<ExclusiveGuard, LainError>; pub fn acquire_shared(&self) -> Result<SharedGuard<'_,>, LainError>; pub fn owner_pid(&self) -> Option<u32>; pub fn read_owner_pid(&self) -> Option<u32>; pub fn write_owner_pid(&self, pid: u32, port: u16) -> Result<(), LainError>; }`
  - `pub struct ExclusiveGuard(File); impl Drop for ExclusiveGuard;`
  - `pub struct SharedGuard<'a>(&'a File); impl Drop for SharedGuard<'_>;`

- [ ] **Step 1: Write the failing test**

In `src/lock.rs` (created as a stub that only contains the public types), add a `#[cfg(test)] mod tests` block:

```rust
use std::fs;
use std::os::unix::fs::PermissionsExt;

fn temp_path(name: &str) -> std::path::PathBuf {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(name);
    (dir, path)
}

#[test]
fn exclusive_lock_blocks_second_attempt() {
    let (_dir, path) = temp_path("lk");
    fs::write(&path, b"").unwrap();
    let lock = WorkspaceLock::new(path.clone());
    let _g1 = lock.acquire_exclusive().expect("first");
    let r = lock.acquire_exclusive();
    assert!(r.is_err(), "second exclusive must fail");
}

#[test]
fn shared_locks_coexist() {
    let (_dir, path) = temp_path("lk2");
    fs::write(&path, b"").unwrap();
    let lock = WorkspaceLock::new(path.clone());
    let g1 = lock.acquire_shared().expect("first");
    let _g2 = lock.acquire_shared().expect("second");
    drop(g1);
}

#[test]
fn shared_blocks_exclusive_and_vice_versa() {
    let (_dir, path) = temp_path("lk3");
    fs::write(&path, b"").unwrap();
    let lock = WorkspaceLock::new(path.clone());
    let _g = lock.acquire_shared().expect("shared");
    assert!(lock.acquire_exclusive().is_err());
}

#[test]
fn text_file_round_trip() {
    let (_dir, path) = temp_path("lk4");
    let lock = WorkspaceLock::new(path.clone());
    lock.write_owner_pid(1234, 9999).unwrap();
    let got = lock.read_owner_pid();
    assert_eq!(got, Some(1234));
}
```

- [ ] **Step 2: Run tests, confirm they fail**

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --lib lock
```

Expected: FAIL with “cannot find type `WorkspaceLock`”.

- [ ] **Step 3: Add `fs2 = "0.4"` to `[dev-dependencies]` if not present; `fs2 = "0.4"` to `[dependencies]` for the lock module**

Check `Cargo.toml` first. If `fs2` is not present, add:

```toml
fs2 = "0.4"
```

to `[dependencies]` and the same to `[dev-dependencies]`. (`fs2` provides a safe `flock` wrapper for `OpenOptions`-style files; if not, use the `nix` crate's `flock` syscall directly. The plan assumes `fs2`.)

- [ ] **Step 4: Implement `WorkspaceLock` and guards in `src/lock.rs`**

```rust
//! Workspace lock around `<workspace>/.lain/server.lock`.
//!
//! Owner processes hold an exclusive `flock` for their lifetime. Sidecar
//! processes briefly take a shared `flock` to verify the owner is alive,
//! then drop the lock. The on-disk file still carries the owner's
//! `pid:port` for debugging.

use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use crate::error::LainError;

#[derive(Debug)]
pub struct WorkspaceLock {
    path: PathBuf,
}

impl WorkspaceLock {
    pub fn new(path: PathBuf) -> Self { Self { path } }
    pub fn path(&self) -> &Path { &self.path }

    pub fn acquire_exclusive(&self) -> Result<ExclusiveGuard, LainError> {
        let f = OpenOptions::new()
            .read(true).write(true).create(true).truncate(false)
            .open(&self.path)
            .map_err(LainError::from)?;
        f.lock_exclusive().map_err(|e| LainError::Other(format!("workspace lock held: {e}")))?;
        Ok(ExclusiveGuard(f))
    }

    pub fn acquire_shared<'a>(&'a self) -> Result<SharedGuard<'a>, LainError> {
        let f = OpenOptions::new()
            .read(true).write(true).create(true).truncate(false)
            .open(&self.path)
            .map_err(LainError::from)?;
        f.lock_shared().map_err(|e| LainError::Other(format!("workspace lock contended: {e}")))?;
        Ok(SharedGuard(&self.path, f))
    }

    pub fn read_owner_pid(&self) -> Option<u32> {
        let s = std::fs::read_to_string(&self.path).ok()?;
        s.split(':').next()?.trim().parse().ok()
    }

    pub fn write_owner_pid(&self, pid: u32, port: u16) -> Result<(), LainError> {
        std::fs::write(&self.path, format!("{pid}:{port}\n")).map_err(LainError::from)
    }
}

pub struct ExclusiveGuard(File);
impl Drop for ExclusiveGuard {
    fn drop(&mut self) { let _ = FileExt::unlock(&self.0); }
}

pub struct SharedGuard<'a>(&'a Path, File);
impl Drop for SharedGuard<'_> {
    fn drop(&mut self) { let _ = FileExt::unlock(&self.1); }
}
```

- [ ] **Step 5: Run the lock tests, confirm they pass**

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --lib lock
```

Expected: PASS for all four tests.

- [ ] **Step 6: Stage but do not commit**

```bash
git add src/lock.rs Cargo.toml Cargo.lock
```

---

### Task 2: Add `LainMode` and gate the writer startup paths

**Files:**
- Modify: `src/main.rs:1-15,80-200,218-230`
- Create: `src/mode.rs` (or add to `src/lib.rs` if a single line is enough)

**Interfaces:**
- Consumes: `WorkspaceLock` from Task 1.
- Produces:
  - `pub enum LainMode { Owner, Sidecar }` with `FromStr` and `Default = Owner`.
  - `--mode owner|sidecar` flag wired into clap on the existing CLI.
  - On `sidecar` startup: skip the workspace write paths (`build_core_memory`, `sync_volatile_overlay`, watcher spawn); open the graph in read-only mode (the actual function is in Task 3; here just gate the call sites).

- [ ] **Step 1: Write the failing test for the mode flag**

In a new test module within `src/main.rs` (or `src/mode.rs`), add:

```rust
#[cfg(test)]
mod tests {
    use super::LainMode;
    #[test]
    fn parse_owner() {
        assert_eq!("owner".parse::<LainMode>().unwrap(), LainMode::Owner);
    }
    #[test]
    fn parse_sidecar() {
        assert_eq!("sidecar".parse::<LainMode>().unwrap(), LainMode::Sidecar);
    }
    #[test]
    fn default_is_owner() {
        assert_eq!(LainMode::default(), LainMode::Owner);
    }
}
```

- [ ] **Step 2: Run the test, confirm it fails**

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --lib main::tests
```

Expected: FAIL because `LainMode` does not exist yet.

- [ ] **Step 3: Add `LainMode` to `src/mode.rs`**

```rust
//! Mode the binary runs in: owner (the default, today) or sidecar
//! (a read-only client that subscribes to the owner's overlay stream).

use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LainMode { Owner, Sidecar }

impl Default for LainMode { fn default() -> Self { LainMode::Owner } }

impl FromStr for LainMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "owner" => Ok(LainMode::Owner),
            "sidecar" => Ok(LainMode::Sidecar),
            other => Err(format!("unknown lain mode: {other}")),
        }
    }
}
```

- [ ] **Step 4: Wire `--mode` into the clap CLI**

In `src/main.rs`, in the `Args` struct, add:

```rust
#[arg(long, default_value = "owner", value_parser = ["owner", "sidecar"])]
mode: String,
```

Add `use crate::mode::LainMode;` at the top of `src/main.rs`.

- [ ] **Step 5: Gate the writer paths**

In `src/main.rs` `main()`:

```rust
let mode: LainMode = args.mode.parse().expect("validated by clap");
match mode {
    LainMode::Owner => {
        // existing path: build_core_memory, sync_volatile_overlay, watcher
        // ... unchanged ...
    }
    LainMode::Sidecar => {
        // build a Sidecar runtime; place-holder until Task 3 lands
        tracing::info!("Starting Lain in sidecar mode");
        return lain::sidecar::run(args).await;
    }
}
```

For this task, `lain::sidecar::run` is a stub that returns `Ok(())`. Task 3 fills it in.

- [ ] **Step 6: Run the test, confirm it passes**

```bash
cargo test --lib
```

Expected: PASS, with no test in the existing suite regressing.

- [ ] **Step 7: Stage but do not commit**

```bash
git add src/main.rs src/mode.rs src/lib.rs
```

---

### Task 3: Read-only graph and Sidecar runtime

**Files:**
- Modify: `src/graph.rs:1-30` (add `open_read_only` and a `mode` field on `GraphDatabase`)
- Modify: `src/server/ingestion.rs` (gate every `graph.insert_*` on `mode == owner`)
- Modify: `src/server/jobs.rs` (same)
- Modify: `src/sensors/graphql_sensor.rs`, `openapi_sensor.rs`, `proto_sensor.rs`, `websocket_sensor.rs` (same)
- Create: `src/sidecar.rs`
- Modify: `src/cmds/agents/adapters/mod.rs` (export `sidecar` module)

**Interfaces:**
- Consumes: `LainMode` from Task 2, `WorkspaceLock` from Task 1.
- Produces:
  - `GraphDatabase::open_read_only(memory_path) -> Result<Self, LainError>` that opens the existing `.lain/graph.bin` as immutable and exposes only the read API.
  - `pub async fn lain::sidecar::run(args: Args) -> Result<(), anyhow::Error>` that starts the MCP server, opens the graph read-only, and serves tools.

- [ ] **Step 1: Write the failing test for `open_read_only`**

In `src/graph.rs`, in the existing `#[cfg(test)] mod tests` block (or a new one), add:

```rust
#[test]
fn open_read_only_rejects_writes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("graph.bin");
    let mut owner = GraphDatabase::new(&path).expect("new");
    let n = owner.insert_node(/* a test node */).expect("insert");
    // ... build a real test node; follow the existing graph_tests.rs pattern ...
    drop(owner);
    let ro = GraphDatabase::open_read_only(&path).expect("open");
    let r = ro.insert_node(/* same shape */);
    assert!(r.is_err());
    let n2 = ro.get_node(&id).expect("get");
    assert_eq!(n2.id, n.id);
}
```

(Follow the existing `graph_tests.rs` pattern for the node shape so the test compiles; the exact line that fails is the `insert_node` on the read-only graph.)

- [ ] **Step 2: Run test, confirm it fails**

```bash
cargo test --lib graph::tests
```

Expected: FAIL because `open_read_only` does not exist yet.

- [ ] **Step 3: Add `open_read_only` to `GraphDatabase`**

In `src/graph.rs`:

```rust
impl GraphDatabase {
    pub fn new(path: &Path) -> Result<Self, LainError> { /* unchanged */ }
    pub fn open_read_only(path: &Path) -> Result<Self, LainError> {
        let g = GraphDatabase::new(path)?;
        g.read_only = true;
        Ok(g)
    }
    pub fn insert_node(&mut self, n: &GraphNode) -> Result<NodeId, LainError> {
        if self.read_only { return Err(LainError::Other("graph is read-only".into())); }
        // ... unchanged ...
    }
    // repeat the gate on every public `insert_*` / `remove_*` / `persist`
}
```

Add a private field on `GraphDatabase`:

```rust
pub struct GraphDatabase {
    storage: ...,
    read_only: bool,
}
```

- [ ] **Step 4: Run the test, confirm it passes**

```bash
cargo test --lib graph::tests
```

Expected: PASS.

- [ ] **Step 5: Write the failing test for the sidecar runtime**

In `src/sidecar.rs`, add a `#[cfg(test)] mod tests` block:

```rust
#[tokio::test]
async fn run_sidecar_reports_operational() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("graph.bin");
    // build a real graph first by spawning an owner process
    let bin = env!("CARGO_BIN_EXE_lain");
    let _owner = std::process::Command::new(bin)
        .args(["--workspace", tmp.path().to_str().unwrap(),
               "--memory-path", path.to_str().unwrap(),
               "--transport", "http", "--port", "9911",
               "--mode", "owner"])
        .env("LAIN_PORT", "9911")
        .spawn().expect("spawn owner");
    // wait for health
    // then spawn sidecar in a temp dir on the same workspace
    let _side = std::process::Command::new(bin)
        .args(["--workspace", tmp.path().to_str().unwrap(),
               "--memory-path", path.to_str().unwrap(),
               "--transport", "http", "--port", "9912",
               "--mode", "sidecar"])
        .env("LAIN_PORT", "9912")
        .env("LAIN_OWNER_URL", "http://localhost:9911/mcp")
        .spawn().expect("spawn sidecar");
    // wait for health on port 9912
    // assert: get_health on 9912 returns Operational AND
    //         get_health on 9911 still returns Operational
    // teardown
}
```

- [ ] **Step 6: Run test, confirm it fails**

```bash
cargo test --lib sidecar::tests
```

Expected: FAIL because `lain::sidecar::run` is a stub.

- [ ] **Step 7: Implement `src/sidecar.rs`**

```rust
//! Sidecar runtime: read-only graph, owner overlay subscription, MCP server.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::mcp::handler::LainMcpServer;

pub struct SidecarConfig {
    pub workspace: PathBuf,
    pub memory_path: PathBuf,
    pub port: u16,
    pub owner_url: String,
    pub embedding_model: Option<PathBuf>,
}

pub async fn run(cfg: SidecarConfig) -> Result<(), LainError> {
    let graph = GraphDatabase::open_read_only(&cfg.memory_path)?;
    let overlay = crate::overlay::VolatileOverlay::new();
    // subscribe to owner's overlay/subscribe; on disconnect retry with backoff
    tokio::spawn(crate::overlay::stream::subscribe(
        cfg.owner_url.clone(),
        overlay.clone(),
    ));
    let server = LainMcpServer::new_read_only(graph, overlay);
    let addr: SocketAddr = ([127, 0, 0, 1], cfg.port).into();
    server.serve(addr).await
}
```

- [ ] **Step 8: Wire `args` to `SidecarConfig` in `src/main.rs`**

In `src/main.rs`, replace the `sidecar` stub from Task 2 with:

```rust
LainMode::Sidecar => {
    tracing::info!("Starting Lain in sidecar mode");
    let cfg = lain::sidecar::SidecarConfig {
        workspace: args.workspace.clone(),
        memory_path: args.memory_path.clone().unwrap_or_else(|| args.workspace.join(".lain/graph.bin")),
        port: args.port,
        owner_url: std::env::var("LAIN_OWNER_URL").unwrap_or_else(|_| format!("http://localhost:{}/mcp", std::env::var("LAIN_PORT").unwrap_or_else(|_| "9999".into()))),
        embedding_model: args.embedding_model.clone().map(PathBuf::from),
    };
    return lain::sidecar::run(cfg).await.map_err(Into::into);
}
```

- [ ] **Step 9: Run the test, confirm it passes**

```bash
cargo test --lib sidecar::tests
```

Expected: PASS.

- [ ] **Step 10: Stage but do not commit**

```bash
git add src/graph.rs src/sidecar.rs src/server/ingestion.rs src/server/jobs.rs src/sensors/*.rs src/main.rs
```

---

### Task 4: Overlay server-stream and the broadcast hook

**Files:**
- Create: `src/overlay/stream.rs`
- Modify: `src/server/ingestion.rs` (broadcast on every overlay insert)
- Modify: `src/server/jobs.rs` (same)
- Modify: `src/sensors/graphql_sensor.rs`, `openapi_sensor.rs`, `proto_sensor.rs`, `websocket_sensor.rs` (same)
- Modify: `src/server/mod.rs` (expose a broadcast helper)
- Modify: `src/mcp/handler.rs` (add `overlay/subscribe` server-stream + `overlay/get_snapshot` polling fallback)
- Modify: `src/lib.rs` (add `pub mod overlay;` if it does not exist already)

**Interfaces:**
- Consumes: `VolatileOverlay` from `src/overlay.rs`.
- Produces:
  - `pub struct OverlayDiff { pub revision: u64, pub added: Vec<GraphNode>, pub removed: Vec<NodeId>, pub updated: Vec<GraphNode> }`
  - `pub fn broadcast_overlay_diff(diff: OverlayDiff)` global broadcast helper.
  - `pub async fn subscribe(owner_url: String, overlay: VolatileOverlay) -> !` in `src/overlay/stream.rs`.

- [ ] **Step 1: Write the failing test for the broadcast**

In `src/overlay/stream.rs`, add:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::VolatileOverlay;
    use crate::graph::GraphNode;

    #[tokio::test]
    async fn broadcast_reaches_subscriber() {
        let mut rx = subscribe_channel();
        let overlay = VolatileOverlay::new();
        tokio::spawn(subscribe_apply(overlay.clone(), rx));
        broadcast_overlay_diff(OverlayDiff {
            revision: 1, added: vec![/* fake node */], removed: vec![], updated: vec![],
        });
        // wait for subscriber to apply
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(overlay.get_node(&"fake-id").is_some());
    }
}
```

(The test only runs if a `subscribe_channel()` helper and a `subscribe_apply` helper exist. They are added in Step 7.)

- [ ] **Step 2: Run test, confirm it fails**

```bash
cargo test --lib overlay::stream::tests
```

Expected: FAIL.

- [ ] **Step 3: Implement `OverlayDiff` and the broadcast channel**

```rust
//! Overlay diff broadcast: every owner overlay insert becomes a JSON
//! message on a per-workspace tokio broadcast channel. Sidecars
//! subscribe to the channel and apply diffs to their in-memory cache.

use std::time::Duration;
use tokio::sync::broadcast;
use crate::graph::GraphNode;
use crate::overlay::VolatileOverlay;
use crate::error::LainError;

pub type RevisionId = u64;

#[derive(Debug, Clone)]
pub struct OverlayDiff {
    pub revision: RevisionId,
    pub added: Vec<GraphNode>,
    pub removed: Vec<String>,
    pub updated: Vec<GraphNode>,
}

static BUS: once_cell::sync::Lazy<broadcast::Sender<OverlayDiff>> =
    once_cell::sync::Lazy::new(|| broadcast::channel::<OverlayDiff>(1024).0);

pub fn broadcast_overlay_diff(diff: OverlayDiff) {
    let _ = BUS.send(diff);
}

pub fn subscribe_channel() -> broadcast::Receiver<OverlayDiff> {
    BUS.subscribe()
}
```

- [ ] **Step 4: Add the broadcast hook on every overlay insert**

In `src/server/ingestion.rs:417`:

```rust
self.overlay.insert_node(symbol.node.clone());
crate::overlay::stream::broadcast_overlay_diff(crate::overlay::stream::OverlayDiff {
    revision: self.next_revision(),
    added: vec![symbol.node.clone()],
    removed: vec![],
    updated: vec![],
});
```

Add a `next_revision` helper on `LainServer` (atomic counter, starts at 0, increments per insert).

- [ ] **Step 5: Add `overlay/subscribe` server-stream and `overlay/get_snapshot` to the MCP handler**

In `src/mcp/handler.rs`, add:

```rust
pub async fn overlay_subscribe(&self) -> impl Stream<Item = OverlayDiff> + 'static {
    let mut rx = crate::overlay::stream::subscribe_channel();
    async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(diff) => yield diff,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }
}

pub fn overlay_snapshot(&self) -> Vec<GraphNode> {
    self.overlay.list_all()
}
```

- [ ] **Step 6: Run all tests, confirm green**

```bash
cargo test --lib
```

Expected: PASS, with no regression.

- [ ] **Step 7: Stage but do not commit**

```bash
git add src/overlay/stream.rs src/server/ingestion.rs src/server/jobs.rs src/sensors/*.rs src/server/mod.rs src/mcp/handler.rs src/lib.rs
```

---

### Task 5: Sidecar overlay subscription client

**Files:**
- Modify: `src/overlay/stream.rs` (add `pub async fn subscribe_apply`)
- Modify: `src/sidecar.rs` (use the subscribe client)

**Interfaces:**
- Consumes: `OverlayDiff` from Task 4.
- Produces:
  - `pub async fn subscribe_apply(overlay: VolatileOverlay, mut rx: broadcast::Receiver<OverlayDiff>) -> !` in `src/overlay/stream.rs`.

- [ ] **Step 1: Write the failing test**

In `src/overlay/stream.rs` `mod tests`:

```rust
#[tokio::test]
async fn subscribe_apply_inserts_node() {
    let overlay = VolatileOverlay::new();
    let (tx, rx) = tokio::sync::broadcast::channel(4);
    tokio::spawn(subscribe_apply(overlay.clone(), rx));
    let node = /* build a fake GraphNode */;
    tx.send(OverlayDiff { revision: 1, added: vec![node.clone()], removed: vec![], updated: vec![] }).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(overlay.get_node(&node.id).is_some());
}
```

- [ ] **Step 2: Run test, confirm it fails**

```bash
cargo test --lib overlay::stream::tests
```

Expected: FAIL.

- [ ] **Step 3: Implement `subscribe_apply`**

```rust
pub async fn subscribe_apply(
    overlay: VolatileOverlay,
    mut rx: tokio::sync::broadcast::Receiver<OverlayDiff>,
) -> ! {
    use tokio::sync::broadcast::error::RecvError;
    loop {
        match rx.recv().await {
            Ok(diff) => {
                for n in diff.added { let _ = overlay.insert_node(n); }
                for id in diff.removed { let _ = overlay.remove_node(&id); }
                for n in diff.updated { let _ = overlay.upsert_node(n); }
            }
            Err(RecvError::Lagged(n)) => {
                tracing::warn!("overlay subscriber lagged by {n} events");
            }
            Err(RecvError::Closed) => break,
        }
    }
}
```

- [ ] **Step 4: Run test, confirm it passes**

```bash
cargo test --lib overlay::stream::tests
```

Expected: PASS.

- [ ] **Step 5: Stage but do not commit**

```bash
git add src/overlay/stream.rs src/sidecar.rs
```

---

### Task 6: HTTP singleton sidecar wiring

**Files:**
- Modify: `src/overlay/stream.rs` (add `pub async fn subscribe(owner_url, overlay)` that hits the owner's `overlay/subscribe` stream)
- Modify: `src/sidecar.rs` (use it in `run`)
- Modify: `src/mcp/handler.rs` (expose `overlay_subscribe` and `overlay_snapshot` over HTTP transport)

**Interfaces:**
- Consumes: `OverlayDiff` and the owner's HTTP `overlay/subscribe` server-stream endpoint.
- Produces:
  - `pub async fn subscribe(owner_url: String, overlay: VolatileOverlay) -> !` in `src/overlay/stream.rs` that opens a `reqwest-eventsource` connection to `{owner_url}/overlay/subscribe` and applies each diff to `overlay`.

- [ ] **Step 1: Write the failing test**

In `src/overlay/stream.rs` `mod tests`:

```rust
#[tokio::test]
async fn subscribe_sse_against_mock_server() {
    // spawn a tiny hyper server that streams one SSE event
    // then assert the overlay receives the diff
}
```

- [ ] **Step 2: Run test, confirm it fails**

```bash
cargo test --lib overlay::stream::tests
```

Expected: FAIL.

- [ ] **Step 3: Implement `subscribe` over `reqwest-eventsource`**

```rust
pub async fn subscribe(owner_url: String, overlay: VolatileOverlay) -> ! {
    use eventsource_client::Client as EsClient;
    let url = format!("{}/overlay/subscribe", owner_url.trim_end_matches('/mcp'));
    let client = EsClient::for_url(&url).expect("url");
    let mut stream = client.stream();
    while let Some(event) = futures::StreamExt::next(&mut stream).await {
        match event {
            Ok(es) => {
                if let Ok(diff) = serde_json::from_str::<OverlayDiff>(&es.data) {
                    apply_diff(&overlay, diff);
                }
            }
            Err(_) => {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
    std::future::pending::<()>().await
}
```

- [ ] **Step 4: Run test, confirm it passes**

```bash
cargo test --lib overlay::stream::tests
```

Expected: PASS.

- [ ] **Step 5: Stage but do not commit**

```bash
git add src/overlay/stream.rs src/sidecar.rs src/mcp/handler.rs
```

---

### Task 7: `dual_instance` integration test

**Files:**
- Create: `tests/dual_instance.rs`

**Interfaces:**
- Consumes: `LainMode` from Task 2, `WorkspaceLock` from Task 1, `GraphDatabase::open_read_only` from Task 3, the sidecar runtime from Task 3, the overlay broadcast from Task 4.
- Produces: a `cargo test` gate that asserts:
  1. one owner + one sidecar coexist on the same workspace;
  2. both report `Operational`;
  3. `query_graph` returns the same answer on both;
  4. an owner-side overlay insert is visible on the sidecar within 1 second;
  5. a second `--mode owner` on the same workspace fails.

- [ ] **Step 1: Write the test**

```rust
//! End-to-end dual-instance test: one owner + one sidecar on the same
//! workspace. Asserts both report `Operational`, that an overlay
//! insert on the owner shows up on the sidecar within 1 second, and that
//! a second `--mode owner` on the same workspace fails.

use std::process::{Command, Stdio};
use std::time::Duration;
use std::net::TcpListener;
use std::io::Write;
use std::thread;

fn pick_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

fn wait_for_health(port: u16, timeout: Duration) {
    let url = format!("http://127.0.0.1:{}/mcp", port);
    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_health","arguments":{}}}).to_string();
    let client = reqwest::blocking::Client::new();
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if let Ok(r) = client.post(&url).header("content-type","application/json").body(body.clone()).send() {
            if r.status().is_success() { return; }
        }
        thread::sleep(Duration::from_millis(200));
    }
    panic!("server on port {port} did not become healthy");
}

#[test]
fn dual_instance_owner_and_sidecar_coexist() {
    let tmp = tempfile::tempdir().unwrap();
    let owner_port = pick_port();
    let sidecar_port = pick_port();
    let model = "/home/sebastian/.local/lain/models/all-MiniLM-L6-v2.onnx";
    let lain = env!("CARGO_BIN_EXE_lain");

    // 1. Owner
    let mut owner = Command::new(lain)
        .args(["--workspace", tmp.path().to_str().unwrap(),
               "--transport", "http", "--port", &owner_port.to_string(),
               "--mode", "owner", "--embedding-model", model])
        .env("LAIN_PORT", owner_port.to_string())
        .stdout(Stdio::null()).stderr(Stdio::null())
        .spawn().expect("spawn owner");
    wait_for_health(owner_port, Duration::from_secs(60));
    assert_eq!(owner.try_wait().unwrap(), None, "owner crashed");

    // 2. Sidecar
    let mut sidecar = Command::new(lain)
        .args(["--workspace", tmp.path().to_str().unwrap(),
               "--transport", "http", "--port", &sidecar_port.to_string(),
               "--mode", "sidecar", "--embedding-model", model])
        .env("LAIN_PORT", sidecar_port.to_string())
        .env("LAIN_OWNER_URL", format!("http://127.0.0.1:{}", owner_port))
        .stdout(Stdio::null()).stderr(Stdio::null())
        .spawn().expect("spawn sidecar");
    wait_for_health(sidecar_port, Duration::from_secs(60));
    assert_eq!(sidecar.try_wait().unwrap(), None, "sidecar crashed");

    // 3. A second owner must fail (the flock blocks it)
    let second_owner = Command::new(lain)
        .args(["--workspace", tmp.path().to_str().unwrap(),
               "--transport", "http", "--port", &pick_port().to_string(),
               "--mode", "owner", "--embedding-model", model])
        .env("LAIN_PORT", "9998")
        .stdout(Stdio::null()).stderr(Stdio::null())
        .output().expect("spawn second owner");
    assert!(!second_owner.status.success(), "second owner must fail");

    // 4. Cleanup
    let _ = owner.kill(); let _ = owner.wait();
    let _ = sidecar.kill(); let _ = sidecar.wait();
}
```

- [ ] **Step 2: Run test, confirm it passes**

```bash
cargo test --test dual_instance -- --nocapture
```

Expected: PASS. The test takes ~10 s.

- [ ] **Step 3: Stage but do not commit**

```bash
git add tests/dual_instance.rs
```

---

### Task 8: Agent install loop defaults to `url: http://localhost:9999/mcp`

**Files:**
- Modify: `agents/manifest.toml` (change `transport = "stdio"` to `transport = "http"`; add a `format` field that distinguishes `http` and `sidecar`)
- Modify: the seven adapter files in `src/cmds/agents/adapters/`
- Modify: `src/cmds/agents/install.rs` (add a `format` branch)
- Modify: `src/cmds/agents/manifest.rs` (add `format` to `AgentEntry`)

**Interfaces:**
- Consumes: `format: String` on `AgentEntry` (`"http"`, `"sidecar"`, or `"json"`).
- Produces: `lain agents install --scope user` writes `url: http://localhost:9999/mcp` for `format = "http"` rows and `command: lain --mode sidecar ...` for `format = "sidecar"` rows. The legacy `format = "json"` row keeps today's stdio shape for backward compatibility.

- [ ] **Step 1: Write the failing test**

In `src/cmds/agents/manifest.rs`, in the `tests` block:

```rust
#[test]
fn loader_returns_format_field() {
    let agents = load_manifest().expect("manifest");
    assert!(agents.iter().any(|a| a.format == "http"));
}
```

- [ ] **Step 2: Run test, confirm it fails**

```bash
cargo test --lib cmds::agents::tests
```

Expected: FAIL because the field does not exist.

- [ ] **Step 3: Add `format` to `AgentEntry` and `manifest.rs`**

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentEntry {
    pub id: String,
    pub display_name: String,
    pub binary: String,
    #[serde(default)]
    pub detect_paths: Vec<String>,
    pub config_user: String,
    pub config_project: String,
    pub config_format: String,
    pub mcp_section: String,
    pub mcp_name: String,
    pub transport: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub default_args: Vec<String>,
    #[serde(default)]
    pub headless_probe: Vec<String>,
    #[serde(default = "default_format")]
    pub format: String,
}

fn default_format() -> String { "json".to_string() }
```

- [ ] **Step 4: Update `agents/manifest.toml`**

For every `[[agent]]` row, add `format = "http"` (or `format = "sidecar"` for the stdio fallback).

- [ ] **Step 5: Update each adapter to write the new shape**

In `src/cmds/agents/adapters/claude.rs` (and the six other files), in the `install` method, branch on `entry.format`:

```rust
if entry.format == "http" {
    let server = json!({ "url": format!("http://localhost:{}/mcp", std::env::var("LAIN_PORT").unwrap_or_else(|_| "9999".into())) });
    /* insert into entry.mcp_section.entry(entry.mcp_name) */
} else if entry.format == "sidecar" {
    /* write the legacy "command": "lain", "args": ["--mode", "sidecar", ...] */
} else {
    /* write the legacy "command" + "args" stdio shape */
}
```

- [ ] **Step 6: Update `src/cmds/agents/install.rs`**

In `run_install`, after computing the workspace, set `LAIN_PORT` on the env for the sidecar spawn (so the sidecar knows where the owner is). No new exports.

- [ ] **Step 7: Run the failing test, confirm it passes**

```bash
cargo test --lib cmds::agents::tests
```

Expected: PASS.

- [ ] **Step 8: Run `cargo test --all-targets`**

```bash
cargo test --all-targets
```

Expected: PASS, with no regression.

- [ ] **Step 9: Stage but do not commit**

```bash
git add agents/manifest.toml src/cmds/agents/
```

---

### Task 9: Documentation

**Files:**
- Modify: `docs/agent-installation.md`
- Modify: `README.md`

**Interfaces:**
- Produces: a new section `## Sidecar mode` that explains the `owner`/`sidecar` model, how to launch an owner, and how the per-agent install loop points the agent at the right one.

- [ ] **Step 1: Add the `Sidecar mode` section to `docs/agent-installation.md`**

```markdown
## Sidecar mode

A `lain --mode owner` process owns the graph, watcher, and write paths
for a workspace. Multiple `lain --mode sidecar` processes can attach to
the same workspace to read the graph and subscribe to overlay updates
without touching the writer lock.

```bash
# owner: long-running process, owns the graph and the HTTP singleton
lain --workspace /abs/path --mode owner --transport http --port 9999 \
    --embedding-model ~/.local/lain/models/all-MiniLM-L6-v2.onnx

# sidecar: a second process on the same workspace, e.g. started by an
# agent that only supports the stdio transport
lain --workspace /abs/path --mode sidecar --transport http --port 9998 \
    --embedding-model ~/.local/lain/models/all-MiniLM-L6-v2.onnx
LAIN_OWNER_URL=http://localhost:9999
```

`lain agents install --scope user` writes `url: http://localhost:9999/mcp`
for agents that support the HTTP transport, and `command: lain --mode
sidecar` for the rest. The HTTP singleton is the single source of truth
for the workspace.
```

- [ ] **Step 2: Update `README.md`**

Add a short pointer to the new section:

```markdown
For multi-instance wiring (one `owner` plus N `sidecars` on the same
workspace), see `docs/agent-installation.md#sidecar-mode`.
```

- [ ] **Step 3: Build docs and confirm no broken cross-references**

```bash
grep -R "sidecar" docs/ README.md
```

Expected: every reference resolves to either a working command or a documented section.

- [ ] **Step 4: Stage but do not commit**

```bash
git add docs/agent-installation.md README.md
```

---

### Task 10: Final automated verification

**Files:** none.

**Interfaces:**
- Produces: a final-pass run of the whole test suite, the release build, the live verify, and a diff inspection.

- [ ] **Step 1: Run focused tests**

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --lib lock overlay::stream sidecar cmds::agents
```

Expected: PASS.

- [ ] **Step 2: Run the full Rust test suite**

```bash
cargo test --all-targets
```

Expected: PASS, with no regressions in the existing watcher, state, tools, or handler suites.

- [ ] **Step 3: Build the release binary**

```bash
cargo build --release
```

Expected: clean build; `target/release/lain` regenerated.

- [ ] **Step 4: Inspect the diff and status**

```bash
git diff --check
git status --short
git diff --stat -- Cargo.toml Cargo.lock src/lock.rs src/mode.rs src/sidecar.rs src/overlay/stream.rs src/graph.rs src/main.rs src/server/ src/mcp/handler.rs tests/dual_instance.rs agents/manifest.toml src/cmds/agents/ docs/agent-installation.md README.md
```

Expected: only files declared in the File Map are changed.

- [ ] **Step 5: Stage but do not commit**

```bash
git add -N Cargo.toml Cargo.lock src/lock.rs src/mode.rs src/sidecar.rs src/overlay/stream.rs src/graph.rs src/main.rs src/server/ src/mcp/handler.rs tests/dual_instance.rs agents/manifest.toml src/cmds/agents/ docs/agent-installation.md README.md
git status --short
```

---

### Task 11: Live multi-instance verification

**Files:** none.

**Interfaces:**
- Produces: a live test with the rebuilt release binary that proves owner + sidecar coexist on the same workspace.

- [ ] **Step 1: Confirm permission baseline**

```bash
stat -c '%A %u:%g %n' /home/sebastian/monitor/monitor_dm_system/infra/neo4j/import
```

Expected: `drwx------ 7474:7474 ...` (unchanged).

- [ ] **Step 2: Install the rebuilt binary and restart the singleton (only after explicit user approval)**

```bash
install -m 755 target/release/lain /home/sebastian/.local/lain/lain
/home/sebastian/monitor/monitor_dm_system/scripts/lain-server-manager.sh restart
```

- [ ] **Step 3: Launch one owner and one sidecar on the same workspace, then run `layin agents verify` and `agy` against the sidecar URL**

```bash
# owner already running via the manager; start a sidecar in a different port
/home/sebastian/.local/lain/lain --workspace /home/sebastian/orca/workspaces/lain/langostino --mode sidecar --transport http --port 9998 --embedding-model /home/sebastian/.local/lain/models/all-MiniLM-L6-v2.onnx &
# prove the sidecar can answer get_health
curl -sS -X POST http://localhost:9998/mcp -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_health","arguments":{}}}' | head -c 200
# prove the owner is still healthy
curl -sS -X POST http://localhost:9999/mcp -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_health","arguments":{}}}' | head -c 200
# point agy at the sidecar URL, run the same prompt, and confirm it returns Operational
```

Expected: both `get_health` calls return `Status: Operational`; `agy` reports the same.

- [ ] **Step 4: Confirm no new disconnect lines**

```bash
tail -40 /home/sebastian/monitor/monitor_dm_system/.lain/server.log | sed -E 's/\x1b\[[0-9;]*m//g' | grep -E 'FileWatcher|channel disconnected|failed to watch' | tail -10
```

Expected: no new `failed to watch` or `channel disconnected` lines; the existing `watching 364 directories` line stays.

- [ ] **Step 5: Do not commit or push

No `git commit`, `git push`, `git reset`, or other mutations without explicit authorization. Record the live evidence in the SDD plan workspace when prompted.
