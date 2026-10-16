#!/usr/bin/env bash
# run-ollama.sh - start host Ollama for local Magician runs.
#
# Idempotent for a matching Magician-owned daemon. A mismatched owned daemon is
# restarted; when configured, a conflicting local Ollama daemon is replaced so
# scheduler/context settings are deterministic. Remote endpoints are never
# stopped. PID + launch-signature files retain shutdown ownership.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DATA_DIR="${MAGICIAN_ROOT_DIR:-${MAGICIAN_STORAGE_PATH:-$HOME/MagicianNotes}}"

# read_env_key KEY — resolve launch-time settings from the process env first,
# then from the runtime env files Magician itself reads. run-supervisor invokes
# this script before Magician loads dotenv, so Ollama daemon settings must be
# visible here too.
read_env_key() {
  local key="$1" val f
  val="$(printenv "$key" 2>/dev/null || true)"
  if [[ -n "$val" ]]; then
    printf '%s' "$val"
    return 0
  fi
  for f in "$DATA_DIR/.env.development" "$DATA_DIR/.env" "$ROOT_DIR/.env.development" "$ROOT_DIR/.env"; do
    [[ -f "$f" ]] || continue
    val="$(grep -E "^[[:space:]]*${key}=" "$f" 2>/dev/null | tail -1 \
      | sed -E "s/^[[:space:]]*${key}=//; s/^[\"']//; s/[\"']\$//")"
    if [[ -n "$val" ]]; then
      printf '%s' "$val"
      return 0
    fi
  done
  printf ''
}

STATE_DIR="${MAGICIAN_OLLAMA_STATE_DIR:-$ROOT_DIR/.cache/ollama}"
PID_FILE="${MAGICIAN_OLLAMA_PID_FILE:-$STATE_DIR/ollama.pid}"
SETTINGS_FILE="${MAGICIAN_OLLAMA_SETTINGS_FILE:-$STATE_DIR/ollama.settings}"
LOG_FILE="${MAGICIAN_OLLAMA_LOG_FILE:-$ROOT_DIR/magician-ollama.log}"
OLLAMA_URL="${MAGICIAN_OLLAMA_URL:-${MAGICIAN_OLLAMA_BASE_URL:-http://127.0.0.1:11434}}"
AUTOSTART="${MAGICIAN_OLLAMA_AUTOSTART:-true}"
EMBEDDING_AUTOSTART="${MAGICIAN_OLLAMA_EMBEDDING_AUTOSTART:-true}"

CONFIG_OUTPUT=""
CONFIG_PATH="${MAGICIAN_CONFIG_PATH:-}"
if [[ -n "$CONFIG_PATH" ]]; then
  if [[ ! -f "$CONFIG_PATH" ]] || ! command -v ruby >/dev/null 2>&1; then
    printf '  WARN explicit MAGICIAN_CONFIG_PATH is missing or Ruby is unavailable: %s\n' "$CONFIG_PATH" >&2
    exit 1
  fi
  CONFIG_OUTPUT="$(ruby "$SCRIPT_DIR/resolve-ollama-config.rb" "$CONFIG_PATH")"
elif [[ -x "$ROOT_DIR/magician.bin" ]]; then
  CONFIG_OUTPUT="$(MAGICIAN_SKIP_KEYCHAIN=1 "$ROOT_DIR/magician.bin" ollama-launch-config 2>/dev/null || true)"
  if ! printf '%s\n' "$CONFIG_OUTPUT" | grep -q '^generation_model_count=' \
    || ! printf '%s\n' "$CONFIG_OUTPUT" | grep -q '^embedding_batch_size='; then
    # Say so: a silent fallback to the YAML resolver once prewarmed the local
    # generation model on an install whose locality was cloud.
    printf '  WARN magician.bin ollama-launch-config did not answer; resolving from magician-config.yaml instead\n' >&2
    CONFIG_OUTPUT=""
  fi
