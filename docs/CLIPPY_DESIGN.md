# Clippy cleanup design

The remaining non-mechanical warnings are API shape warnings. They are being
handled as two deliberate refactors instead of broad `allow` attributes.

## Shared result and dependency types

Repeated implementation-shaped types now have names at their ownership
boundary:

- `server::tools::UiSessionStore` and `UiLink` describe interactive UI state.
- `query::executor::TraversalResult` describes graph traversal output.
- `presence::OccupancySnapshot` documents the persisted occupancy tuple.
- test-only embedding caches use a local `EmbedderCache` alias.

These names make signatures readable and give future fields a single place to
change. They do not change wire formats or ownership semantics.

## Constructor and pipeline arguments

The constructor and ingestion warnings are intentionally deferred until their
inputs can be grouped without hiding lifecycle boundaries. The next refactor
should introduce two value objects:

`ToolContextDeps` for graph, overlay, language services, caches, and session
registries; and `IndexRequest` for repository path, revision, namespace,
resolver, and force mode. Each should have a constructor that validates its
invariants, then the existing functions can become thin compatibility wrappers
before the wrappers are removed in the next minor release.

This sequencing avoids a flag-day change across integration tests and keeps the
public tool behavior stable while the internal ownership model is clarified.
