#!/usr/bin/env bash
# ensure-connect-access.sh — idempotently gate the customer-owned device connection hostname
# behind Cloudflare Zero Trust Access. iOS, Android, and ESP32 learn this hostname
# at runtime from Magician's one-time enrollment; no client compiles it.
# Ordinary requests need the shared outer Access service credential *and* a
# revocable per-device Magician token. Only the exact one-time enrollment
# exchange paths are bypassed at the edge, where a 256-bit, five-minute,
# single-use capability remains mandatory.
#
# It is idempotent: existing Access app / service token / policy are reused, not
# duplicated. The service-token SECRET is only returned by Cloudflare at creation
# time, so it is captured and written ONCE to the runtime env file
# (default ~/MagicianNotes/.env.development, gitignored) as CF_ACCESS_CLIENT_ID /
# CF_ACCESS_CLIENT_SECRET. The enrollment exchange delivers them to a confirmed
# phone and the app stores them in Keychain/Keystore/NVS. Rotation is
# make-before-break: `MAGICIAN_CONNECT_ACCESS_ROTATION_ACTION=stage` asks
# Cloudflare to keep the previous secret valid for a bounded grace period,
# verifies both secrets, and writes the successor locally. Run with `finalize`
# only after the device has accepted the successor. The legacy
# MAGICIAN_IOS_ACCESS_* spellings remain compatibility aliases.
#
# Requires (resolved from the process env, else the runtime .env files):
#   CLOUDFLARE_API_TOKEN  (or CF_API_TOKEN) with, at minimum:
#       Account · Access: Apps and Policies · Edit
#       Account · Access: Service Tokens · Edit
#       Account · Access: Organizations, Identity Providers, and Groups · Read
#   CLOUDFLARED_TOKEN     (the connector token — the account id is decoded from it;
#                          override with MAGICIAN_CF_ACCOUNT_ID to skip that)
#
# Honours MAGICIAN_INSTALL_DRYRUN=1 (prints "would …" instead of mutating). The
# API token and the service-token secret are NEVER printed.
set -euo pipefail

# --- inputs (env-overridable) ----------------------------------------------
DATA_DIR="${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}"
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
CONNECT_HOST="${MAGICIAN_CONNECT_HOST:-${MAGICIAN_IOS_HOST:-}}"
if [ -z "$CONNECT_HOST" ]; then
  for _zf in "$DATA_DIR/.env.development" "$DATA_DIR/.env"; do
    [ -f "$_zf" ] || continue
    _cl="$(grep -E '^MAGICIAN_CONNECT_HOST=' "$_zf" | tail -1)"
    [ -n "$_cl" ] || _cl="$(grep -E '^MAGICIAN_IOS_HOST=' "$_zf" | tail -1)"
    if [ -n "$_cl" ]; then CONNECT_HOST="${_cl#*=}"; CONNECT_HOST="${CONNECT_HOST%\"}"; CONNECT_HOST="${CONNECT_HOST#\"}"; break; fi
  done
fi
[ -n "$CONNECT_HOST" ] || CONNECT_HOST="connect.${ZONE}"
MOBILE_PUBLIC_ORIGIN="https://${CONNECT_HOST}"
ENROLLMENT_PATH="/api/magician/v2/devices/enrollment/exchange"
ANDROID_ENROLLMENT_PATH="/api/magician/v2/devices/apps-automation/enrollment/exchange"
ENROLLMENT_DOMAIN="${CONNECT_HOST}${ENROLLMENT_PATH}"
ENROLLMENT_METADATA_URL="https://${CONNECT_HOST}/.well-known/cloudflare-access-protected-resource${ENROLLMENT_PATH}"
ENROLLMENT_APP_NAME="${MAGICIAN_MOBILE_ENROLLMENT_ACCESS_APP_NAME:-Magican mobile one-time enrollment}"
ENROLLMENT_POLICY_NAME="${MAGICIAN_MOBILE_ENROLLMENT_ACCESS_POLICY_NAME:-Magican one-time enrollment bypass}"
APP_NAME="${MAGICIAN_CONNECT_ACCESS_APP_NAME:-${MAGICIAN_IOS_ACCESS_APP_NAME:-Magican device connection}}"
# Preserve the deployed token identity by default. Changing the name would mint
# a second secret and invalidate devices until every profile was replaced.
TOKEN_NAME="${MAGICIAN_CONNECT_ACCESS_TOKEN_NAME:-${MAGICIAN_IOS_ACCESS_TOKEN_NAME:-magios-ios}}"
POLICY_NAME="${MAGICIAN_CONNECT_ACCESS_POLICY_NAME:-${MAGICIAN_IOS_ACCESS_POLICY_NAME:-magican device service token}}"
SERVICE_POLICY_PRECEDENCE="${MAGICIAN_CONNECT_ACCESS_SERVICE_POLICY_PRECEDENCE:-${MAGICIAN_IOS_ACCESS_SERVICE_POLICY_PRECEDENCE:-1}}"
# Optional ADDITIVE browser access — lets Safari authenticate to connect.<zone> via a
# one-time email PIN so "Open in Browser" (md/html/pdf artifacts) works, ALONGSIDE
# the service token the app uses. Set at least one of these to enable; leave both
# unset to keep the host service-token-only. Falls back to the dev-UI allowlist.
EMAIL_POLICY_NAME="${MAGICIAN_CONNECT_ACCESS_EMAIL_POLICY_NAME:-${MAGICIAN_IOS_ACCESS_EMAIL_POLICY_NAME:-magican device browser emails}}"
EMAIL_POLICY_PRECEDENCE="${MAGICIAN_CONNECT_ACCESS_EMAIL_POLICY_PRECEDENCE:-${MAGICIAN_IOS_ACCESS_EMAIL_POLICY_PRECEDENCE:-2}}"
CONNECT_EMAILS="${MAGICIAN_CONNECT_ACCESS_EMAILS:-${MAGICIAN_IOS_ACCESS_EMAILS:-}}"
CONNECT_EMAIL_DOMAIN="${MAGICIAN_CONNECT_ACCESS_EMAIL_DOMAIN:-${MAGICIAN_IOS_ACCESS_EMAIL_DOMAIN:-}}"
SESSION="${MAGICIAN_CONNECT_ACCESS_SESSION:-${MAGICIAN_IOS_ACCESS_SESSION:-24h}}"
ENV_FILE="${MAGICIAN_CONNECT_ACCESS_ENV_FILE:-${MAGICIAN_IOS_ACCESS_ENV_FILE:-$DATA_DIR/.env.development}}"
ROTATE="${MAGICIAN_CONNECT_ACCESS_ROTATE_TOKEN:-${MAGICIAN_IOS_ACCESS_ROTATE_TOKEN:-0}}"
ROTATION_ACTION="${MAGICIAN_CONNECT_ACCESS_ROTATION_ACTION:-${MAGICIAN_IOS_ACCESS_ROTATION_ACTION:-}}"
ROTATION_GRACE_SECONDS="${MAGICIAN_CONNECT_ACCESS_ROTATION_GRACE_SECONDS:-${MAGICIAN_IOS_ACCESS_ROTATION_GRACE_SECONDS:-259200}}"
REQUIRE_EMAIL_ALLOWLIST="${MAGICIAN_CONNECT_ACCESS_REQUIRE_EMAIL_ALLOWLIST:-${MAGICIAN_IOS_ACCESS_REQUIRE_EMAIL_ALLOWLIST:-0}}"
# The Notes provisioner reuses the root Access app/policy machinery but must not
# create a mobile-enrollment bypass on the Notes hostname or overwrite the
# backend's mobile origin/audience. These switches default on for the real
# mobile target and are explicitly disabled by ensure-notes-access.sh.
MOBILE_ENROLLMENT_ENABLED="${MAGICIAN_CONNECT_ACCESS_MOBILE_ENROLLMENT_ENABLED:-${MAGICIAN_IOS_ACCESS_MOBILE_ENROLLMENT_ENABLED:-1}}"
PERSIST_MOBILE_RUNTIME="${MAGICIAN_CONNECT_ACCESS_PERSIST_MOBILE_RUNTIME:-${MAGICIAN_IOS_ACCESS_PERSIST_MOBILE_RUNTIME:-1}}"
ORIGIN_ACCESS_MODE="${MAGICIAN_CONNECT_ORIGIN_ACCESS_MODE:-}"
READY_FILE="${MAGICIAN_ACCESS_READY_FILE:-}"
DRYRUN="${MAGICIAN_INSTALL_DRYRUN:-0}"
API="https://api.cloudflare.com/client/v4"

