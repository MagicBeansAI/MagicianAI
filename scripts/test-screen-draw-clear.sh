#!/usr/bin/env bash
set -euo pipefail

GATEWAY_URL="${MAGICIAN_HOST_GATEWAY_URL:-http://127.0.0.1:3017}"

curl -sS -X POST "${GATEWAY_URL%/}/host/overlay/draw" \
  -H "Content-Type: application/json" \
  -d '{"type":"clear"}'

printf '\n'
