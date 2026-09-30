#!/usr/bin/env bash
# Start the Magician-owned, embedding-only Ollama daemon and pin its model.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DATA_DIR="${MAGICIAN_ROOT_DIR:-${MAGICIAN_STORAGE_PATH:-$HOME/MagicianNotes}}"

# The source path is resolved from this script's runtime location.
# shellcheck disable=SC1091
source "$SCRIPT_DIR/ollama-embedding-qos.sh"

MAGICIAN_OLLAMA_EMBEDDING_QOS_DRY_RUN="${MAGICIAN_OLLAMA_EMBEDDING_QOS_DRY_RUN:-}"
for _arg in "$@"; do
  case "$_arg" in
    --dry-run)
      if [[ -z "$MAGICIAN_OLLAMA_EMBEDDING_QOS_DRY_RUN" ]]; then
        MAGICIAN_OLLAMA_EMBEDDING_QOS_DRY_RUN=1
      fi
      ;;
  esac
done

embedding_qos_is_full_dry_run() {
  case "$(printf '%s' "$MAGICIAN_OLLAMA_EMBEDDING_QOS_DRY_RUN" | tr '[:upper:]' '[:lower:]')" in
    1|true|yes|on) return 0 ;;
    *) return 1 ;;
  esac
}

embedding_dry_run_on_config_fail() {
  if embedding_qos_is_full_dry_run; then
    embedding_print_wrap_command
    exit 0
  fi
  exit 1
}

if [[ "$(printf '%s' "$MAGICIAN_OLLAMA_EMBEDDING_QOS_DRY_RUN" | tr '[:upper:]' '[:lower:]')" == "prefix" ]]; then
  embedding_print_wrap_command
  exit 0
fi

read_env_key() {
  local key="$1" val f
  val="$(printenv "$key" 2>/dev/null || true)"
  if [[ -n "$val" ]]; then printf '%s' "$val"; return 0; fi
  for f in "$DATA_DIR/.env.development" "$DATA_DIR/.env" "$ROOT_DIR/.env.development" "$ROOT_DIR/.env"; do
    [[ -f "$f" ]] || continue
    val="$(grep -E "^[[:space:]]*${key}=" "$f" 2>/dev/null | tail -1 \
      | sed -E "s/^[[:space:]]*${key}=//; s/^[\"']//; s/[\"']\$//")"
    if [[ -n "$val" ]]; then printf '%s' "$val"; return 0; fi
  done
  printf ''
}

CONFIG_OUTPUT=""
CONFIG_PATH="${MAGICIAN_CONFIG_PATH:-}"
if [[ -n "$CONFIG_PATH" ]]; then
  if [[ ! -f "$CONFIG_PATH" ]] || ! command -v ruby >/dev/null 2>&1; then
    printf '  WARN explicit MAGICIAN_CONFIG_PATH is missing or Ruby is unavailable: %s\n' "$CONFIG_PATH" >&2
    embedding_dry_run_on_config_fail
  fi
  CONFIG_OUTPUT="$(ruby "$SCRIPT_DIR/resolve-ollama-config.rb" "$CONFIG_PATH")"
elif [[ -x "$ROOT_DIR/magician.bin" ]]; then
  CONFIG_OUTPUT="$(MAGICIAN_SKIP_KEYCHAIN=1 "$ROOT_DIR/magician.bin" ollama-launch-config 2>/dev/null || true)"
fi
if ! printf '%s\n' "$CONFIG_OUTPUT" | grep -q '^embedding_base_url='; then
  for candidate in "$DATA_DIR/magician-config.yaml" "$ROOT_DIR/magician-config.yaml"; do
    if [[ -f "$candidate" ]]; then CONFIG_PATH="$candidate"; break; fi
  done
  if [[ -n "$CONFIG_PATH" && -f "$CONFIG_PATH" ]] && command -v ruby >/dev/null 2>&1; then
    CONFIG_OUTPUT="$(ruby "$SCRIPT_DIR/resolve-ollama-config.rb" "$CONFIG_PATH")"
  fi
fi
if [[ -z "$CONFIG_OUTPUT" ]]; then
  printf '  WARN unable to resolve dedicated embedding Ollama settings\n' >&2
  embedding_dry_run_on_config_fail
fi

read_config_key() {
  local key="$1"
  printf '%s\n' "$CONFIG_OUTPUT" | awk -F= -v key="$key" '$1 == key { sub(/^[^=]*=/, "", $0); print; exit }'
}

