#!/usr/bin/env bash
# ensure-ui-access.sh — idempotently gate the dev-UI hostname (ui.<zone>, which
# the tunnel maps to the Vite dev server on :5173) behind a Cloudflare Zero Trust
# Access application with an INTERACTIVE email policy.
#
# Unlike the device connection endpoint (ensure-connect-access.sh), whose clients inject
# CF-Access-Client-Id/Secret headers and therefore uses a SERVICE TOKEN, the dev
# UI is opened in a BROWSER — which cannot attach those headers. So access here is
# a human login: open https://ui.<zone>, Cloudflare emails a one-time PIN to an
# allowlisted address (its built-in One-Time PIN identity provider — no external
# IdP to configure), you enter the code, done. No app changes, no stored secret.
#
# Idempotent: reuses/updates the Access app + policy instead of duplicating. To
# actually publish ui.<zone> through the tunnel, run the tunnel with
# MAGICIAN_TUNNEL_UI=1 (or `make serve-ui-tunnel`).
#
# Requires an allowlist (at least one of):
#   MAGICIAN_UI_ACCESS_EMAILS        comma-separated emails (e.g. a@x.com,b@y.com)
#   MAGICIAN_UI_ACCESS_EMAIL_DOMAIN  a whole email domain (e.g. example.com)
# and CLOUDFLARE_API_TOKEN (or CF_API_TOKEN) with Account · Access: Apps and
# Policies · Edit. The account id is decoded from CLOUDFLARED_TOKEN (override with
# MAGICIAN_CF_ACCOUNT_ID). Honours MAGICIAN_INSTALL_DRYRUN=1; never logs the token.
set -euo pipefail

# --- inputs (env-overridable) ----------------------------------------------
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
UI_HOST="${MAGICIAN_UI_HOST:-ui.${ZONE}}"
APP_NAME="${MAGICIAN_UI_ACCESS_APP_NAME:-Magician Dev UI}"
POLICY_NAME="${MAGICIAN_UI_ACCESS_POLICY_NAME:-dev-ui allowed emails}"
SESSION="${MAGICIAN_UI_ACCESS_SESSION:-730h}"   # ~1 month — dev convenience, infrequent re-login
EMAILS="${MAGICIAN_UI_ACCESS_EMAILS:-}"
EMAIL_DOMAIN="${MAGICIAN_UI_ACCESS_EMAIL_DOMAIN:-}"
DATA_DIR="${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}"
DRYRUN="${MAGICIAN_INSTALL_DRYRUN:-0}"
API="https://api.cloudflare.com/client/v4"

log()  { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }
warn() { printf '\033[1;33mWARN: %s\033[0m\n' "$*" >&2; }
die()  { printf '\033[1;31mERROR: %s\033[0m\n' "$*" >&2; exit 1; }

command -v curl >/dev/null 2>&1 || die "curl not found."
command -v jq   >/dev/null 2>&1 || die "jq not found (brew install jq)."

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

[ -n "$EMAILS" ] || EMAILS="$(read_env_key MAGICIAN_UI_ACCESS_EMAILS)"
[ -n "$EMAIL_DOMAIN" ] || EMAIL_DOMAIN="$(read_env_key MAGICIAN_UI_ACCESS_EMAIL_DOMAIN)"
if [ -z "$EMAILS" ] && [ -z "$EMAIL_DOMAIN" ]; then
  die "no allowlist. Set MAGICIAN_UI_ACCESS_EMAILS=you@example.com (comma-separated) and/or MAGICIAN_UI_ACCESS_EMAIL_DOMAIN=example.com"
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

# cf METHOD PATH [JSON-body] — Cloudflare API call; prints the raw JSON response.
cf() {
  local method="$1" path="$2" body="${3:-}"
  if [ "$DRYRUN" = 1 ] && [ "$method" != GET ]; then
    log "would ${method} ${path}${body:+  (body: $(printf '%s' "$body" | jq -c '.' 2>/dev/null || echo '...'))}" >&2
    printf '{"success":true,"result":{"id":"DRYRUN"}}'
    return 0
  fi
  curl -s -X "$method" -H "Authorization: Bearer $API_TOKEN" -H "Content-Type: application/json" \
    ${body:+--data "$body"} "$API$path"
}

