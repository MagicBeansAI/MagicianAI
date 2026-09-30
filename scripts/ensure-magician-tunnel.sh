#!/usr/bin/env bash
# ensure-magician-tunnel.sh — public Cloudflare Tunnel (cloudflared) for the
# Kapso WhatsApp webhook ingress (and, optionally, the dev UI), on the operator's
# own domain. Replaces the old Tailscale funnel (ensure-magician-funnel.sh).
#
# Why cloudflared + a NAMED tunnel: a named tunnel + a DNS route on the operator
# domain gives a STABLE public HTTPS URL (for example, webhook.<zone>/webhook)
# that you set in Kapso ONCE — unlike a Tailscale `*.ts.net` host (derived live)
# or a cloudflared *quick* tunnel (`--url`, ephemeral `*.trycloudflare.com`).
#
# Upstream is unchanged: the host-native kapso bot receiver listens on
# WEBHOOK_PORT (default 3010) and forwards each callback to magician :3002. The
# tunnel's ingress maps  <WEBHOOK_HOST> -> http://localhost:<WEBHOOK_PORT>  and
# (when a UI host is configured)  <UI_HOST> -> http://localhost:<UI_PORT>.
# The device connection host <CONNECT_HOST> is PATH-SPLIT: /host/* -> the desktop
# gateway; /health + /api/* (REST + WebSockets) -> the explicitly selected
# Magician backend. The stable hostname does not change when that backend moves.
#
# TWO AUTH MODES — MAGICIAN_TUNNEL_MODE (default "browser"):
#   browser — cert.pem login (`cloudflared tunnel login`) + a locally-managed
#             NAMED tunnel: this script writes ~/.cloudflared/config.yml ingress,
#             ensures the DNS routes, and runs it via brew services.
#   token   — a DASHBOARD-created tunnel run headless with its CONNECTOR token
#             (CLOUDFLARED_TOKEN). Ingress (public hostnames) + DNS live in the
#             Cloudflare Zero Trust dashboard, so this script does NO login /
#             config.yml / route-dns work — it just runs `cloudflared tunnel run
#             --token …`. token REQUIRES a non-empty CLOUDFLARED_TOKEN (resolved
#             from the process env, else the runtime .env files); if it is missing
#             the script FALLS BACK to browser mode and logs that it did. The
#             effective mode is always echoed to the terminal.
#
# In BROWSER mode the first run is interactive and is NOT scripted (browser OAuth +
# a one-time tunnel create). When cloudflared is not logged in / the named tunnel
# does not exist, this script prints the exact one-time steps LOUDLY and exits 0
# (never opens a browser, never sudo). Once set up it is idempotent: ensures the
# config, the DNS routes, and the running service. In BOTH modes it ALWAYS writes
# the resolved webhook URL to $DATA_DIR/funnel-url (what install-verify.sh reads)
# and prints it.
#
# Honours MAGICIAN_INSTALL_DRYRUN=1 (prints "would run" instead of mutating
# anything) so the installer orchestration can be verified without touching DNS
# or the service.
set -euo pipefail

# --- inputs (env-overridable) ----------------------------------------------
TUNNEL_NAME="${MAGICIAN_TUNNEL_NAME:-magician}"
ZONE="${MAGICIAN_TUNNEL_ZONE:-}"
if [ -z "$ZONE" ]; then
  # Identity-layer fallback: the zone written by `make setup-identity`.
  for _zf in "${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}/.env" "${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}/.env.development"; do
    [ -f "$_zf" ] || continue
    _zl="$(grep -E '^MAGICIAN_TUNNEL_ZONE=' "$_zf" | tail -1)"
    if [ -n "$_zl" ]; then ZONE="${_zl#MAGICIAN_TUNNEL_ZONE=}"; ZONE="${ZONE%\"}"; ZONE="${ZONE#\"}"; break; fi
  done
fi
if [ -z "$ZONE" ]; then
  printf '\033[1;31mERROR: MAGICIAN_TUNNEL_ZONE is not set — your Cloudflare zone (e.g. example.com). Run `make setup-identity` or export it; no zone is ever defaulted.\033[0m\n' >&2
  exit 1
fi
WEBHOOK_HOST="${MAGICIAN_WEBHOOK_HOST:-webhook.${ZONE}}"   # Kapso ingress
KAPSO_WEBHOOK_PORT="${KAPSO_WEBHOOK_PORT:-3010}"
WEBHOOK_PATH="${MAGICIAN_WEBHOOK_PATH:-/webhook}"
# Optional dev-UI ingress. Empty UI_HOST -> only the webhook hostname is mapped.
# Default ON (Tailscale dropped) so the private :8443 serve has a replacement;
# put ui.<zone> behind a Cloudflare Access policy so it is not open to the world.
DATA_DIR="${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}"
UI_HOST="${MAGICIAN_UI_HOST:-ui.${ZONE}}"
UI_PORT="${MAGICIAN_UI_PORT:-5173}"
CONNECT_HOST="${MAGICIAN_CONNECT_HOST:-${MAGICIAN_IOS_HOST:-}}"
CONNECT_GATEWAY_PORT="${MAGICIAN_CONNECT_GATEWAY_PORT:-${MAGICIAN_IOS_PORT:-3017}}"
NOTES_HOST="${MAGICIAN_NOTES_HOST:-notes.${ZONE}}"
NOTES_PORT="${MAGICIAN_SILVERBULLET_PORT:-3021}"
NOTES_READY_FILE="${MAGICIAN_NOTES_ACCESS_READY_FILE:-$DATA_DIR/.magician/notes-access-ready}"
NOTES_ATTESTATION_MAX_AGE="${MAGICIAN_NOTES_ACCESS_ATTESTATION_MAX_AGE_SECONDS:-900}"
CF_DIR="${CLOUDFLARED_HOME:-$HOME/.cloudflared}"
CF_CONFIG="${CLOUDFLARED_CONFIG:-$CF_DIR/config.yml}"
TOKEN_LAUNCH_LABEL="${MAGICIAN_CLOUDFLARED_TOKEN_LAUNCH_LABEL:-com.magican.magician-cloudflared-token}"
TOKEN_FILE="${MAGICIAN_CLOUDFLARED_TOKEN_FILE:-$CF_DIR/magician-token}"
DRYRUN="${MAGICIAN_INSTALL_DRYRUN:-0}"