ROTATION_HOST_KEY="$(printf '%s' "$CONNECT_HOST" | tr -c 'A-Za-z0-9._-' '_')"
ROTATION_STATE_FILE="${MAGICIAN_CONNECT_ACCESS_ROTATION_STATE_FILE:-${MAGICIAN_IOS_ACCESS_ROTATION_STATE_FILE:-$DATA_DIR/.magician/access-token-rotation-${ROTATION_HOST_KEY}.state}}"
ROTATION_VERIFY_URL="${MAGICIAN_CONNECT_ACCESS_ROTATION_VERIFY_URL:-${MAGICIAN_IOS_ACCESS_ROTATION_VERIFY_URL:-https://${CONNECT_HOST}/}}"

log()  { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }
warn() { printf '\033[1;33mWARN: %s\033[0m\n' "$*" >&2; }
die()  { printf '\033[1;31mERROR: %s\033[0m\n' "$*" >&2; exit 1; }

if [ -z "$ROTATION_ACTION" ] && [ "$ROTATE" = 1 ]; then
  ROTATION_ACTION="stage"
fi
case "$ROTATION_ACTION" in
  ""|stage|status|extend|finalize) ;;
  *) die "MAGICIAN_CONNECT_ACCESS_ROTATION_ACTION must be stage, status, extend, or finalize." ;;
esac
case "$ROTATION_GRACE_SECONDS" in
  ''|*[!0-9]*) die "MAGICIAN_CONNECT_ACCESS_ROTATION_GRACE_SECONDS must be a positive integer." ;;
esac
if [ "$ROTATION_GRACE_SECONDS" -lt 300 ]; then
  die "MAGICIAN_CONNECT_ACCESS_ROTATION_GRACE_SECONDS must be at least 300 seconds."
fi

command -v curl >/dev/null 2>&1 || die "curl not found."
command -v jq   >/dev/null 2>&1 || die "jq not found (brew install jq)."
case "$SERVICE_POLICY_PRECEDENCE:$EMAIL_POLICY_PRECEDENCE" in
  *[!0-9:]*) die "Access policy precedence values must be non-negative integers." ;;
esac
case "$MOBILE_ENROLLMENT_ENABLED:$PERSIST_MOBILE_RUNTIME" in
  0:0|0:1|1:0|1:1) ;;
  *) die "MAGICIAN_CONNECT_ACCESS_MOBILE_ENROLLMENT_ENABLED and MAGICIAN_CONNECT_ACCESS_PERSIST_MOBILE_RUNTIME must be 0 or 1." ;;
esac

# read_env_key KEY — process env first, else the runtime .env files. Never logged.
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

# `require` is the secure default for an origin reached directly from an
# untrusted network. A loopback-only host port behind the Cloudflare edge needs
# `verify`: Apple container forwarding appears non-loopback inside the guest,
# while the desktop still authenticates with its Magician bearer. Preserve an
# explicitly configured runtime mode so an Access-policy refresh cannot silently
# break that local desktop route. The public edge remains Access-protected in
# either case.
if [ -z "$ORIGIN_ACCESS_MODE" ]; then
  ORIGIN_ACCESS_MODE="$(read_env_key MAGICIAN_CF_ACCESS_MODE)"
fi
[ -n "$ORIGIN_ACCESS_MODE" ] || ORIGIN_ACCESS_MODE=require
case "$ORIGIN_ACCESS_MODE" in
  require|verify) ;;
  *) die "MAGICIAN_CONNECT_ORIGIN_ACCESS_MODE must be require or verify." ;;
esac

