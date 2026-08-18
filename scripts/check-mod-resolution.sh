#!/usr/bin/env bash
# Pre-push check: every `mod foo;` declaration in src/ must resolve
# to a committed file at <dir-of-mod-statement>/foo.rs or
# <dir-of-mod-statement>/foo/mod.rs. Catches the failure mode local
# tests structurally cannot — the mod compiles locally because the
# file is in the working tree, but the commit doesn't include it, so
# a clean clone or CI checkout fails with E0583.

set -e

# Find untracked .rs files via git status (mtime-based heuristics
# lie after rebase / submodule updates).
untracked=$(git status --porcelain -- src/ 2>/dev/null \
    | grep '^??' \
    | grep '\.rs$' \
    | awk '{print $2}')
if [ -n "$untracked" ]; then
    echo "Untracked .rs files (will fail in a clean checkout):"
    echo "$untracked" | sed 's/^/  /'
    exit 1
fi

# Resolve every `mod foo;` / `pub mod foo;` declaration in src/. Each
# `mod` lives inside a parent file; the target is at
# <dir-of-containing-file>/foo.rs or <dir-of-containing-file>/foo/mod.rs.
#
# Stream the grep output through a sentinel-guarded pipeline so that
# any error messages from this script (printed to stdout by an
# earlier failing run) don't get re-parsed by the loop.
fail=0
while IFS=: read -r file lineno stmt; do
    # Skip lines that don't look like grep output: the file path
    # must end in `.rs`.
    case "$file" in
        *.rs) ;;
        *) continue ;;
    esac
    name=$(echo "$stmt" | sed -E 's/.*mod ([a-zA-Z0-9_]+);.*/\1/')
    [ -z "$name" ] && continue
    parent_dir=$(dirname "$file")
    # Module entry points (`mod.rs` and `lib.rs` at the crate root)
    # are themselves the module — their sub-modules live in the
    # same directory as the file. For all other files (`X.rs`), the
    # sub-modules live in a sub-directory named after the file.
    base=$(basename "$file")
    if [ "$base" = "mod.rs" ] || [ "$base" = "lib.rs" ]; then
        mod_dir="$parent_dir"
    else
        mod_dir="$parent_dir/${base%.rs}"
    fi
    candidate_a="$mod_dir/$name.rs"
    candidate_b="$mod_dir/$name/mod.rs"
    if [ ! -f "$candidate_a" ] && [ ! -f "$candidate_b" ]; then
        echo "mod declaration without matching file: $file: pub mod $name; (looked for $candidate_a or $candidate_b)" >&2
        fail=1
    fi
done < <(cd src && find . -name '*.rs' -print0 | xargs -0 grep -nE '^\s*(pub\s+)?mod\s+[a-zA-Z0-9_]+\s*;' | sed 's|^\./|src/|')

if [ "$fail" -ne 0 ]; then
    exit 1
fi

echo "ok: all mod declarations resolve"