log()  { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }
warn() { printf '\033[1;33mWARN: %s\033[0m\n' "$*" >&2; }
would_run() { if [ "$DRYRUN" = 1 ]; then log "would run: $*"; return 0; fi; "$@"; }

WEBHOOK_URL="https://${WEBHOOK_HOST}${WEBHOOK_PATH}"

# read_env_key KEY — resolve a flag/secret from the process env first, else from
# the runtime .env files ($DATA_DIR/.env.development preferred, then $DATA_DIR/.env),
# taking only an UNcommented `KEY=...` line and stripping surrounding quotes. Lets
# this script pick up CLOUDFLARED_TOKEN / MAGICIAN_TUNNEL_MODE whether the caller
# exported them or they only live in the runtime .env. The VALUE is never logged.
read_env_key() {
  local key="$1" val f
  val="$(printenv "$key" 2>/dev/null || true)"
  if [ -n "$val" ]; then printf '%s' "$val"; return 0; fi
  for f in "$DATA_DIR/.env.development" "$DATA_DIR/.env"; do
    [ -f "$f" ] || continue
    val="$(grep -E "^[[:space:]]*${key}=" "$f" 2>/dev/null | tail -1 \
      | sed -E "s/^[[:space:]]*${key}=//; s/^[\"']//; s/[\"']\$//")"
    if [ -n "$val" ]; then printf '%s' "$val"; return 0; fi
  done
  printf ''
}

# The public origin is platform-neutral. MAGICIAN_IOS_HOST and
# MAGICIAN_API_PORT remain read-only compatibility aliases for deployments made
# before Android and ESP32 shared the same endpoint.
[ -n "$CONNECT_HOST" ] || CONNECT_HOST="$(read_env_key MAGICIAN_CONNECT_HOST)"
[ -n "$CONNECT_HOST" ] || CONNECT_HOST="$(read_env_key MAGICIAN_IOS_HOST)"
[ -n "$CONNECT_HOST" ] || CONNECT_HOST="connect.${ZONE}"
CONNECT_BACKEND="$(read_env_key MAGICIAN_CONNECT_BACKEND)"
[ -n "$CONNECT_BACKEND" ] || CONNECT_BACKEND="local"
CONNECT_BACKEND="$(printf '%s' "$CONNECT_BACKEND" | tr '[:upper:]' '[:lower:]')"
CONNECT_API_PORT="$(read_env_key MAGICIAN_CONNECT_API_PORT)"
CONNECT_REMOTE_URL="$(read_env_key MAGICIAN_CONNECT_REMOTE_URL)"
[ -n "$CONNECT_REMOTE_URL" ] || CONNECT_REMOTE_URL="${MAGICIAN_CONNECT_REMOTE_URL:-}"
CONNECT_REMOTE_URL="${CONNECT_REMOTE_URL%/}"
if [ -z "$CONNECT_API_PORT" ]; then
  CONNECT_API_PORT="$(read_env_key MAGICIAN_API_PORT)"
fi
case "$CONNECT_BACKEND" in
  local)
    [ -n "$CONNECT_API_PORT" ] || CONNECT_API_PORT="$(read_env_key MAGICIAN_CONNECT_LOCAL_API_PORT)"
    [ -n "$CONNECT_API_PORT" ] || CONNECT_API_PORT=3002
    ;;
  container)
    [ -n "$CONNECT_API_PORT" ] || CONNECT_API_PORT="$(read_env_key MAGICIAN_CONNECT_CONTAINER_API_PORT)"
    [ -n "$CONNECT_API_PORT" ] || CONNECT_API_PORT=13002
    ;;
  remote)
    [ -n "$CONNECT_REMOTE_URL" ] || {
      printf '\033[1;31mERROR: MAGICIAN_CONNECT_REMOTE_URL is required when MAGICIAN_CONNECT_BACKEND=remote.\033[0m\n' >&2
      exit 1
    }
    if ! printf '%s' "$CONNECT_REMOTE_URL" | grep -Eq '^https://[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?(:[0-9]{1,5})?$'; then
      printf '\033[1;31mERROR: MAGICIAN_CONNECT_REMOTE_URL must be an HTTPS origin with no path, query, credentials, or fragment.\033[0m\n' >&2
      exit 1
    fi
    CONNECT_REMOTE_HOST="$(printf '%s' "$CONNECT_REMOTE_URL" | sed -E 's#^https://([^:/]+).*$#\1#' | tr '[:upper:]' '[:lower:]')"
    if [ "$CONNECT_REMOTE_HOST" = "$(printf '%s' "$CONNECT_HOST" | tr '[:upper:]' '[:lower:]')" ]; then
      printf '\033[1;31mERROR: the remote backend cannot equal https://%s; that would create an ingress loop.\033[0m\n' "$CONNECT_HOST" >&2
      exit 1
    fi
    CONNECT_SERVICE="$CONNECT_REMOTE_URL"
    ;;
  *)
    printf '\033[1;31mERROR: MAGICIAN_CONNECT_BACKEND must be local, container, or remote; got %s.\033[0m\n' "$CONNECT_BACKEND" >&2
    exit 1
    ;;