fi
if [[ -z "$CONFIG_OUTPUT" ]]; then
  for candidate in \
    "$DATA_DIR/magician-config.yaml" \
    "$ROOT_DIR/magician-config.yaml"; do
    if [[ -f "$candidate" ]]; then
      CONFIG_PATH="$candidate"
      break
    fi
  done
  if [[ -n "$CONFIG_PATH" && -f "$CONFIG_PATH" ]] && command -v ruby >/dev/null 2>&1; then
    CONFIG_OUTPUT="$(ruby "$SCRIPT_DIR/resolve-ollama-config.rb" "$CONFIG_PATH")"
  fi
fi
if [[ -z "$CONFIG_OUTPUT" ]]; then
  printf '  WARN unable to resolve Ollama settings from magician-config.yaml\n' >&2
  exit 1
fi

read_config_key() {
  local key="$1"
  printf '%s\n' "$CONFIG_OUTPUT" | awk -F= -v key="$key" '$1 == key { sub(/^[^=]*=/, "", $0); print; exit }'
}

resolve_setting() {
  local env_key="$1" config_key="$2" value
  value="$(read_env_key "$env_key")"
  if [[ -z "$value" ]]; then
    value="$(read_config_key "$config_key")"
  fi
  if [[ -z "$value" ]]; then
    printf '  WARN missing required Ollama setting %s in magician-config.yaml\n' "$config_key" >&2
    return 1
  fi
  printf '%s' "$value"
}

OLLAMA_NUM_PARALLEL="${OLLAMA_NUM_PARALLEL:-$(read_env_key OLLAMA_NUM_PARALLEL)}"
OLLAMA_NUM_PARALLEL="${OLLAMA_NUM_PARALLEL:-$(read_env_key MAGICIAN_OLLAMA_NUM_PARALLEL)}"
OLLAMA_NUM_PARALLEL="${OLLAMA_NUM_PARALLEL:-1}"
OLLAMA_MAX_LOADED_MODELS="${OLLAMA_MAX_LOADED_MODELS:-$(read_env_key OLLAMA_MAX_LOADED_MODELS)}"
OLLAMA_MAX_LOADED_MODELS="${OLLAMA_MAX_LOADED_MODELS:-$(resolve_setting MAGICIAN_OLLAMA_MAX_LOADED_MODELS max_loaded_models)}"
OLLAMA_CONTEXT_LENGTH="${OLLAMA_CONTEXT_LENGTH:-$(read_env_key OLLAMA_CONTEXT_LENGTH)}"
OLLAMA_CONTEXT_LENGTH="${OLLAMA_CONTEXT_LENGTH:-$(read_config_key daemon_context_tokens)}"
GENERATION_MODEL_COUNT="$(read_config_key generation_model_count)"
if [[ -z "$GENERATION_MODEL_COUNT" || ! "$GENERATION_MODEL_COUNT" =~ ^[0-9]+$ ]]; then
  printf '  WARN invalid generation_model_count from magician-config.yaml\n' >&2
  exit 1
fi
GENERATION_MODELS=()
GENERATION_CONTEXTS=()
for ((index = 0; index < GENERATION_MODEL_COUNT; index++)); do
  model="$(read_config_key "generation_model_${index}")"
  context="$(read_config_key "generation_context_tokens_${index}")"
  if [[ -z "$model" || -z "$context" ]]; then
    printf '  WARN incomplete mapped Ollama generation profile at index %s\n' "$index" >&2
    exit 1
  fi
  GENERATION_MODELS+=("$model")
  GENERATION_CONTEXTS+=("$context")
