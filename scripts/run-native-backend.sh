#!/usr/bin/env bash
# Native LaunchAgent entrypoint: restore required host model services on login.
set -euo pipefail
PREFIX="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PREFIX"
unset RUST_MIN_STACK
bash "$PREFIX/scripts/run-ollama.sh"
exec "$PREFIX/magic-supervisor.bin"
