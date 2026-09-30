#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
python_bin="${LIVE_EVAL_PYTHON:-python3}"
magician_auth_headers=()
if [[ -n "${MAGICIAN_BEARER_TOKEN:-}" ]]; then
  magician_auth_headers=(-H "Authorization: Bearer $MAGICIAN_BEARER_TOKEN")
fi

mode="live"
case "${1:-}" in
  "") ;;
  --self-test) mode="self-test" ;;
  --dry-run) mode="dry-run" ;;
  -h|--help)
    cat <<'EOF'
Usage: scripts/run-live-evals-with-report.sh [--self-test|--dry-run]

Run every real-provider or resident-local-model evaluator and write one linked
dashboard. The default is live traffic. --self-test is provider-free; --dry-run
resolves request plans without issuing provider calls.

Environment overrides:
  LIVE_EVAL_RUNS        Repeats per scenario/variant (default: 1)
  LIVE_EVAL_WORKERS     Parallel workers for the native-tool eval (default: 1)
  LIVE_CHUNK_EVAL_RUNS  Repeats per Ollama chunking fixture (default: 5)
  LIVE_AUTH_EVAL_RUNS   Repeats per tool-authorization scenario (default: 5)
  TOOL_RESULT_PROJECTION_LIVE_RUNS  Structured-result/context A/B repeats (default: 5)
  TOOL_RESULT_PROJECTION_LIVE_PROFILE  Low-cost OpenAI Responses profile
  PROVIDER_REPLAY_LIVE_PROFILES  Optional space-separated exact profiles; default is one per configured provider family
  PREPLAN_LIVE_API_BASE_URL  Running API used for the full pre-plan lifecycle (default: http://127.0.0.1:3002)
  PREPLAN_LIVE_PRINCIPAL  Scope principal for disposable live tasks (default: anonymous)
  PREPLAN_LIVE_WORKSPACE  Scope workspace for disposable live tasks (default: default)
  PREPLAN_LIVE_HITL_MODE  fixtures for automation; terminal for direct operator runs (default: fixtures)
  PREPLAN_LIVE_TIMEOUT_SECS  Per-case planning/replanning deadline (default: 1200)
  PREPLAN_LIVE_PROJECTION_TIMEOUT_SECS  Plan-panel/Attention/event convergence deadline (default: 45)
  PREPLAN_LIVE_HTTP_TIMEOUT_SECS  Deadline for one API probe (default: 120)
  WEB_RESEARCHER_LIVE_API_BASE_URL  Running API used for real research tasks (default: http://127.0.0.1:3002)
  WEB_RESEARCHER_LIVE_RUNS  Repeats for each direct/delegated query (default: 1)
  WEB_RESEARCHER_LIVE_HTTP_TIMEOUT_SECS  Deadline for one runtime API call (default: 120)
  WEB_RESEARCHER_LIVE_CITATION_PROBE_LIMIT  Emitted citations checked for reachability per case (default: 3)
  CHAT_CONTEXT_RETRIEVAL_LIVE_RUNS  Measured local retrieval turns (default: 10)
  CHAT_CONTEXT_RETRIEVAL_LIVE_WARMUPS  Warmup turns (default: 2)
  CHAT_CONTEXT_RETRIEVAL_LIVE_P50_MAX_MS  Concurrent wall p50 gate (default: 500)
  CHAT_CONTEXT_RETRIEVAL_LIVE_P95_MAX_MS  Concurrent wall p95 gate (default: 800)
  CHAT_CONTEXT_RETRIEVAL_LIVE_INDEX_WAIT_SECS  Procedure-index wait (default: 30)
  CHAT_CONTEXT_RETRIEVAL_BACKGROUND_RUNS  Turns per exact background lane (default: 3)
  CHAT_CONTEXT_RETRIEVAL_BACKGROUND_INPUTS  Synthetic inputs per gated turn (default: 4)
  CHAT_CONTEXT_RETRIEVAL_BACKGROUND_P95_MAX_MS  Foreground-under-background p95 gate (default: 5000)
  MEMORY_TEMPERATURE_LIVE_MIN_RECALL  Real anchor recall gate (default: 0.80)
  MEMORY_TEMPERATURE_LIVE_MIN_HYBRID  Real hybrid-backend coverage gate (default: 0.80)
  MEMORY_TEMPERATURE_LIVE_MIN_UTILITY  Utility-review label accuracy gate (default: 0.75)
  MEMORY_TEMPERATURE_LIVE_RETRIEVAL_P95_MAX_MS  Real retrieval p95 gate (default: 1000)
  MEMORY_TEMPERATURE_LIVE_UTILITY_P95_MAX_MS  Local reviewer p95 gate (default: 120000)
  CONTENT_RETRIEVAL_RUNTIME_LIVE_QUERY  Query passed through the compiled discovery handler
  CONTENT_RETRIEVAL_RUNTIME_LIVE_URL  Credential-free HTTPS target for compiled read handlers
  CONTENT_RETRIEVAL_RUNTIME_LIVE_TIMEOUT_SECS  Per-handler deadline (default: 25)
  TASK_RECIPES_LIVE_BASE_URL  Running Magician used for cold/warm/variant/drift proof
  TASK_RECIPES_LIVE_MAGICUTOR_BASE_URL  Magicutor used for no-browser tab-count proof
  TASK_RECIPES_LIVE_TIMEOUT_SECS  Per-task terminal deadline (default: 900)
  TASK_RECIPES_FIXTURE_TIMEOUT_SECS  Per-task deadline for the fixture-site lane (default: 600)
  MAGICIAN_EVAL_USERNAME / MAGICIAN_EVAL_PASSWORD  Login for the fixture lane when MAGICIAN_BEARER_TOKEN is not bound to workspace recipes-eval
  OBSERVABLE_SOURCES_LIVE_API_BASE_URL  Magician API used for read-only offer checks
  OBSERVABLE_SOURCES_LIVE_PRINCIPAL  Observe scope principal (default: anonymous)
  OBSERVABLE_SOURCES_LIVE_WORKSPACE  Observe scope workspace (default: default)
  OBSERVABLE_SOURCES_LIVE_TIMEOUT_SECS  Per-feed/API deadline (default: 20)
  STORAGE_GOVERNANCE_LIVE_API_BASE_URL  Magician API for isolated maintenance checks
  STORAGE_GOVERNANCE_LIVE_PRINCIPAL  Dedicated eval principal (default: storage-live-eval)
  STORAGE_GOVERNANCE_LIVE_WORKSPACE  Dedicated eval workspace (default: governance)
  STORAGE_GOVERNANCE_LIVE_TIMEOUT_SECS  Per-maintenance deadline (default: 60)
  LLM_PHASE2F_SETTLE_SECONDS  Wait for the temporary legacy mirror flush (default: 35)
  LLM_PHASE3_PRINCIPAL  Scope principal for sanitized-content audit (default: anonymous)
  LLM_PHASE3_WORKSPACE  Scope workspace for sanitized-content audit (default: default)
  LLM_PHASE3_CALL_ID  Optional one-call target for the authenticated grant/read probe
  MAGICIAN_SETUP_TOKEN  Setup token used only when LLM_PHASE3_CALL_ID is present
  MONITOR_LIVE_EVAL_RUNS  Scripted monitor lifecycle matrix repetitions (default: 1)
  COMPACTOR_EVAL_RUNS   Routed compaction repetitions per golden fixture window (default: 3)
  COMPACTOR_MIN_VALID_RATE  Compactor valid-patch rate gate (default: 0.9)
  COMPACTOR_MAX_EXHAUSTION_RATE  Compactor retry-exhaustion rate gate (default: 0.1)
  COMPACTOR_MIN_SEMANTIC_RETENTION  Pre/post-budget semantic recall gate (default: 0.95)
  COMPACTOR_MIN_PROTECTED_SEMANTIC_RETENTION  Protected semantic recall gate (default: 1.0)
  LIVE_EVAL_ONLY        Optional evaluator slug to run alone
  OLLAMA_CHUNK_EVAL_MODEL  Isolated Ollama candidate tag (default: qwen3.8-ud2-mtp)
  LOCAL_CHUNK_EVAL_RUNTIME  Actual runtime: ollama, llama-server, mlx-lm, or openai-chat
  LOCAL_CHUNK_EVAL_SERVED_MODEL_LABEL  Physical model behind the config alias
  LOCAL_CHUNK_EVAL_STRUCTURED_OUTPUT_MODE  Optional constraint-mode override
  LOCAL_CHUNK_EVAL_PHASE_TIMING_SOURCE  Optional phase-timing override
  LOCAL_CHUNK_EVAL_CONTEXT_TOKENS  Actual runtime context limit for reports
  LIVE_EVAL_CONFIG      Optional magician-config.yaml path
  LIVE_EVAL_REPORT_DIR  Aggregate report root
EOF
    exit 0
    ;;
  *)
    echo "Usage: $0 [--self-test|--dry-run]" >&2
    exit 2
    ;;
esac

runs="${LIVE_EVAL_RUNS:-1}"
workers="${LIVE_EVAL_WORKERS:-1}"
chunk_runs="${LIVE_CHUNK_EVAL_RUNS:-5}"
auth_runs="${LIVE_AUTH_EVAL_RUNS:-5}"
web_researcher_runs="${WEB_RESEARCHER_LIVE_RUNS:-1}"
tool_projection_runs="${TOOL_RESULT_PROJECTION_LIVE_RUNS:-5}"
retrieval_runs="${CHAT_CONTEXT_RETRIEVAL_LIVE_RUNS:-10}"
retrieval_warmups="${CHAT_CONTEXT_RETRIEVAL_LIVE_WARMUPS:-2}"
retrieval_p50_max_ms="${CHAT_CONTEXT_RETRIEVAL_LIVE_P50_MAX_MS:-500}"
retrieval_p95_max_ms="${CHAT_CONTEXT_RETRIEVAL_LIVE_P95_MAX_MS:-800}"
retrieval_index_wait_secs="${CHAT_CONTEXT_RETRIEVAL_LIVE_INDEX_WAIT_SECS:-30}"
retrieval_background_runs="${CHAT_CONTEXT_RETRIEVAL_BACKGROUND_RUNS:-3}"
retrieval_background_inputs="${CHAT_CONTEXT_RETRIEVAL_BACKGROUND_INPUTS:-4}"
retrieval_background_p95_max_ms="${CHAT_CONTEXT_RETRIEVAL_BACKGROUND_P95_MAX_MS:-5000}"
memory_min_recall="${MEMORY_TEMPERATURE_LIVE_MIN_RECALL:-0.80}"
memory_min_hybrid="${MEMORY_TEMPERATURE_LIVE_MIN_HYBRID:-0.80}"
memory_min_utility="${MEMORY_TEMPERATURE_LIVE_MIN_UTILITY:-0.75}"
memory_retrieval_p95_max_ms="${MEMORY_TEMPERATURE_LIVE_RETRIEVAL_P95_MAX_MS:-1000}"
memory_utility_p95_max_ms="${MEMORY_TEMPERATURE_LIVE_UTILITY_P95_MAX_MS:-120000}"
content_retrieval_runtime_query="${CONTENT_RETRIEVAL_RUNTIME_LIVE_QUERY:-AI}"
content_retrieval_runtime_url="${CONTENT_RETRIEVAL_RUNTIME_LIVE_URL:-https://news.ycombinator.com/}"
content_retrieval_runtime_timeout_secs="${CONTENT_RETRIEVAL_RUNTIME_LIVE_TIMEOUT_SECS:-25}"
observable_sources_api_base_url="${OBSERVABLE_SOURCES_LIVE_API_BASE_URL:-${MAGICIAN_BASE_URL:-http://127.0.0.1:3002}}"
observable_sources_principal="${OBSERVABLE_SOURCES_LIVE_PRINCIPAL:-anonymous}"
observable_sources_workspace="${OBSERVABLE_SOURCES_LIVE_WORKSPACE:-default}"
observable_sources_timeout_secs="${OBSERVABLE_SOURCES_LIVE_TIMEOUT_SECS:-20}"
storage_governance_api_base_url="${STORAGE_GOVERNANCE_LIVE_API_BASE_URL:-${MAGICIAN_BASE_URL:-http://127.0.0.1:3002}}"
storage_governance_principal="${STORAGE_GOVERNANCE_LIVE_PRINCIPAL:-storage-live-eval}"
storage_governance_workspace="${STORAGE_GOVERNANCE_LIVE_WORKSPACE:-governance}"
storage_governance_timeout_secs="${STORAGE_GOVERNANCE_LIVE_TIMEOUT_SECS:-60}"
llm_phase2f_settle_seconds="${LLM_PHASE2F_SETTLE_SECONDS:-35}"
monitor_live_runs="${MONITOR_LIVE_EVAL_RUNS:-1}"
compactor_runs="${COMPACTOR_EVAL_RUNS:-3}"
compactor_min_valid_rate="${COMPACTOR_MIN_VALID_RATE:-0.9}"
compactor_max_exhaustion_rate="${COMPACTOR_MAX_EXHAUSTION_RATE:-0.1}"
compactor_min_semantic_retention="${COMPACTOR_MIN_SEMANTIC_RETENTION:-0.95}"
compactor_min_protected_semantic_retention="${COMPACTOR_MIN_PROTECTED_SEMANTIC_RETENTION:-1.0}"
only_eval="${LIVE_EVAL_ONLY:-}"
config="${LIVE_EVAL_CONFIG:-}"
case "$only_eval" in
  ""|task-state-schema|decision-metadata|decision-rationale|terminal-contract|native-tool-contract|agent-tool-visibility-authorization|ollama-logical-chunking|chat-context-retrieval|tool-result-projection-context|provider-replay|preplan-flow|web-researcher|memory-temperature|monitor-live|compactor-patch-validity|content-retrieval-runtime|task-recipes|task-recipes-fixture|observable-sources|storage-governance|llm-observability-phase2f|llm-observability-phase3|llm-observability-phase4) ;;
  *)
    echo "LIVE_EVAL_ONLY names an unknown evaluator: '$only_eval'" >&2
    exit 2
    ;;