done
OLLAMA_KV_CACHE_TYPE="${OLLAMA_KV_CACHE_TYPE:-$(read_env_key OLLAMA_KV_CACHE_TYPE)}"
OLLAMA_KV_CACHE_TYPE="${OLLAMA_KV_CACHE_TYPE:-$(resolve_setting MAGICIAN_OLLAMA_KV_CACHE_TYPE kv_cache_type)}"
OLLAMA_FLASH_ATTENTION="${OLLAMA_FLASH_ATTENTION:-$(read_env_key OLLAMA_FLASH_ATTENTION)}"
OLLAMA_FLASH_ATTENTION="${OLLAMA_FLASH_ATTENTION:-$(resolve_setting MAGICIAN_OLLAMA_FLASH_ATTENTION flash_attention)}"
PREWARM="$(resolve_setting MAGICIAN_OLLAMA_PREWARM prewarm)"
REPLACE_EXISTING="$(resolve_setting MAGICIAN_OLLAMA_REPLACE_EXISTING_LOCAL_DAEMON replace_existing_local_daemon)"
OLLAMA_KEEP_ALIVE="$(read_env_key MAGICIAN_OLLAMA_KEEP_ALIVE)"
OLLAMA_KEEP_ALIVE="${OLLAMA_KEEP_ALIVE:-$(read_config_key keep_alive)}"

ok() { printf '  OK %s\n' "$*"; }
warn() { printf '  WARN %s\n' "$*" >&2; }

enabled() {
  case "$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')" in
    0|false|no|off|disabled) return 1 ;;
    *) return 0 ;;
  esac
}

health_url() {
  printf '%s/api/tags' "${OLLAMA_URL%/}"
}

api_url() {
  printf '%s/%s' "${OLLAMA_URL%/}" "$1"
}

server_healthy() {
  command -v curl >/dev/null 2>&1 && curl -fsS --max-time 2 "$(health_url)" >/dev/null 2>&1
}

pid_alive() {
  local pid="$1"
  [[ -n "$pid" ]] && kill -0 "$pid" >/dev/null 2>&1
}

pid_command_matches() {
  local pid="$1"
  local command_line
  command_line="$(ps -p "$pid" -o command= 2>/dev/null || true)"
  [[ "$command_line" == *"ollama"* && "$command_line" == *"serve"* ]]
}

is_local_url() {
  local without_scheme host_port host
  without_scheme="${OLLAMA_URL#http://}"
  without_scheme="${without_scheme#https://}"
  host_port="${without_scheme%%/*}"
  host="${host_port%%:*}"
  case "$host" in
    ""|localhost|127.0.0.1|0.0.0.0|::1|\[::1\]) return 0 ;;
    *) return 1 ;;
  esac
}

settings_payload() {
  cat <<EOF
ollama_url=$OLLAMA_URL
num_parallel=$OLLAMA_NUM_PARALLEL
max_loaded_models=$OLLAMA_MAX_LOADED_MODELS
daemon_context_tokens=$OLLAMA_CONTEXT_LENGTH
kv_cache_type=$OLLAMA_KV_CACHE_TYPE
flash_attention=$OLLAMA_FLASH_ATTENTION
keep_alive=$OLLAMA_KEEP_ALIVE
EOF
  local index
  for ((index = 0; index < GENERATION_MODEL_COUNT; index++)); do
    printf 'generation_model_%s=%s\n' "$index" "${GENERATION_MODELS[$index]}"
    printf 'generation_context_tokens_%s=%s\n' "$index" "${GENERATION_CONTEXTS[$index]}"
  done
}

settings_match() {
  [[ -f "$SETTINGS_FILE" ]] && [[ "$(cat "$SETTINGS_FILE" 2>/dev/null || true)" == "$(settings_payload)" ]]
}

wait_server_down() {
  for _ in $(seq 1 30); do
    if ! server_healthy; then
      return 0
    fi
    sleep 0.25
  done
  return 1
}

stop_owned_daemon() {
  local pid="$1"
  kill "$pid" 2>/dev/null || true
  for _ in $(seq 1 20); do
    if ! pid_alive "$pid"; then
      rm -f "$PID_FILE" "$SETTINGS_FILE"
      wait_server_down || true
      return 0
    fi
    sleep 0.25
  done
  kill -9 "$pid" 2>/dev/null || true
  rm -f "$PID_FILE" "$SETTINGS_FILE"
  wait_server_down || true
}

local_port() {
  local without_scheme host_port
  without_scheme="${OLLAMA_URL#http://}"
  without_scheme="${without_scheme#https://}"
  host_port="${without_scheme%%/*}"
  if [[ "$host_port" == *:* ]]; then
    printf '%s' "${host_port##*:}"
  else
    printf '11434'
  fi
}

