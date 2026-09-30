#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/Volumes/build/magician/builds}"

case "${1:-}" in
  --live)
    shift
    exec python3 scripts/eval-agentic-terminal-contract-live.py "$@"
    ;;
  -h|--help)
    cat <<'EOF'
Usage: scripts/eval-agentic-terminal-contract.sh [--live [LIVE_OPTIONS...]]

Without arguments, run provider-free prompt/catalog/runtime-guidance checks and
persisted-history compatibility regressions. With --live, compare the current
yield-only prompts with their immediately previous versions using the configured
agentic-decision profile and write JSON, JSONL, and HTML reports under
coverage/evals/agentic-terminal-contract/.
EOF
    exit 0
    ;;
esac

python3 scripts/eval-agentic-terminal-contract-live.py --self-test

for test_filter in \
  agentic_decision_metadata_prompt_resolves_compact_contracts \
  native_tool_instruction_is_positive_schema_authoritative_and_semantically_complete \
  generated_terminal_guidance_only_advertises_yield \
  model_facing_control_schemas_do_not_advertise_retired_terminals \
  build_decision_tools_flat_returns_hot_tier_and_deferred_block \
  completed_primitive_objective_is_recorded_and_retrievable \
  retired_goal_reached_alias_still_lowers_persisted_history \
  retired_cannot_proceed_alias_still_lowers_persisted_history
do
  cargo test -p magician --lib "$test_filter" -- --nocapture
done
