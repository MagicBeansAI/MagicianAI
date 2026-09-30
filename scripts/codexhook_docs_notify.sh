#!/usr/bin/env bash
set -euo pipefail

if [ "${DOCS_HOOK_DISABLE:-}" = "1" ]; then
  exit 0
fi

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [ -z "$repo_root" ]; then
  exit 0
fi

cd "$repo_root"

if [ ! -f "scripts/docs_guard.py" ] || [ ! -f "docs/docs_guard_rules.json" ]; then
  exit 0
fi

python3 scripts/docs_guard.py --working-tree --remind-only --source codexhook
