#!/usr/bin/env bash
# Shared helpers for e2e shell scripts.
#
# Sourced by every `tests/e2e/*.sh` that drives `lain` over stdio.
# Keeping the MCP protocol version lookup here means a single place to
# bump when the binary's negotiated version changes — no copy-pasted
# string in N fixture files.
#
# Usage:
#   source "$(dirname "$0")/lib.sh"
#   printf '{"jsonrpc":"2.0","method":"initialize","params":{"protocolVersion":"'$LAIN_MCP_PROTOCOL_VERSION'",...}}\n' | "$LAIN_BIN" mcp ...

# Resolve the protocol version from the binary under test. Cached so
# repeated `source lib.sh` calls don't re-invoke the binary. The
# fallback is the version we last verified by hand; if the binary
# suddenly can't be found or the flag isn't there, we'd rather fail
# visibly than silently negotiate the wrong version.
_lain_bin_for_proto() {
  local bin="${LAIN_BIN:-$(git rev-parse --show-toplevel 2>/dev/null)/target/debug/lain}"
  if [[ -x "$bin" ]]; then
    "$bin" --print-mcp-protocol-version 2>/dev/null
  fi
}

# Fallback: query the release binary if the debug one isn't built yet.
_lain_bin_for_proto_release() {
  local bin="${LAIN_BIN:-$(git rev-parse --show-toplevel 2>/dev/null)/target/release/lain}"
  if [[ -x "$bin" ]]; then
    "$bin" --print-mcp-protocol-version 2>/dev/null
  fi
}

if [[ -z "${LAIN_MCP_PROTOCOL_VERSION:-}" ]]; then
  LAIN_MCP_PROTOCOL_VERSION="$(_lain_bin_for_proto || _lain_bin_for_proto_release || true)"
  if [[ -z "$LAIN_MCP_PROTOCOL_VERSION" ]]; then
    echo "tests/e2e/lib.sh: could not determine MCP protocol version from any lain binary." >&2
    echo "Build first (cargo build or cargo build --release) or set LAIN_MCP_PROTOCOL_VERSION explicitly." >&2
    return 1 2>/dev/null || exit 1
  fi
fi

# Re-export so source'd callers see it.
export LAIN_MCP_PROTOCOL_VERSION

# Common ok/no/chk helpers — used by mcp-resolve.sh and any future
# script that wants them. Kept here so each test isn't a 12-line
# copy of the same trio. The unprefixed aliases (`ok`, `no`, `chk`)
# let existing scripts that call those names directly keep working
# unchanged after sourcing this file.
PASS=0
FAIL=0
_e2e_ok()  { printf "  \033[32mPASS\033[0m %s\n" "$1"; PASS=$((PASS + 1)); }
_e2e_no()  { printf "  \033[31mFAIL\033[0m %s\n     got: %s\n" "$1" "$2"; FAIL=$((FAIL + 1)); }
# Usage: _e2e_chk "name" "expected-substring" "actual-string"
_e2e_chk() {
  case "$3" in
    *"$2"*) _e2e_ok "$1" ;;
    *)      _e2e_no "$1" "$(printf '%s' "$3" | head -c 200)" ;;
  esac
}
ok()  { _e2e_ok  "$@"; }
no()  { _e2e_no  "$@"; }
chk() { _e2e_chk "$@"; }
export -f ok no chk _e2e_ok _e2e_no _e2e_chk
