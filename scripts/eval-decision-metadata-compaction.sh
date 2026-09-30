#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/Volumes/build/magician/builds}"

case "${1:-}" in
  --live)
    shift
    exec python3 scripts/eval-decision-metadata-compaction-live.py "$@"
    ;;
  -h|--help)
    cat <<'EOF'
Usage: scripts/eval-decision-metadata-compaction.sh [--live [LIVE_OPTIONS...]]

Without arguments, run provider-free schema, prompt, lowering, compatibility,
conflict, and parameter-stripping regressions. With --live, run the bounded
real-model compact-vs-legacy parity eval and write JSON/JSONL/HTML reports under
coverage/evals/decision-metadata/. Pass --live --help for live options.
EOF
    exit 0
    ;;
esac

cargo test -p magician --lib decision_metadata -- --nocapture