ok_or_explain() {
  local resp="$1" ctx="$2" ok errs
  ok="$(printf '%s' "$resp" | jq -r '.success // false' 2>/dev/null)"
  [ "$ok" = "true" ] && return 0
  errs="$(printf '%s' "$resp" | jq -rc '.errors // empty' 2>/dev/null)"
  warn "${ctx} failed: ${errs:-<unparseable response>}"
  if printf '%s' "$errs" | grep -qiE "authentication|permission|not allowed|9109|forbidden"; then
    warn "  the API token likely lacks: Account · Access: Apps and Policies · Edit"
  fi
  return 1
}

# --- build the include rules (emails + optional domain) --------------------
INCLUDE="$(jq -cn --arg emails "$EMAILS" --arg domain "$EMAIL_DOMAIN" '
  ([ $emails | split(",")[] | gsub("^\\s+|\\s+$";"") | select(length>0) | {email:{email:.}} ]
   + (if ($domain|length) > 0 then [ {email_domain:{domain:($domain|gsub("^\\s+|\\s+$";""))}} ] else [] end))
')"
[ "$(printf '%s' "$INCLUDE" | jq 'length')" -gt 0 ] || die "allowlist parsed to nothing — check MAGICIAN_UI_ACCESS_EMAILS / _EMAIL_DOMAIN."
log "Access allowlist for ${UI_HOST}: $(printf '%s' "$INCLUDE" | jq -c '[.[] | (.email.email // .email_domain.domain)]')"

# --- 1. Access application for the hostname --------------------------------
log "Ensuring Access application for ${UI_HOST}"
APP_LIST="$(cf GET "/accounts/$ACCT/access/apps")"
ok_or_explain "$APP_LIST" "list access apps" || exit 1
APP_ID="$(printf '%s' "$APP_LIST" | jq -r --arg d "$UI_HOST" '.result[] | select(.domain==$d) | .id' | head -1)"
if [ -z "$APP_ID" ]; then
  log "  creating self-hosted app (domain ${UI_HOST}, session ${SESSION})"
  APP_CREATE="$(cf POST "/accounts/$ACCT/access/apps" \
    "{\"name\":\"${APP_NAME}\",\"domain\":\"${UI_HOST}\",\"type\":\"self_hosted\",\"session_duration\":\"${SESSION}\"}")"
  ok_or_explain "$APP_CREATE" "create access app" || exit 1
  APP_ID="$(printf '%s' "$APP_CREATE" | jq -r '.result.id')"
  log "  ✓ created app ${APP_ID}"
else
  log "  ✓ reusing existing app ${APP_ID}"
fi

# --- 2. allow policy (interactive email) — create or update ----------------
log "Ensuring the email allow policy on the app"
POL_LIST="$(cf GET "/accounts/$ACCT/access/apps/$APP_ID/policies")"
ok_or_explain "$POL_LIST" "list app policies" || exit 1
POL_ID="$(printf '%s' "$POL_LIST" | jq -r --arg n "$POLICY_NAME" '.result[]? | select(.name==$n) | .id' | head -1)"
POL_BODY="$(jq -cn --arg n "$POLICY_NAME" --argjson inc "$INCLUDE" '{name:$n, decision:"allow", include:$inc}')"
if [ -z "$POL_ID" ]; then
  log "  creating allow policy"
  ok_or_explain "$(cf POST "/accounts/$ACCT/access/apps/$APP_ID/policies" "$POL_BODY")" "create policy" || exit 1
  log "  ✓ policy created"
else
  log "  updating existing policy ${POL_ID} to the current allowlist"
  ok_or_explain "$(cf PUT "/accounts/$ACCT/access/apps/$APP_ID/policies/$POL_ID" "$POL_BODY")" "update policy" || exit 1
  log "  ✓ policy updated"
fi

log "Access gate ensured for ${UI_HOST}."
cat <<EOF

  Next:
    1. Publish the dev UI through the tunnel: MAGICIAN_TUNNEL_UI=1 make ensure-tunnel
       (or \`make serve-ui-tunnel\`), with the Vite dev server running on :5173.
    2. Open https://${UI_HOST} in a browser — Cloudflare will email a one-time PIN
       to an allowlisted address; enter it to reach the dev UI.

  To change who's allowed, re-run with an updated MAGICIAN_UI_ACCESS_EMAILS /
  MAGICIAN_UI_ACCESS_EMAIL_DOMAIN — the policy is updated in place.
EOF
