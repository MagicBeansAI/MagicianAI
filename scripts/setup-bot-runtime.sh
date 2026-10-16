#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
make -C "$ROOT_DIR/skillshub" setup-wizard-bots \
  DATA_ROOT="${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}" \
  SCOPE="${MAGICIAN_SETUP_SCOPE:-anonymous/default}"