[ -n "$CONNECT_EMAILS" ] || CONNECT_EMAILS="$(read_env_key MAGICIAN_CONNECT_ACCESS_EMAILS)"
[ -n "$CONNECT_EMAILS" ] || CONNECT_EMAILS="$(read_env_key MAGICIAN_IOS_ACCESS_EMAILS)"
[ -n "$CONNECT_EMAILS" ] || CONNECT_EMAILS="$(read_env_key MAGICIAN_UI_ACCESS_EMAILS)"
[ -n "$CONNECT_EMAIL_DOMAIN" ] || CONNECT_EMAIL_DOMAIN="$(read_env_key MAGICIAN_CONNECT_ACCESS_EMAIL_DOMAIN)"
[ -n "$CONNECT_EMAIL_DOMAIN" ] || CONNECT_EMAIL_DOMAIN="$(read_env_key MAGICIAN_IOS_ACCESS_EMAIL_DOMAIN)"
[ -n "$CONNECT_EMAIL_DOMAIN" ] || CONNECT_EMAIL_DOMAIN="$(read_env_key MAGICIAN_UI_ACCESS_EMAIL_DOMAIN)"
if [ "$REQUIRE_EMAIL_ALLOWLIST" = 1 ] && [ -z "$CONNECT_EMAILS" ] && [ -z "$CONNECT_EMAIL_DOMAIN" ]; then
  die "an owner Allow policy is required before ${CONNECT_HOST} can be published; set MAGICIAN_NOTES_ACCESS_EMAILS or MAGICIAN_NOTES_ACCESS_EMAIL_DOMAIN."
fi

API_TOKEN="$(read_env_key CLOUDFLARE_API_TOKEN)"; [ -n "$API_TOKEN" ] || API_TOKEN="$(read_env_key CF_API_TOKEN)"
[ -n "$API_TOKEN" ] || die "CLOUDFLARE_API_TOKEN (or CF_API_TOKEN) not set (env or $DATA_DIR/.env.development)."

ACCT="${MAGICIAN_CF_ACCOUNT_ID:-}"
if [ -z "$ACCT" ]; then
  CFTOK="$(read_env_key CLOUDFLARED_TOKEN)"
  [ -n "$CFTOK" ] || die "CLOUDFLARED_TOKEN not set and MAGICIAN_CF_ACCOUNT_ID not given — cannot resolve the account id."
  ACCT="$(printf '%s' "$CFTOK" | base64 -d 2>/dev/null | jq -r '.a // empty' 2>/dev/null)"
  [ -n "$ACCT" ] || die "could not decode the account id from CLOUDFLARED_TOKEN (set MAGICIAN_CF_ACCOUNT_ID instead)."
fi

# cf METHOD PATH [JSON-body] — Cloudflare API call. Prints the raw JSON response.
cf() {
  local method="$1" path="$2" body="${3:-}"
  local -a transport_args=(-sS --connect-timeout 10 --max-time 30)
  # A dry-run create returns a synthetic id. Do not ask Cloudflare to resolve
  # dependent policy collections for that id; model them as empty so the rest
  # of the provisioning plan can be printed without a false 404 failure.
  if [ "$DRYRUN" = 1 ] && [ "$method" = GET ] \
    && [[ "$path" == */access/apps/DRYRUN/policies ]]; then
    printf '{"success":true,"result":[]}'
    return 0
  fi
  if [ "$DRYRUN" = 1 ] && [ "$method" != GET ]; then
    log "would ${method} ${path}${body:+  (body: $(printf '%s' "$body" | jq -c '.' 2>/dev/null || echo '...'))}" >&2
    printf '{"success":true,"result":{"id":"DRYRUN","client_id":"DRYRUN","aud":"DRYRUN"}}'
    return 0
  fi
  # Reads are safe to retry. Mutations are only time-bounded: a timed-out POST
  # may already have reached Cloudflare, and the next idempotent provisioning
  # pass will discover it without risking an automatic duplicate.
  if [ "$method" = GET ]; then
    transport_args+=(--retry 2 --retry-delay 1 --retry-max-time 75)
  fi
  curl "${transport_args[@]}" -X "$method" -H "Authorization: Bearer $API_TOKEN" -H "Content-Type: application/json" \
    ${body:+--data "$body"} "$API$path"
}

upsert_env_file() { # upsert_env_file FILE KEY VALUE — never logs VALUE
  local target="$1" key="$2" val="$3" tmp
  [ -n "$val" ] || return 0
  if [ "$DRYRUN" = 1 ]; then log "would write ${key} to ${target}"; return 0; fi
  mkdir -p "$(dirname "$target")"
  touch "$target"; chmod 600 "$target" 2>/dev/null || true
  tmp="$(mktemp "${target}.XXXXXX")"; grep -vE "^[[:space:]]*${key}=" "$target" > "$tmp" 2>/dev/null || true
  printf '%s=%s\n' "$key" "$val" >> "$tmp"
  chmod 600 "$tmp" 2>/dev/null || true
  mv "$tmp" "$target"
  log "  wrote ${key} to ${target}"
}

upsert_env() { # upsert_env KEY VALUE — primary runtime env
  upsert_env_file "$ENV_FILE" "$1" "$2"
}

iso_after_seconds() {
  ROTATION_SECONDS="$1" python3 - <<'PY'
from datetime import datetime, timedelta, timezone
import os

seconds = int(os.environ["ROTATION_SECONDS"])
print((datetime.now(timezone.utc) + timedelta(seconds=seconds)).isoformat(timespec="seconds").replace("+00:00", "Z"))
PY
}

validate_rotation_verify_url() {
  ROTATION_VERIFY_URL="$ROTATION_VERIFY_URL" CONNECT_HOST="$CONNECT_HOST" python3 - <<'PY'
import os
from urllib.parse import urlsplit

url = urlsplit(os.environ["ROTATION_VERIFY_URL"])
host = os.environ["CONNECT_HOST"].lower()
if (
    url.scheme != "https"
    or (url.hostname or "").lower() != host
    or url.username is not None
    or url.password is not None
    or url.port not in (None, 443)
    or url.fragment
):
    raise SystemExit(
        "MAGICIAN_CONNECT_ACCESS_ROTATION_VERIFY_URL must be HTTPS on the exact protected host"
    )
PY
}