esac
if [ "$CONNECT_BACKEND" != remote ]; then
  case "$CONNECT_API_PORT" in
    ''|*[!0-9]*) printf '\033[1;31mERROR: the selected Magician connection API port must be numeric.\033[0m\n' >&2; exit 1 ;;
  esac
  if [ "$CONNECT_API_PORT" -lt 1 ] || [ "$CONNECT_API_PORT" -gt 65535 ]; then
    printf '\033[1;31mERROR: the selected Magician connection API port must be between 1 and 65535.\033[0m\n' >&2
    exit 1
  fi
  CONNECT_SERVICE="http://localhost:${CONNECT_API_PORT}"
fi
if [ -n "${MAGICIAN_CONNECT_SERVICE:-}" ] && [ "$MAGICIAN_CONNECT_SERVICE" != "$CONNECT_SERVICE" ]; then
  printf '\033[1;31mERROR: MAGICIAN_CONNECT_SERVICE does not match the selected backend target.\033[0m\n' >&2
  exit 1
fi
log "Device connection route: https://${CONNECT_HOST} -> ${CONNECT_BACKEND} backend at ${CONNECT_SERVICE}"

# Runtime env files may own these non-secret deployment values. A notes route is
# enabled only when a previous Access provisioning run wrote an exact readiness
# attestation. Requesting it explicitly without that proof still fails closed.
NOTES_HOST_ENV="$(read_env_key MAGICIAN_NOTES_HOST)"
[ -z "$NOTES_HOST_ENV" ] || NOTES_HOST="$NOTES_HOST_ENV"
NOTES_PORT_ENV="$(read_env_key MAGICIAN_SILVERBULLET_PORT)"
[ -z "$NOTES_PORT_ENV" ] || NOTES_PORT="$NOTES_PORT_ENV"
NOTES_ATTESTATION_MAX_AGE_ENV="$(read_env_key MAGICIAN_NOTES_ACCESS_ATTESTATION_MAX_AGE_SECONDS)"
[ -z "$NOTES_ATTESTATION_MAX_AGE_ENV" ] || NOTES_ATTESTATION_MAX_AGE="$NOTES_ATTESTATION_MAX_AGE_ENV"
# Token-mode migrations may add exact hostnames to the dashboard-managed tunnel
# without making this deployment's primary zone switch immediately. Preserve
# only the explicitly declared remote hosts during reconciliation; this keeps a
# coexistence alias from being erased while avoiding ownership of unrelated
# dashboard rules. The list is comma-separated and does not create routes/DNS.
PRESERVED_REMOTE_HOSTS="$(read_env_key MAGICIAN_TUNNEL_PRESERVED_REMOTE_HOSTS)"
NOTES_TUNNEL_MODE="$(read_env_key MAGICIAN_NOTES_TUNNEL_ENABLED)"
[ -n "$NOTES_TUNNEL_MODE" ] || NOTES_TUNNEL_MODE="auto"
case "$(printf '%s' "$NOTES_TUNNEL_MODE" | tr '[:upper:]' '[:lower:]')" in
  0|false|off) NOTES_TUNNEL_MODE=0 ;;
  1|true|on) NOTES_TUNNEL_MODE=1 ;;
  auto) NOTES_TUNNEL_MODE=auto ;;
  *) warn "Unknown MAGICIAN_NOTES_TUNNEL_ENABLED='${NOTES_TUNNEL_MODE}' — using fail-closed auto mode."; NOTES_TUNNEL_MODE=auto ;;
esac
# The SilverBullet notes server is gone, so its public hostname is never published.
NOTES_TUNNEL_MODE=0
NOTES_ENABLED=0
NOTES_ATTESTATION_FRESH=0
NOTES_ATTESTATION_VALID=0
NOTES_ATTESTATION_TIME_VALID=0
NOTES_PRESERVE_EXISTING=0
if [ -f "$NOTES_READY_FILE" ]; then
  NOTES_VERIFIED_AT="$(sed -n 's/^verified_at_epoch=//p' "$NOTES_READY_FILE" | tail -1)"
  case "$NOTES_VERIFIED_AT:$NOTES_ATTESTATION_MAX_AGE" in
    *[!0-9:]*|:*|*:) ;;
    *)
      NOTES_ATTESTATION_AGE="$(( $(date +%s) - NOTES_VERIFIED_AT ))"
      if [ "$NOTES_ATTESTATION_AGE" -ge 0 ]; then
        NOTES_ATTESTATION_TIME_VALID=1
        if [ "$NOTES_ATTESTATION_AGE" -le "$NOTES_ATTESTATION_MAX_AGE" ]; then
          NOTES_ATTESTATION_FRESH=1
        fi
      fi
      ;;
  esac
fi
if [ -f "$NOTES_READY_FILE" ] \
  && grep -Fxq "hostname=${NOTES_HOST}" "$NOTES_READY_FILE" \
  && grep -Eq '^access_app_id=.+$' "$NOTES_READY_FILE" \
  && grep -Fxq "service_token_policy=verified" "$NOTES_READY_FILE" \
  && grep -Fxq "owner_allow_policy=verified" "$NOTES_READY_FILE" \
  && grep -Fxq "service_worker_bypass=verified" "$NOTES_READY_FILE" \
  && grep -Fxq "client_runtime_bypass=verified" "$NOTES_READY_FILE"; then
  NOTES_ATTESTATION_VALID=1
fi
if [ "$NOTES_TUNNEL_MODE" != 0 ] \
  && [ "$NOTES_ATTESTATION_VALID" = 1 ] \
  && [ "$NOTES_ATTESTATION_FRESH" = 1 ]; then
  NOTES_ENABLED=1
elif [ "$NOTES_TUNNEL_MODE" != 0 ] \
  && [ "$NOTES_ATTESTATION_VALID" = 1 ] \
  && [ "$NOTES_ATTESTATION_TIME_VALID" = 1 ]; then
  # A short-lived attestation authorizes only a NEW publication. Once a route
  # has been published behind verified Access policies, an unrelated tunnel
  # reconciliation must not erase it merely because the marker aged out.
  # Each mode below preserves only an exact existing loopback route; it never
  # creates one from stale evidence.
  NOTES_PRESERVE_EXISTING=1
  warn "Notes Access verification is stale; a new ${NOTES_HOST} route remains fail-closed, but an exact existing loopback route will be preserved. Run make ensure-notes-tunnel to re-verify it."