replace_existing_local_daemon() {
  local listener_pid command_line port
  if [[ "$(uname -s)" == "Darwin" ]] && command -v osascript >/dev/null 2>&1; then
    osascript -e 'tell application "Ollama" to quit' >/dev/null 2>&1 || true
    wait_server_down && return 0
  fi

  if ! command -v lsof >/dev/null 2>&1; then
    warn "cannot replace existing Ollama daemon: lsof is unavailable"
    return 1
  fi
  port="$(local_port)"
  listener_pid="$(lsof -nP -tiTCP:"$port" -sTCP:LISTEN 2>/dev/null | head -1 || true)"
  command_line="$(ps -p "$listener_pid" -o command= 2>/dev/null || true)"
  if [[ -z "$listener_pid" || "$command_line" != *"ollama"* ]]; then
    warn "refusing to replace non-Ollama listener on $OLLAMA_URL"
    return 1
  fi
  kill "$listener_pid" 2>/dev/null || true
  wait_server_down
}

prewarm_models() {
  if ! enabled "$PREWARM"; then
    ok "Ollama model prewarm skipped"
    return 0
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    warn "python3 unavailable; cannot construct Ollama prewarm payloads"
    return 1
  fi

  local payload index generation_model generation_context generation_expected

  if [[ "$GENERATION_MODEL_COUNT" -eq 0 ]]; then
    # Nothing to load and nothing to verify: the residency verifier refuses an
    # empty expected set, and a failed verification here exits the supervisor.
    ok "Ollama generation prewarm skipped: no local generation model is mapped (privacy.processing.mode is cloud)"
    return 0
  fi

  # Load generation last so mapped generation runners remain preferred under
  # temporary host-memory pressure.
  generation_expected=""
  for ((index = 0; index < GENERATION_MODEL_COUNT; index++)); do
    generation_model="${GENERATION_MODELS[$index]}"
    generation_context="${GENERATION_CONTEXTS[$index]}"
    payload="$(MODEL="$generation_model" CONTEXT="$generation_context" KEEP_ALIVE="$OLLAMA_KEEP_ALIVE" python3 - <<'PY'
import json
import os

body = {
    "model": os.environ["MODEL"],
    "prompt": "Reply with OK.",
    "stream": False,
    "options": {"num_ctx": int(os.environ["CONTEXT"]), "num_predict": 1},
}
if os.environ.get("KEEP_ALIVE"):
    body["keep_alive"] = os.environ["KEEP_ALIVE"]
print(json.dumps(body))
PY
)"
    if curl -fsS --max-time 300 -H "Content-Type: application/json" -d "$payload" "$(api_url api/generate)" >/dev/null; then
      ok "pre-warmed $generation_model at ${generation_context} tokens"
    else
      warn "failed to pre-warm mapped generation model $generation_model; ensure it is installed"
      return 1
    fi
    generation_expected+="${generation_model}"$'\t'"${generation_context}"$'\n'
  done

  local process_state verification_error
  verification_error="model residency did not settle"
  for _ in $(seq 1 20); do
    process_state="$(curl -fsS --max-time 5 "$(api_url api/ps)" 2>/dev/null || true)"
    if [[ -z "$process_state" ]]; then
      verification_error="could not read Ollama process state"
      sleep 0.5
      continue
    fi
    if verification_error="$(
      PROCESS_STATE="$process_state" \
        EXPECTED_MODELS_TSV="$generation_expected" \
        python3 "$SCRIPT_DIR/verify_ollama_residency.py"
    )"; then
      ok "verified all configured Ollama models are resident"
      return 0
    fi
    sleep 0.5
  done
  warn "Ollama prewarm verification failed after 10s: $verification_error"
  return 1
}