access_pair_is_accepted() { # access_pair_is_accepted CLIENT_ID CLIENT_SECRET
  local client_id="$1" client_secret="$2" result code redirect
  result="$({
    printf 'CF-Access-Client-Id: %s\n' "$client_id"
    printf 'CF-Access-Client-Secret: %s\n' "$client_secret"
  } | curl -sS -o /dev/null -w '%{http_code} %{redirect_url}' --max-time 12 \
    -H @- "$ROTATION_VERIFY_URL" 2>/dev/null || true)"
  code="${result%% *}"
  redirect="${result#* }"
  case "$code" in
    200|204|404|405) return 0 ;;
    301|302|303|307|308)
      case "$redirect" in
        "https://${CONNECT_HOST}"|"https://${CONNECT_HOST}/"*)
          printf '%s' "$redirect" | grep -q '/cdn-cgi/access/' && return 1
          return 0
          ;;
      esac
      ;;
  esac
  return 1
}

state_value() { # state_value KEY
  sed -n "s/^$1=//p" "$ROTATION_STATE_FILE" 2>/dev/null | tail -1
}

decode_state_secret() { # decode_state_secret KEY
  state_value "$1" | python3 -c 'import base64, sys; sys.stdout.buffer.write(base64.b64decode(sys.stdin.buffer.read()))' 2>/dev/null
}

write_rotation_state() { # token_id client_id expiry phase previous_secret next_secret
  local token_id="$1" client_id="$2" expiry="$3" phase="$4" previous_secret="$5" next_secret="$6" tmp
  mkdir -p "$(dirname "$ROTATION_STATE_FILE")"
  umask 077
  tmp="$(mktemp "${ROTATION_STATE_FILE}.XXXXXX")"
  {
    printf 'token_id=%s\n' "$token_id"
    printf 'client_id=%s\n' "$client_id"
    printf 'previous_secret_expires_at=%s\n' "$expiry"
    printf 'phase=%s\n' "$phase"
    printf 'previous_client_secret_b64='; printf '%s' "$previous_secret" | base64 | tr -d '\n'; printf '\n'
    printf 'next_client_secret_b64='; printf '%s' "$next_secret" | base64 | tr -d '\n'; printf '\n'
  } > "$tmp"
  chmod 600 "$tmp"
  mv "$tmp" "$ROTATION_STATE_FILE"
}

# ok_or_explain RESPONSE CONTEXT — return 0 if .success, else warn with the errors
# (spotting the common missing-permission case) and return 1.
ok_or_explain() {
  local resp="$1" ctx="$2" ok errs
  ok="$(printf '%s' "$resp" | jq -r '.success // false' 2>/dev/null)"
  [ "$ok" = "true" ] && return 0
  errs="$(printf '%s' "$resp" | jq -rc '.errors // empty' 2>/dev/null)"
  warn "${ctx} failed: ${errs:-<unparseable response>}"
  if printf '%s' "$errs" | grep -qiE "authentication|permission|not allowed|9109|forbidden"; then
    warn "  the API token likely lacks an Access Apps/Policies Edit, Service Tokens Edit, or Organizations Read permission"
  fi
  return 1
}

# --- 1. service token ------------------------------------------------------
log "Ensuring Access service token '${TOKEN_NAME}'"
ST_LIST="$(cf GET "/accounts/$ACCT/access/service_tokens")"
ok_or_explain "$ST_LIST" "list service tokens" || exit 1
ST_ID="$(printf '%s' "$ST_LIST" | jq -r --arg n "$TOKEN_NAME" '.result[] | select(.name==$n) | .id' | head -1)"
ST_CLIENT_ID="$(printf '%s' "$ST_LIST" | jq -r --arg n "$TOKEN_NAME" '.result[] | select(.name==$n) | .client_id' | head -1)"
ST_SECRET=""
TOKEN_WAS_CREATED=0

if [ -z "$ST_ID" ]; then
  log "  creating service token"
  ST_CREATE="$(cf POST "/accounts/$ACCT/access/service_tokens" "{\"name\":\"${TOKEN_NAME}\"}")"
  ok_or_explain "$ST_CREATE" "create service token" || exit 1
  ST_ID="$(printf '%s' "$ST_CREATE" | jq -r '.result.id')"
  ST_CLIENT_ID="$(printf '%s' "$ST_CREATE" | jq -r '.result.client_id')"
  ST_SECRET="$(printf '%s' "$ST_CREATE" | jq -r '.result.client_secret // empty')"
  TOKEN_WAS_CREATED=1
  log "  ✓ created (client_id ${ST_CLIENT_ID})"
else
  log "  ✓ reusing existing token (client_id ${ST_CLIENT_ID}) — its secret is only shown at creation"
fi

# --- 2. Access application for the hostname --------------------------------
log "Ensuring Access application for ${CONNECT_HOST}"
APP_LIST="$(cf GET "/accounts/$ACCT/access/apps")"
ok_or_explain "$APP_LIST" "list access apps" || exit 1
APP_ID="$(printf '%s' "$APP_LIST" | jq -r --arg d "$CONNECT_HOST" '.result[] | select(.domain==$d and .type=="self_hosted") | .id' | head -1)"
APP_AUD="$(printf '%s' "$APP_LIST" | jq -r --arg d "$CONNECT_HOST" '.result[] | select(.domain==$d and .type=="self_hosted") | .aud // empty' | head -1)"

if [ -z "$APP_ID" ]; then
  log "  creating self-hosted app (domain ${CONNECT_HOST}, session ${SESSION})"
  APP_CREATE="$(cf POST "/accounts/$ACCT/access/apps" \
    "{\"name\":\"${APP_NAME}\",\"domain\":\"${CONNECT_HOST}\",\"type\":\"self_hosted\",\"session_duration\":\"${SESSION}\"}")"
  ok_or_explain "$APP_CREATE" "create access app" || exit 1
  APP_ID="$(printf '%s' "$APP_CREATE" | jq -r '.result.id')"
  APP_AUD="$(printf '%s' "$APP_CREATE" | jq -r '.result.aud // empty')"
  log "  ✓ created app ${APP_ID}"
else
  log "  ✓ reusing existing app ${APP_ID}"
fi