elif [ "$NOTES_TUNNEL_MODE" = 1 ]; then
  warn "Notes tunnel requested, but a fresh matching Access readiness attestation is absent at ${NOTES_READY_FILE}; refusing to publish ${NOTES_HOST}. Run make notes-access first."
fi

# Resolve the effective auth mode. token REQUIRES a non-empty CLOUDFLARED_TOKEN;
# if it is missing we fall back to browser login (and say so). Token value is
# never printed — only the resolved mode.
CLOUDFLARED_TOKEN="$(read_env_key CLOUDFLARED_TOKEN)"
TUNNEL_MODE_RAW="$(read_env_key MAGICIAN_TUNNEL_MODE)"
[ -n "$TUNNEL_MODE_RAW" ] || TUNNEL_MODE_RAW="browser"
case "$(printf '%s' "$TUNNEL_MODE_RAW" | tr '[:upper:]' '[:lower:]')" in
  token)
    if [ -n "$CLOUDFLARED_TOKEN" ]; then
      EFFECTIVE_MODE="token"
    else
      warn "MAGICIAN_TUNNEL_MODE=token requested but CLOUDFLARED_TOKEN is empty/unset (checked env + \$DATA_DIR/.env.development + \$DATA_DIR/.env) — FALLING BACK to BROWSER login."
      EFFECTIVE_MODE="browser"
    fi
    ;;
  browser|"") EFFECTIVE_MODE="browser" ;;
  *) warn "Unknown MAGICIAN_TUNNEL_MODE='${TUNNEL_MODE_RAW}' — using BROWSER login."; EFFECTIVE_MODE="browser" ;;
esac
if [ "$EFFECTIVE_MODE" = "token" ]; then
  log "Cloudflare Tunnel auth mode: token (headless connector token). Override with MAGICIAN_TUNNEL_MODE=browser."
else
  log "Cloudflare Tunnel auth mode: browser (login + cert.pem). Set MAGICIAN_TUNNEL_MODE=token (with CLOUDFLARED_TOKEN) for the headless connector."
fi

# Webhook-only tunnel: MAGICIAN_TUNNEL_UI=0 (env or runtime .env) drops the optional
# dev-UI host (ui.<zone>) entirely. Done here, after read_env_key is available, so
# UI_HOST="" propagates to ingress config + DNS + the dashboard guidance below.
if [ "$(read_env_key MAGICIAN_TUNNEL_UI)" = "0" ]; then
  UI_HOST=""
  log "Dev-UI host disabled (MAGICIAN_TUNNEL_UI=0) — webhook-only tunnel."
fi

# start_token_connector — run the DASHBOARD-managed tunnel with its connector
# token. A token tunnel's ingress (public hostnames) + DNS live in the Cloudflare
# Zero Trust dashboard, NOT in config.yml, so token mode does no cert.pem /
# config.yml / route-dns work. Idempotent; backgrounded (no sudo). The token is
# passed on argv (visible to `ps`, inherent to --token) but never logged.
start_token_connector() {
  local cloudflared_bin
  cloudflared_bin="$(command -v cloudflared)"
  if [ "$(uname -s)" = Darwin ] \
    && launchctl print "gui/$(id -u)/${TOKEN_LAUNCH_LABEL}" >/dev/null 2>&1; then
    log "cloudflared token connector launchd job already running — leaving it."
    return 0
  fi
  if pgrep -f "cloudflared tunnel run --token" >/dev/null 2>&1; then
    log "cloudflared token connector already running — leaving it."
    return 0
  fi
  if [ "$DRYRUN" = 1 ]; then
    log "would run: cloudflared tunnel run --token-file ${TOKEN_FILE}  (backgrounded)"
    return 0
  fi
  mkdir -p "$CF_DIR"
  local logf="$CF_DIR/magician-token-tunnel.log"
  umask 077
  printf '%s\n' "$CLOUDFLARED_TOKEN" > "$TOKEN_FILE"
  chmod 600 "$TOKEN_FILE" 2>/dev/null || true
  if [ "$(uname -s)" = Darwin ] && command -v launchctl >/dev/null 2>&1; then
    launchctl remove "$TOKEN_LAUNCH_LABEL" >/dev/null 2>&1 || true
    launchctl submit -l "$TOKEN_LAUNCH_LABEL" -o "$logf" -e "$logf" -- \
      "$cloudflared_bin" tunnel run --token-file "$TOKEN_FILE"
    log "Started cloudflared token connector as launchd job ${TOKEN_LAUNCH_LABEL} (logs: ${logf})."
  else
    nohup "$cloudflared_bin" tunnel run --token-file "$TOKEN_FILE" >"$logf" 2>&1 &
    disown 2>/dev/null || true
    log "Started cloudflared token connector in the background (logs: ${logf})."
  fi
  log "For reboot persistence, install it once as a launchd service (needs sudo): sudo cloudflared service install <your CLOUDFLARED_TOKEN>"
}

# write_funnel_url — record the stable webhook URL where install-verify.sh reads
# it ($DATA_DIR/funnel-url). ALWAYS run (even dry-run) since it's config-known,
# not derived from a live tunnel — closes the verify gap the Tailscale path left.
write_funnel_url() {
  mkdir -p "$DATA_DIR" 2>/dev/null || true
  printf '%s\n' "$WEBHOOK_URL" > "$DATA_DIR/funnel-url" 2>/dev/null \
    && log "Wrote webhook URL to $DATA_DIR/funnel-url" \
    || warn "could not write $DATA_DIR/funnel-url"
}

