#!/usr/bin/env bash
set -euo pipefail

port="${MAGICIAN_FLUID_AUDIO_PORT:-3029}"
listener_pids="$(lsof -nP -tiTCP:"${port}" -sTCP:LISTEN 2>/dev/null || true)"

if [[ -z "${listener_pids}" ]]; then
  echo "🔈 No stale FluidAudio listener found on port ${port}."
  exit 0
fi

matched=0
for pid in ${listener_pids}; do
  command_line="$(ps -p "${pid}" -o command= 2>/dev/null || true)"
  if [[ "${command_line}" != *"magician-macos-audio-engine.bin"* ]]; then
    echo "⚠️  Port ${port} is owned by PID ${pid}, but it is not Magician's FluidAudio engine; leaving it untouched."
    continue
  fi

  matched=1
  echo "🔈 Stopping stale FluidAudio engine (PID ${pid}, port ${port})..."
  kill "${pid}" 2>/dev/null || true
  for _ in {1..20}; do
    if ! kill -0 "${pid}" 2>/dev/null; then
      break
    fi
    sleep 0.1
  done
  if kill -0 "${pid}" 2>/dev/null; then
    echo "   FluidAudio PID ${pid} did not exit after 2s; sending SIGKILL."
    kill -9 "${pid}" 2>/dev/null || true
    for _ in {1..10}; do
      if ! kill -0 "${pid}" 2>/dev/null; then
        break
      fi
      sleep 0.05
    done
  fi
done

if [[ "${matched}" -eq 0 ]]; then
  exit 0
fi

remaining="$(lsof -nP -tiTCP:"${port}" -sTCP:LISTEN 2>/dev/null || true)"
if [[ -n "${remaining}" ]]; then
  echo "⚠️  Port ${port} still has a listener after FluidAudio cleanup: ${remaining}" >&2
  exit 1
fi

echo "   FluidAudio listener stopped."
