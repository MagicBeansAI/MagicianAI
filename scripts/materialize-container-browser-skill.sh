#!/usr/bin/env bash
set -euo pipefail

SKILLSHUB_ROOT="${1:?usage: materialize-container-browser-skill.sh <skillshub-root> <scoped-skills-root>}"
SCOPED_SKILLS_ROOT="${2:?usage: materialize-container-browser-skill.sh <skillshub-root> <scoped-skills-root>}"
INSTALLER="$SKILLSHUB_ROOT/scripts/install_skill_layer.py"

[[ -f "$INSTALLER" ]] || { echo "skill installer is missing: $INSTALLER" >&2; exit 1; }

PYTHON_BIN="${SKILLSHUB_PYTHON:-}"
if [[ -z "$PYTHON_BIN" ]] && [[ -x "$SKILLSHUB_ROOT/.venv/bin/python" ]] && \
   "$SKILLSHUB_ROOT/.venv/bin/python" -c 'import yaml' >/dev/null 2>&1; then
  PYTHON_BIN="$SKILLSHUB_ROOT/.venv/bin/python"
fi
if [[ -z "$PYTHON_BIN" ]] && command -v python3 >/dev/null 2>&1 && \
   python3 -c 'import yaml' >/dev/null 2>&1; then
  PYTHON_BIN="$(command -v python3)"
fi
if [[ -z "$PYTHON_BIN" ]] || ! "$PYTHON_BIN" -c 'import yaml' >/dev/null 2>&1; then
  echo "PyYAML is required for skill materialization; run 'make setup-agent-browser'." >&2
  exit 1
fi

"$PYTHON_BIN" "$INSTALLER" \
  --layer all \
  --source-root "$SKILLSHUB_ROOT" \
  --dest "$SCOPED_SKILLS_ROOT" \
  --names browser,macos-ui-automation,macos-script-automation

SCOPED_BIN="$SCOPED_SKILLS_ROOT/browser/bin/agent-browser"
[[ -x "$SCOPED_BIN" ]] || {
    echo "scoped agent-browser binary is missing after materialization: $SCOPED_BIN" >&2
    exit 1
}

AGENT_BROWSER_SKILLS_DIR= "$SCOPED_BIN" skills get core --full >/dev/null
echo "  ✓ scoped browser materialized with bundled core skill"
