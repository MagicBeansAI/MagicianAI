#!/usr/bin/env bash
# install.sh — one-time CloakBrowser binary fetch.
#
# `make setup-python` puts the cloakbrowser pip package into
# skillshub/.venv. This script then runs the binary downloader so the
# first real session doesn't have to pay the ~200MB download cost. Run
# after `make setup-python` (or as part of `make setup-cloak-browser`).
#
# Idempotent: re-running on an already-installed binary is a no-op.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VENV_PYTHON="$SCRIPT_DIR/../../.venv/bin/python"

if [[ ! -x "$VENV_PYTHON" ]]; then
  echo "  ✗ venv python not found at $VENV_PYTHON" >&2
  echo "    → run \`make setup-python\` from skillshub/ first" >&2
  exit 1
fi

if ! "$VENV_PYTHON" -c 'import cloakbrowser' >/dev/null 2>&1; then
  echo "  ✗ cloakbrowser package not importable from $VENV_PYTHON" >&2
  echo "    → check that skillshub/cloak-browser/SKILL.md has been included" >&2
  echo "      in the python_packages aggregation, then re-run setup-python" >&2
  exit 1
fi

echo "  Fetching CloakBrowser stealth Chromium binary (~200MB, one-time)..."
"$VENV_PYTHON" -m cloakbrowser install

echo "  Verifying binary..."
"$VENV_PYTHON" -c '
from cloakbrowser import binary_info
info = binary_info()
print("    version: ", info.get("version", "?"))
print("    platform:", info.get("platform", "?"))
print("    path:    ", info.get("binary_path", "?"))
print("    installed:", info.get("installed", False))
'
echo "  ✓ CloakBrowser binary ready"
