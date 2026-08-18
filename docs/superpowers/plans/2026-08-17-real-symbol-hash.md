# Real SymbolHash — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Compute SymbolHash from the actual symbol body (the tree-sitter-extracted definition) instead of the placeholder `[0u8; 32]`, so symbol-level claims survive index rebuilds.

**Architecture:** `OccupancyMap::claim` for symbol-level claims computes the hash by reading the file from disk and feeding each symbol's byte range to a tree-sitter extractor (or BLAKE3 of the range bytes). Falls back to `SymbolHash::zero()` if the file is unreadable or the symbol body is empty. The hash is then serialized into the persisted `PresenceState` JSON so claims survive `lain` restarts.

**Tech Stack:** Rust 1.75+ (existing). Tree-sitter is already in deps via `src/server/treesitter.rs`. No new deps.

**Branch:** `main` at `/home/sebastian/lain`. After PRs 14 + 15 (head `ed86463`). 495 tests pass.

---

## Global Constraints

- Branch: main
- No new Cargo deps
- Existing tests (495) must continue to pass
- Backwards-compatible: claims that already exist with `SymbolHash::zero()` continue to work; the new hash is computed on `claim()` calls
- Single commit (1 task)

---

## File Structure (final)

```
src/server/presence.rs                       (modify: compute real SymbolHash from file bytes; persist via serde)

tests/presence.rs                            (modify: add 1-2 tests for the new behavior)
```

(No `src/server/treesitter.rs` changes — use the existing `extract_definitions` from PR 15's `detect_overlap` flow.)

---

## Task 1: Real SymbolHash from file bytes

**Files:**
- Modify: `src/server/presence.rs`
- Test: append to `tests/presence.rs`

**Interfaces:**
- `Claim.content_hash: Option<SymbolHash>` — populated as `Some(SymbolHash::from_bytes(body))` where `body` is the byte range of the symbol's definition in the file, extracted via the existing tree-sitter `extract_definitions` path.
- `SymbolHash::from_bytes(&[u8]) -> Self` — wraps BLAKE3-256 (already in `presence.rs`).

- [ ] **Step 1: Locate the existing `OccupancyMap::claim` symbol-level branch**

Run: `grep -n "claim\|symbol\|content_hash\|Some(SymbolHash::zero)" /home/sebastian/lain/src/server/presence.rs | head -10`

- [ ] **Step 2: Write the failing test**

Append to `tests/presence.rs`:

```rust
#[test]
fn symbol_level_claim_records_nonzero_content_hash() {
    let occ = OccupancyMap::new();
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("auth.rs");
    std::fs::write(&path, "pub fn login() -> &'static str { \"A\" }\n").unwrap();
    let agent = AgentId("alice".into());
    let req = ClaimRequest {
        path: path.clone(),
        symbols: vec!["login".into()],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
    };
    occ.claim(&agent, vec![req]);
    let claims = occ.list_for_agent(&agent);
    assert_eq!(claims.len(), 1);
    let hash = claims[0].content_hash.expect("symbol-level claim must have content_hash");
    // The hash must be non-zero (the placeholder), and re-computing the same
    // body must yield the same hash.
    assert_ne!(hash, SymbolHash::zero());
    let again = SymbolHash::from_bytes(b"pub fn login() -> &'static str { \"A\" }\n");
    assert_eq!(hash, again);
}

#[test]
fn symbol_level_claim_hash_changes_when_body_changes() {
    let occ = OccupancyMap::new();
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("auth.rs");
    std::fs::write(&path, "pub fn login() -> &'static str { \"A\" }\n").unwrap();
    let agent = AgentId("alice".into());
    occ.claim(&agent, vec![ClaimRequest {
        path: path.clone(),
        symbols: vec!["login".into()],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
    }]);
    let hash1 = occ.list_for_agent(&agent)[0].content_hash.unwrap();

    std::fs::write(&path, "pub fn login() -> &'static str { \"B\" }\n").unwrap();
    let agent2 = AgentId("alice".into());
    occ.claim(&agent2, vec![ClaimRequest {
        path: path.clone(),
        symbols: vec!["login".into()],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
    }]);
    let hash2 = occ.list_for_agent(&agent)[0].content_hash.unwrap();

    assert_ne!(hash1, hash2);
}
```

- [ ] **Step 3: Implement the hash in `OccupancyMap::claim`**

Find the symbol-level branch in `OccupancyMap::claim` (where `req.symbols` is non-empty). Where the current code writes `Some(SymbolHash::zero())`, replace with a real hash:

```rust
let content_hash = compute_symbol_hash(&req.path, req.symbols.first().map(|s| s.as_str()).unwrap_or(""));
// content_hash is Option<SymbolHash>; None if file/symbol not extractable
```

Where `compute_symbol_hash` reads the file (or uses the tree-sitter extractor), finds the symbol's byte range, and returns `Some(SymbolHash::from_bytes(body))`. Falls back to `Some(SymbolHash::zero())` or `None` if the symbol can't be found in the file.

Add a private helper at the bottom of `presence.rs`:

```rust
fn compute_symbol_hash(path: &Path, symbol: &str) -> Option<SymbolHash> {
    let bytes = std::fs::read(path).ok()?;
    let src = String::from_utf8(bytes.clone()).ok()?;
    let defs = crate::server::treesitter::extract_definitions(&src);
    let body = defs.into_iter().find(|d| d.name == symbol)?;
    Some(SymbolHash::from_bytes(body.bytes.as_bytes()))
}
```

(The exact field names on the tree-sitter `Definition` struct may differ — verify by reading `src/server/treesitter.rs`. Adapt accordingly.)

- [ ] **Step 4: Run the tests**

Run: `cd /home/sebastian/lain && export PATH=/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH && cargo test --test presence 2>&1 | tail -5`
Expected: 36/36 pass (34 prior + 2 new).

- [ ] **Step 5: Run full suite**

Run: `cd /home/sebastian/lain && export PATH=/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH && cargo test --lib 2>&1 | tail -3`
Expected: 374 lib pass + 0 fail (3 pre-existing CLI failures unrelated).

- [ ] **Step 6: Commit**

```bash
cd /home/sebastian/lain
git add src/server/presence.rs tests/presence.rs
git commit -m "feat(presence): real SymbolHash from file bytes — symbol claims survive rebuilds"
```

---

## Self-Review

**Spec coverage:**
- SymbolHash populated from file bytes via tree-sitter extractor → Task 1 ✓
- Symbol-level claims carry the new hash → Task 1 ✓
- Persisted via existing JSON serialization → Task 1 (auto-included since `SymbolHash` is already `Serialize`/`Deserialize`)

**No placeholders.**

**Type consistency:**
- `SymbolHash::from_bytes(&[u8])` is the existing constructor.
- `Claim.content_hash: Option<SymbolHash>` is the existing field.

**Coverage gaps:** none for this PR's scope.

---

## Execution Handoff

Plan complete and saved to `/home/sebastian/lain/docs/superpowers/plans/2026-08-17-real-symbol-hash.md`. 1 task, 1 commit.

Two execution options:

**1. Subagent-Driven (recommended)** — dispatch a single subagent.

**2. Inline Execution** — execute directly.

Which approach?
