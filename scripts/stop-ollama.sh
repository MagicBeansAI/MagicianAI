#!/usr/bin/env bash
# stop-ollama.sh - unload Ollama models and stop only Magician-launched daemon.
#
# Model unload is independent from daemon ownership: by default this asks an
# already reachable Ollama server to unload resident models so local memory is
# released after Magician work. This stop path intentionally avoids the `ollama`
# CLI because invoking it on macOS can launch the menu bar app/daemon while we
# are trying to stop. Process termination remains ownership-safe: only the PID
# written by run-ollama.sh is stopped.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

STATE_DIR="${MAGICIAN_OLLAMA_STATE_DIR:-$ROOT_DIR/.cache/ollama}"
PID_FILE="${MAGICIAN_OLLAMA_PID_FILE:-$STATE_DIR/ollama.pid}"
SETTINGS_FILE="${MAGICIAN_OLLAMA_SETTINGS_FILE:-$STATE_DIR/ollama.settings}"
OLLAMA_URL="${MAGICIAN_OLLAMA_URL:-${MAGICIAN_OLLAMA_BASE_URL:-http://127.0.0.1:11434}}"
UNLOAD_ON_STOP="${MAGICIAN_OLLAMA_UNLOAD_ON_STOP:-true}"

ok() { printf '  OK %s\n' "$*"; }
warn() { printf '  WARN %s\n' "$*" >&2; }

enabled() {
  case "$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')" in
    0|false|no|off|disabled) return 1 ;;
    *) return 0 ;;
  esac
}

# Stop the independently owned embedding daemon even if the generation daemon
# has no PID file or exits through an early return below.
bash "$SCRIPT_DIR/stop-ollama-embedding.sh" || true

api_url() {
  printf '%s/%s' "${OLLAMA_URL%/}" "$1"
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

loaded_models() {
  if command -v curl >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; then
    local payload
    payload="$(curl -fsS --max-time 2 "$(api_url api/ps)" 2>/dev/null || true)"
    if [[ -n "$payload" ]]; then
      PAYLOAD="$payload" python3 - <<'PY'
import json
import os

try:
    data = json.loads(os.environ["PAYLOAD"])
except Exception:
    data = {}

seen = set()
for item in data.get("models", []):
    if not isinstance(item, dict):
        continue
    name = item.get("model") or item.get("name")
    if isinstance(name, str) and name and name not in seen:
        seen.add(name)
        print(name)
PY
      return 0
    fi
  fi
}

unload_model() {
  local model="$1"
  if command -v curl >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; then
    local payload
    payload="$(MODEL="$model" python3 - <<'PY'
import json
import os

print(json.dumps({"model": os.environ["MODEL"], "keep_alive": 0}))
PY
)"
    if curl -fsS --max-time 10 -H "Content-Type: application/json" -d "$payload" "$(api_url api/generate)" >/dev/null 2>&1; then
      ok "unloaded Ollama model $model"
      return 0
    fi
  fi

  warn "could not unload Ollama model $model"
  return 1
}

unload_models() {
  local models model count
  models="$(loaded_models || true)"
  if [[ -z "$models" ]]; then
    ok "No loaded Ollama models to unload"
    return 0
  fi

  count="$(printf '%s\n' "$models" | sed '/^[[:space:]]*$/d' | wc -l | tr -d ' ')"
  printf '==> Unloading %s Ollama model(s)\n' "$count"
  while IFS= read -r model; do
    [[ -z "$model" ]] && continue
    unload_model "$model" || true
  done <<<"$models"
}

if enabled "$UNLOAD_ON_STOP"; then
  unload_models
else
  ok "Ollama model unload skipped: MAGICIAN_OLLAMA_UNLOAD_ON_STOP=$UNLOAD_ON_STOP"
fi

if [[ ! -f "$PID_FILE" ]]; then
  rm -f "$SETTINGS_FILE"
  ok "No Magician-launched Ollama PID file found"
  exit 0
fi

pid="$(cat "$PID_FILE" 2>/dev/null || true)"
if ! pid_alive "$pid"; then
  rm -f "$PID_FILE" "$SETTINGS_FILE"
  ok "Removed stale Ollama PID file"
  exit 0
fi

if ! pid_command_matches "$pid"; then
  warn "PID $pid is not an ollama serve process; leaving it running"
  rm -f "$PID_FILE" "$SETTINGS_FILE"
  exit 0
fi

printf '==> Stopping Magician-launched Ollama daemon (pid %s)\n' "$pid"
kill "$pid" 2>/dev/null || true
for _ in $(seq 1 10); do
  if ! pid_alive "$pid"; then
    rm -f "$PID_FILE"
    rm -f "$SETTINGS_FILE"
    ok "Ollama daemon stopped"
    exit 0
  fi
  sleep 1
done

warn "Ollama pid $pid did not stop after SIGTERM; sending SIGKILL"
kill -9 "$pid" 2>/dev/null || true
rm -f "$PID_FILE" "$SETTINGS_FILE"
ok "Ollama daemon stopped"
