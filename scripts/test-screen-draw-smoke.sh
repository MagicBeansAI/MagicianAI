#!/usr/bin/env bash
set -euo pipefail

GATEWAY_URL="${MAGICIAN_HOST_GATEWAY_URL:-http://127.0.0.1:3017}"

post_shape() {
  curl -sS -X POST "${GATEWAY_URL%/}/host/overlay/draw" \
    -H "Content-Type: application/json" \
    -d "$1"
  printf '\n'
}

post_shape '{"type":"clear"}'
post_shape '{"type":"highlight","x":220,"y":220,"w":420,"h":140,"color":"orange","label":"Overlay smoke"}'
sleep 1
post_shape '{"type":"arrow","from_x":700,"from_y":290,"to_x":980,"to_y":210,"color":"red","label":"Animated arrow"}'

echo "Overlay smoke test sent. Clear with: scripts/test-screen-draw-clear.sh"
