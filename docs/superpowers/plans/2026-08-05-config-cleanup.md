# Config Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove dead/orphaned config artifacts and fix version drift across the LAIN repo, leaving load-bearing config untouched.

**Architecture:** Four small, independently-reviewable changes — (1) tracked placeholder deletion, (2) tracked manifest edits for version consistency, (3) `.gitignore` updates, (4) untracked artifact deletion — followed by a final verification pass.

**Tech Stack:** Git, Bash, Python (`python3` for JSON validation), Cargo (sanity build), `scripts/pre-flight-check.sh` (version consistency gate).

## Global Constraints

- Source-of-truth version: `Cargo.toml` `version = "0.3.0"`. All other version fields must match.
- Branch: `claude/ieee-830-srs-spec-m3ajim`. All commits land here.
- Out of scope (do NOT touch): `.claude/settings.local.json`, anything under `hooks/`, tracked HTML files in `docs/srs/presentacion/`, untracked SRS work-in-progress files (`SRS-final.md`, `SRS-LAIN-IEEE830.pdf`, `anexos/G-...`).
- Conventional commits matching the project's existing style (e.g. `chore:`, `fix:`, `docs:`).
- `.gitignore` patterns must not conflict with any tracked file (verified before adding).
- Working directory: `/home/sebastian/lain`. All paths in commands are relative to this unless noted.

---

## Task 1: Remove tracked `toolchains/zig` placeholder

**Files:**
- Delete: `toolchains/zig` (tracked, 10 bytes, content = literal text `build.zig`)

**Interfaces:**
- Consumes: nothing
- Produces: `toolchains/` directory contains only `toolchains/README.md` after the task.

- [ ] **Step 1: Verify the file is tracked and unreferenced**

Run:
```bash
git ls-files toolchains/zig
```
Expected: outputs `toolchains/zig`.

Run:
```bash
grep -r "toolchains/zig" src/ scripts/ docs/ 2>/dev/null
```
Expected: empty output (no code references this path).

- [ ] **Step 2: Verify nothing in `toolchains/` references `zig`**

Run:
```bash
grep -i "zig" toolchains/README.md 2>/dev/null
```
Expected: empty output. If non-empty, STOP and ask the user — the README may need updating first.

- [ ] **Step 3: Delete the file via `git rm`**

Run:
```bash
git rm toolchains/zig
```
Expected: prints `rm 'toolchains/zig'`.

- [ ] **Step 4: Verify the deletion**

Run:
```bash
git ls-files toolchains/
```
Expected output (exact, single line):
```
toolchains/README.md
```

Run:
```bash
test ! -f toolchains/zig && echo "removed"
```
Expected: prints `removed`.

- [ ] **Step 5: Commit**

Run:
```bash
git commit -m "chore: remove toolchains/zig placeholder file"
```
Expected: one new commit on `claude/ieee-830-srs-spec-m3ajim`.

---

## Task 2: Fix version drift in `server.json` and `npm-shim/package.json`

**Files:**
- Modify: `server.json` lines 5 and 36
- Modify: `npm-shim/package.json` line 3

**Interfaces:**
- Consumes: `Cargo.toml` version `0.3.0` (source of truth)
- Produces: all version fields in the listed files equal `0.3.0`; both files still parse as valid JSON.

- [ ] **Step 1: Verify current state**

Run:
```bash
grep -n '"version"' server.json npm-shim/package.json
```
Expected: shows `0.2.0` in three lines (server.json:5, server.json:36, npm-shim/package.json:3).

- [ ] **Step 2: Edit `server.json` line 5**

The file uses 2-space indent. The current line is:
```
  "version": "0.2.0",
```

Open `server.json` and change that line to:
```
  "version": "0.3.0",
```

- [ ] **Step 3: Edit `server.json` line 36**

The second occurrence is inside the `packages` array. The current line is:
```
      "version": "0.2.0",
```

Open `server.json` and change that line to:
```
      "version": "0.3.0",
```

- [ ] **Step 4: Edit `npm-shim/package.json` line 3**

The current line is:
```
  "version": "0.2.0",
```

Open `npm-shim/package.json` and change that line to:
```
  "version": "0.3.0",
```

- [ ] **Step 5: Verify no `0.2.0` remains**

Run:
```bash
grep -n '"0\.2\.0"' server.json npm-shim/package.json
```
Expected: empty output. If non-empty, fix the remaining occurrence and re-run.

- [ ] **Step 6: Verify both files still parse as valid JSON**

Run:
```bash
python3 -c "import json; json.load(open('server.json')); print('server.json: ok')"
python3 -c "import json; json.load(open('npm-shim/package.json')); print('npm-shim/package.json: ok')"
```
Expected: both print `ok`.

- [ ] **Step 7: Verify all manifests are at `0.3.0`**

