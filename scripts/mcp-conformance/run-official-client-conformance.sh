#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
runner="$script_dir/node_modules/.bin/conformance"

if [[ ! -x "$runner" ]]; then
  echo "Official MCP conformance runner is not installed." >&2
  echo "Run: npm ci --prefix scripts/mcp-conformance --ignore-scripts" >&2
  exit 2
fi

# The official runner is locked by package-lock.json. The SDK checkout is locked to
# the dereferenced rmcp-v3.1.0 commit so a moved tag cannot change qualification.
readonly rust_sdk_commit="1f9358eddca42d3a510c70ae6446dd6548c7c856"
readonly cargo_root="${CARGO_TARGET_DIR:-${TMPDIR:-/tmp}/magician-cargo-target}"
readonly cache_dir="${MCP_CONFORMANCE_CACHE_DIR:-$cargo_root/mcp-conformance-sdk-cache}"
readonly artifact_root="${MCP_CONFORMANCE_ARTIFACT_DIR:-${TMPDIR:-/tmp}/magician-mcp-conformance}"

mkdir -p "$cache_dir" "$artifact_root"
run_dir="$(mktemp -d "$artifact_root/run.XXXXXX")"

echo "Official MCP client conformance artifacts: $run_dir"

"$runner" sdk "rust-sdk@$rust_sdk_commit" \
  --cache-dir "$cache_dir" \
  --build-cmd "CARGO_TARGET_DIR=target cargo build -p mcp-conformance" \
  --client-cmd "./target/debug/conformance-client" \
  --mode client \
  --suite all \
  --spec-version 2025-11-25 \
  --output "$run_dir/2025-11-25"

"$runner" sdk "rust-sdk@$rust_sdk_commit" \
  --cache-dir "$cache_dir" \
  --skip-build \
  --client-cmd "./target/debug/conformance-client" \
  --mode client \
  --suite all \
  --spec-version 2026-07-28 \
  --output "$run_dir/2026-07-28"

echo "Official MCP client conformance passed for 2025-11-25 and 2026-07-28."