remove_stale_pid_file() {
  local existing_pid
  if [[ ! -f "$PID_FILE" ]]; then
    return 0
  fi
  existing_pid="$(cat "$PID_FILE" 2>/dev/null || true)"
  if pid_alive "$existing_pid" && pid_command_matches "$existing_pid"; then
    return 0
  fi
  rm -f "$PID_FILE" "$SETTINGS_FILE"
}

if ! enabled "$AUTOSTART"; then
  ok "Ollama autostart skipped: MAGICIAN_OLLAMA_AUTOSTART=$AUTOSTART"
  exit 0
fi

remove_stale_pid_file

start_embedding_daemon() {
  # Setup temporarily disables this while it pulls a not-yet-installed model.
  if enabled "$EMBEDDING_AUTOSTART"; then
    bash "$SCRIPT_DIR/run-ollama-embedding.sh"
  fi
}

if server_healthy; then
  existing_pid="$(cat "$PID_FILE" 2>/dev/null || true)"
  if pid_alive "$existing_pid" && pid_command_matches "$existing_pid"; then
    if settings_match; then
      ok "Ollama already reachable at $OLLAMA_URL with matching Magician settings (managed pid $existing_pid)"
      prewarm_models
      start_embedding_daemon
      exit 0
    fi
    warn "Magician-owned Ollama settings changed; restarting pid $existing_pid"
    stop_owned_daemon "$existing_pid"
  elif ! is_local_url; then
    ok "Ollama already reachable at remote URL $OLLAMA_URL; leaving it externally managed"
    prewarm_models
    start_embedding_daemon
    exit 0
  elif enabled "$REPLACE_EXISTING"; then
    warn "replacing existing local Ollama daemon so Magician launch settings are deterministic"
    if ! replace_existing_local_daemon; then
      warn "existing local Ollama daemon could not be replaced"
      exit 1
    fi
  else
    warn "Ollama already reachable at $OLLAMA_URL; using it without launch-setting verification"
    prewarm_models
    start_embedding_daemon
    exit 0
  fi
fi

if ! is_local_url; then
  start_embedding_daemon
  warn "Ollama is not reachable at remote URL $OLLAMA_URL; not starting a local daemon"
  exit 0
fi

if ! command -v ollama >/dev/null 2>&1; then
  warn "ollama binary not found; run 'make setup-ollama' or install Ollama manually"
  exit 0
fi

mkdir -p "$STATE_DIR"
printf '==> Starting Ollama at %s (parallel=%s, models=%s, context=%s, kv=%s)\n' \
  "$OLLAMA_URL" "$OLLAMA_NUM_PARALLEL" "$OLLAMA_MAX_LOADED_MODELS" "$OLLAMA_CONTEXT_LENGTH" "$OLLAMA_KV_CACHE_TYPE"
nohup env \
  OLLAMA_NUM_PARALLEL="$OLLAMA_NUM_PARALLEL" \
  OLLAMA_MAX_LOADED_MODELS="$OLLAMA_MAX_LOADED_MODELS" \
  OLLAMA_CONTEXT_LENGTH="$OLLAMA_CONTEXT_LENGTH" \
  OLLAMA_KV_CACHE_TYPE="$OLLAMA_KV_CACHE_TYPE" \
  OLLAMA_FLASH_ATTENTION="$OLLAMA_FLASH_ATTENTION" \
  OLLAMA_KEEP_ALIVE="$OLLAMA_KEEP_ALIVE" \
  ollama serve >>"$LOG_FILE" 2>&1 &
pid="$!"
printf '%s\n' "$pid" >"$PID_FILE"

for _ in $(seq 1 30); do
  if server_healthy; then
    settings_payload >"$SETTINGS_FILE"
    ok "Ollama started with pid $pid"
    prewarm_models
    start_embedding_daemon
    exit 0
  fi
  if ! pid_alive "$pid"; then
    rm -f "$PID_FILE" "$SETTINGS_FILE"
    warn "Ollama exited before becoming healthy; see $LOG_FILE"
    exit 0
  fi
  sleep 1
done

warn "Ollama pid $pid did not become healthy within 30s; see $LOG_FILE"
stop_owned_daemon "$pid"
exit 1
