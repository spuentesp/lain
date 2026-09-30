#!/usr/bin/env bash
# Measure peak RSS for the §8.5 memory ceiling and emit
# `1.5 × the larger` of the §15.1 fixture peak and the tokio + bytes
# federation snapshot peak to the output file.
#
# Phases:
#   1. Build the §15.1 fixture (no network). Run
#      `measure_snapshot_memory` against `orders/`, `billing/`,
#      `reports/`, `platform/`.
#   2. Clone `tokio-rs/bytes` and `tokio-rs/tokio` (shallow,
#      network needed). Run the same binary against both.
#
# The script then takes `1.5 × max(phase1_peak, phase2_peak)` and
# writes the result to the output file. This is what the
# committed `tests/fixtures/contracts/memory_ceiling.txt` carries.
#
# Network is required only for phase 2. The CI gating that uses
# this ceiling runs on the `main` full battery per §8.5; the
# default `cargo test` cycle does NOT run this script (it's
# gated behind `LAIN_RUN_MEMORY_MEASUREMENT=1`).
#
# Usage:
#   scripts/measure_snapshot_memory.sh <output-file>

set -euo pipefail

OUT="${1:?usage: measure_snapshot_memory.sh <output-file>}"
ROOT="$(mktemp -d -t lain-snapshot-mem-XXXXXX)"
trap 'rm -rf "$ROOT"' EXIT

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

run_binary() {
    # Args: list of repo paths. Echoes the peak_bytes value on stdout.
    cargo run --quiet --release -p lain --bin measure_snapshot_memory -- "$@" 2>/dev/null \
        | grep -oE 'peak_bytes=[0-9]+' | head -1 | sed 's/peak_bytes=//'
}

echo "[mem] phase 1: §15.1 fixture"
FIXTURE_DIR="$ROOT/fixture"
bash "$REPO_ROOT/scripts/contracts-fixture.sh" "$FIXTURE_DIR" >/dev/null 2>&1
FIXTURE_PEAK_BYTES="$(run_binary \
    "$FIXTURE_DIR/orders" \
    "$FIXTURE_DIR/billing" \
    "$FIXTURE_DIR/reports" \
    "$FIXTURE_DIR/platform")"
FIXTURE_PEAK_KB=$(( FIXTURE_PEAK_BYTES / 1024 ))
echo "[mem] fixture peak: ${FIXTURE_PEAK_KB} KiB"

echo "[mem] phase 2: cloning tokio + bytes"
mkdir -p "$ROOT/repos"
git clone --depth 1 --filter=blob:none https://github.com/tokio-rs/bytes.git "$ROOT/repos/bytes" >/dev/null 2>&1
git clone --depth 1 --filter=blob:none https://github.com/tokio-rs/tokio.git "$ROOT/repos/tokio" >/dev/null 2>&1

echo "[mem] phase 2: measuring"
TOKIO_PEAK_BYTES="$(run_binary \
    "$ROOT/repos/bytes" \
    "$ROOT/repos/tokio")"
if [ -z "${TOKIO_PEAK_BYTES:-}" ]; then
    echo "[mem] ERROR: measure_snapshot_memory did not report peak_bytes" >&2
    exit 2
fi
TOKIO_PEAK_KB=$(( TOKIO_PEAK_BYTES / 1024 ))
echo "[mem] tokio+bytes peak: ${TOKIO_PEAK_KB} KiB"

# §8.5: 1.5 × the larger of the two phases.
LARGEST_KB=$(( FIXTURE_PEAK_KB > TOKIO_PEAK_KB ? FIXTURE_PEAK_KB : TOKIO_PEAK_KB ))
CEILING_BYTES=$(( LARGEST_KB * 1024 * 3 / 2 ))

cat >"$OUT" <<EOF
memory_ceiling_bytes: ${CEILING_BYTES}
measurement_target: §15.1 fixture + tokio+bytes federation snapshots
measurement_factor: 1.5
phase1_kb: ${FIXTURE_PEAK_KB}
phase2_kb: ${TOKIO_PEAK_KB}
EOF
echo "[mem] wrote $OUT (${CEILING_BYTES} bytes = $(( CEILING_BYTES / 1024 / 1024 )) MiB)"
