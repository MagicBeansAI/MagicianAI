#!/usr/bin/env bash
# Cross-compile the Linux application payload directly on a macOS host.
set -euo pipefail

source_dir=$1
cache_dir=$2
output_dir=$3
rust_target=$4
zig_target=$5
jobs=$6

test "$(uname -s)" = Darwin
for program in cargo cargo-zigbuild zig rustc rustup node npm npx make python3 shasum; do
    command -v "$program" >/dev/null || { echo "missing program: $program" >&2; exit 1; }
done
llvm_strip=${LLVM_STRIP:-}
if [ -z "$llvm_strip" ]; then llvm_strip=$(command -v llvm-strip || true); fi
for candidate in /opt/homebrew/opt/llvm/bin/llvm-strip /usr/local/opt/llvm/bin/llvm-strip; do
    if [ -z "$llvm_strip" ] && [ -x "$candidate" ]; then llvm_strip=$candidate; fi
done
if [ -z "$llvm_strip" ]; then
    echo "missing program: llvm-strip (install Homebrew llvm or set LLVM_STRIP)" >&2
    exit 1
fi
if ! rustup target list --installed | grep -Fxq "$rust_target"; then
    echo "missing Rust target: $rust_target; run: rustup target add $rust_target" >&2
    exit 1
fi

export CARGO_HOME="$cache_dir/cargo-home"
export CARGO_TARGET_DIR="$cache_dir/target-zig-$rust_target"
export CARGO_ZIGBUILD_CACHE_DIR="$cache_dir/cargo-zigbuild"
# sccache derives a Unix-domain socket below TMPDIR on macOS. Keep this path
# short enough for SUN_LEN while the Python driver keeps it on the cache disk.
export TMPDIR="${MAGICIAN_CONTAINER_BUILD_TMPDIR:?build helper must provide a short compiler temp directory}"
export npm_config_cache="$cache_dir/npm"
export CARGO_INCREMENTAL=0
export CARGO_BUILD_JOBS="$jobs"
export CMAKE_BUILD_PARALLEL_LEVEL="$jobs"
export CARGO_PROFILE_RELEASE_LTO=false
export NODE_OPTIONS=--max-old-space-size=3072
mkdir -p "$CARGO_HOME" "$CARGO_TARGET_DIR" "$CARGO_ZIGBUILD_CACHE_DIR" "$TMPDIR" "$output_dir/binaries"

# Keep host node_modules in this isolated snapshot. The fingerprint avoids a
# second npm install when neither inputs nor the host Node/npm versions changed.
install_node_deps() {
    local directory=$1
    local fingerprint
    fingerprint=$(cd "$directory"; { shasum -a 256 package.json package-lock.json; node --version; npm --version; } | shasum -a 256)
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

# Cargo-zigbuild supplies the target C/C++ compiler, archiver, linker and CMake
# toolchain for bundled DuckDB, SQLCipher and OpenSSL dependencies.
cd "$source_dir"
make FAST=0 CARGO_TARGET_DIR="$CARGO_TARGET_DIR" \
    CONTAINER_ZIG_TARGET="$zig_target" build-container-runtime-zig
for binary in magician magicutor magic-supervisor; do
    name=$binary
    if [ "$binary" != magic-supervisor ]; then name="$binary.bin"; fi
    "$llvm_strip" -o "$output_dir/binaries/$name" \
        "$CARGO_TARGET_DIR/$rust_target/release/$binary"
done