print_urls() {
  log "Kapso webhook URL (set this ONCE in Kapso): ${WEBHOOK_URL}"
  # `if` (not `[ … ] && log`): when UI_HOST is empty (MAGICIAN_TUNNEL_UI=0,
  # webhook-only) the bare `&&` test returns 1 as the function's LAST command, so
  # `print_urls` returns 1 and `set -e` aborts the whole script — a FALSE failure
  # reported to the caller even though the tunnel is fully up.
  if [ -n "$UI_HOST" ]; then
    log "Dev UI URL (gate with Cloudflare Access): https://${UI_HOST}/"
  fi
  if [ "$NOTES_ENABLED" = 1 ]; then
    log "Protected Notes URL: https://${NOTES_HOST}/"
  fi
  log "Protected device connection URL: https://${CONNECT_HOST}/ (${CONNECT_BACKEND} backend)"
}

# print_setup_steps — the one-time, interactive operator steps. Printed (not run)
# whenever login / the named tunnel is missing.
print_setup_steps() {
  cat >&2 <<EOF

  One-time Cloudflare Tunnel setup (interactive — run these yourself):
    1) Ensure ${ZONE} is an active zone in your Cloudflare account
       (its nameservers point at Cloudflare).
    2) cloudflared tunnel login            # browser OAuth; pick the ${ZONE} zone
    3) cloudflared tunnel create ${TUNNEL_NAME}
    4) cloudflared tunnel route dns ${TUNNEL_NAME} ${WEBHOOK_HOST}
$( [ -n "$UI_HOST" ] && echo "    5) cloudflared tunnel route dns ${TUNNEL_NAME} ${UI_HOST}" )
    6) cloudflared tunnel route dns ${TUNNEL_NAME} ${CONNECT_HOST}
$( [ "$NOTES_ENABLED" = 1 ] && echo "    7) cloudflared tunnel route dns ${TUNNEL_NAME} ${NOTES_HOST}" )
  Then re-run this script (or the installer with MAGICIAN_ENABLE_FUNNEL=1) and it
  will write the config, ensure the routes, and start the service.
EOF
}

