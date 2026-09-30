#!/usr/bin/env bash
set -euo pipefail

BIN="${1:?usage: verify-skill-discovery.sh <agent-browser-bin> <agent-browser-package-root>}"
PACKAGE_ROOT="${2:?usage: verify-skill-discovery.sh <agent-browser-bin> <agent-browser-package-root>}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
SKILLSHUB_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd -P)"

[[ -x "$BIN" ]] || { echo "agent-browser binary is not executable: $BIN" >&2; exit 1; }
[[ -d "$PACKAGE_ROOT/skills" ]] || { echo "bundled skills directory is missing: $PACKAGE_ROOT/skills" >&2; exit 1; }
[[ -f "$PACKAGE_ROOT/skill-data/core/SKILL.md" ]] || {
    echo "bundled core skill is missing: $PACKAGE_ROOT/skill-data/core/SKILL.md" >&2
    exit 1
}

TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

MIRROR_ROOT="$TMP_ROOT/browser"
MIRROR_BIN="$MIRROR_ROOT/bin/agent-browser"
MIRROR_PACKAGE="$MIRROR_ROOT/node_modules/agent-browser"
mkdir -p "$MIRROR_ROOT/bin" "$MIRROR_PACKAGE"
cp "$BIN" "$MIRROR_BIN"
chmod +x "$MIRROR_BIN"
cp -R "$PACKAGE_ROOT/skills" "$PACKAGE_ROOT/skill-data" "$MIRROR_PACKAGE/"

CORE_OUTPUT="$TMP_ROOT/core-skill.md"
AGENT_BROWSER_SKILLS_DIR= "$MIRROR_BIN" skills get core --full > "$CORE_OUTPUT"
grep -q '^name: core$' "$CORE_OUTPUT" || {
    echo "relocated mirror returned content that is not the bundled core skill" >&2
    exit 1
}

ACTUAL_PATH="$(AGENT_BROWSER_SKILLS_DIR= "$MIRROR_BIN" skills path core)"
EXPECTED_PATH="$(cd "$MIRROR_PACKAGE/skill-data/core" && pwd -P)"
[[ "$ACTUAL_PATH" == "$EXPECTED_PATH" ]] || {
    echo "relocated mirror resolved core to '$ACTUAL_PATH', expected '$EXPECTED_PATH'" >&2
    exit 1
}

SCOPED_SKILLS="$TMP_ROOT/runtime/scopes/anonymous/default/skills"
bash "$SKILLSHUB_ROOT/../scripts/materialize-container-browser-skill.sh" \
  "$SKILLSHUB_ROOT" "$SCOPED_SKILLS" >/dev/null
SCOPED_BIN="$SCOPED_SKILLS/browser/bin/agent-browser"
[[ -L "$SCOPED_BIN" ]] || {
    echo "browser scope did not materialize its binary as a source symlink" >&2
    exit 1
}
AGENT_BROWSER_SKILLS_DIR= "$SCOPED_BIN" skills get core --full >/dev/null

SCOPED_ENV="$SCOPED_SKILLS/browser/config/.env"
mkdir -p "$(dirname "$SCOPED_ENV")"
printf 'operator-owned-marker\n' > "$SCOPED_ENV"
bash "$SKILLSHUB_ROOT/../scripts/materialize-container-browser-skill.sh" \
  "$SKILLSHUB_ROOT" "$SCOPED_SKILLS" >/dev/null
[[ "$(cat "$SCOPED_ENV")" == "operator-owned-marker" ]] || {
    echo "browser rematerialization overwrote its scoped config/.env" >&2
    exit 1
}

echo "  ✓ relocated mirror and materialized scope discover bundled core skill"