esac
if ! [[ "$runs" =~ ^[1-9][0-9]*$ ]]; then
  echo "LIVE_EVAL_RUNS must be a positive integer; got '$runs'" >&2
  exit 2
fi
if ! [[ "$workers" =~ ^[1-9][0-9]*$ ]]; then
  echo "LIVE_EVAL_WORKERS must be a positive integer; got '$workers'" >&2
  exit 2
fi
if ! [[ "$chunk_runs" =~ ^[1-9][0-9]*$ ]]; then
  echo "LIVE_CHUNK_EVAL_RUNS must be a positive integer; got '$chunk_runs'" >&2
  exit 2
fi
if ! [[ "$auth_runs" =~ ^[1-9][0-9]*$ ]]; then
  echo "LIVE_AUTH_EVAL_RUNS must be a positive integer; got '$auth_runs'" >&2
  exit 2
fi
if ! [[ "$web_researcher_runs" =~ ^[1-9][0-9]*$ ]]; then
  echo "WEB_RESEARCHER_LIVE_RUNS must be a positive integer; got '$web_researcher_runs'" >&2
  exit 2
fi
if ! [[ "$tool_projection_runs" =~ ^[1-9][0-9]*$ ]]; then
  echo "TOOL_RESULT_PROJECTION_LIVE_RUNS must be a positive integer; got '$tool_projection_runs'" >&2
  exit 2
fi
if ! [[ "$retrieval_runs" =~ ^[1-9][0-9]*$ ]]; then
  echo "CHAT_CONTEXT_RETRIEVAL_LIVE_RUNS must be a positive integer; got '$retrieval_runs'" >&2
  exit 2
fi
if ! [[ "$retrieval_warmups" =~ ^[0-9]+$ ]]; then
  echo "CHAT_CONTEXT_RETRIEVAL_LIVE_WARMUPS must be a non-negative integer; got '$retrieval_warmups'" >&2
  exit 2
