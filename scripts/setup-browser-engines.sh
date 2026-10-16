#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "$(uname -s)" == Darwin ]]; then
  export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
fi
make -C "$ROOT_DIR/skillshub" setup-wizard-browser-engines \
  DATA_ROOT="${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}" \
  SCOPE="${MAGICIAN_SETUP_SCOPE:-anonymous/default}"