Run:
```bash
grep -H '"version"\|version ' server.json npm-shim/package.json Formula/lain.rb Cargo.toml
```
Expected: every line shows `0.3.0`. (`Cargo.toml` uses unquoted key, others use JSON quoted key.)

- [ ] **Step 8: Run the version check from `pre-flight-check.sh` in isolation**

The script checks `Cargo.toml` ↔ `server.json` ↔ `README.md`. Extract just that check:

Run:
```bash
VERSION=$(grep '^version = "' Cargo.toml | head -1 | sed 's/version = "\([^"]*\)"/\1/')
echo "Source-of-truth version: $VERSION"
grep -q "\"version\": \"$VERSION\"" server.json && echo "server.json: matches" || echo "server.json: MISMATCH"
grep -q "v$VERSION" README.md && echo "README.md: matches" || echo "README.md: MISMATCH (or no v-prefixed version in README)"
```
Expected: `Source-of-truth version: 0.3.0`, `server.json: matches`. The README check may pass or warn; both are acceptable.

- [ ] **Step 9: Commit**

Run:
```bash
git add server.json npm-shim/package.json
git commit -m "fix: bump server.json and npm-shim package.json to 0.3.0"
```
Expected: one new commit on `claude/ieee-830-srs-spec-m3ajim`.

---

## Task 3: Add `.gitignore` patterns for build artifacts

**Files:**
- Modify: `.gitignore` (append three patterns)

**Interfaces:**
- Consumes: existing `.gitignore` content (read-only at start of task)
- Produces: `.gitignore` that ignores `__pycache__/`, `*.pyc`, and `_*.html`, without conflicting with any tracked file.

- [ ] **Step 1: Confirm no tracked file matches the new patterns**

Run:
```bash
git ls-files | grep -E '(^|/)__pycache__/|(^|/)\.pyc$|(^|/)_[^/]*\.html$' || echo "no conflicts"
```
Expected: prints `no conflicts`.

If anything is printed, STOP and ask the user — a tracked file would be ignored by the new patterns.

- [ ] **Step 2: Read current `.gitignore` to find insertion point**

Run:
```bash
cat -n .gitignore
```

Locate the Python-cache section. It currently contains:
```
# Python
.mypy_cache/
.pytest_cache/
```

- [ ] **Step 3: Append the new patterns**

Open `.gitignore` and replace the Python section so it reads:
```
# Python
__pycache__/
*.pyc
.mypy_cache/
.pytest_cache/
```

(Insert `__pycache__/` and `*.pyc` immediately under the `# Python` comment, before the existing `.mypy_cache/` and `.pytest_cache/` lines. Keep the existing two lines.)

- [ ] **Step 4: Verify the patterns work against the artifacts we plan to delete**

Run:
```bash
git check-ignore -v docs/srs/final/__pycache__/foo.pyc docs/srs/final/_doc.html
```
Expected: two lines, one per path, each showing the matching `.gitignore` pattern and line number.

- [ ] **Step 5: Verify nothing tracked is newly ignored**

Run:
```bash
git ls-files -ci --exclude-standard | head -20
```
Expected: empty output, or only files that were already ignored before this task.

(If non-empty and they look newly ignored, STOP — the pattern is too broad.)

- [ ] **Step 6: Commit**

Run:
```bash
git add .gitignore
git commit -m "chore: ignore python build artifacts and underscore-prefixed renders"
```
Expected: one new commit on `claude/ieee-830-srs-spec-m3ajim`.

---

## Task 4: Remove untracked build artifacts and stray PDF

**Files:**
- Delete (untracked): `docs/srs/final/` (entire directory)
- Delete (untracked): `docs/SRS - DataPort - Gabriel Aillapán.docx (1) (3) (1).pdf`

**Interfaces:**
- Consumes: confirmation that both targets are untracked.
- Produces: working tree no longer contains either target. No commit needed (both are untracked). However, a final `git status` must show no accidental removals of tracked files.

- [ ] **Step 1: Confirm both targets are untracked**

Run:
```bash
git ls-files docs/srs/final/ 2>&1
git ls-files 'docs/SRS - DataPort - Gabriel Aillapán.docx (1) (3) (1).pdf' 2>&1
```
Expected: both produce empty output (no tracked files match).

If either path is tracked, STOP and ask the user — the spec says these are untracked.

- [ ] **Step 2: Capture current `git status` snapshot for later comparison**

Run:
```bash
git status --porcelain > /tmp/before-cleanup.txt
cat /tmp/before-cleanup.txt
```
Expected: shows the existing uncommitted `M` entries in `docs/srs/` and the pre-existing untracked items (`SRS-final.md`, `SRS-LAIN-IEEE830.pdf`, `anexos/G-estado-real-lain.md`). These should remain unchanged after this task.

- [ ] **Step 3: Delete the build-artifact directory**

Run:
```bash
rm -rf docs/srs/final/
```