setting() {
  local env_key="$1" config_key="$2" value
  value="$(read_env_key "$env_key")"
  if [[ -z "$value" ]]; then value="$(read_config_key "$config_key")"; fi
  if [[ -z "$value" ]]; then
    printf '  WARN missing dedicated embedding Ollama setting %s\n' "$config_key" >&2
    return 1
  fi
  printf '%s' "$value"
}

required_config() {
  local key="$1" value
  value="$(read_config_key "$key")"
  if [[ -z "$value" ]]; then
    printf '  WARN missing dedicated embedding Ollama setting %s\n' "$key" >&2
    return 1
  fi
  printf '%s' "$value"
}

OLLAMA_URL="$(setting MAGICIAN_MEMORY_OLLAMA_URL embedding_base_url)" || embedding_dry_run_on_config_fail
MODEL="$(required_config embedding_model)" || embedding_dry_run_on_config_fail
CONTEXT="$(required_config embedding_context_tokens)" || embedding_dry_run_on_config_fail
BATCH_TOKENS="$(required_config embedding_batch_tokens)" || embedding_dry_run_on_config_fail
KEEP_ALIVE="$(required_config embedding_keep_alive)" || embedding_dry_run_on_config_fail
NUM_PARALLEL="$(required_config embedding_num_parallel)" || embedding_dry_run_on_config_fail
MAX_LOADED_MODELS="$(required_config embedding_max_loaded_models)" || embedding_dry_run_on_config_fail
KV_CACHE_TYPE="$(setting MAGICIAN_OLLAMA_KV_CACHE_TYPE kv_cache_type)" || embedding_dry_run_on_config_fail
FLASH_ATTENTION="$(setting MAGICIAN_OLLAMA_FLASH_ATTENTION flash_attention)" || embedding_dry_run_on_config_fail
REPLACE_EXISTING="$(setting MAGICIAN_OLLAMA_REPLACE_EXISTING_LOCAL_DAEMON replace_existing_local_daemon)" || embedding_dry_run_on_config_fail

if [[ -z "${MAGICIAN_OLLAMA_EMBEDDING_QOS:-}" ]]; then
  MAGICIAN_OLLAMA_EMBEDDING_QOS="$(read_env_key MAGICIAN_OLLAMA_EMBEDDING_QOS)"
fi
MAGICIAN_OLLAMA_EMBEDDING_QOS="${MAGICIAN_OLLAMA_EMBEDDING_QOS:-background}"
if [[ -z "${MAGICIAN_OLLAMA_EMBEDDING_CPUSET:-}" ]]; then
  MAGICIAN_OLLAMA_EMBEDDING_CPUSET="$(read_env_key MAGICIAN_OLLAMA_EMBEDDING_CPUSET)"
fi

if [[ "$NUM_PARALLEL" != "1" ]]; then
  printf '  WARN embedding num_parallel must be 1 until multi-sequence execution is verified\n' >&2
  embedding_dry_run_on_config_fail
fi
if [[ "$MAX_LOADED_MODELS" != "1" || "$KEEP_ALIVE" != "-1" ]]; then
  printf '  WARN dedicated embedding Ollama requires max_loaded_models=1 and keep_alive=-1\n' >&2
  embedding_dry_run_on_config_fail
fi

STATE_DIR="${MAGICIAN_OLLAMA_STATE_DIR:-$ROOT_DIR/.cache/ollama}"
PID_FILE="${MAGICIAN_OLLAMA_EMBEDDING_PID_FILE:-$STATE_DIR/embedding.pid}"
SETTINGS_FILE="${MAGICIAN_OLLAMA_EMBEDDING_SETTINGS_FILE:-$STATE_DIR/embedding.settings}"
LOG_FILE="${MAGICIAN_OLLAMA_EMBEDDING_LOG_FILE:-$ROOT_DIR/magician-ollama-embedding.log}"