# Magician validates the assertion Cloudflare adds after edge admission. The
# application audience binds it to this exact Access app; the organization
# domain selects the account's signing keys. Without both values a shared
# service-token assertion could be mistaken for a durable Magician identity.
if [ -z "$APP_AUD" ]; then
  APP_DETAILS="$(cf GET "/accounts/$ACCT/access/apps/$APP_ID")"
  ok_or_explain "$APP_DETAILS" "read access app audience" || exit 1
  APP_AUD="$(printf '%s' "$APP_DETAILS" | jq -r '.result.aud // empty')"
fi
[ -n "$APP_AUD" ] || die "Access app ${APP_ID} did not expose an audience tag."
ACCESS_ISSUER=""
if [ "$PERSIST_MOBILE_RUNTIME" = 1 ]; then
  ACCESS_AUTH_DOMAIN="$(read_env_key MAGICIAN_CF_ACCESS_TEAM_DOMAIN)"
  if [ -z "$ACCESS_AUTH_DOMAIN" ]; then
    ACCESS_ORGANIZATION="$(cf GET "/accounts/$ACCT/access/organizations")"
    if ok_or_explain "$ACCESS_ORGANIZATION" "read Access organization"; then
      ACCESS_AUTH_DOMAIN="$(printf '%s' "$ACCESS_ORGANIZATION" | jq -r '.result.auth_domain // empty')"
    else
      # Apps/Policies Edit is sufficient to provision the narrowly-scoped
      # enrollment bypass. Do not strand that repair merely because the token
      # lacks Organizations Read: Cloudflare publishes the same non-secret team
      # domain through RFC 9728 protected-resource metadata for this host.
      warn "  falling back to public protected-resource metadata for the Access team domain"
      ACCESS_METADATA="$(curl -sS --connect-timeout 10 --max-time 30 "$ENROLLMENT_METADATA_URL" 2>/dev/null || true)"
      ACCESS_AUTH_DOMAIN="$(printf '%s' "$ACCESS_METADATA" | jq -r '.authorization_servers[0] // .team_domain // empty' 2>/dev/null)"
    fi
  fi
  [ -n "$ACCESS_AUTH_DOMAIN" ] || die "the Access organization did not expose an auth_domain."
  case "$ACCESS_AUTH_DOMAIN" in
    https://*) ACCESS_ISSUER="${ACCESS_AUTH_DOMAIN%/}" ;;
    *) ACCESS_ISSUER="https://${ACCESS_AUTH_DOMAIN%/}" ;;
  esac
  case "$ACCESS_ISSUER" in
    https://*.cloudflareaccess.com) ;;
    *) die "the resolved Access team domain is not a Cloudflare Access issuer." ;;
  esac
fi

# --- 3. policy: allow (non_identity) the service token ---------------------
log "Ensuring the service-token allow policy on the app"
POL_LIST="$(cf GET "/accounts/$ACCT/access/apps/$APP_ID/policies")"
ok_or_explain "$POL_LIST" "list app policies" || exit 1
POL_ID="$(printf '%s' "$POL_LIST" | jq -r --arg id "$ST_ID" \
  '.result[]? | select(.decision=="non_identity" and any(.include[]?; .service_token.token_id==$id)) | .id' | head -1)"
POL_BODY="$(jq -cn \
  --arg n "$POLICY_NAME" \
  --arg id "$ST_ID" \
  --arg duration "$SESSION" \
  --argjson precedence "$SERVICE_POLICY_PRECEDENCE" \
  '{name:$n, decision:"non_identity", precedence:$precedence, session_duration:$duration, include:[{service_token:{token_id:$id}}]}')"
if [ -z "$POL_ID" ]; then
  log "  creating non_identity policy including the service token"
  ok_or_explain "$(cf POST "/accounts/$ACCT/access/apps/$APP_ID/policies" \
    "$POL_BODY")" \
    "create policy" || exit 1
  log "  ✓ policy created"
else
  POL_MATCH="$(printf '%s' "$POL_LIST" | jq -r \
    --arg id "$POL_ID" \
    --arg n "$POLICY_NAME" \
    --arg duration "$SESSION" \
    --argjson precedence "$SERVICE_POLICY_PRECEDENCE" \
    '[.result[]? | select(.id==$id and .name==$n and .precedence==$precedence and .session_duration==$duration)] | length')"
  if [ "$POL_MATCH" = 1 ]; then
    log "  ✓ a policy already allows this service token with current settings"
  else
    log "  updating service-token policy precedence/session settings"
    ok_or_explain "$(cf PUT "/accounts/$ACCT/access/apps/$APP_ID/policies/$POL_ID" "$POL_BODY")" "update service-token policy" || exit 1
    log "  ✓ service-token policy updated"
  fi
fi

