#!/usr/bin/env bash
set -euo pipefail

# Shared docs-guard reminder for coding-agent hooks.
# Usage: docs_remind_hook.sh <source>
#   source labels the reminder (gemini, zcode, agy, ...).
# Agents pipe a JSON event to stdin; drain it so the python child is not blocked.
# agy PostToolUse/Stop require JSON on stdout (`{}`); reminder text goes to stderr.

if [ ! -t 0 ]; then
  cat >/dev/null || true
fi

source_label="${1:-docs-guard}"

emit_agy_ok() {
  printf '%s\n' '{}'
}

if [ "${DOCS_HOOK_DISABLE:-}" = "1" ]; then
  if [ "$source_label" = "agy" ]; then
    emit_agy_ok
  fi
  exit 0
fi

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [ -z "$repo_root" ]; then
  if [ "$source_label" = "agy" ]; then
    emit_agy_ok
  fi
  exit 0
fi

cd "$repo_root"

if [ ! -f "scripts/docs_guard.py" ] || [ ! -f "docs/docs_guard_rules.json" ]; then
  if [ "$source_label" = "agy" ]; then
    emit_agy_ok
  fi
  exit 0
fi

if [ "$source_label" = "agy" ]; then
  python3 scripts/docs_guard.py --working-tree --remind-only --source agy >&2 || true
  emit_agy_ok
  exit 0
fi

python3 scripts/docs_guard.py --working-tree --remind-only --source "$source_label"
exit 0