ok() { printf '  OK %s\n' "$*"; }
warn() { printf '  WARN %s\n' "$*" >&2; }
enabled() { case "$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')" in 0|false|no|off|disabled) return 1;; *) return 0;; esac; }
api_url() { printf '%s/%s' "${OLLAMA_URL%/}" "$1"; }
server_healthy() { command -v curl >/dev/null 2>&1 && curl -fsS --max-time 2 "$(api_url api/tags)" >/dev/null 2>&1; }
pid_alive() { [[ -n "$1" ]] && kill -0 "$1" >/dev/null 2>&1; }
pid_matches() { local line; line="$(ps -p "$1" -o command= 2>/dev/null || true)"; [[ "$line" == *ollama* && "$line" == *serve* ]]; }
is_local_url() { local value host; value="${OLLAMA_URL#http://}"; value="${value#https://}"; host="${value%%:*}"; case "$host" in localhost|127.0.0.1|0.0.0.0|::1|\[::1\]) return 0;; *) return 1;; esac; }
local_port() { local value authority; value="${OLLAMA_URL#http://}"; value="${value#https://}"; authority="${value%%/*}"; printf '%s' "${authority##*:}"; }
ollama_host() { local value; value="${OLLAMA_URL#http://}"; value="${value#https://}"; printf '%s' "${value%%/*}"; }

settings_payload() {
  printf 'ollama_url=%s\nmodel=%s\ncontext=%s\nbatch_tokens=%s\nkeep_alive=%s\nnum_parallel=%s\nmax_loaded_models=%s\nkv_cache_type=%s\nflash_attention=%s\nqos=%s\ncpuset=%s\n' \
    "$OLLAMA_URL" "$MODEL" "$CONTEXT" "$BATCH_TOKENS" "$KEEP_ALIVE" "$NUM_PARALLEL" "$MAX_LOADED_MODELS" "$KV_CACHE_TYPE" "$FLASH_ATTENTION" \
    "$(embedding_qos_mode)" "${MAGICIAN_OLLAMA_EMBEDDING_CPUSET:-}"
}
settings_match() { [[ -f "$SETTINGS_FILE" ]] && [[ "$(cat "$SETTINGS_FILE" 2>/dev/null || true)" == "$(settings_payload)" ]]; }

if embedding_qos_is_full_dry_run; then
  prefix="$(embedding_serve_prefix)"
  printf 'env OLLAMA_HOST=%s OLLAMA_NUM_PARALLEL=%s OLLAMA_MAX_LOADED_MODELS=%s OLLAMA_CONTEXT_LENGTH=%s OLLAMA_KV_CACHE_TYPE=%s OLLAMA_FLASH_ATTENTION=%s OLLAMA_KEEP_ALIVE=%s' \
    "$(ollama_host)" "$NUM_PARALLEL" "$MAX_LOADED_MODELS" "$CONTEXT" "$KV_CACHE_TYPE" "$FLASH_ATTENTION" "$KEEP_ALIVE"
  if [[ -n "$prefix" ]]; then
    printf ' %s' "$prefix"
  fi
  printf ' ollama serve\n'
  exit 0
fi

stop_owned() {
  local pid="$1"
  kill "$pid" 2>/dev/null || true
  for _ in $(seq 1 20); do
    if ! pid_alive "$pid"; then rm -f "$PID_FILE" "$SETTINGS_FILE"; return 0; fi
    sleep 0.25
  done
  kill -9 "$pid" 2>/dev/null || true
  rm -f "$PID_FILE" "$SETTINGS_FILE"
}

replace_listener() {
  command -v lsof >/dev/null 2>&1 || return 1
  local pid line
  pid="$(lsof -nP -tiTCP:"$(local_port)" -sTCP:LISTEN 2>/dev/null | head -1 || true)"
  line="$(ps -p "$pid" -o command= 2>/dev/null || true)"
  [[ -n "$pid" && "$line" == *ollama* ]] || return 1
  kill "$pid" 2>/dev/null || true
  for _ in $(seq 1 20); do server_healthy || return 0; sleep 0.25; done
  return 1
}

