#!/usr/bin/env bash
# Keep all generated firmware artifacts on the Makefile-selected build volume.
set -euo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
idf="${IDF_PATH:-$HOME/esp/esp-idf}"
out="${MAGESP_BUILD_DIR:-${CARGO_TARGET_DIR:?run through make build-magesp}/magesp}"
mkdir -p "$out/tmp"
out="$(cd "$out" && pwd)"
export TMPDIR="$out/tmp"
[ -f "$idf/export.sh" ] || { echo "Set IDF_PATH to ESP-IDF v5.5.1." >&2; exit 1; }
# export.sh installs no tools; it selects the already installed SDK/toolchain.
source "$idf/export.sh" >/dev/null
cd "$repo/magesp"
idf.py -B "$out" -D "SDKCONFIG=$out/sdkconfig" -D IDF_TARGET=esp32c6 reconfigure
# idf.py's Ninja helper has no job option in this SDK. Invoke the generated
# project directly so another agent's Rust build cannot multiply CPU pressure.
cmake --build "$out" -- -j1
echo "Firmware: $out/magesp.bin"
