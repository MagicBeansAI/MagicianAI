#!/usr/bin/env bash
# Select which host-side Magician backend connect.<zone> reaches. The choice is
# explicit and persisted; process discovery never decides between two listeners.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA_DIR="${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}"
TUNNEL_SCRIPT="${MAGICIAN_CONNECT_TUNNEL_SCRIPT:-$ROOT_DIR/scripts/ensure-magician-tunnel.sh}"
MODE="${1:-status}"

log() { printf '==> %s\n' "$*"; }
die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

read_env_key() {
  local key="$1" value file
  value="$(printenv "$key" 2>/dev/null || true)"
  if [ -n "$value" ]; then printf '%s' "$value"; return 0; fi
  for file in "$DATA_DIR/.env.development" "$DATA_DIR/.env"; do
    [ -f "$file" ] || continue
    value="$(grep -E "^[[:space:]]*${key}=" "$file" 2>/dev/null | tail -1 \
      | sed -E "s/^[[:space:]]*${key}=//; s/^[\"']//; s/[\"']\$//")"
    if [ -n "$value" ]; then printf '%s' "$value"; return 0; fi
  done
  printf ''
}

upsert_env_file() {
  local file="$1" key="$2" value="$3" tmp
  mkdir -p "$(dirname "$file")"
  touch "$file"
  chmod 600 "$file" 2>/dev/null || true
  tmp="$(mktemp "${file}.XXXXXX")"
  grep -vE "^[[:space:]]*${key}=" "$file" > "$tmp" 2>/dev/null || true
  printf '%s=%s\n' "$key" "$value" >> "$tmp"
  chmod 600 "$tmp" 2>/dev/null || true
  mv "$tmp" "$file"
}

ZONE="$(read_env_key MAGICIAN_TUNNEL_ZONE)"
[ -n "$ZONE" ] || die "MAGICIAN_TUNNEL_ZONE is required; run make setup-identity first."
CONNECT_HOST="$(read_env_key MAGICIAN_CONNECT_HOST)"
[ -n "$CONNECT_HOST" ] || CONNECT_HOST="$(read_env_key MAGICIAN_IOS_HOST)"
[ -n "$CONNECT_HOST" ] || CONNECT_HOST="connect.${ZONE}"
LOCAL_PORT="$(read_env_key MAGICIAN_CONNECT_LOCAL_API_PORT)"
[ -n "$LOCAL_PORT" ] || LOCAL_PORT=3002
CONTAINER_PORT="$(read_env_key MAGICIAN_CONNECT_CONTAINER_API_PORT)"
[ -n "$CONTAINER_PORT" ] || CONTAINER_PORT=13002
REMOTE_URL="${2:-}"
[ -n "$REMOTE_URL" ] || REMOTE_URL="$(read_env_key MAGICIAN_CONNECT_REMOTE_URL)"
REMOTE_URL="${REMOTE_URL%/}"

port_for_mode() {
  case "$1" in
    local) printf '%s' "$LOCAL_PORT" ;;
    container) printf '%s' "$CONTAINER_PORT" ;;
    *) return 1 ;;
  esac
}

listener_state() {
  local service="$1"
  if curl -fsS --connect-timeout 2 --max-time 5 "${service}/health" >/dev/null 2>&1; then
    printf healthy
  else
    printf unavailable
  fi
}

verify_configured_route() {
  local tunnel_mode api_token connector_token account tunnel response config_file
  tunnel_mode="$(read_env_key MAGICIAN_TUNNEL_MODE)"
  [ -n "$tunnel_mode" ] || tunnel_mode=browser
  if [ "$(printf '%s' "$tunnel_mode" | tr '[:upper:]' '[:lower:]')" = token ]; then
    command -v jq >/dev/null 2>&1 || return 1
    api_token="$(read_env_key CLOUDFLARE_API_TOKEN)"
    [ -n "$api_token" ] || api_token="$(read_env_key CF_API_TOKEN)"
    connector_token="$(read_env_key CLOUDFLARED_TOKEN)"
    [ -n "$api_token" ] && [ -n "$connector_token" ] || return 1
    account="$(printf '%s' "$connector_token" | base64 -d 2>/dev/null | jq -r '.a // empty')"
    tunnel="$(printf '%s' "$connector_token" | base64 -d 2>/dev/null | jq -r '.t // empty')"
    [ -n "$account" ] && [ -n "$tunnel" ] || return 1
    response="$(curl -fsS --connect-timeout 10 --max-time 30 \
      -H "Authorization: Bearer ${api_token}" \
      "https://api.cloudflare.com/client/v4/accounts/${account}/cfd_tunnel/${tunnel}/configurations")" || return 1
    printf '%s' "$response" | jq -e \
      --arg host "$CONNECT_HOST" \
      --arg service "$SERVICE" '
        .success == true and
        (.result.config.ingress // []) as $ingress |
        all(["^/health/?$", "^/api(/.*)?$"][];
          . as $path |
          any($ingress[]?;
            .hostname == $host and .service == $service and .path == $path))
      ' >/dev/null
    return
  fi
  config_file="${CLOUDFLARED_CONFIG:-${CLOUDFLARED_HOME:-$HOME/.cloudflared}/config.yml}"
  [ -f "$config_file" ] || return 1
  awk -v host="$CONNECT_HOST" -v service="$SERVICE" '
    function commit() {
      if (entry_host == host && entry_service == service) {
        if (entry_path == "^/health/?$") health = 1
        if (entry_path == "^/api(/.*)?$") api = 1
      }
    }
    /^  - hostname:/ {
      commit()
      entry_host = substr($0, length("  - hostname: ") + 1)
      entry_path = ""
      entry_service = ""
      next
    }
    /^    path:/ { entry_path = substr($0, length("    path: ") + 1); next }
    /^    service:/ { entry_service = substr($0, length("    service: ") + 1); next }
    END { commit(); exit(health && api ? 0 : 1) }
  ' "$config_file"
}

if [ "$MODE" = status ]; then
  configured="$(read_env_key MAGICIAN_CONNECT_BACKEND)"
  [ -n "$configured" ] || configured=local
  case "$configured" in local|container|remote) ;; *) configured="invalid:${configured}" ;; esac
  printf 'hostname=%s\n' "$CONNECT_HOST"
  printf 'selected_backend=%s\n' "$configured"
  printf 'local=http://127.0.0.1:%s (%s)\n' "$LOCAL_PORT" "$(listener_state "http://127.0.0.1:${LOCAL_PORT}")"
  printf 'container=http://127.0.0.1:%s (%s)\n' "$CONTAINER_PORT" "$(listener_state "http://127.0.0.1:${CONTAINER_PORT}")"
  if [ -n "$REMOTE_URL" ]; then
    printf 'remote=%s (%s)\n' "$REMOTE_URL" "$(listener_state "$REMOTE_URL")"
  else
    printf 'remote=unconfigured\n'
  fi
  exit 0