# configure_token_ingress_dns_via_api — when CLOUDFLARE_API_TOKEN is set, configure
# the token (dashboard-managed) tunnel's PUBLIC HOSTNAME (ingress) + the proxied DNS
# CNAME via the Cloudflare API, so it routes WITHOUT any dashboard clicks. Account +
# tunnel id are decoded from CLOUDFLARED_TOKEN. Idempotent (PUT replaces the ingress;
# DNS create is skipped if the record exists). The API token is NEVER printed.
# Returns 1 ONLY when no API token is present (caller then prints manual guidance);
# permission/zone failures warn with specifics and still return 0.
configure_token_ingress_dns_via_api() {
  local cf_api acct tun zone_id ingress_json connect_json notes_json ok exist host name API dns_resp
  local include_notes current_config current_success preserved_hosts_json preserved_rules_json
  cf_api="$(read_env_key CLOUDFLARE_API_TOKEN)"; [ -n "$cf_api" ] || cf_api="$(read_env_key CF_API_TOKEN)"
  [ -n "$cf_api" ] || return 1
  command -v jq >/dev/null 2>&1 || { warn "CLOUDFLARE_API_TOKEN set but 'jq' not found — skipping API auto-config (brew install jq, or configure in the dashboard)."; return 0; }
  if [ "$DRYRUN" = 1 ]; then log "would configure via Cloudflare API: public hostname(s) + DNS for ${WEBHOOK_HOST}$( [ -n "$UI_HOST" ] && echo ", ${UI_HOST}" )"; return 0; fi
  acct="$(printf '%s' "$CLOUDFLARED_TOKEN" | base64 -d 2>/dev/null | jq -r '.a // empty' 2>/dev/null)"
  tun="$(printf '%s' "$CLOUDFLARED_TOKEN" | base64 -d 2>/dev/null | jq -r '.t // empty' 2>/dev/null)"
  [ -n "$acct" ] && [ -n "$tun" ] || { warn "could not decode account/tunnel id from CLOUDFLARED_TOKEN — skipping API auto-config."; return 0; }
  API="https://api.cloudflare.com/client/v4"
  log "Configuring tunnel via Cloudflare API (account=${acct:0:6}…, tunnel=${tun})…"
  include_notes="$NOTES_ENABLED"
  if [ -n "$PRESERVED_REMOTE_HOSTS" ] \
    || { [ "$include_notes" != 1 ] && [ "$NOTES_PRESERVE_EXISTING" = 1 ]; }; then
    current_config="$(curl -s -H "Authorization: Bearer $cf_api" \
      "$API/accounts/$acct/cfd_tunnel/$tun/configurations")"
    current_success="$(printf '%s' "$current_config" | jq -r \
      'if .success == true and (.result.config.ingress | type) == "array" then "true" else "false" end' \
      2>/dev/null)"
    if [ "$current_success" != true ]; then
      warn "Could not inspect the existing tunnel ingress while protected routes must be preserved; leaving the complete remote ingress unchanged rather than risking route deletion."
      return 0
    fi
  fi
  if [ "$include_notes" != 1 ] && [ "$NOTES_PRESERVE_EXISTING" = 1 ]; then
    if printf '%s' "$current_config" | jq -e \
      --arg host "$NOTES_HOST" \
      --arg service "http://127.0.0.1:${NOTES_PORT}" \
      'any(.result.config.ingress[]?; .hostname == $host and .service == $service)' \
      >/dev/null 2>&1; then
      include_notes=1
      log "Preserving existing protected Notes ingress while its Access verification is stale; no new route was authorized."
    else
      warn "Notes verification is stale and no exact existing loopback ingress was found; leaving Notes unpublished."
    fi
  fi
  # connect.<zone> is path-split: host automation -> the desktop gateway;
  # health and API -> the explicitly selected host, local-container, or remote backend.
  connect_json="{\"hostname\":\"${CONNECT_HOST}\",\"path\":\"^/host(/.*)?\$\",\"service\":\"http://localhost:${CONNECT_GATEWAY_PORT}\"},{\"hostname\":\"${CONNECT_HOST}\",\"path\":\"^/health/?\$\",\"service\":\"${CONNECT_SERVICE}\"},{\"hostname\":\"${CONNECT_HOST}\",\"path\":\"^/api(/.*)?\$\",\"service\":\"${CONNECT_SERVICE}\"}"
  notes_json=""
  if [ "$include_notes" = 1 ]; then
    notes_json="{\"hostname\":\"${NOTES_HOST}\",\"service\":\"http://127.0.0.1:${NOTES_PORT}\"},"
  fi
  if [ -n "$UI_HOST" ]; then
    ingress_json="[${notes_json}{\"hostname\":\"${WEBHOOK_HOST}\",\"service\":\"http://localhost:${KAPSO_WEBHOOK_PORT}\"},{\"hostname\":\"${UI_HOST}\",\"service\":\"http://localhost:${UI_PORT}\"},${connect_json},{\"service\":\"http_status:404\"}]"
  else
    ingress_json="[${notes_json}{\"hostname\":\"${WEBHOOK_HOST}\",\"service\":\"http://localhost:${KAPSO_WEBHOOK_PORT}\"},${connect_json},{\"service\":\"http_status:404\"}]"
  fi
  if [ -n "$PRESERVED_REMOTE_HOSTS" ]; then
    preserved_hosts_json="$(printf '%s' "$PRESERVED_REMOTE_HOSTS" | jq -Rc '
      split(",") | map(gsub("^\\s+|\\s+$"; "")) | map(select(length > 0)) | unique
    ')"
    preserved_rules_json="$(printf '%s' "$current_config" | jq -c \
      --argjson hosts "$preserved_hosts_json" \
      --arg webhook "$WEBHOOK_HOST" \
      --arg ui "$UI_HOST" \
      --arg connect "$CONNECT_HOST" \
      --arg notes "$NOTES_HOST" \
      --arg notes_mode "$NOTES_TUNNEL_MODE" \
      --arg notes_service "http://127.0.0.1:${NOTES_PORT}" '
        [.result.config.ingress[]?
          | select(.hostname? as $host
            | ($hosts | index($host)) != null
              and $host != $webhook
              and $host != $ui
              and $host != $connect
              and $host != $notes)
          | select($notes_mode != "0" or .service != $notes_service)]
      ')"
    ingress_json="$(jq -cn \
      --argjson preserved "$preserved_rules_json" \
      --argjson desired "$ingress_json" '
        $preserved
        + [$desired[] | select(.service != "http_status:404")]
        + [{service:"http_status:404"}]
      ')"
    log "  preserving declared migration ingress for: $(printf '%s' "$preserved_hosts_json" | jq -r 'join(", ")')"
  fi
  ok="$(curl -s -H "Authorization: Bearer $cf_api" -H "Content-Type: application/json" -X PUT \
        "$API/accounts/$acct/cfd_tunnel/$tun/configurations" \
        --data "{\"config\":{\"ingress\":$ingress_json}}" | jq -r '.success' 2>/dev/null)"
  if [ "$ok" = "true" ]; then
    log "  ✓ public hostname(s) set: ${WEBHOOK_HOST} -> :${KAPSO_WEBHOOK_PORT}$( [ -n "$UI_HOST" ] && echo ", ${UI_HOST} -> :${UI_PORT}" )$( [ "$include_notes" = 1 ] && echo ", ${NOTES_HOST} -> 127.0.0.1:${NOTES_PORT}" )"
  else
    warn "  ✗ could not set tunnel ingress — the token needs Account · Cloudflare Tunnel · Edit. Configure in the dashboard, or fix the token."
    return 0
  fi
  zone_id="$(curl -s -H "Authorization: Bearer $cf_api" "$API/zones?name=${ZONE}" | jq -r '.result[0].id // empty' 2>/dev/null)"
  if [ -z "$zone_id" ]; then
    warn "  ✗ zone ${ZONE} not visible to the token (needs Zone · DNS · Edit on ${ZONE}) — add the DNS CNAME(s) in the dashboard."
    return 0
  fi
  for host in "$WEBHOOK_HOST" $( [ -n "$UI_HOST" ] && printf '%s' "$UI_HOST" ) "$CONNECT_HOST" $( [ "$include_notes" = 1 ] && printf '%s' "$NOTES_HOST" ); do
    name="${host%.${ZONE}}"
    # Match ANY existing record for the name (not just CNAME) so we never try to
    # POST over a pre-existing A/AAAA/CNAME and report a misleading error.
    exist="$(curl -s -H "Authorization: Bearer $cf_api" "$API/zones/$zone_id/dns_records?name=${host}" | jq -r '.result[0].id // empty' 2>/dev/null)"
    if [ -n "$exist" ]; then log "  ✓ DNS ${host} already present — leaving it"; continue; fi
    dns_resp="$(curl -s -H "Authorization: Bearer $cf_api" -H "Content-Type: application/json" -X POST \
            "$API/zones/$zone_id/dns_records" \
            --data "{\"type\":\"CNAME\",\"name\":\"${name}\",\"content\":\"${tun}.cfargotunnel.com\",\"proxied\":true,\"comment\":\"magician tunnel\"}")"
    if [ "$(printf '%s' "$dns_resp" | jq -r '.success' 2>/dev/null)" = "true" ]; then
      log "  ✓ DNS CNAME created: ${host} -> ${tun}.cfargotunnel.com (proxied)"
    else
      warn "  ✗ could not create DNS CNAME for ${host}: $(printf '%s' "$dns_resp" | jq -rc '.errors' 2>/dev/null)"
    fi
  done
  return 0
}

