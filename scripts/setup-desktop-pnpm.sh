#!/usr/bin/env bash
# Provision the exact pnpm version declared by desktop/package.json into a
# checkout-local, gitignored tool directory. Node 25 no longer bundles Corepack,
# and global pnpm installations can drift independently of this repository.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PACKAGE_JSON="${DESKTOP_PACKAGE_JSON:-$ROOT/desktop/package.json}"
TOOL_ROOT="${DESKTOP_PNPM_TOOL_ROOT:-$ROOT/.cache/desktop-pnpm}"

if ! command -v node >/dev/null 2>&1; then
  echo "✗ node is required to read the desktop package-manager pin." >&2
  echo "  Run 'make setup-prerequisites' and retry." >&2
  exit 1
fi
if [ ! -f "$PACKAGE_JSON" ]; then
  echo "✗ desktop package manifest is missing: $PACKAGE_JSON" >&2
  exit 1
fi

PACKAGE_MANAGER="$(node -e '
const fs = require("fs");
const manifest = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
process.stdout.write(manifest.packageManager || "");
' "$PACKAGE_JSON")"
case "$PACKAGE_MANAGER" in
  pnpm@*) PNPM_VERSION="${PACKAGE_MANAGER#pnpm@}" ;;
  *)
    echo "✗ desktop/package.json must declare an exact pnpm packageManager pin; found '$PACKAGE_MANAGER'." >&2
    exit 1
    ;;
esac
case "$PNPM_VERSION" in
  ""|*[!0-9.]*)
    echo "✗ unsupported desktop pnpm version pin: '$PNPM_VERSION'." >&2
    exit 1
    ;;
esac

verify_command() {
  local actual
  actual="$("$@" --version 2>/dev/null || true)"
  if [ "$actual" != "$PNPM_VERSION" ]; then
    echo "✗ desktop pnpm $PNPM_VERSION is required; command reported '${actual:-unavailable}'." >&2
    return 1
  fi
  echo "✓ desktop pnpm $actual: $*"
}

if [ "${1:-}" = "--verify-command" ]; then
  shift
  if [ "$#" -eq 0 ]; then
    echo "✗ --verify-command requires a pnpm command." >&2
    exit 2
  fi
  verify_command "$@"
  exit 0
fi
if [ "$#" -ne 0 ]; then
  echo "Usage: $0 [--verify-command <command> [args...]]" >&2
  exit 2
fi

PNPM_BIN="$TOOL_ROOT/node_modules/.bin/pnpm"
if [ -x "$PNPM_BIN" ] && verify_command "$PNPM_BIN"; then
  exit 0
fi
if ! command -v npm >/dev/null 2>&1; then
  echo "✗ npm is required to provision desktop pnpm $PNPM_VERSION." >&2
  echo "  Run 'make setup-prerequisites' and retry." >&2
  exit 1
fi

echo "Installing checkout-local pnpm $PNPM_VERSION..."
mkdir -p "$TOOL_ROOT"
npm install \
  --prefix "$TOOL_ROOT" \
  --no-save \
  --package-lock=false \
  --ignore-scripts \
  --no-audit \
  --no-fund \
  "pnpm@$PNPM_VERSION"
verify_command "$PNPM_BIN"
