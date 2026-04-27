# Lain - Development Guide

## Skills
- `/lain-skill` — LAIN-mcp agent strategy and tool usage guide

## Build & Test Commands
- Build: `cargo build`
- Release Build: `cargo build --release`
- Run: `cargo run --workspace <path>`
- Test: `cargo test`
- Lint: `cargo check`

## Architectural Principles (SOLID & DRY)
- **Modular Handlers**: All MCP tool logic must live in `src/tools/handlers/`.
- **Dispatcher Pattern**: `src/tools.rs` is a pure dispatcher; avoid putting logic there.
- **Asynchronous Flow**: Use `async/await` for all LSP and Graph operations.
- **Deterministic Identity**: Node IDs must be generated via UUID v5 using `(NodeType, Path, Name)`.
- **Persistence**: All knowledge must be stored in the petgraph bin file at `.lain/graph.bin`.

## Code Style
- Follow standard Rust idioms and `rustfmt`.
- Use `anyhow` and `LainError` for error handling.
- Use `tracing` macros (`info!`, `warn!`, `error!`, `debug!`) for logging.
- Prefer `parking_lot` for sync locks and `tokio::sync::Mutex` for async locks.