# --- 3b. narrowly bypass the two one-time bootstrap exchanges --------------
# A generic app has no service credential before this request. The route is
# still protected by a 256-bit, five-minute, single-use capability and returns a
# per-device Magician token; every other path remains under the hostname app.
ensure_enrollment_bypass() {
  local ENROLLMENT_DOMAIN="$1" ENROLLMENT_APP_NAME="$2"
  local APP_LIST ENROLLMENT_APP_ID ENROLLMENT_CREATE ENROLLMENT_POLICIES
  local ENROLLMENT_POLICY_ID ENROLLMENT_POLICY_BODY ENROLLMENT_POLICY_MATCH
  log "Ensuring the exact mobile enrollment bypass on ${ENROLLMENT_DOMAIN}"
  APP_LIST="$(cf GET "/accounts/$ACCT/access/apps")"
  ok_or_explain "$APP_LIST" "list access apps for mobile enrollment" || exit 1
  ENROLLMENT_APP_ID="$(printf '%s' "$APP_LIST" | jq -r --arg d "$ENROLLMENT_DOMAIN" \
    '.result[] | select(.domain==$d and .type=="self_hosted") | .id' | head -1)"
  if [ -z "$ENROLLMENT_APP_ID" ]; then
    ENROLLMENT_CREATE="$(cf POST "/accounts/$ACCT/access/apps" \
      "$(jq -cn --arg n "$ENROLLMENT_APP_NAME" --arg d "$ENROLLMENT_DOMAIN" \
        '{name:$n,domain:$d,type:"self_hosted",session_duration:"5m"}')")"
    ok_or_explain "$ENROLLMENT_CREATE" "create mobile enrollment Access app" || exit 1
    ENROLLMENT_APP_ID="$(printf '%s' "$ENROLLMENT_CREATE" | jq -r '.result.id')"
  else
    log "  ✓ reusing enrollment app ${ENROLLMENT_APP_ID}"
  fi
  ENROLLMENT_POLICIES="$(cf GET "/accounts/$ACCT/access/apps/$ENROLLMENT_APP_ID/policies")"
  ok_or_explain "$ENROLLMENT_POLICIES" "list mobile enrollment policies" || exit 1
  ENROLLMENT_POLICY_ID="$(printf '%s' "$ENROLLMENT_POLICIES" | jq -r \
    '.result[]? | select(.decision=="bypass" and any(.include[]?; has("everyone"))) | .id' | head -1)"
  ENROLLMENT_POLICY_BODY="$(jq -cn --arg n "$ENROLLMENT_POLICY_NAME" \
    '{name:$n,decision:"bypass",precedence:1,include:[{everyone:{}}]}')"
  if [ -z "$ENROLLMENT_POLICY_ID" ]; then
    ok_or_explain "$(cf POST "/accounts/$ACCT/access/apps/$ENROLLMENT_APP_ID/policies" \
      "$ENROLLMENT_POLICY_BODY")" "create mobile enrollment bypass policy" || exit 1
    log "  ✓ enrollment bypass created"
  else
    ENROLLMENT_POLICY_MATCH="$(printf '%s' "$ENROLLMENT_POLICIES" | jq -r \
      --arg id "$ENROLLMENT_POLICY_ID" --arg n "$ENROLLMENT_POLICY_NAME" \
      '[.result[]? | select(.id==$id and .name==$n and .decision=="bypass" and .precedence==1 and any(.include[]?; has("everyone")))] | length')"
    if [ "$ENROLLMENT_POLICY_MATCH" != 1 ]; then
      ok_or_explain "$(cf PUT "/accounts/$ACCT/access/apps/$ENROLLMENT_APP_ID/policies/$ENROLLMENT_POLICY_ID" \
        "$ENROLLMENT_POLICY_BODY")" "repair mobile enrollment bypass policy" || exit 1
    fi
    log "  ✓ enrollment bypass verified"
  fi
}
if [ "$MOBILE_ENROLLMENT_ENABLED" = 1 ]; then
  ensure_enrollment_bypass "$ENROLLMENT_DOMAIN" "$ENROLLMENT_APP_NAME"
  ensure_enrollment_bypass "${CONNECT_HOST}${ANDROID_ENROLLMENT_PATH}" "${ENROLLMENT_APP_NAME} (Android Apps)"
else
  log "Mobile enrollment bypass disabled for this Access-only application."
fi

# --- 3c. OPTIONAL additive: browser access via interactive email -----------
# Purely additive — creates/updates a SECOND `allow` policy (interactive email
# one-time PIN) on the SAME app, so a browser (Safari "Open in Browser" for
# md/html/pdf artifacts on connect.<zone>) can authenticate. The service-token
# non_identity policy above is never touched, so the device app keeps working. Skips
# cleanly when no allowlist is set.
EMAIL_POLICY_READY=0
if [ -n "$CONNECT_EMAILS" ] || [ -n "$CONNECT_EMAIL_DOMAIN" ]; then
  EMAIL_INCLUDE="$(jq -cn --arg emails "$CONNECT_EMAILS" --arg domain "$CONNECT_EMAIL_DOMAIN" '
    ([ $emails | split(",")[] | gsub("^\\s+|\\s+$";"") | select(length>0) | {email:{email:.}} ]
     + (if ($domain|length) > 0 then [ {email_domain:{domain:($domain|gsub("^\\s+|\\s+$";""))}} ] else [] end))')"
  if [ "$(printf '%s' "$EMAIL_INCLUDE" | jq 'length')" -gt 0 ]; then
    log "Ensuring an ADDITIVE email allow policy (browser access) on the app"
    log "  allowlist: $(printf '%s' "$EMAIL_INCLUDE" | jq -c '[.[] | (.email.email // .email_domain.domain)]')"
    EPOL_LIST="$(cf GET "/accounts/$ACCT/access/apps/$APP_ID/policies")"
    ok_or_explain "$EPOL_LIST" "list app policies" || exit 1
    EPOL_ID="$(printf '%s' "$EPOL_LIST" | jq -r --arg n "$EMAIL_POLICY_NAME" '.result[]? | select(.name==$n) | .id' | head -1)"
    EPOL_BODY="$(jq -cn \
      --arg n "$EMAIL_POLICY_NAME" \
      --arg duration "$SESSION" \
      --argjson precedence "$EMAIL_POLICY_PRECEDENCE" \
      --argjson inc "$EMAIL_INCLUDE" \
      '{name:$n, decision:"allow", precedence:$precedence, session_duration:$duration, include:$inc}')"
    if [ -z "$EPOL_ID" ]; then
      log "  creating email allow policy (service-token policy untouched)"
      ok_or_explain "$(cf POST "/accounts/$ACCT/access/apps/$APP_ID/policies" "$EPOL_BODY")" "create email policy" || exit 1
      log "  ✓ email policy created"
    else
      log "  updating existing email allow policy"
      ok_or_explain "$(cf PUT "/accounts/$ACCT/access/apps/$APP_ID/policies/$EPOL_ID" "$EPOL_BODY")" "update email policy" || exit 1
      log "  ✓ email policy updated"
    fi
    EMAIL_POLICY_READY=1
  fi
else
  log "No browser email allowlist set — host stays service-token-only."
  log "  To allow Safari 'Open in Browser' for artifacts, set MAGICIAN_CONNECT_ACCESS_EMAILS=you@example.com and re-run."
fi

# --- 4. staged service-token secret rotation -------------------------------
# Cloudflare's rotate endpoint retains the previous secret until the requested
# deadline. State is mode-0600 and intentionally contains both secrets so an
# interrupted stage can resume without rotating a second time. No secret is
# printed, passed in a URL, or placed in repository-owned configuration.
if [ "$TOKEN_WAS_CREATED" = 1 ] && [ "$ROTATION_ACTION" = stage ]; then
  log "A new service token was just created; persisting that initial credential instead of rotating it again."
  ROTATION_ACTION=""
