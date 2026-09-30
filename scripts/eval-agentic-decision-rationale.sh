#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/Volumes/build/magician/builds}"

history_mode="auto"
history_root="${MAGICIAN_RATIONALE_HISTORY_ROOT:-}"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --history-root)
            history_root="${2:?--history-root requires a path}"
            history_mode="required"
            shift 2
            ;;
        --no-history)
            history_mode="disabled"
            shift
            ;;
        -h|--help)
            cat <<'EOF'
Usage: scripts/eval-agentic-decision-rationale.sh [options]

Runs the offline deterministic Rust rationale contract/payload regressions and
the historical-auditor self-test. It makes no LLM/provider calls. When a local
scoped data root is available, it also prints a non-mutating historical usage
report. For real calls, use eval-agentic-decision-rationale-live.py.

Options:
  --history-root PATH  Audit this scoped data root after deterministic checks.
  --no-history         Skip auto-detected historical data.
  -h, --help           Show this help.

Environment:
  CARGO_TARGET_DIR                 Rust build directory.
  MAGICIAN_RATIONALE_HISTORY_ROOT  Scoped data root override.
EOF
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

python3 scripts/eval-agentic-decision-rationale-history.py --self-test
cargo test -p magician --lib decision_rationale -- --nocapture

if [[ "$history_mode" == "disabled" ]]; then
    exit 0
fi

if [[ -z "$history_root" ]]; then
    candidate="$HOME/MagicianNotes/scopes/anonymous/default"
    if [[ -d "$candidate" ]]; then
        history_root="$candidate"
    fi
fi

if [[ -n "$history_root" ]]; then
    python3 scripts/eval-agentic-decision-rationale-history.py "$history_root"
elif [[ "$history_mode" == "required" ]]; then
    echo "historical rationale root is required but unavailable" >&2
    exit 2
else
    echo "Historical rationale audit skipped: no scoped data root detected."
fi