prewarm() {
  command -v python3 >/dev/null 2>&1 || { warn "python3 unavailable; cannot prewarm embedding model"; return 1; }
  local payload state verification_error
  payload="$(MODEL="$MODEL" CONTEXT="$CONTEXT" BATCH_TOKENS="$BATCH_TOKENS" KEEP_ALIVE="$KEEP_ALIVE" python3 - <<'PY'
import json, os
keep_alive = os.environ["KEEP_ALIVE"]
try:
    keep_alive = int(keep_alive)
except ValueError:
    pass
print(json.dumps({
    "model": os.environ["MODEL"],
    "input": "Magician embedding startup",
    "keep_alive": keep_alive,
    "options": {"num_ctx": int(os.environ["CONTEXT"]), "num_batch": int(os.environ["BATCH_TOKENS"])},
}))
PY
)"
  curl -fsS --max-time 300 -H 'Content-Type: application/json' -d "$payload" "$(api_url api/embed)" >/dev/null || {
    warn "failed to prewarm dedicated embedding model $MODEL"
    return 1
  }
  local expected_model
  expected_model="${MODEL}"$'\t'"${CONTEXT}"$'\n'
  verification_error="model residency did not settle"
  for _ in $(seq 1 20); do
    state="$(curl -fsS --max-time 5 "$(api_url api/ps)" 2>/dev/null || true)"
    if [[ -z "$state" ]]; then
      verification_error="could not read Ollama process state"
      sleep 0.5
      continue
    fi
    if verification_error="$(
      PROCESS_STATE="$state" \
        EXPECTED_MODELS_TSV="$expected_model" \
        python3 "$SCRIPT_DIR/verify_ollama_residency.py"
    )"; then
      ok "dedicated embedding model pinned at $OLLAMA_URL"
      return 0
    fi
    sleep 0.5
  done
  warn "embedding prewarm verification failed after 10s: $verification_error"
  return 1
}

mkdir -p "$STATE_DIR"
if [[ -f "$PID_FILE" ]]; then
  pid="$(cat "$PID_FILE" 2>/dev/null || true)"
  if ! pid_alive "$pid" || ! pid_matches "$pid"; then rm -f "$PID_FILE" "$SETTINGS_FILE"; fi
fi

if server_healthy; then
  pid="$(cat "$PID_FILE" 2>/dev/null || true)"
  if pid_alive "$pid" && pid_matches "$pid" && settings_match; then
    prewarm
    exit 0
  fi
  if is_local_url && enabled "$REPLACE_EXISTING"; then
    if pid_alive "$pid" && pid_matches "$pid"; then stop_owned "$pid"; elif ! replace_listener; then warn "refusing to replace non-Ollama listener at $OLLAMA_URL"; exit 1; fi
  else
    prewarm
    exit 0
  fi
fi

if ! is_local_url; then warn "dedicated embedding Ollama is unreachable at $OLLAMA_URL"; exit 1; fi
command -v ollama >/dev/null 2>&1 || { warn "ollama binary not found"; exit 1; }

prefix="$(embedding_serve_prefix)"

printf '==> Starting dedicated embedding Ollama at %s (parallel=%s, context=%s)\n' "$OLLAMA_URL" "$NUM_PARALLEL" "$CONTEXT"
if [[ -n "$prefix" ]]; then
  read -r -a prefix_args <<< "$prefix"
  nohup env \
    OLLAMA_HOST="$(ollama_host)" \
    OLLAMA_NUM_PARALLEL="$NUM_PARALLEL" \
    OLLAMA_MAX_LOADED_MODELS="$MAX_LOADED_MODELS" \
    OLLAMA_CONTEXT_LENGTH="$CONTEXT" \
    OLLAMA_KV_CACHE_TYPE="$KV_CACHE_TYPE" \
    OLLAMA_FLASH_ATTENTION="$FLASH_ATTENTION" \
    OLLAMA_KEEP_ALIVE="$KEEP_ALIVE" \
    "${prefix_args[@]}" \
    ollama serve >>"$LOG_FILE" 2>&1 &
else
  nohup env \
    OLLAMA_HOST="$(ollama_host)" \
    OLLAMA_NUM_PARALLEL="$NUM_PARALLEL" \
    OLLAMA_MAX_LOADED_MODELS="$MAX_LOADED_MODELS" \
    OLLAMA_CONTEXT_LENGTH="$CONTEXT" \
    OLLAMA_KV_CACHE_TYPE="$KV_CACHE_TYPE" \
    OLLAMA_FLASH_ATTENTION="$FLASH_ATTENTION" \
    OLLAMA_KEEP_ALIVE="$KEEP_ALIVE" \
    ollama serve >>"$LOG_FILE" 2>&1 &
fi
pid="$!"
printf '%s\n' "$pid" >"$PID_FILE"
for _ in $(seq 1 30); do
  if server_healthy; then settings_payload >"$SETTINGS_FILE"; prewarm; exit 0; fi
  if ! pid_alive "$pid"; then rm -f "$PID_FILE" "$SETTINGS_FILE"; warn "embedding Ollama exited; see $LOG_FILE"; exit 1; fi
  sleep 1
done
warn "embedding Ollama did not become healthy; see $LOG_FILE"
stop_owned "$pid"
exit 1
