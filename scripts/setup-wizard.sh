#!/usr/bin/env bash
# setup-wizard.sh - the guided installer.
#
# Its job is to be runnable by someone who has just cloned the repository and
# has no idea what a cargo profile is. So: no arguments required, a release
# build cached between runs, and every failure explained rather than dumped.
#
# The wizard itself decides what to install. This script only makes sure there
# is a wizard to run, and hands it the arguments it was given.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

err() { printf '  ERROR %s\n' "$*" >&2; }

if ! command -v cargo >/dev/null 2>&1; then
  err "Rust is not installed, and the wizard is a Rust program."
  err "Install it from https://rustup.rs and run this again."
  exit 1
fi

# The same build directory the Makefile uses when it is set, so running the
# wizard does not fork a second multi-gigabyte target tree.
BUILD_DIR="${CARGO_TARGET_DIR:-$ROOT_DIR/target}"
BINARY="$BUILD_DIR/release/magician-setup"

# Rebuild when the binary is missing or older than any source it is built from.
# `cargo build` would decide this correctly on its own, but it prints a wall of
# progress on every run; this keeps a warm re-run silent.
needs_build=0
if [[ ! -x "$BINARY" ]]; then
  needs_build=1
else
  while IFS= read -r source; do
    [[ "$source" -nt "$BINARY" ]] && { needs_build=1; break; }
  done < <(find "$ROOT_DIR/magician-setup/src" "$ROOT_DIR/magician-components/src" \
             -type f \( -name '*.rs' -o -name '*.yaml' \) 2>/dev/null)
fi

if [[ "$needs_build" -eq 1 ]]; then
  echo "Building the setup wizard (first run takes a few minutes)…"
  if ! CARGO_TARGET_DIR="$BUILD_DIR" cargo build --release -p magician-setup --quiet; then
    err "the wizard did not build. The output above says why."
    exit 1
  fi
fi

exec "$BINARY" "$@"