Verify:
```bash
test ! -d docs/srs/final && echo "removed"
test ! -e 'docs/SRS - DataPort - Gabriel Aillapán.docx (1) (3) (1).pdf' && echo "stray not yet deleted"
```
Expected: first line prints `removed`. Second prints `stray not yet deleted` (we haven't deleted it yet).

- [ ] **Step 4: Delete the stray PDF**

Run:
```bash
rm 'docs/SRS - DataPort - Gabriel Aillapán.docx (1) (3) (1).pdf'
```

Verify:
```bash
test ! -e 'docs/SRS - DataPort - Gabriel Aillapán.docx (1) (3) (1).pdf' && echo "removed"
```
Expected: prints `removed`.

- [ ] **Step 5: Verify no tracked files were lost**

Run:
```bash
git status --porcelain > /tmp/after-cleanup.txt
diff /tmp/before-cleanup.txt /tmp/after-cleanup.txt
```
Expected: diff shows only that `docs/srs/final/` and `docs/SRS - DataPort - Gabriel Aillapán.docx (1) (3) (1).pdf` are no longer in the untracked list. All other lines (the `M` entries in `docs/srs/`, the other untracked SRS files) must remain identical.

- [ ] **Step 6: Confirm no commit is needed**

Run:
```bash
git diff --stat
git diff --cached --stat
```
Expected: both produce empty output. (The deletions are untracked, so there's nothing to stage or commit.)

If any path appears in `git status --porcelain` as `D` (deletion of a tracked file), STOP — something went wrong; investigate before continuing.

---

## Task 5: Final verification

**Files:** none — this task only runs verification commands.

- [ ] **Step 1: All eight spec verification checks**

Run each command and confirm the expected output:

```bash
# 1. toolchains/ only contains README.md
git ls-files toolchains/

# 2. No "0.2.0" in tracked manifests
grep -n '"0\.2\.0"' server.json npm-shim/package.json; echo "exit=$?"

# 3. Each manifest has exactly one version line
for f in server.json npm-shim/package.json Formula/lain.rb Cargo.toml; do
  printf "%s: %s version line(s)\n" "$f" "$(grep -c '^[[:space:]]*\"version\"\|^version ' "$f")"
done

# 4. docs/srs/final/ is gone and untracked
git ls-files docs/srs/final/
test ! -d docs/srs/final && echo "working tree clean"

# 5. Stray PDF is gone
test ! -e 'docs/SRS - DataPort - Gabriel Aillapán.docx (1) (3) (1).pdf' && echo "stray removed"

# 6. .gitignore patterns apply
git check-ignore -v docs/srs/final/__pycache__/foo.pyc docs/srs/final/_doc.html

# 7. Cargo still builds (sanity)
cargo build 2>&1 | tail -5

# 8. Pre-flight check passes (version consistency)
bash -c 'VERSION=$(grep "^version = \"" Cargo.toml | head -1 | sed "s/version = \"\([^\"]*\)\"/\1/"); \
  grep -q "\"version\": \"$VERSION\"" server.json && echo "server.json: matches $VERSION" || echo "server.json: MISMATCH"; \
  grep -q "v$VERSION" README.md && echo "README.md: matches v$VERSION" || echo "README.md: MISMATCH (acceptable if no v-prefixed version exists)"'
```

Expected:
1. prints exactly `toolchains/README.md`
2. prints `exit=1` (grep found nothing)
3. prints `1 version line(s)` for all four files
4. prints `working tree clean`
5. prints `stray removed`
6. prints two `gitignore`-match lines
7. ends with `Compiling lain` then `Finished` lines, no errors
8. prints `server.json: matches 0.3.0`; README line either matches or warns — both acceptable

- [ ] **Step 2: Commit any tracked changes**

Run:
```bash
git status --porcelain
```

Expected: only `M` entries in `docs/srs/` (pre-existing uncommitted work) and the untracked SRS work-in-progress files. No `D`, `M`, or `A` entries related to this cleanup.

If this task's changes appear (e.g. accidentally added a file), commit them with an appropriate message — but in the normal flow there should be nothing left to commit.

- [ ] **Step 3: Report results**

Print a summary line for each of the four cleanup areas:
- `toolchains/zig`: removed in commit `<hash>`
- `server.json` + `npm-shim/package.json`: bumped to 0.3.0 in commit `<hash>`
- `.gitignore`: extended in commit `<hash>`
- Untracked artifacts (`docs/srs/final/`, stray PDF): removed (no commit needed)

Use:
```bash
git log --oneline -5
```
to fetch the commit hashes.

---

## Out of scope (do not implement here)

- Adding a pre-flight check for `npm-shim/package.json` version drift — listed as a follow-up in the design spec.
- Resolving the untracked SRS files (`SRS-final.md`, `SRS-LAIN-IEEE830.pdf`, `anexos/G-...`) — separate decision needed.
- Any work on the four `hooks/` files — explicitly excluded.