fi
if [ -n "$ROTATION_ACTION" ]; then
  case "$ROTATION_ACTION" in
    status)
      if [ -f "$ROTATION_STATE_FILE" ]; then
        log "Access service-token rotation is $(state_value phase) for ${CONNECT_HOST}; previous secret expires at $(state_value previous_secret_expires_at)."
      else
        log "No staged Access service-token rotation exists for ${CONNECT_HOST}."
      fi
      ;;
    stage)
      if [ "$DRYRUN" = 1 ]; then
        log "would stage a grace-period rotation for ${CONNECT_HOST}, verify both secrets, and persist only after verification"
      else
        validate_rotation_verify_url
        if [ -f "$ROTATION_STATE_FILE" ]; then
          ROTATION_TOKEN_ID="$(state_value token_id)"
          ROTATION_CLIENT_ID="$(state_value client_id)"
          ROTATION_EXPIRY="$(state_value previous_secret_expires_at)"
          PREVIOUS_SECRET="$(decode_state_secret previous_client_secret_b64)"
          NEXT_SECRET="$(decode_state_secret next_client_secret_b64)"
          [ "$ROTATION_TOKEN_ID" = "$ST_ID" ] || die "rotation state belongs to a different service token; refusing to continue."
          [ "$ROTATION_CLIENT_ID" = "$ST_CLIENT_ID" ] || die "rotation state client ID does not match Cloudflare; refusing to continue."
          [ -n "$PREVIOUS_SECRET" ] && [ -n "$NEXT_SECRET" ] || die "rotation state is incomplete; keep the old device credential and inspect ${ROTATION_STATE_FILE}."
          log "Resuming the existing staged rotation for ${CONNECT_HOST}."
        else
          PREVIOUS_SECRET="$(read_env_key CF_ACCESS_CLIENT_SECRET)"
          [ -n "$PREVIOUS_SECRET" ] || die "cannot stage rotation because the current CF_ACCESS_CLIENT_SECRET is unavailable."
          if ! access_pair_is_accepted "$ST_CLIENT_ID" "$PREVIOUS_SECRET"; then
            die "current Access credential did not pass the exact-host verification; no rotation was attempted."
          fi
          ROTATION_EXPIRY="$(iso_after_seconds "$ROTATION_GRACE_SECONDS")"
          ROTATE_BODY="$(jq -cn --arg expiry "$ROTATION_EXPIRY" '{previous_client_secret_expires_at:$expiry}')"
          ROTATE_RESPONSE="$(cf POST "/accounts/$ACCT/access/service_tokens/$ST_ID/rotate" "$ROTATE_BODY")"
          ok_or_explain "$ROTATE_RESPONSE" "stage service-token rotation" || exit 1
          ROTATION_TOKEN_ID="$(printf '%s' "$ROTATE_RESPONSE" | jq -r '.result.id // empty')"
          ROTATION_CLIENT_ID="$(printf '%s' "$ROTATE_RESPONSE" | jq -r '.result.client_id // empty')"
          NEXT_SECRET="$(printf '%s' "$ROTATE_RESPONSE" | jq -r '.result.client_secret // empty')"
          [ "$ROTATION_TOKEN_ID" = "$ST_ID" ] || die "Cloudflare rotated an unexpected token; local credentials were not changed."
          [ "$ROTATION_CLIENT_ID" = "$ST_CLIENT_ID" ] || die "Cloudflare changed the service-token client ID unexpectedly; local credentials were not changed."
          [ -n "$NEXT_SECRET" ] || die "Cloudflare did not return the successor secret; local credentials were not changed."
          write_rotation_state "$ST_ID" "$ST_CLIENT_ID" "$ROTATION_EXPIRY" issued "$PREVIOUS_SECRET" "$NEXT_SECRET"
        fi
        if ! access_pair_is_accepted "$ST_CLIENT_ID" "$PREVIOUS_SECRET"; then
          die "the previous secret is not accepted during the grace period; successor state was retained for recovery."
        fi
        if ! access_pair_is_accepted "$ST_CLIENT_ID" "$NEXT_SECRET"; then
          die "the successor secret was not accepted; the previous local credential remains active and rotation state was retained."
        fi
        upsert_env CF_ACCESS_CLIENT_ID "$ST_CLIENT_ID"
        upsert_env CF_ACCESS_CLIENT_SECRET "$NEXT_SECRET"
        write_rotation_state "$ST_ID" "$ST_CLIENT_ID" "$ROTATION_EXPIRY" staged "$PREVIOUS_SECRET" "$NEXT_SECRET"
        ST_SECRET=""
        log "Staged rotation verified. The previous secret remains valid until ${ROTATION_EXPIRY}."
        log "Create a fresh mobile connection QR during the grace period, verify the phone, then finalize explicitly."
      fi
      ;;
    extend)
      [ -f "$ROTATION_STATE_FILE" ] || die "no staged rotation exists to extend."
      ROTATION_TOKEN_ID="$(state_value token_id)"
      ROTATION_CLIENT_ID="$(state_value client_id)"
      PREVIOUS_SECRET="$(decode_state_secret previous_client_secret_b64)"
      NEXT_SECRET="$(decode_state_secret next_client_secret_b64)"
      [ "$ROTATION_TOKEN_ID" = "$ST_ID" ] && [ "$ROTATION_CLIENT_ID" = "$ST_CLIENT_ID" ] \
        || die "rotation state does not match the configured Cloudflare token."
      ROTATION_EXPIRY="$(iso_after_seconds "$ROTATION_GRACE_SECONDS")"
      if [ "$DRYRUN" = 1 ]; then
        log "would extend the previous-secret grace period through ${ROTATION_EXPIRY}"
      else
        EXTEND_BODY="$(jq -cn --arg expiry "$ROTATION_EXPIRY" '{previous_client_secret_expires_at:$expiry}')"
        ok_or_explain "$(cf PUT "/accounts/$ACCT/access/service_tokens/$ST_ID" "$EXTEND_BODY")" "extend service-token rotation grace period" || exit 1
        write_rotation_state "$ST_ID" "$ST_CLIENT_ID" "$ROTATION_EXPIRY" staged "$PREVIOUS_SECRET" "$NEXT_SECRET"
        log "Extended the previous-secret grace period through ${ROTATION_EXPIRY}."
      fi
      ;;
    finalize)
      [ -f "$ROTATION_STATE_FILE" ] || die "no staged rotation exists to finalize."
      validate_rotation_verify_url
      ROTATION_TOKEN_ID="$(state_value token_id)"
      ROTATION_CLIENT_ID="$(state_value client_id)"
      NEXT_SECRET="$(decode_state_secret next_client_secret_b64)"
      [ "$ROTATION_TOKEN_ID" = "$ST_ID" ] && [ "$ROTATION_CLIENT_ID" = "$ST_CLIENT_ID" ] \
        || die "rotation state does not match the configured Cloudflare token."
      [ -n "$NEXT_SECRET" ] || die "rotation state has no successor secret."
      if [ "$DRYRUN" = 1 ]; then
        log "would verify the successor and expire the previous service-token secret"
      else
        if ! access_pair_is_accepted "$ST_CLIENT_ID" "$NEXT_SECRET"; then
          die "successor verification failed; previous-secret grace was not revoked."
        fi
        FINAL_EXPIRY="$(iso_after_seconds 0)"
        FINAL_BODY="$(jq -cn --arg expiry "$FINAL_EXPIRY" '{previous_client_secret_expires_at:$expiry}')"
        ok_or_explain "$(cf PUT "/accounts/$ACCT/access/service_tokens/$ST_ID" "$FINAL_BODY")" "finalize service-token rotation" || exit 1
        upsert_env CF_ACCESS_CLIENT_ID "$ST_CLIENT_ID"
        upsert_env CF_ACCESS_CLIENT_SECRET "$NEXT_SECRET"
        rm -f "$ROTATION_STATE_FILE"
        ST_SECRET=""
        log "Rotation finalized; Cloudflare no longer accepts the previous secret."
      fi
      ;;
  esac
