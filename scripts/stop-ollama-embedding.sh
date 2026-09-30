#!/usr/bin/env bash
# Stop only the dedicated embedding Ollama daemon owned by Magician.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
STATE_DIR="${MAGICIAN_OLLAMA_STATE_DIR:-$ROOT_DIR/.cache/ollama}"
PID_FILE="${MAGICIAN_OLLAMA_EMBEDDING_PID_FILE:-$STATE_DIR/embedding.pid}"
SETTINGS_FILE="${MAGICIAN_OLLAMA_EMBEDDING_SETTINGS_FILE:-$STATE_DIR/embedding.settings}"

ok() { printf '  OK %s\n' "$*"; }
warn() { printf '  WARN %s\n' "$*" >&2; }
pid_alive() { [[ -n "$1" ]] && kill -0 "$1" >/dev/null 2>&1; }
pid_matches() { local line; line="$(ps -p "$1" -o command= 2>/dev/null || true)"; [[ "$line" == *ollama* && "$line" == *serve* ]]; }

if [[ ! -f "$PID_FILE" ]]; then
  rm -f "$SETTINGS_FILE"
  ok "No Magician-launched embedding Ollama PID file found"
  exit 0
fi
pid="$(cat "$PID_FILE" 2>/dev/null || true)"
if ! pid_alive "$pid"; then
  rm -f "$PID_FILE" "$SETTINGS_FILE"
  ok "Removed stale embedding Ollama PID file"
  exit 0
fi
if ! pid_matches "$pid"; then
  warn "PID $pid is not an ollama serve process; leaving it running"
  rm -f "$PID_FILE" "$SETTINGS_FILE"
  exit 0
fi

printf '==> Stopping Magician-launched embedding Ollama daemon (pid %s)\n' "$pid"
kill "$pid" 2>/dev/null || true
for _ in $(seq 1 10); do
  if ! pid_alive "$pid"; then
    rm -f "$PID_FILE" "$SETTINGS_FILE"
    ok "Embedding Ollama daemon stopped"
    exit 0
  fi
  sleep 1
done
warn "Embedding Ollama pid $pid did not stop after SIGTERM; sending SIGKILL"
kill -9 "$pid" 2>/dev/null || true
rm -f "$PID_FILE" "$SETTINGS_FILE"
ok "Embedding Ollama daemon stopped"
