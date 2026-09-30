#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/Volumes/build/magician/builds}"

case "${1:-}" in
  --live)
    shift
    exec python3 scripts/eval-agentic-native-tool-contract-live.py "$@"
    ;;
  -h|--help)
    cat <<'EOF'
Usage: scripts/eval-agentic-native-tool-contract.sh [--live [LIVE_OPTIONS...]]

Without arguments, run the provider-free semantic A/B audit plus focused Rust
prompt-rendering, routing, runtime-instruction, native-schema, lowering, and
batch/terminal-order regressions. With --live, run the repeated real-model A/B
suite and write JSON, JSONL, and HTML reports under
coverage/evals/agentic-native-tool-contract/.
EOF
    exit 0
    ;;
esac

python3 scripts/eval-agentic-native-tool-contract.py --self-test
python3 scripts/eval-agentic-native-tool-contract.py

for test_filter in \
  agentic_decision_metadata_prompt_resolves_compact_contracts \
  native_tool_instruction_is_positive_schema_authoritative_and_semantically_complete \
  chat_native_tool_instruction_preserves_chat_specific_contract \
  test_build_capabilities_prompt_section \
  test_capabilities_prompt_for_context \
  need_user_input_schema_has_question_input_type_hint_options \
  pack_tool_merges_common_metadata \
  folded_pack_calls_keep_per_candidate_decision_rationales \
  terminal_first_defers_following_calls
do
  cargo test -p magician --lib "$test_filter" -- --nocapture
done
