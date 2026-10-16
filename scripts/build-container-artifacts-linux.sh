#!/usr/bin/env bash
# Called by build-container-artifacts.py, in an isolated Linux source checkout.
set -euo pipefail
source_dir=$1
cache_dir=$2
output_dir=$3
test "$(uname -s)" = Linux
export CARGO_HOME="$cache_dir/cargo-home"
export CARGO_TARGET_DIR="$cache_dir/target"
export TMPDIR="$cache_dir/tmp"
export npm_config_cache="$cache_dir/npm"
export CARGO_INCREMENTAL=0
export CMAKE_BUILD_PARALLEL_LEVEL="$CARGO_BUILD_JOBS"
export NODE_OPTIONS=--max-old-space-size=3072
mkdir -p "$CARGO_HOME" "$CARGO_TARGET_DIR" "$TMPDIR" "$output_dir/binaries"

# Keep node_modules in this Linux-only checkout; skip npm ci when its inputs
# and Node/npm versions are unchanged. Downloads also persist in the cache.
install_node_deps() {
    local directory=$1
    local fingerprint
    fingerprint=$(cd "$directory"; { sha256sum package.json package-lock.json; node --version; npm --version; } | sha256sum)
    if [ ! -d "$directory/node_modules" ] || [ ! -f "$directory/.magician-npm-fingerprint" ] || \
       [ "$(cat "$directory/.magician-npm-fingerprint")" != "$fingerprint" ]; then
        printf '%s\n' incomplete > "$directory/.magician-npm-fingerprint"
        (cd "$directory"; npm ci --ignore-scripts --no-audit --no-fund)
        printf '%s\n' "$fingerprint" > "$directory/.magician-npm-fingerprint"
    fi
}
install_node_deps "$source_dir/sdk/typescript"
install_node_deps "$source_dir/ui/unified-ui"
cd "$source_dir/ui/unified-ui"
npx --no-install svelte-kit sync
npm run build
cp -R build "$output_dir/ui"

# Sequential with the UI heap; no browser/skill installation or image build.
cd "$source_dir"
make FAST=0 CARGO_TARGET_DIR="$CARGO_TARGET_DIR" build-container-runtime
for binary in magician magicutor magic-supervisor; do
    name=$binary
    if [ "$binary" != magic-supervisor ]; then name="$binary.bin"; fi
    strip -o "$output_dir/binaries/$name" "$CARGO_TARGET_DIR/release/$binary"
done