fi

case "$MODE" in local|container|remote) ;; *) die "usage: $0 local|container|remote [https-origin]|status" ;; esac
if [ "$MODE" = remote ]; then
  [ -n "$REMOTE_URL" ] || die "remote selection requires MAGICIAN_CONNECT_REMOTE_URL or a second HTTPS-origin argument."
  if ! printf '%s' "$REMOTE_URL" | grep -Eq '^https://[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?(:[0-9]{1,5})?$'; then
    die "the remote backend must be an HTTPS origin with no path, query, credentials, or fragment."
  fi
  REMOTE_HOST="$(printf '%s' "$REMOTE_URL" | sed -E 's#^https://([^:/]+).*$#\1#' | tr '[:upper:]' '[:lower:]')"
  if [ "$REMOTE_HOST" = "$(printf '%s' "$CONNECT_HOST" | tr '[:upper:]' '[:lower:]')" ]; then
    die "the remote backend cannot be https://${CONNECT_HOST}; that would route the public endpoint back into itself."
  fi
  SERVICE="$REMOTE_URL"
  HEALTH_ORIGIN="$REMOTE_URL"
  TARGET_LABEL="remote backend at ${REMOTE_URL}"
else
  PORT="$(port_for_mode "$MODE")"
  case "$PORT" in ''|*[!0-9]*) die "the ${MODE} backend port is not numeric: ${PORT}" ;; esac
  [ "$PORT" -ge 1 ] && [ "$PORT" -le 65535 ] || die "the ${MODE} backend port is outside 1..65535: ${PORT}"
  SERVICE="http://localhost:${PORT}"
  HEALTH_ORIGIN="http://127.0.0.1:${PORT}"
  TARGET_LABEL="${MODE} backend on 127.0.0.1:${PORT}"
fi

if [ "$(listener_state "$HEALTH_ORIGIN")" != healthy ]; then
  die "the ${MODE} backend is not healthy at ${HEALTH_ORIGIN}/health; the public route was not changed."
fi
[ -f "$TUNNEL_SCRIPT" ] || die "tunnel helper is missing: ${TUNNEL_SCRIPT}"

log "Routing https://${CONNECT_HOST} to the healthy ${TARGET_LABEL}"
MAGICIAN_CONNECT_HOST="$CONNECT_HOST" \
MAGICIAN_CONNECT_BACKEND="$MODE" \
MAGICIAN_CONNECT_API_PORT="${PORT:-}" \
MAGICIAN_CONNECT_REMOTE_URL="$REMOTE_URL" \
MAGICIAN_CONNECT_SERVICE="$SERVICE" \
bash "$TUNNEL_SCRIPT"
verify_configured_route \
  || die "Cloudflare did not confirm https://${CONNECT_HOST} -> ${SERVICE}; the saved selection was not changed."

# Persist only after the tunnel helper completed. The hostname is written with
# the choice so a future zone change cannot silently move enrolled devices.
for env_file in "$DATA_DIR/.env" "$DATA_DIR/.env.development"; do
  upsert_env_file "$env_file" MAGICIAN_CONNECT_HOST "$CONNECT_HOST"
  upsert_env_file "$env_file" MAGICIAN_CONNECT_BACKEND "$MODE"
  upsert_env_file "$env_file" MAGICIAN_CONNECT_LOCAL_API_PORT "$LOCAL_PORT"
  upsert_env_file "$env_file" MAGICIAN_CONNECT_CONTAINER_API_PORT "$CONTAINER_PORT"
  if [ -n "$REMOTE_URL" ]; then
    upsert_env_file "$env_file" MAGICIAN_CONNECT_REMOTE_URL "$REMOTE_URL"
  fi
done
log "Selected ${MODE}; devices continue using https://${CONNECT_HOST}."
