# Coordination specs live in zoanthrope

The implementations that used to be modeled here — file-claim
arbitration (`OccupancyMap` / `FileOccupancy`), the filesystem lease
layer (`presence_lock`), and the agent rejoin protocol — moved out of
lain into **zoanthrope** (`github.com/spuentesp/zoanthrope`), which
owns the coordination core. Lain's remaining coordination surface is a
thin proxy (`src/server/coordination/`) over zoanthrope's CLI and MCP
contract; when zoanthrope is not installed the proxy fails soft to
advisory semantics (`"degraded": "zoanthrope-missing"` in the tool
responses) and never blocks.

## Where the specs are now

zoanthrope's `formal/` directory is the home of the coordination TLA+
trio (see its README "Formal guarantees", `formal/check.sh` /
`formal/MANIFEST`):

- **`PresenceClaims`** — claim arbitration: no conflicting holders,
  read co-holding, strongest-intent recording;
- **`FsLease` / `FsLeaseGuard`** — filesystem lease exclusion, the
  anti-clobbering core (move-aside-and-verify release, `O_EXCL`
  acquire guard);
- **`RejoinProtocol`** (zoanthrope's) — returning agents re-assert
  without breaking claims, own or foreign.

Verified upstream with TLC via `formal/check.sh` (11/11
configurations).

## Deleted from this directory

The lain-side copies of those specs were removed with their
implementations:

| Spec | Modeled (deleted code) |
|---|---|
| `PresenceClaims.tla` (+ `.cfg`, `_LastWins.cfg`) | `OccupancyMap` / `FileOccupancy` claim arbitration in `src/server/presence.rs` |
| `FsLease.tla` (+ `.cfg`, `_Excl.cfg`, `_ExclExpiry.cfg`) | `src/server/presence_lock.rs` fixed-path lease |
| `FsLeaseGuard.tla` (+ `.cfg`, `_Stall.cfg`, `_Unguarded.cfg`) | `presence_lock::try_lock` acquire guard |

**What stays here:** `RejoinProtocol*.tla` — despite the shared name,
this set models `FederatedIndex::rejoin_contracts_if_dirty` (the
*federation contract index* rebuild in
`src/server/federation/federated_index.rs` §9.1), which is lain code
and was not migrated. It is not the agent-rejoin protocol; the
agent-rejoin model is zoanthrope's `formal/RejoinProtocol.tla`.

## Conformance

Lain's proxy conforms to the zoanthrope specs: it delegates every
claim, release, rejoin, presence, and postboard operation to the
zoanthrope binary and adds no arbitration of its own. The one
addition — opaque `caller_stamps` (`symbol:<name>` fingerprints
computed by lain and verified by lain later) — is stored and returned
by zoanthrope without being interpreted, per the caller-stamps
contract in zoanthrope's README.