# print_token_setup_guidance — token mode, no CLOUDFLARE_API_TOKEN: tell the
# operator BOTH the API-token route (hands-off next time) and the dashboard route.
print_token_setup_guidance() {
  cat >&2 <<EOF

  The connector is running, but its PUBLIC HOSTNAME + DNS still need configuring
  (a token / dashboard-managed tunnel stores these in Cloudflare, not in config.yml).
  Two ways:

  A) Let this script do it — create a Cloudflare API token (one-time), then re-run:
     1) https://dash.cloudflare.com  →  My Profile  →  API Tokens  →  Create Token  →  Custom token
     2) Permissions:  Account · Cloudflare Tunnel · Edit   AND   Zone · DNS · Edit (zone ${ZONE})
        (optional: Account · Access: Apps and Policies · Edit — only if you also want
         the script to detect/clear an Access gate on the webhook)
        NOTE: ignore the "/user/tokens/verify Invalid token" notice — a token scoped to
        just Tunnel+DNS legitimately can't call that user endpoint; it still works here.
     3) Save the token in ${DATA_DIR}/.env.development under this exact key:
          CLOUDFLARE_API_TOKEN=<token>
     4) Re-run:  MAGICIAN_TUNNEL_MODE=token bash scripts/ensure-magician-tunnel.sh
        (it sets the public hostname + DNS for you).

  B) Or do it in the dashboard (no token):
     Zero Trust → Networks → Tunnels → your tunnel → Public Hostname → Add:
       ${WEBHOOK_HOST}   ->   HTTP   localhost:${KAPSO_WEBHOOK_PORT}
$( [ -n "$UI_HOST" ] && echo "       ${UI_HOST}   ->   HTTP   localhost:${UI_PORT}   (then gate it behind Cloudflare Access)" )
       ${CONNECT_HOST}  (Path ^/host)  ->  HTTP  localhost:${CONNECT_GATEWAY_PORT}
       ${CONNECT_HOST}  (^/health and ^/api) ->  ${CONNECT_SERVICE} (${CONNECT_BACKEND})
$( [ "$NOTES_ENABLED" = 1 ] && echo "       ${NOTES_HOST}  ->  HTTP  127.0.0.1:${NOTES_PORT}  (Access preflight verified)" )
     Saving auto-creates the DNS record.

  ⚠ IMPORTANT: do NOT put ${WEBHOOK_HOST} behind Cloudflare Access. Webhooks can't
  complete an interactive login, so Access returns a 302 and Kapso never reaches the
  endpoint. Access belongs ONLY on the dev UI (${UI_HOST:-ui.${ZONE}}).
EOF
}

# verify_webhook_public — best-effort post-config check: is the webhook reachable
# AND not behind an Access gate (the #1 gotcha)? A new CNAME may take ~1 min to
# resolve, so HTTP 000 here is informational, not fatal.
verify_webhook_public() {
  [ "$DRYRUN" = 1 ] && return 0
  command -v curl >/dev/null 2>&1 || return 0
  local code loc
  code="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 12 "$WEBHOOK_URL" 2>/dev/null)"
  case "$code" in
    200|204|404|405) log "  ✓ webhook reachable through the tunnel (HTTP ${code}) — set ${WEBHOOK_URL} in Kapso." ;;
    301|302|303|307|308)
      loc="$(curl -sSI --max-time 12 "$WEBHOOK_URL" 2>/dev/null | awk 'tolower($1)=="location:"{print $2}' | tr -d "\r")"
      if printf '%s' "$loc" | grep -qi 'cloudflareaccess.com'; then
        warn "  ✗ ${WEBHOOK_HOST} is behind Cloudflare ACCESS (302 → login) — Kapso can't pass it. Remove the Access app on this hostname: Zero Trust → Access → Applications."
      else
        log "  webhook returned HTTP ${code} (redirect to ${loc:-?})."
      fi ;;
    530) warn "  ✗ HTTP 530 — the connector isn't connected to Cloudflare for this hostname yet (give it a few seconds, or check the connector log)." ;;
    000) log "  webhook not resolvable yet (HTTP 000) — DNS for a new record can take ~1 min; re-test: curl -I ${WEBHOOK_URL}" ;;
    *) log "  webhook returned HTTP ${code}." ;;
  esac
}

# --- 1. cloudflared present? (both modes) ----------------------------------
if ! command -v cloudflared >/dev/null 2>&1; then
  warn "cloudflared not found — skipping tunnel (Kapso webhooks won't be reachable). Install via 'brew install cloudflared'."
  write_funnel_url   # still record the intended URL for verify/Kapso
  exit 0
fi

# --- token mode: run the dashboard-managed connector, then stop. -----------
# A token tunnel's ingress + DNS live in the Cloudflare Zero Trust dashboard, so
# there is no cert.pem / config.yml / route-dns work here — just run the connector.
if [ "$EFFECTIVE_MODE" = "token" ]; then
  # A leftover browser-mode brew service would run a SECOND connector — flag it.
  if command -v brew >/dev/null 2>&1 && brew services list 2>/dev/null | grep -qE '^cloudflared[[:space:]]+started'; then
    warn "(token mode) the browser-mode 'cloudflared' brew service is also running — stop it to avoid two connectors: brew services stop cloudflared"
  fi
  start_token_connector
  write_funnel_url
  print_urls
  # If a Cloudflare API token is present, set the public hostname + DNS for them;
  # otherwise print how to create that token (correct perms + key) AND the
  # dashboard alternative. Then self-verify (catches an Access gate on the webhook).
  # Disable errexit for the tail: these are network ops that handle their own
  # errors via success/warn checks — a transient curl/jq failure must not abort
  # before the guidance/verify run (the connector is already up by here).
  set +e
  if ! configure_token_ingress_dns_via_api; then
    print_token_setup_guidance
  fi
  verify_webhook_public
  exit 0
fi

