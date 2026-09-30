#!/usr/bin/env bash
#
# Private-scale readiness check for App Platform experiments.
#
# Run this after pulling changes that touch app-platform ownership before
# publishing or approving daily candidates.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROJECT_ROOT="${PROJECT_ROOT:-$ROOT_DIR}"
CARGO_TARGET_DIR_DEFAULT="/tmp/magician-app-platform-check"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$CARGO_TARGET_DIR_DEFAULT}"
CONFIG_FILES=(
  "$PROJECT_ROOT/magician-config.yaml"
  "$HOME/MagicianNotes/magician-config.yaml"
)
export CARGO_TARGET_DIR

log_step() {
  printf '\n===> %s\n' "$1"
}

require_existing_file() {
  local f="$1"
  if [[ ! -f "$f" ]]; then
    echo "WARN: missing file (expected for this workspace): $f"
    return 1
  fi
}

require_config_key() {
  local file="$1"
  local pattern="$2"
  if ! rg -n "$pattern" "$file" >/dev/null; then
    echo "FAIL: $file missing required config key: $pattern"
    return 1
  fi
}

log_step "1) Repo-level app-platform gates"
cd "$PROJECT_ROOT"
if [[ "${APP_PLATFORM_SKIP_HEAVY_CHECKS:-0}" != "1" ]]; then
  if ! CARGO_TARGET_DIR="$CARGO_TARGET_DIR" make app-contract-check; then
    echo "FAIL: app-contract-check must pass before publishing private-scale candidates."
    echo "Hint: ensure cargo dependencies can write to $CARGO_TARGET_DIR."
    exit 1
  fi
  if ! CARGO_TARGET_DIR="$CARGO_TARGET_DIR" make test-app-authoring; then
    echo "FAIL: test-app-authoring must pass before publishing private-scale candidates."
    exit 1
  fi
else
  echo "SKIP_HEAVY_CHECKS=1 is set; skipping contract/test gates."
fi

log_step "2) Validate app-platform config surfaces"
for cfg in "${CONFIG_FILES[@]}"; do
  if ! require_existing_file "$cfg"; then
    continue
  fi
  require_config_key "$cfg" "privacy:"
  require_config_key "$cfg" "mode: (local|cloud)"
  require_config_key "$cfg" "app_platform:"
  require_config_key "$cfg" "local_profile: op-app-workflow-local"
  require_config_key "$cfg" "remote_profile: op-app-workflow-remote"
  require_config_key "$cfg" "op-app-workflow-local:"
  require_config_key "$cfg" "op-app-workflow-remote:"
done

log_step "3) Validate authoring CLI is callable"
MAGICIAN_BIN="${MAGICIAN_BIN:-}"
if [[ -z "${MAGICIAN_BIN}" ]] && command -v magician >/dev/null 2>&1; then
  MAGICIAN_BIN="$(command -v magician)"
fi
if [[ -z "${MAGICIAN_BIN}" ]]; then
  if [[ "$CARGO_TARGET_DIR" = /* ]]; then
    CARGO_BIN="$CARGO_TARGET_DIR/debug/magician"
  else
    CARGO_BIN="$PROJECT_ROOT/$CARGO_TARGET_DIR/debug/magician"
  fi
fi
if [[ -z "${MAGICIAN_BIN}" && -x "${CARGO_BIN:-}" ]]; then
  MAGICIAN_BIN="$CARGO_BIN"
fi
if [[ -z "${MAGICIAN_BIN}" && -x "$PROJECT_ROOT/target/debug/magician" ]]; then
  MAGICIAN_BIN="$PROJECT_ROOT/target/debug/magician"
fi
if [[ -z "${MAGICIAN_BIN}" ]]; then
  echo "FAIL: magician CLI not found on PATH and local debug binary not available."
  echo "Either install PATH symlink or set MAGICIAN_BIN before running."
  exit 1
fi
"$MAGICIAN_BIN" app --help >/dev/null

log_step "4) Verify no doc contract drift guardrail is pending"
if make -n docs-remind >/dev/null 2>&1; then
  make docs-remind
else
  echo "NOTICE: docs-remind target is not defined in this branch; skipping docs contract drift guard."
fi

echo
echo "PASS: app private-scale readiness checks completed."
echo "Next:"
echo "  1) run the server: cargo run -p magician -- --port 3002"
echo "  2) follow docs/runbooks/2026-08-19-app-platform-private-scale-readiness.md for daily smoke + publish flow"
