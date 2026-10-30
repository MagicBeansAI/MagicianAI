#!/usr/bin/env bash
# Native LaunchAgent entrypoint: restore required host model services on login.
set -euo pipefail
PREFIX="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PREFIX"
unset RUST_MIN_STACK
bash "$PREFIX/scripts/run-ollama.sh"
# LaunchAgent arguments after this script (written by Magican Desktop:
# --magician-config, --desktop-app, --desktop-log) go to the supervisor.
exec "$PREFIX/magic-supervisor.bin" "$@"