# --- 2. logged in? (cert.pem) + named tunnel exists? (browser mode) --------
if [ ! -f "$CF_DIR/cert.pem" ]; then
  warn "cloudflared is not logged in (no $CF_DIR/cert.pem)."
  print_setup_steps
  write_funnel_url
  exit 0
fi

TUNNEL_ID=""
if [ "$DRYRUN" != 1 ]; then
  TUNNEL_ID="$(cloudflared tunnel list --output json 2>/dev/null \
    | jq -r --arg n "$TUNNEL_NAME" '.[] | select(.name==$n) | .id' 2>/dev/null | head -1)"
fi
if [ "$DRYRUN" != 1 ] && [ -z "$TUNNEL_ID" ]; then
  warn "named tunnel '$TUNNEL_NAME' does not exist yet."
  print_setup_steps
  write_funnel_url
  exit 0
fi

# --- 3. ensure the config (managed by the installer) -----------------------
# We OWN $CF_CONFIG for the '$TUNNEL_NAME' tunnel. Write it (idempotently) with
# the ingress rules; the catch-all 404 MUST be the last rule or `cloudflared`
# validation/start fails.
CREDS_FILE="$CF_DIR/${TUNNEL_ID}.json"
write_config() {
  mkdir -p "$CF_DIR"
  {
    echo "tunnel: ${TUNNEL_NAME}"
    echo "credentials-file: ${CREDS_FILE}"
    echo "ingress:"
    echo "  - hostname: ${WEBHOOK_HOST}"
    echo "    service: http://localhost:${KAPSO_WEBHOOK_PORT}"
    if [ -n "$UI_HOST" ]; then
      echo "  - hostname: ${UI_HOST}"
      echo "    service: http://localhost:${UI_PORT}"
    fi
    if [ "$NOTES_ENABLED" = 1 ]; then
      echo "  - hostname: ${NOTES_HOST}"
      echo "    service: http://127.0.0.1:${NOTES_PORT}"
    fi
    if [ -n "$CONNECT_HOST" ]; then
      # Path-split: host automation -> the desktop gateway; aggregated health
      # and magician API (REST + WebSockets) -> magician; anything else -> 404.
      echo "  - hostname: ${CONNECT_HOST}"
      echo '    path: ^/host(/.*)?$'
      echo "    service: http://localhost:${CONNECT_GATEWAY_PORT}"
      echo "  - hostname: ${CONNECT_HOST}"
      echo '    path: ^/health/?$'
      echo "    service: ${CONNECT_SERVICE}"
      echo "  - hostname: ${CONNECT_HOST}"
      echo '    path: ^/api(/.*)?$'
      echo "    service: ${CONNECT_SERVICE}"
    fi
    echo "  - service: http_status:404"
  } > "$CF_CONFIG"
}

existing_browser_notes_route_is_safe() {
  [ -f "$CF_CONFIG" ] || return 1
  awk -v host="$NOTES_HOST" -v service="http://127.0.0.1:${NOTES_PORT}" '
    previous == "  - hostname: " host && $0 == "    service: " service { found = 1 }
    { previous = $0 }
    END { exit(found ? 0 : 1) }
  ' "$CF_CONFIG"
}

if [ "$NOTES_ENABLED" != 1 ] \
  && [ "$NOTES_PRESERVE_EXISTING" = 1 ] \
  && existing_browser_notes_route_is_safe; then
  NOTES_ENABLED=1
  log "Preserving existing protected Notes ingress while its Access verification is stale; no new route was authorized."
fi
if [ "$DRYRUN" = 1 ]; then
  log "would write $CF_CONFIG (ingress: ${WEBHOOK_HOST}->:${KAPSO_WEBHOOK_PORT}$( [ -n "$UI_HOST" ] && echo ", ${UI_HOST}->:${UI_PORT}" )$( [ "$NOTES_ENABLED" = 1 ] && echo ", protected ${NOTES_HOST}->127.0.0.1:${NOTES_PORT}" ), then catch-all 404)"
else
  log "Writing cloudflared config $CF_CONFIG (tunnel ${TUNNEL_NAME})"
  write_config
fi

# --- 4. ensure DNS routes (idempotent) -------------------------------------
ensure_route() { # ensure_route HOST
  would_run cloudflared tunnel route dns "$TUNNEL_NAME" "$1" 2>/dev/null \
    || log "DNS route for $1 already present (or needs --overwrite-dns) — leaving as is."
}
log "Ensuring DNS routes"
ensure_route "$WEBHOOK_HOST"
[ -n "$UI_HOST" ] && ensure_route "$UI_HOST"
ensure_route "$CONNECT_HOST"
[ "$NOTES_ENABLED" = 1 ] && ensure_route "$NOTES_HOST"

# --- 5. ensure the tunnel is running (launchd via brew services, no sudo) ---
if command -v brew >/dev/null 2>&1; then
  if [ "$DRYRUN" != 1 ] && brew services list 2>/dev/null | grep -qE '^cloudflared\s+started'; then
    log "cloudflared service already running — restarting to pick up config changes"
    would_run brew services restart cloudflared >/dev/null 2>&1 || true
  else
    log "Starting cloudflared service (brew services start cloudflared)"
    would_run brew services start cloudflared >/dev/null 2>&1 \
      || warn "brew services start cloudflared failed — run it manually, or 'cloudflared tunnel run ${TUNNEL_NAME}' in the foreground to test."
  fi
else
  warn "brew not found — start the tunnel yourself: 'cloudflared tunnel run ${TUNNEL_NAME}' (foreground) or 'sudo cloudflared service install'."
fi

# --- 6. record + print the stable URL --------------------------------------
write_funnel_url
print_urls
log "Tunnel ensured. If you just created DNS, give it ~1 min to propagate, then verify: curl -fsS ${WEBHOOK_URL%/webhook}/health 2>/dev/null || curl -fsS ${WEBHOOK_URL}"