fi

# --- 5. persist credentials (secret written only when created or rotated) ---

upsert_env CF_ACCESS_CLIENT_ID "$ST_CLIENT_ID"
if [ -n "$ST_SECRET" ]; then
  upsert_env CF_ACCESS_CLIENT_SECRET "$ST_SECRET"
elif [ "$(read_env_key CF_ACCESS_CLIENT_SECRET)" = "" ]; then
  warn "The service token already existed and its SECRET is not in ${ENV_FILE}."
  warn "Cloudflare cannot re-show a token secret. Stage a grace-period rotation to mint a recoverable successor."
fi
if [ "$PERSIST_MOBILE_RUNTIME" = 1 ]; then
  upsert_env MAGICIAN_MOBILE_PUBLIC_ORIGIN "$MOBILE_PUBLIC_ORIGIN"
  upsert_env MAGICIAN_CF_ACCESS_TEAM_DOMAIN "$ACCESS_ISSUER"
  upsert_env MAGICIAN_CF_ACCESS_AUD "$APP_AUD"
  upsert_env MAGICIAN_CF_ACCESS_MODE "$ORIGIN_ACCESS_MODE"
  for runtime_env in "$DATA_DIR/.env" "$DATA_DIR/.env.development"; do
    [ "$runtime_env" = "$ENV_FILE" ] && continue
    upsert_env_file "$runtime_env" MAGICIAN_MOBILE_PUBLIC_ORIGIN "$MOBILE_PUBLIC_ORIGIN"
    upsert_env_file "$runtime_env" MAGICIAN_CF_ACCESS_TEAM_DOMAIN "$ACCESS_ISSUER"
    upsert_env_file "$runtime_env" MAGICIAN_CF_ACCESS_AUD "$APP_AUD"
    upsert_env_file "$runtime_env" MAGICIAN_CF_ACCESS_MODE "$ORIGIN_ACCESS_MODE"
  done
else
  log "Mobile runtime origin and Access audience persistence disabled for this Access-only application."
fi

# A caller may request a readiness attestation. The Notes tunnel consumes it
# only while the hostname and both policy classes still match and the timestamp
# is fresh; it is deliberately written after Access application/policy success.
if [ -n "$READY_FILE" ] && [ "$DRYRUN" != 1 ]; then
  if [ "$REQUIRE_EMAIL_ALLOWLIST" = 1 ] && [ "$EMAIL_POLICY_READY" != 1 ]; then
    die "owner Allow policy was not verified; refusing to write ${READY_FILE}."
  fi
  mkdir -p "$(dirname "$READY_FILE")"
  READY_TMP="$(mktemp "${READY_FILE}.XXXXXX")"
  {
    printf 'hostname=%s\n' "$CONNECT_HOST"
    printf 'access_app_id=%s\n' "$APP_ID"
    printf 'verified_at_epoch=%s\n' "$(date +%s)"
    printf 'service_token_policy=verified\n'
    printf 'mobile_enrollment_bypass=%s\n' "$([ "$MOBILE_ENROLLMENT_ENABLED" = 1 ] && printf verified || printf not_configured)"
    printf 'owner_allow_policy=%s\n' "$([ "$EMAIL_POLICY_READY" = 1 ] && printf verified || printf not_configured)"
  } > "$READY_TMP"
  chmod 600 "$READY_TMP" 2>/dev/null || true
  mv "$READY_TMP" "$READY_FILE"
  log "Wrote Access readiness attestation to ${READY_FILE}."
fi

log "Access gate ensured for ${CONNECT_HOST}."
if [ "$PERSIST_MOBILE_RUNTIME" = 1 ]; then
  cat <<EOF

  Next:
    1. The service-token credentials are in ${ENV_FILE}:
         CF_ACCESS_CLIENT_ID / CF_ACCESS_CLIENT_SECRET
    2. Open Magician Settings → Mobile devices and create a connection QR.
       The iOS and Android apps receive this runtime origin and the current
       outer Access credential only after consuming that one-time capability.
    3. No customer hostname or Cloudflare secret is compiled into either app.

  Do NOT gate the webhook host (webhook.${ZONE}) — webhooks can't complete Access.
EOF
fi
