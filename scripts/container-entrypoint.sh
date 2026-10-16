#!/usr/bin/env bash
set -euo pipefail
# System owners share runtime directories; create them privately before any
# subsystem can establish a permissive parent for the sealed pairing roster.
umask 077

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNTIME_ROOT="${MAGICIAN_ROOT_DIR:-/data}"
if [[ -n "${MAGICIAN_KEYRING_STATE_DIR:-}" || -n "${MAGICIAN_KEYRING_PASSWORD_FILE:-}" ]]; then
  if [[ -z "${MAGICIAN_KEYRING_STATE_DIR:-}" || -z "${MAGICIAN_KEYRING_PASSWORD_FILE:-}" ]]; then
    echo "error: configure both MAGICIAN_KEYRING_STATE_DIR and MAGICIAN_KEYRING_PASSWORD_FILE" >&2
    exit 1
  fi
  # Private bind mounts can retain guest-root attribute-cache entries briefly
  # at VM startup. Observe readiness before any seeding; never widen permissions.
  python3 "$ROOT_DIR/scripts/run-linux-keyring.py" --check-inputs \
    --state-dir "$MAGICIAN_KEYRING_STATE_DIR" \
    --password-file "$MAGICIAN_KEYRING_PASSWORD_FILE"
fi
DEST="$RUNTIME_ROOT/magician-config.yaml"
cd "$ROOT_DIR"

seed_if_missing() {
  local src="$1"
  local dest="$2"
  local label="$3"

  if [ -e "$dest" ]; then
    return
  fi
  if [ ! -f "$src" ]; then
    echo "warning: seed source missing for $label: $src" >&2
    return
  fi

  mkdir -p "$(dirname "$dest")"
  cp "$src" "$dest"
  echo "Seeded $label: $dest"
}

if [ ! -e "$DEST" ]; then
  SRC="$ROOT_DIR/magician-config.yaml"
  LABEL="magician-config.yaml"

  if [ -f "$SRC" ]; then
    mkdir -p "$RUNTIME_ROOT"
    cp "$SRC" "$DEST"
    echo "Seeded runtime config from $LABEL: $DEST"
  else
    echo "warning: no config seed found for $DEST" >&2
  fi
fi

SEED_ROOT="${MAGICIAN_SEED_ROOT:-$ROOT_DIR/magician_data_v3}"
# Router profiles/mappings are loaded beside magician-config.yaml. Seed even
# when the main config already exists (upgrades from before the split).
seed_if_missing "$ROOT_DIR/llm-router.yaml" "$RUNTIME_ROOT/llm-router.yaml" "LLM router tables"
# The decision engine's settings live beside it (Magician keeps only the
# socket); seeded on upgrade too, like the router tables.
seed_if_missing "$ROOT_DIR/decision-engine.yaml" "$RUNTIME_ROOT/decision-engine.yaml" "decision engine settings"
for required in "$DEST" "$RUNTIME_ROOT/llm-router.yaml"; do
  if [ ! -f "$required" ] || [ ! -s "$required" ]; then
    echo "error: required runtime config is missing or empty: $required" >&2
    exit 1
  fi
done

DEFAULT_SCOPE_SEED="$SEED_ROOT/scopes/anonymous/default"
DEFAULT_SCOPE_ROOT="$RUNTIME_ROOT/scopes/anonymous/default"

seed_if_missing \
  "$DEFAULT_SCOPE_SEED/programs/harness_reliability.md" \
  "$DEFAULT_SCOPE_ROOT/programs/harness_reliability.md" \
  "default harness reliability program"

for agent in harness-sre cto internal-system-analyst; do
  seed_if_missing \
    "$DEFAULT_SCOPE_SEED/agent_runtime/agents/$agent/definition.agent.yaml" \
    "$DEFAULT_SCOPE_ROOT/agent_runtime/agents/$agent/definition.agent.yaml" \
    "default agent definition $agent"
done

# A clean mounted runtime root has no materialized skills. Install the browser
# skill as file-level symlinks to the image's pinned skillshub source before the
# supervisor starts, preserving any real per-scope config/.env managed by the
# installer. This makes the runtime-required scoped binary path deterministic
# on first boot and refreshes stale host-absolute links after an image upgrade.
# Host automation packages contain Linux Python/procedure files; their macOS
# binaries and permissions remain with the desktop's private relay.
bash "$ROOT_DIR/scripts/materialize-container-browser-skill.sh" \
  "$ROOT_DIR/skillshub" \
  "$DEFAULT_SCOPE_ROOT/skills"

# Managed containers use guest loopback for the desktop-owned exec relay. Give
# it a bounded startup window before Magician caches host capability readiness.
# Headless/standalone containers without this explicit URL do not wait.
if [[ "${MAGICIAN_HOST_GATEWAY_URL:-}" == "http://127.0.0.1:3017" ]]; then
  python3 - <<'PY'
import time, urllib.request
deadline = time.monotonic() + 20
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
while time.monotonic() < deadline:
    try:
        with opener.open("http://127.0.0.1:3017/health", timeout=1) as response:
            if response.status == 200:
                break
    except OSError:
        time.sleep(0.2)
else:
    print("warning: desktop host relay unavailable; host skills need a service restart after the desktop reconnects", flush=True)
PY
fi

if [[ -n "${MAGICIAN_KEYRING_STATE_DIR:-}" || -n "${MAGICIAN_KEYRING_PASSWORD_FILE:-}" ]]; then
  exec python3 "$ROOT_DIR/scripts/run-linux-keyring.py" \
    --state-dir "$MAGICIAN_KEYRING_STATE_DIR" \
    --password-file "$MAGICIAN_KEYRING_PASSWORD_FILE" \
    -- "$ROOT_DIR/magic-supervisor"
fi

exec "$ROOT_DIR/magic-supervisor"