fi
if ! [[ "$retrieval_index_wait_secs" =~ ^[1-9][0-9]*$ ]]; then
  echo "CHAT_CONTEXT_RETRIEVAL_LIVE_INDEX_WAIT_SECS must be a positive integer; got '$retrieval_index_wait_secs'" >&2
  exit 2
fi
if ! [[ "$retrieval_background_runs" =~ ^[1-9][0-9]*$ ]]; then
  echo "CHAT_CONTEXT_RETRIEVAL_BACKGROUND_RUNS must be a positive integer; got '$retrieval_background_runs'" >&2
  exit 2
fi
if ! [[ "$retrieval_background_inputs" =~ ^[1-9][0-9]*$ ]]; then
  echo "CHAT_CONTEXT_RETRIEVAL_BACKGROUND_INPUTS must be a positive integer; got '$retrieval_background_inputs'" >&2
  exit 2
fi
if ! [[ "$retrieval_p50_max_ms" =~ ^[0-9]+([.][0-9]+)?$ ]] || ! [[ "$retrieval_p95_max_ms" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
  echo "chat-context retrieval latency gates must be positive numbers" >&2
  exit 2
fi
if ! [[ "$retrieval_background_p95_max_ms" =~ ^[0-9]+([.][0-9]+)?$ ]] \
  || [[ "$retrieval_background_p95_max_ms" =~ ^0+([.]0+)?$ ]]; then
  echo "CHAT_CONTEXT_RETRIEVAL_BACKGROUND_P95_MAX_MS must be a positive number; got '$retrieval_background_p95_max_ms'" >&2
  exit 2
fi
for memory_gate in "$memory_min_recall" "$memory_min_hybrid" "$memory_min_utility" "$memory_retrieval_p95_max_ms" "$memory_utility_p95_max_ms"; do
  if ! [[ "$memory_gate" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
    echo "memory-temperature gates must be non-negative numbers" >&2
    exit 2
  fi
done
if ! [[ "$llm_phase2f_settle_seconds" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
  echo "LLM_PHASE2F_SETTLE_SECONDS must be a non-negative number" >&2
  exit 2
fi
if ! [[ "$monitor_live_runs" =~ ^[1-9][0-9]*$ ]]; then
  echo "MONITOR_LIVE_EVAL_RUNS must be a positive integer; got '$monitor_live_runs'" >&2
  exit 2
fi
if ! [[ "$compactor_runs" =~ ^[1-9][0-9]*$ ]]; then
  echo "COMPACTOR_EVAL_RUNS must be a positive integer; got '$compactor_runs'" >&2
  exit 2
fi
for compactor_gate in "$compactor_min_valid_rate" "$compactor_max_exhaustion_rate" "$compactor_min_semantic_retention" "$compactor_min_protected_semantic_retention"; do
  if ! [[ "$compactor_gate" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
    echo "compactor patch-validity gates must be non-negative numbers" >&2
    exit 2
  fi
done

report_root="${LIVE_EVAL_REPORT_DIR:-$repo_root/coverage/evals/live-suite}"
run_id="$(date '+%Y%m%d-%H%M%S')-$$"
run_dir="$report_root/results/$run_id"
summary_path="$report_root/latest.html"
archive_path="$run_dir/index.html"
manifest_path="$run_dir/manifest.json"
latest_manifest_path="$report_root/latest.json"
started_at="$(date '+%Y-%m-%d %H:%M:%S %Z')"
started_epoch="$(date '+%s')"
mkdir -p "$run_dir"

if [[ "$mode" == "live" ]]; then
  echo "⚠ Running cost-bearing live LLM evals: standard_runs=$runs auth_runs=$auth_runs chunk_runs=$chunk_runs native_workers=$workers"
elif [[ "$mode" == "dry-run" ]]; then
  echo "Planning live LLM evals without provider calls"
else
  echo "Running provider-free live-evaluator self-tests"
fi

eval_status=0
eval_state="passed"
eval_report=""

run_eval() {
  local name="$1"
  local slug="$2"
  local script="$3"
  local native_workers="$4"
  local output_dir="$run_dir/$slug"
  local -a args=()

  if [[ -n "$only_eval" && "$only_eval" != "$slug" ]]; then
    echo "Skipping $name (LIVE_EVAL_ONLY=$only_eval)"
    eval_status=0
    eval_state="skipped"
    eval_report=""
    return
  fi

  if [[ "$mode" == "self-test" ]]; then
    args+=(--self-test)
    if [[ "$slug" == "preplan-flow" || "$slug" == "web-researcher" ]]; then
      args+=(--output-dir "$output_dir")
    fi
  else
    args+=(--runs "$runs")
    if [[ -n "$config" ]]; then
      args+=(--config "$config")
    fi
    if [[ "$mode" == "dry-run" ]]; then
      args+=(--dry-run)
      if [[ "$slug" == "preplan-flow" || "$slug" == "web-researcher" ]]; then
        args+=(--output-dir "$output_dir")
      fi
    else
      args+=(--output-dir "$output_dir")
    fi
    if [[ "$native_workers" == "yes" ]]; then
      args+=(--workers "$workers")
    fi
    if [[ "$slug" == "ollama-logical-chunking" ]]; then
      args+=(
        --local-model "${OLLAMA_CHUNK_EVAL_MODEL:-qwen3.8-ud2-mtp}"
        --runtime-kind "${LOCAL_CHUNK_EVAL_RUNTIME:-ollama}"
      )
      if [[ -n "${LOCAL_CHUNK_EVAL_SERVED_MODEL_LABEL:-}" ]]; then
        args+=(--served-model-label "$LOCAL_CHUNK_EVAL_SERVED_MODEL_LABEL")
      fi
      if [[ -n "${LOCAL_CHUNK_EVAL_STRUCTURED_OUTPUT_MODE:-}" ]]; then
        args+=(--structured-output-mode "$LOCAL_CHUNK_EVAL_STRUCTURED_OUTPUT_MODE")
      fi
      if [[ -n "${LOCAL_CHUNK_EVAL_PHASE_TIMING_SOURCE:-}" ]]; then
        args+=(--phase-timing-source "$LOCAL_CHUNK_EVAL_PHASE_TIMING_SOURCE")
      fi
      if [[ -n "${LOCAL_CHUNK_EVAL_CONTEXT_TOKENS:-}" ]]; then
        args+=(--runtime-context-tokens "$LOCAL_CHUNK_EVAL_CONTEXT_TOKENS")
      fi
    fi
  fi

  echo
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "▶ Running $name"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  eval_status=0
  set +e
  "$python_bin" "$repo_root/$script" "${args[@]}"
  eval_status=$?
  set -e
  if [[ $eval_status -eq 0 ]]; then
    eval_state="passed"
  else
    eval_state="failed"
  fi
  eval_report=""
  # The pre-plan evaluator writes a real report in every mode (including its
  # provider-free self-test and dry-run), so preserve that child link in the
  # aggregate dashboard as well as in cost-bearing runs.
  if [[ -f "$output_dir/report.html" ]]; then
    eval_report="$output_dir/report.html"
  fi
}

run_chat_context_retrieval_eval() {
  local name="Chat context retrieval"
  local slug="chat-context-retrieval"
  local output_dir="$run_dir/$slug"
  local output_json="$output_dir/report.json"

  if [[ -n "$only_eval" && "$only_eval" != "$slug" ]]; then
    echo "Skipping $name (LIVE_EVAL_ONLY=$only_eval)"
    eval_status=0
    eval_state="skipped"
    eval_report=""
    return
  fi

  echo
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "▶ Running $name"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  eval_status=0
  set +e
  if [[ "$mode" == "self-test" ]]; then
    make --no-print-directory test-chat-context-retrieval-eval
    eval_status=$?
  elif [[ "$mode" == "dry-run" ]]; then
    printf 'Would run %s measured retrieval turns after %s warmups plus 2x%s exact contention turns (%s synthetic inputs each); hybrid/coalescing/priority required; baseline p50<=%sms p95<=%sms contention p95<=%sms\n' \
      "$retrieval_runs" "$retrieval_warmups" "$retrieval_background_runs" "$retrieval_background_inputs" \
      "$retrieval_p50_max_ms" "$retrieval_p95_max_ms" "$retrieval_background_p95_max_ms"
    eval_status=0
  else
    mkdir -p "$output_dir"
    make --no-print-directory test-chat-context-retrieval-live-eval \
      CHAT_CONTEXT_RETRIEVAL_LIVE_RUNS="$retrieval_runs" \
      CHAT_CONTEXT_RETRIEVAL_LIVE_WARMUPS="$retrieval_warmups" \
      CHAT_CONTEXT_RETRIEVAL_LIVE_P50_MAX_MS="$retrieval_p50_max_ms" \
      CHAT_CONTEXT_RETRIEVAL_LIVE_P95_MAX_MS="$retrieval_p95_max_ms" \
      CHAT_CONTEXT_RETRIEVAL_LIVE_INDEX_WAIT_SECS="$retrieval_index_wait_secs" \
      CHAT_CONTEXT_RETRIEVAL_BACKGROUND_RUNS="$retrieval_background_runs" \
      CHAT_CONTEXT_RETRIEVAL_BACKGROUND_INPUTS="$retrieval_background_inputs" \
      CHAT_CONTEXT_RETRIEVAL_BACKGROUND_P95_MAX_MS="$retrieval_background_p95_max_ms" \
      LIVE_EVAL_CONFIG="$config" \
      CHAT_CONTEXT_RETRIEVAL_LIVE_OUTPUT="$output_json"
    eval_status=$?
  fi
  set -e
  if [[ $eval_status -eq 0 ]]; then
    eval_state="passed"
  else
    eval_state="failed"
  fi
  eval_report=""
  if [[ "$mode" == "live" && -f "$output_json" ]]; then
    eval_report="$output_json"
  fi
}

run_tool_result_projection_context_eval() {
  local name="Tool-result projection and staged context"
  local slug="tool-result-projection-context"
  local output_dir="$run_dir/$slug"

  if [[ -n "$only_eval" && "$only_eval" != "$slug" ]]; then
    echo "Skipping $name (LIVE_EVAL_ONLY=$only_eval)"
    eval_status=0
    eval_state="skipped"
    eval_report=""
    return
  fi

  echo
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "▶ Running $name"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  eval_status=0
  set +e
  if [[ "$mode" == "self-test" ]]; then
    make --no-print-directory test-tool-result-projection-context-eval-harness
  elif [[ "$mode" == "dry-run" ]]; then
    local -a dry_args=(
      --dry-run
      --runs "$tool_projection_runs"
      --profile "${TOOL_RESULT_PROJECTION_LIVE_PROFILE:-chat-gptterra-responses-vision-toolsauto-fast}"
    )
    if [[ -n "$config" ]]; then
      dry_args+=(--config "$config")
    fi
    "$python_bin" "$repo_root/scripts/eval-tool-result-projection-context-live.py" "${dry_args[@]}"
  else
    mkdir -p "$output_dir"
    make --no-print-directory test-tool-result-projection-context-live-eval \
      TOOL_RESULT_PROJECTION_LIVE_RUNS="$tool_projection_runs" \
      TOOL_RESULT_PROJECTION_LIVE_PROFILE="${TOOL_RESULT_PROJECTION_LIVE_PROFILE:-chat-gptterra-responses-vision-toolsauto-fast}" \
      TOOL_RESULT_PROJECTION_LIVE_OUTPUT_DIR="$output_dir" \
      LIVE_EVAL_CONFIG="$config"
  fi
  eval_status=$?
  set -e
  if [[ $eval_status -eq 0 ]]; then eval_state="passed"; else eval_state="failed"; fi
  eval_report=""
  if [[ "$mode" == "live" && -f "$output_dir/report.html" ]]; then
    eval_report="$output_dir/report.html"
  fi
}

run_provider_replay_eval() {
  local name="Cross-provider replay and checkpoint continuation"
  local slug="provider-replay"
  local output_dir="$run_dir/$slug"

  if [[ -n "$only_eval" && "$only_eval" != "$slug" ]]; then
    echo "Skipping $name (LIVE_EVAL_ONLY=$only_eval)"
    eval_status=0
    eval_state="skipped"
    eval_report=""
    return
  fi

  echo
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "▶ Running $name"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  eval_status=0
  set +e
  if [[ "$mode" == "self-test" ]]; then
    make --no-print-directory test-provider-replay-eval-harness
  else
    mkdir -p "$output_dir"
    local replay_args=""
    if [[ "$mode" == "dry-run" ]]; then
      replay_args="--dry-run"
    fi
    make --no-print-directory test-provider-replay-live-eval \
      LIVE_EVAL_CONFIG="$config" \
      PROVIDER_REPLAY_LIVE_PROFILES="${PROVIDER_REPLAY_LIVE_PROFILES:-}" \
      PROVIDER_REPLAY_LIVE_OUTPUT_DIR="$output_dir" \
      PROVIDER_REPLAY_LIVE_EVAL_ARGS="$replay_args"
  fi
  eval_status=$?
  set -e
  if [[ $eval_status -eq 0 ]]; then eval_state="passed"; else eval_state="failed"; fi
  eval_report=""
  if [[ "$mode" == "live" && -f "$output_dir/report.html" ]]; then
    eval_report="$output_dir/report.html"
  fi
}

run_memory_temperature_eval() {
  local name="Memory temperature recall and utility"
  local slug="memory-temperature"
  local output_dir="$run_dir/$slug"

  if [[ -n "$only_eval" && "$only_eval" != "$slug" ]]; then
    echo "Skipping $name (LIVE_EVAL_ONLY=$only_eval)"
    eval_status=0
    eval_state="skipped"
    eval_report=""
    return
  fi

  echo
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "▶ Running $name (final cost-bearing live evaluator)"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  eval_status=0
  set +e
  if [[ "$mode" == "self-test" ]]; then
    make --no-print-directory test-memory-temperature-eval-harness
    eval_status=$?
  elif [[ "$mode" == "dry-run" ]]; then
    local -a args=(--dry-run)
    if [[ -n "$config" ]]; then
      args+=(--config "$config")
    fi
    cargo run --quiet -p magician --example memory_temperature_live_eval -- "${args[@]}"
    eval_status=$?
  else
    mkdir -p "$output_dir"
    make --no-print-directory test-memory-temperature-live-eval \
      MEMORY_TEMPERATURE_LIVE_OUTPUT_DIR="$output_dir" \
      MEMORY_TEMPERATURE_LIVE_MIN_RECALL="$memory_min_recall" \
      MEMORY_TEMPERATURE_LIVE_MIN_HYBRID="$memory_min_hybrid" \
      MEMORY_TEMPERATURE_LIVE_MIN_UTILITY="$memory_min_utility" \
      MEMORY_TEMPERATURE_LIVE_RETRIEVAL_P95_MAX_MS="$memory_retrieval_p95_max_ms" \
      MEMORY_TEMPERATURE_LIVE_UTILITY_P95_MAX_MS="$memory_utility_p95_max_ms" \
      LIVE_EVAL_CONFIG="$config"
    eval_status=$?
  fi
  set -e
  if [[ $eval_status -eq 0 ]]; then
    eval_state="passed"
  else
    eval_state="failed"
  fi
  eval_report=""
  if [[ "$mode" == "live" && -f "$output_dir/report.html" ]]; then
    eval_report="$output_dir/report.html"
  fi
}

run_monitor_live_eval() {
  local name="Recurring monitors live run-quality"
  local slug="monitor-live"
  local output_dir="$run_dir/$slug"
  local base_url="${MAGICIAN_BASE_URL:-http://127.0.0.1:3002}"

  if [[ -n "$only_eval" && "$only_eval" != "$slug" ]]; then
    echo "Skipping $name (LIVE_EVAL_ONLY=$only_eval)"
    eval_status=0
    eval_state="skipped"
    eval_report=""
    return
  fi

  echo
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "▶ Running $name"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  eval_status=0
  set +e
  if [[ "$mode" == "self-test" ]]; then
    make --no-print-directory eval-monitor-golden
    eval_status=$?
  elif [[ "$mode" == "dry-run" ]]; then
    printf 'Would drive %s scripted monitor lifecycle repetition(s) (pricing, release_notes, status, auth) against %s\n' \
      "$monitor_live_runs" "$base_url"
    eval_status=0
  else
    local probe_status
    probe_status="$(curl -s -o /dev/null -m 10 -w '%{http_code}' \
      "${magician_auth_headers[@]}" \
      "$base_url/api/magician/v3/monitors" 2>/dev/null)" || probe_status="000"
    if [[ ! "$probe_status" =~ ^2[0-9][0-9]$ ]]; then
      set -e
      echo "Skipping $name: server predates monitors — rebuild required (GET $base_url/api/magician/v3/monitors -> HTTP ${probe_status:-000})"
      eval_status=0
      eval_state="skipped"
      eval_report=""
      return
    fi
    mkdir -p "$output_dir"
    MONITOR_LIVE_EVAL_RUNS="$monitor_live_runs" \
      MONITOR_LIVE_REPORT_DIR="$output_dir" \
      "$python_bin" "$repo_root/scripts/eval-monitor-live.py"
    eval_status=$?
  fi
  set -e
  if [[ $eval_status -eq 0 ]]; then
    eval_state="passed"
  else
    eval_state="failed"
  fi
  eval_report=""
  if [[ "$mode" == "live" && -f "$output_dir/latest.html" ]]; then
    eval_report="$output_dir/latest.html"
  fi
}

run_compactor_patch_validity_eval() {
  local name="Agentic compactor patch validity"
  local slug="compactor-patch-validity"
  local output_dir="$run_dir/$slug"

  if [[ -n "$only_eval" && "$only_eval" != "$slug" ]]; then
    echo "Skipping $name (LIVE_EVAL_ONLY=$only_eval)"
    eval_status=0
    eval_state="skipped"
    eval_report=""
    return
  fi

  echo
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "▶ Running $name"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  eval_status=0
  set +e
  if [[ "$mode" == "self-test" ]]; then
    make --no-print-directory test-compactor-patch-validity-eval-harness
    eval_status=$?
  elif [[ "$mode" == "dry-run" ]]; then
    printf 'Would run %s routed compaction repetition(s) per golden fixture window via the ignored Rust probe; valid-rate >= %s, exhaustion <= %s, semantic >= %s, protected semantic >= %s, zero protected attempts/enforcement failures\n' \
      "$compactor_runs" "$compactor_min_valid_rate" "$compactor_max_exhaustion_rate" "$compactor_min_semantic_retention" "$compactor_min_protected_semantic_retention"
    eval_status=0
  else
    if [[ -z "$config" \
      && ! -f "${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}/magician-config.yaml" \
      && ! -f "$repo_root/magician-config.yaml" ]]; then
      set -e
      echo "Skipping $name: LIVE_EVAL_CONFIG is unset and no default magician config resolves (runtime root or repo root)"
      eval_status=0
      eval_state="skipped"
      eval_report=""
      return
    fi
    mkdir -p "$output_dir"
    COMPACTOR_EVAL_RUNS="$compactor_runs" \
      COMPACTOR_MIN_VALID_RATE="$compactor_min_valid_rate" \
      COMPACTOR_MAX_EXHAUSTION_RATE="$compactor_max_exhaustion_rate" \
      COMPACTOR_MIN_SEMANTIC_RETENTION="$compactor_min_semantic_retention" \
      COMPACTOR_MIN_PROTECTED_SEMANTIC_RETENTION="$compactor_min_protected_semantic_retention" \
      COMPACTOR_EVAL_REPORT_DIR="$output_dir" \
      LIVE_EVAL_CONFIG="$config" \
      "$python_bin" "$repo_root/scripts/eval-compactor-patch-validity.py"
    eval_status=$?
  fi
  set -e
  if [[ $eval_status -eq 0 ]]; then
    eval_state="passed"
  else
    eval_state="failed"
  fi
  eval_report=""
  if [[ "$mode" == "live" && -f "$output_dir/latest.html" ]]; then
    eval_report="$output_dir/latest.html"
  fi
}

run_content_retrieval_runtime_eval() {
  local name="Content retrieval runtime"
  local slug="content-retrieval-runtime"
  local output_dir="$run_dir/$slug"
  local report="$output_dir/report.json"
  local -a make_args=(
    "CONTENT_RETRIEVAL_RUNTIME_LIVE_QUERY=$content_retrieval_runtime_query"
    "CONTENT_RETRIEVAL_RUNTIME_LIVE_URL=$content_retrieval_runtime_url"
    "CONTENT_RETRIEVAL_RUNTIME_LIVE_TIMEOUT_SECS=$content_retrieval_runtime_timeout_secs"
    "CONTENT_RETRIEVAL_RUNTIME_LIVE_OUTPUT=$report"
  )

  if [[ -n "$only_eval" && "$only_eval" != "$slug" ]]; then
    echo "Skipping $name (LIVE_EVAL_ONLY=$only_eval)"
    eval_status=0
    eval_state="skipped"
    eval_report=""
    return
  fi

  echo
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "▶ Running $name"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  eval_status=0
  set +e
  if [[ "$mode" == "self-test" ]]; then
    # The whole content-retrieval harness, not just the runtime evaluator's own
    # self-test: it also covers the shipped source tests and the provider-free
    # browser corpus, and it ends by invoking the runtime harness itself.
    make --no-print-directory test-content-retrieval-eval-harness
  else
    mkdir -p "$output_dir"
    if [[ -n "$config" ]]; then
      make_args+=("CONTENT_RETRIEVAL_RUNTIME_LIVE_CONFIG=$config")
    fi
    if [[ "$mode" == "dry-run" ]]; then
      make_args+=("CONTENT_RETRIEVAL_RUNTIME_LIVE_EVAL_ARGS=--dry-run")
    fi
    make --no-print-directory test-content-retrieval-runtime-live-eval "${make_args[@]}"
  fi
  eval_status=$?
  set -e
  if [[ $eval_status -eq 0 ]]; then
    eval_state="passed"
  else
    eval_state="failed"
  fi
  eval_report=""
  if [[ "$mode" == "live" && -f "$report" ]]; then
    eval_report="$report"
  fi
}

run_task_recipes_eval() {
  local name="Task Recipes browserless replay"
  local slug="task-recipes"
  local output_dir="$run_dir/$slug"

  if [[ -n "$only_eval" && "$only_eval" != "$slug" ]]; then
    echo "Skipping $name (LIVE_EVAL_ONLY=$only_eval)"
    eval_status=0
    eval_state="skipped"
    eval_report=""
    return
  fi

  echo
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "▶ Running $name"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  eval_status=0
  set +e
  if [[ "$mode" == "self-test" ]]; then
    make --no-print-directory test-task-recipes-live-eval \
      TASK_RECIPES_LIVE_OUTPUT="$output_dir" \
      TASK_RECIPES_LIVE_EVAL_ARGS="--self-test"
  elif [[ "$mode" == "dry-run" ]]; then
    printf 'Would run cold HN browser learning, identical and golang browserless replay, forced extractor drift, recompilation, and a final browserless proof\n'
  else
    mkdir -p "$output_dir"
    make --no-print-directory test-task-recipes-live-eval \
      TASK_RECIPES_LIVE_BASE_URL="${TASK_RECIPES_LIVE_BASE_URL:-${MAGICIAN_BASE_URL:-http://127.0.0.1:3002}}" \
      TASK_RECIPES_LIVE_MAGICUTOR_BASE_URL="${TASK_RECIPES_LIVE_MAGICUTOR_BASE_URL:-http://127.0.0.1:3003}" \
      TASK_RECIPES_LIVE_TIMEOUT_SECS="${TASK_RECIPES_LIVE_TIMEOUT_SECS:-900}" \
      TASK_RECIPES_LIVE_OUTPUT="$output_dir"
  fi
  eval_status=$?
  set -e
  if [[ $eval_status -eq 0 ]]; then eval_state="passed"; else eval_state="failed"; fi
  eval_report=""
  if [[ "$mode" == "live" && -f "$output_dir/latest.html" ]]; then
    eval_report="$output_dir/latest.html"
  fi
}

run_task_recipes_fixture_eval() {
  local name="Task Recipes fixture sites"
  local slug="task-recipes-fixture"
  local output_dir="$run_dir/$slug"

  if [[ -n "$only_eval" && "$only_eval" != "$slug" ]]; then
    echo "Skipping $name (LIVE_EVAL_ONLY=$only_eval)"
    eval_status=0
    eval_state="skipped"
    eval_report=""
    return
  fi

  echo
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "▶ Running $name"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  eval_status=0
  set +e
  if [[ "$mode" == "self-test" ]]; then
    make --no-print-directory test-task-recipes-fixture-eval \
      TASK_RECIPES_FIXTURE_OUTPUT="$output_dir" \
      TASK_RECIPES_FIXTURE_EVAL_ARGS="--self-test"
  elif [[ "$mode" == "dry-run" ]]; then
    printf 'Would start four local fixture sites and run six cases: search, list->detail, Document answer, cookie session + auth heal, guarded write + HITL, GraphQL + drift; each cold with the real browser agent, then browserless\n'
  else
    mkdir -p "$output_dir"
    make --no-print-directory test-task-recipes-fixture-eval \
      TASK_RECIPES_LIVE_BASE_URL="${TASK_RECIPES_LIVE_BASE_URL:-${MAGICIAN_BASE_URL:-http://127.0.0.1:3002}}" \
      TASK_RECIPES_LIVE_MAGICUTOR_BASE_URL="${TASK_RECIPES_LIVE_MAGICUTOR_BASE_URL:-http://127.0.0.1:3003}" \
      TASK_RECIPES_FIXTURE_TIMEOUT_SECS="${TASK_RECIPES_FIXTURE_TIMEOUT_SECS:-600}" \
      TASK_RECIPES_FIXTURE_OUTPUT="$output_dir"
  fi
  eval_status=$?
  set -e
  if [[ $eval_status -eq 0 ]]; then eval_state="passed"; else eval_state="failed"; fi
  eval_report=""
  if [[ "$mode" == "live" && -f "$output_dir/latest.html" ]]; then
    eval_report="$output_dir/latest.html"
  fi
}

run_observable_sources_eval() {
  local name="Observable sources"
  local slug="observable-sources"
  local output_dir="$run_dir/$slug"
  local report="$output_dir/report.json"

  if [[ -n "$only_eval" && "$only_eval" != "$slug" ]]; then
    echo "Skipping $name (LIVE_EVAL_ONLY=$only_eval)"
    eval_status=0
    eval_state="skipped"
    eval_report=""
    return
  fi

  echo
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "▶ Running $name"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  eval_status=0
  set +e
  if [[ "$mode" == "self-test" ]]; then
    make --no-print-directory test-observable-sources-eval-harness
  elif [[ "$mode" == "dry-run" ]]; then
    mkdir -p "$output_dir"
    "$python_bin" "$repo_root/scripts/eval-observable-sources-live.py" \
      --dry-run \
      --api-base-url "$observable_sources_api_base_url" \
      --principal "$observable_sources_principal" \
      --workspace "$observable_sources_workspace" \
      --timeout-secs "$observable_sources_timeout_secs" \
      --output "$report"
  else
    mkdir -p "$output_dir"
    make --no-print-directory test-observable-sources-live-eval \
      OBSERVABLE_SOURCES_LIVE_API_BASE_URL="$observable_sources_api_base_url" \
      OBSERVABLE_SOURCES_LIVE_PRINCIPAL="$observable_sources_principal" \
      OBSERVABLE_SOURCES_LIVE_WORKSPACE="$observable_sources_workspace" \
      OBSERVABLE_SOURCES_LIVE_TIMEOUT_SECS="$observable_sources_timeout_secs" \
      OBSERVABLE_SOURCES_LIVE_OUTPUT="$report"
  fi
  eval_status=$?
  set -e
  if [[ $eval_status -eq 0 ]]; then
    eval_state="passed"
  else
    eval_state="failed"
  fi
  eval_report=""
  if [[ "$mode" == "live" && -f "$report" ]]; then
    eval_report="$report"
  fi
}

run_storage_governance_eval() {
  local name="Storage governance"
  local slug="storage-governance"
  local output_dir="$run_dir/$slug"

  if [[ -n "$only_eval" && "$only_eval" != "$slug" ]]; then
    echo "Skipping $name (LIVE_EVAL_ONLY=$only_eval)"
    eval_status=0
    eval_state="skipped"
    eval_report=""
    return
  fi

  echo
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "▶ Running $name (isolated scope)"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  eval_status=0
  set +e
  if [[ "$mode" == "self-test" ]]; then
    make --no-print-directory test-storage-governance-eval-harness
  elif [[ "$mode" == "dry-run" ]]; then
    mkdir -p "$output_dir"
    "$python_bin" "$repo_root/scripts/eval-storage-governance-live.py" \
      --dry-run \
      --api-base-url "$storage_governance_api_base_url" \
      --principal "$storage_governance_principal" \
      --workspace "$storage_governance_workspace" \
      --timeout-secs "$storage_governance_timeout_secs" \
      --output-dir "$output_dir"
  else
    mkdir -p "$output_dir"
    make --no-print-directory test-storage-governance-live-eval \
      STORAGE_GOVERNANCE_LIVE_API_BASE_URL="$storage_governance_api_base_url" \
      STORAGE_GOVERNANCE_LIVE_PRINCIPAL="$storage_governance_principal" \
      STORAGE_GOVERNANCE_LIVE_WORKSPACE="$storage_governance_workspace" \
      STORAGE_GOVERNANCE_LIVE_TIMEOUT_SECS="$storage_governance_timeout_secs" \
      STORAGE_GOVERNANCE_LIVE_OUTPUT_DIR="$output_dir"
  fi
  eval_status=$?
  set -e
  if [[ $eval_status -eq 0 ]]; then
    eval_state="passed"
  else
    eval_state="failed"
  fi
  eval_report=""
  if [[ "$mode" == "live" && -f "$output_dir/report.html" ]]; then
    eval_report="$output_dir/report.html"
  fi
}

run_llm_phase2f_eval() {
  local name="LLM observability Phase 2F"
  local slug="llm-observability-phase2f"
  local output_dir="$run_dir/$slug"

  if [[ -n "$only_eval" && "$only_eval" != "$slug" ]]; then
    echo "Skipping $name (LIVE_EVAL_ONLY=$only_eval)"
    eval_status=0
    eval_state="skipped"
    eval_report=""
    return
  fi

  echo
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "▶ Running $name"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  eval_status=0
  set +e
  if [[ "$mode" == "self-test" ]]; then
    "$python_bin" "$repo_root/scripts/eval-llm-observability-phase2f.py" --self-test
  elif [[ "$mode" == "dry-run" ]]; then
    "$python_bin" "$repo_root/scripts/eval-llm-observability-phase2f.py" \
      --dry-run --settle-seconds "$llm_phase2f_settle_seconds"
  else
    "$python_bin" "$repo_root/scripts/eval-llm-observability-phase2f.py" \
      --strict --settle-seconds "$llm_phase2f_settle_seconds" --output-dir "$output_dir"
  fi
  eval_status=$?
  set -e
  if [[ $eval_status -eq 0 ]]; then
    eval_state="passed"
  else
    eval_state="failed"
  fi
  eval_report=""
  if [[ "$mode" == "live" && -f "$output_dir/report.html" ]]; then
    eval_report="$output_dir/report.html"
  fi
}

run_llm_phase3_eval() {
  local name="LLM observability Phase 3"
  local slug="llm-observability-phase3"
  local output_dir="$run_dir/$slug"

  if [[ -n "$only_eval" && "$only_eval" != "$slug" ]]; then
    echo "Skipping $name (LIVE_EVAL_ONLY=$only_eval)"
    eval_status=0
    eval_state="skipped"
    eval_report=""
    return
  fi

  echo
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "▶ Running $name"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  eval_status=0
  set +e
  if [[ "$mode" == "self-test" ]]; then
    "$python_bin" "$repo_root/scripts/eval-llm-observability-phase3.py" --self-test
  elif [[ "$mode" == "dry-run" ]]; then
    "$python_bin" "$repo_root/scripts/eval-llm-observability-phase3.py" --dry-run
  else
    local -a args=(--strict --output-dir "$output_dir")
    if [[ -n "$config" ]]; then
      args+=(--runtime-root "$(dirname "$config")")
    fi
    "$python_bin" "$repo_root/scripts/eval-llm-observability-phase3.py" "${args[@]}"
  fi
  eval_status=$?
  set -e
  if [[ $eval_status -eq 3 ]]; then
    eval_status=0
    eval_state="skipped"
  elif [[ $eval_status -eq 0 ]]; then
    eval_state="passed"
  else
    eval_state="failed"
  fi
  eval_report=""
  if [[ "$mode" == "live" && -f "$output_dir/report.html" ]]; then
    eval_report="$output_dir/report.html"
  fi
}

run_llm_phase4_eval() {
  local name="LLM observability Phase 4"
  local slug="llm-observability-phase4"
  local output_dir="$run_dir/$slug"

  if [[ -n "$only_eval" && "$only_eval" != "$slug" ]]; then
    echo "Skipping $name (LIVE_EVAL_ONLY=$only_eval)"
    eval_status=0
    eval_state="skipped"
    eval_report=""
    return
  fi

  echo
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "▶ Running $name"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  eval_status=0
  set +e
  if [[ "$mode" == "self-test" ]]; then
    "$python_bin" "$repo_root/scripts/eval-llm-observability-phase4.py" --self-test
  elif [[ "$mode" == "dry-run" ]]; then
    "$python_bin" "$repo_root/scripts/eval-llm-observability-phase4.py" --dry-run
  else
    local -a args=(
      --strict
      --principal "${LLM_PHASE4_PRINCIPAL:-anonymous}"
      --workspace "${LLM_PHASE4_WORKSPACE:-default}"
      --output-dir "$output_dir"
    )
    if [[ -n "$config" ]]; then
      args+=(--runtime-root "$(dirname "$config")")
    fi
    "$python_bin" "$repo_root/scripts/eval-llm-observability-phase4.py" "${args[@]}"
  fi
  eval_status=$?
  set -e
  if [[ $eval_status -eq 3 ]]; then
    eval_status=0
    eval_state="skipped"
  elif [[ $eval_status -eq 0 ]]; then
    eval_state="passed"
  else
    eval_state="failed"
  fi
  eval_report=""
  if [[ "$mode" == "live" && -f "$output_dir/report.html" ]]; then
    eval_report="$output_dir/report.html"
  fi
}

run_eval "Task-state schema compaction" "task-state-schema" "scripts/eval-task-state-schema-compaction-live.py" "no"
task_state_status=$eval_status
task_state_state=$eval_state
task_state_report=$eval_report

run_eval "Decision-metadata compaction" "decision-metadata" "scripts/eval-decision-metadata-compaction-live.py" "no"
decision_metadata_status=$eval_status
decision_metadata_state=$eval_state
decision_metadata_report=$eval_report

run_eval "Decision-rationale contract" "decision-rationale" "scripts/eval-agentic-decision-rationale-live.py" "no"
rationale_status=$eval_status
rationale_state=$eval_state
rationale_report=$eval_report

run_eval "Terminal decision contract" "terminal-contract" "scripts/eval-agentic-terminal-contract-live.py" "no"
terminal_status=$eval_status
terminal_state=$eval_state
terminal_report=$eval_report

run_eval "Native-tool decision contract" "native-tool-contract" "scripts/eval-agentic-native-tool-contract-live.py" "yes"
native_status=$eval_status
native_state=$eval_state
native_report=$eval_report

standard_runs="$runs"
runs="$auth_runs"
run_eval "Agent tool visibility authorization" "agent-tool-visibility-authorization" "scripts/eval-agent-tool-visibility-authorization-live.py" "no"
authorization_status=$eval_status
authorization_state=$eval_state
authorization_report=$eval_report
runs="$standard_runs"

runs="$chunk_runs"
run_eval "Ollama logical-context chunking" "ollama-logical-chunking" "scripts/eval-ollama-logical-chunking.py" "no"
chunk_status=$eval_status
chunk_state=$eval_state
chunk_report=$eval_report
runs="$standard_runs"

run_chat_context_retrieval_eval
retrieval_status=$eval_status
retrieval_state=$eval_state
retrieval_report=$eval_report

run_tool_result_projection_context_eval
tool_projection_status=$eval_status
tool_projection_state=$eval_state
tool_projection_report=$eval_report

run_provider_replay_eval
provider_replay_status=$eval_status
provider_replay_state=$eval_state
provider_replay_report=$eval_report

# Full public-API pre-plan lifecycle. Fixture answers keep the aggregate lane
# non-interactive, while the dedicated Make companion switches the exact same
# evaluator to /dev/tty. Run before memory-temperature so that local-model lane
# remains the final cost-bearing evaluator and the final fact audits see these
# linked planning calls too.
run_eval "Pre-plan lifecycle" "preplan-flow" "scripts/eval-preplan-flow-live.py" "no"
preplan_status=$eval_status
preplan_state=$eval_state
preplan_report=$eval_report

# Real task-level web research after the planning gate: one direct specialist
# run and one parent→specialist delegation. Latency is recorded for diagnosis
# without imposing an evaluator deadline; Ctrl-C cancels and cleans the active
# disposable task while preserving partial evidence.
runs="$web_researcher_runs"
run_eval "Web researcher" "web-researcher" "scripts/eval-web-researcher-live.py" "no"
web_researcher_status=$eval_status
web_researcher_state=$eval_state
web_researcher_report=$eval_report
runs="$standard_runs"

# Keep this as the final cost-bearing evaluator: it can start both embedding
# and generation Ollama models. The content-free Phase 2F reconciliation audit
# follows it so all live traffic is included in the evidence window.
run_memory_temperature_eval
memory_temperature_status=$eval_status
memory_temperature_state=$eval_state
memory_temperature_report=$eval_report

# Post-Ollama live lanes: monitor run-quality needs the rebuilt server's
# monitor routes and the compactor probe needs a resolvable config plus a
# compiling workspace; both skip clean (recorded on the dashboard) until then.
run_monitor_live_eval
monitor_live_status=$eval_status
monitor_live_state=$eval_state
monitor_live_report=$eval_report

run_compactor_patch_validity_eval
compactor_status=$eval_status
compactor_state=$eval_state
compactor_report=$eval_report

# Content retrieval gate: builds the production scoped resolver and invokes the
# compiled content_search/content_read cores, so discovery, static read, browser
# read, replay fallback, handoff, and the approval boundary are all measured on
# the code path a real agent call takes.
run_content_retrieval_runtime_eval
content_retrieval_runtime_status=$eval_status
content_retrieval_runtime_state=$eval_state
content_retrieval_runtime_report=$eval_report

# Task-level API mining proof. This lane intentionally runs after the general
# browser runtime gate and before read-only audits because its cold and drift
# phases create real browser traffic that later observability audits should see.
run_task_recipes_eval
task_recipes_status=$eval_status
task_recipes_state=$eval_state
task_recipes_report=$eval_report

# Same rail against local fixture sites: deterministic, safe writes, and the
# fixture's own request log as the browserless oracle.
run_task_recipes_fixture_eval
task_recipes_fixture_status=$eval_status
task_recipes_fixture_state=$eval_state
task_recipes_fixture_report=$eval_report

# Read-only API projection plus capped public RSS checks. The deterministic
# subscription runner itself is covered hermetically in the Rust suite.
run_observable_sources_eval
observable_sources_status=$eval_status
observable_sources_state=$eval_state
observable_sources_report=$eval_report

# Runtime contract lane. It mutates only the dedicated storage-live-eval scope,
# exercising the same verified swap/retention APIs exposed by the Storage UI.
run_storage_governance_eval
storage_governance_status=$eval_status
storage_governance_state=$eval_state
storage_governance_report=$eval_report

# Final evidence gate: all earlier live evaluators have now produced real LLM
# traffic, so reconcile the active canonical journal against its compatibility
# mirror and governed API after the mirror's bounded flush interval.
run_llm_phase2f_eval
llm_phase2f_status=$eval_status
llm_phase2f_state=$eval_state
llm_phase2f_report=$eval_report

# Penultimate read-only privacy gate. It can inspect sanitized revisions emitted by
# earlier evaluators without issuing another provider call. Metadata-only
# configurations produce an explicit skipped child report.
run_llm_phase3_eval
llm_phase3_status=$eval_status
llm_phase3_state=$eval_state
llm_phase3_report=$eval_report

# Final fact-only lineage gate. It observes completed tool lifecycles produced
# by all preceding evaluators and reports only bounded aggregates and machine
# violation categories; no stable IDs, arguments, or results leave Parquet.
run_llm_phase4_eval
llm_phase4_status=$eval_status
llm_phase4_state=$eval_state
llm_phase4_report=$eval_report

ended_epoch="$(date '+%s')"
duration_seconds=$(( ended_epoch - started_epoch ))
report_status=0
"$python_bin" "$repo_root/scripts/test_suite_summary_report.py" \
  --output "$summary_path" \
  --archive-output "$archive_path" \
  --manifest "$manifest_path" \
  --mode "standard" \
  --run-id "$run_id" \
  --started-at "$started_at" \
  --duration-seconds "$duration_seconds" \
  --suite "Task-state schema" "$task_state_state" "$task_state_status" "test-live-evals" "Optional task-state sidecar selection and mutation parity" "html" "$task_state_report" \
  --suite "Decision metadata" "$decision_metadata_state" "$decision_metadata_status" "test-live-evals" "Sparse execution-signal selection and schema parity" "html" "$decision_metadata_report" \
  --suite "Decision rationale" "$rationale_state" "$rationale_status" "test-live-evals" "Bounded native decision-rationale quality and attribution" "html" "$rationale_report" \
  --suite "Terminal contract" "$terminal_state" "$terminal_status" "test-live-evals" "Complete, partial, blocked, user-input, and continue routing" "html" "$terminal_report" \
  --suite "Native-tool contract" "$native_state" "$native_status" "test-live-evals" "Full prompt/runtime/routing native-tool semantic parity" "html" "$native_report" \
  --suite "Tool visibility authorization" "$authorization_state" "$authorization_status" "test-live-evals" "Typed feature surfaces, hidden agents, scoped delegation, and reduced-catalog semantic parity" "html" "$authorization_report" \
  --suite "Ollama logical chunking" "$chunk_state" "$chunk_status" "test-live-evals" "Dormant config, six adapters, local shadow validity, latency, and baseline cost comparison" "html" "$chunk_report" \
  --suite "Chat context retrieval" "$retrieval_state" "$retrieval_status" "test-live-evals" "Live hybrid coverage, one embedding provider call per turn, coalesced sibling reuse, and wall latency" "json" "$retrieval_report" \
  --suite "Tool-result projection and staged context" "$tool_projection_state" "$tool_projection_status" "test-live-evals" "Five-repeat exact evidence, partial context, continuation, privacy, token, latency, rotation, and multi-tool parity" "html" "$tool_projection_report" \
  --suite "Provider replay" "$provider_replay_state" "$provider_replay_status" "test-live-evals" "Every configured provider family accepts repaired interrupted tool history; OpenAI Responses also reuses a persisted clean continuation checkpoint" "html" "$provider_replay_report" \
  --suite "Pre-plan lifecycle" "$preplan_state" "$preplan_status" "test-live-evals" "Real mapped profiles, task planning, clarification and plan-approval HITL, Plan-panel and Attention projection, resolution cleanup, rejection/replan, and linked LLM facts" "html" "$preplan_report" \
  --suite "Web researcher" "$web_researcher_state" "$web_researcher_status" "test-live-evals" "Real direct and delegated web-research tasks, observed answer-ready latency, cited official sources, paired tool events, lineage, model routing, cost, operator cancellation, and cleanup" "html" "$web_researcher_report" \
  --suite "Memory temperature" "$memory_temperature_state" "$memory_temperature_status" "test-live-evals" "Read-only real recall, adversarial temperature retrieval, durable leased local utility review, exactly-once overlay writes, and hot-projection factuality" "html" "$memory_temperature_report" \
  --suite "Monitor live run-quality" "$monitor_live_state" "$monitor_live_status" "test-live-evals" "Real monitor lifecycles over local fixtures: terminal extraction, changed/unchanged/degraded classification, and the quiet-run rule" "html" "$monitor_live_report" \
  --suite "Compactor patch validity" "$compactor_state" "$compactor_status" "test-live-evals" "Routed compaction validity, pre/post-budget semantic retention, strict protected-information gates, retry exhaustion, and latency" "html" "$compactor_report" \
  --suite "Content retrieval runtime" "$content_retrieval_runtime_state" "$content_retrieval_runtime_status" "test-live-evals" "Production resolver, compiled handlers, configured browser engine, replay fallback, handoff, and authenticated approval boundary" "json" "$content_retrieval_runtime_report" \
  --suite "Task Recipes" "$task_recipes_state" "$task_recipes_status" "test-live-evals" "Cold browser learning, identical and variant browserless replay, forced extractor drift, recompilation, run ledger, and browser-signal immobility" "html" "$task_recipes_report" \
  --suite "Task Recipes fixture sites" "$task_recipes_fixture_state" "$task_recipes_fixture_status" "test-live-evals" "Local fixture sites: search, list->detail, Document answer, cookie session + auth heal, guarded write + HITL, GraphQL + drift; cold with the real browser agent, then browserless replay proven by the fixture request log" "html" "$task_recipes_fixture_report" \
  --suite "Observable sources" "$observable_sources_state" "$observable_sources_status" "test-live-evals" "Strict source manifests, exact RSS policy, capped public feeds, and server-paginated Observe offers" "json" "$observable_sources_report" \
  --suite "Storage governance" "$storage_governance_state" "$storage_governance_status" "test-live-evals" "Scope inventory, exact confirmations, lossless owner compaction, verified Parquet maintenance, retention coverage, and protected mail/journal boundaries" "html" "$storage_governance_report" \
  --suite "LLM observability Phase 2F" "$llm_phase2f_state" "$llm_phase2f_status" "test-live-evals" "Canonical journal activation, compatibility reconciliation, attempt gaps, pricing, timing, and governed API parity" "html" "$llm_phase2f_report" \
  --suite "LLM observability Phase 3" "$llm_phase3_state" "$llm_phase3_status" "test-live-evals" "Sanitized request/response/context privacy, restricted storage, and optional one-use grant audit" "html" "$llm_phase3_report" \
  --suite "LLM observability Phase 4" "$llm_phase4_state" "$llm_phase4_status" "test-live-evals" "Fact-only tool lifecycle, result consumption, branch, rollback, and cross-execution delegation lineage" "html" "$llm_phase4_report" \
  || report_status=$?

if [[ -f "$manifest_path" ]]; then
  cp "$manifest_path" "$latest_manifest_path"
fi

for status in "$task_state_status" "$decision_metadata_status" "$rationale_status" "$terminal_status" "$native_status" "$authorization_status" "$chunk_status" "$retrieval_status" "$tool_projection_status" "$provider_replay_status" "$preplan_status" "$web_researcher_status" "$memory_temperature_status" "$monitor_live_status" "$compactor_status" "$content_retrieval_runtime_status" "$task_recipes_status" "$task_recipes_fixture_status" "$observable_sources_status" "$storage_governance_status" "$llm_phase2f_status" "$llm_phase3_status" "$llm_phase4_status"; do
  if [[ $status -ne 0 ]]; then
    exit "$status"
  fi
done
exit "$report_status"
