#!/usr/bin/env bash
# Compatibility entrypoint. New automation should call ensure-connect-access.sh.
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec bash "$ROOT_DIR/scripts/ensure-connect-access.sh" "$@"
