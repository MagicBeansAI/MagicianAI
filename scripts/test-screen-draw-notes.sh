#!/usr/bin/env bash
set -euo pipefail

GATEWAY_URL="${MAGICIAN_HOST_GATEWAY_URL:-http://127.0.0.1:3017}"
X="${1:-1152}"
Y="${2:-127}"
W="${3:-42}"
H="${4:-44}"
COLOR="${5:-red}"
LABEL="${6:-New note}"

payload=$(
  printf '{"type":"highlight","x":%s,"y":%s,"w":%s,"h":%s,"color":"%s","label":"%s"}' \
    "$X" "$Y" "$W" "$H" "$COLOR" "$LABEL"
)

curl -sS -X POST "${GATEWAY_URL%/}/host/overlay/draw" \
  -H "Content-Type: application/json" \
  -d '{"type":"clear"}'
printf '\n'

curl -sS -X POST "${GATEWAY_URL%/}/host/overlay/draw" \
  -H "Content-Type: application/json" \
  -d "$payload"
printf '\n'

echo "Notes highlight sent at x=${X}, y=${Y}, w=${W}, h=${H}. Clear with: scripts/test-screen-draw-clear.sh"
