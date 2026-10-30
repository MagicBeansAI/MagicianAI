#!/usr/bin/env bash
# magesp-connect.sh — point an ESP32 voice terminal at this machine's Magician.
#
# The terminal stores its backend URL and Cloudflare Access pair in NVS and
# changes them only through its own /config route, guarded by the passphrase
# on its screen. Moving it to a new origin drops its old device bearer, so this
# also asks it to pair again (repair=1); the pairing is accepted because it
# arrives through Cloudflare Access with the device service token.
#
# Usage: scripts/magesp-connect.sh <terminal-ip> [base-url]
#   base-url defaults to this machine's api_origin in remote-machines.yaml
#   (e.g. https://my-machine.example.com). The Access pair is read from the
#   runtime env (CF_ACCESS_CLIENT_ID / CF_ACCESS_CLIENT_SECRET). Nothing secret
#   is printed; the passphrase is read without echo.
set -euo pipefail

ip="${1:?usage: magesp-connect.sh <terminal-ip> [base-url]}"
data_dir="${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}"

env_value() {
  local key="$1" value file
  value="$(printenv "$key" 2>/dev/null || true)"
  for file in "$data_dir/.env.development" "$data_dir/.env"; do
    [ -n "$value" ] && break
    [ -f "$file" ] || continue
    value="$(grep -E "^[[:space:]]*${key}=" "$file" | tail -1 | cut -d= -f2- | sed -e 's/^"//' -e 's/"$//')"
  done
  printf '%s' "$value"
}

base="${2:-}"
if [ -z "$base" ]; then
  base="$(awk '/^[[:space:]]*api_origin:/ {print $2; exit}' "$data_dir/remote-machines.yaml" 2>/dev/null || true)"
fi
[ -n "$base" ] || { echo "No base URL: pass one, or record this machine in remote-machines.yaml." >&2; exit 1; }

cf_id="$(env_value CF_ACCESS_CLIENT_ID)"
cf_secret="$(env_value CF_ACCESS_CLIENT_SECRET)"
[ -n "$cf_id" ] && [ -n "$cf_secret" ] || { echo "CF_ACCESS_CLIENT_ID / CF_ACCESS_CLIENT_SECRET are not set in the runtime env." >&2; exit 1; }

# The pair must open this machine through Access, or the terminal cannot pair.
# Secrets go to curl as a config on stdin, never on its command line (`ps`).
curl_quote() { printf '"%s"' "$(printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g')"; }
status="$(printf 'header = %s\nheader = %s\n' \
  "$(curl_quote "CF-Access-Client-Id: $cf_id")" "$(curl_quote "CF-Access-Client-Secret: $cf_secret")" |
  curl -s -m10 -o /dev/null -w '%{http_code}' -K - "$base/health" || true)"
[ "$status" = "200" ] || { echo "$base/health answered $status with the Access pair; fix Access first." >&2; exit 1; }

read -r -s -p "Passphrase shown on the terminal's screen: " passphrase; echo
[ -n "$passphrase" ] || { echo "No passphrase entered." >&2; exit 1; }

echo "Pointing $ip at $base and asking it to pair again…"
# The terminal's /config is plain HTTP on the LAN (it has no TLS server): run
# this on a network you trust.
reply="$(printf 'data-urlencode = %s\ndata-urlencode = %s\ndata-urlencode = %s\ndata-urlencode = %s\ndata-urlencode = %s\n' \
  "$(curl_quote "auth=$passphrase")" "$(curl_quote "url=$base")" \
  "$(curl_quote "cfid=$cf_id")" "$(curl_quote "cfsec=$cf_secret")" "$(curl_quote "repair=1")" |
  curl -s -m20 -w '\n%{http_code}' -K - "http://$ip/config")" ||
  { echo "Could not reach the terminal at http://$ip/config — check its IP and that it is on this network." >&2; exit 1; }
code="${reply##*$'\n'}"
body="${reply%$'\n'*}"
case "$code" in
  200) echo "Saved. The terminal restarts, pairs, and shows \"connected\" on its For you page within about a minute." ;;
  403) echo "The passphrase was refused — read it again from the terminal's screen." >&2; exit 1 ;;
  409) echo "The terminal is in a voice turn; let it finish and run this again." >&2; exit 1 ;;
  *)   echo "The terminal answered $code: $(printf '%s' "$body" | head -c 300)" >&2; exit 1 ;;
esac
