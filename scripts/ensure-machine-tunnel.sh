#!/usr/bin/env bash
# ensure-machine-tunnel.sh — one Cloudflare Tunnel per Magician machine.
#
# Several machines (a MacBook Pro, a Mac Studio, ...) used to share one tunnel
# and one hostname. Cloudflare load-balances a tunnel's connectors, so requests
# landed on whichever machine it picked: MCP OAuth callbacks reached a Magician
# that never started the flow ("Sign-in link expired"), and phones paired to
# one machine hit the other. Each machine now gets its own remotely-managed
# tunnel `magician-<machine>` and its own hostnames:
#
#   <machine>.<zone>          devices: /api + /health -> :3002, /host -> :3017
#                             (Cloudflare Access, ensure-connect-access.sh)
#   ui-<machine>.<zone>       dev UI -> :5173 (Cloudflare Access, ensure-ui-access.sh)
#   webhook-<machine>.<zone>  Kapso :3010, agentmail :3011 (public; providers
#                             cannot pass Access)
#
# Usage:
#   ensure-machine-tunnel.sh <machine>            create/update the tunnel, its
#                                                 ingress and DNS (from any machine)
#   ensure-machine-tunnel.sh <machine> --install  also, ON that machine: run this
#                                                 machine's connector on it, point
#                                                 the runtime env at its hostnames,
#                                                 and gate them with Access
#   ensure-machine-tunnel.sh <machine> --record-only
#                                                 only add <machine> to this machine's
#                                                 registry (its tunnel already exists)
#   ensure-machine-tunnel.sh park <hostname>      answer <hostname> with 404 on any
#                                                 tunnel that still serves it
#
# Every run also records the machine in $DATA_DIR/remote-machines.yaml, the
# registry the apps read to choose a machine (owner data, never in the repo).
#
# Requires CLOUDFLARE_API_TOKEN (Tunnel edit, Zone read, DNS edit, Access edit/read) and
# MAGICIAN_TUNNEL_ZONE, from the process env or the runtime .env files. The
# account id comes from MAGICIAN_CF_ACCOUNT_ID, else the existing connector
# token. Secrets are never printed. MAGICIAN_INSTALL_DRYRUN=1 prints what would
# change without changing anything.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA_DIR="${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}"
CF_DIR="${CLOUDFLARED_HOME:-$HOME/.cloudflared}"
DRYRUN="${MAGICIAN_INSTALL_DRYRUN:-0}"
LAUNCH_LABEL="${MAGICIAN_CLOUDFLARED_TOKEN_LAUNCH_LABEL:-com.magican.magician-cloudflared-token}"

log() { printf '==> %s\n' "$*"; }
die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

read_env_key() { # first non-empty value from the process env, then the runtime env files
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

upsert_env_key() { # set KEY=VALUE in every live env file that has it, and in .env.development
  local key="$1" value="$2" file tmp
  for file in "$DATA_DIR/.env.development" "$DATA_DIR/.env"; do
    [ -f "$file" ] || continue
    if [ "$file" != "$DATA_DIR/.env.development" ] && ! grep -qE "^[[:space:]]*${key}=" "$file"; then
      continue
    fi
    if [ "$DRYRUN" = 1 ]; then log "would set ${key} in ${file}"; continue; fi
    tmp="$(mktemp "${file}.XXXXXX")"
    grep -vE "^[[:space:]]*${key}=" "$file" > "$tmp" || true
    # dotenvy stops at the first line it cannot parse, and an unquoted space
    # is such a line: everything below it is silently never loaded. Quote any
    # value that is not a plain token.
    if [[ "$value" =~ [^A-Za-z0-9_./:@%+,=-] ]]; then
      local escaped="${value//\\/\\\\}"
      escaped="${escaped//\"/\\\"}"
      escaped="${escaped//\$/\\\$}"
      printf '%s="%s"\n' "$key" "$escaped" >> "$tmp"
    else
      printf '%s=%s\n' "$key" "$value" >> "$tmp"
    fi
    chmod 600 "$tmp" 2>/dev/null || true
    mv "$tmp" "$file"
  done
}

[ $# -ge 1 ] || die "usage: $0 <machine> [--install] | park <hostname>"

ZONE="$(read_env_key MAGICIAN_TUNNEL_ZONE)"
[ -n "$ZONE" ] || die "MAGICIAN_TUNNEL_ZONE is required (e.g. example.com)."
CF_API_TOKEN="$(read_env_key CLOUDFLARE_API_TOKEN)"
[ -n "$CF_API_TOKEN" ] || CF_API_TOKEN="$(read_env_key CF_API_TOKEN)"
[ -n "$CF_API_TOKEN" ] || die "CLOUDFLARE_API_TOKEN is required."
ACCOUNT_ID="$(read_env_key MAGICIAN_CF_ACCOUNT_ID)"
EXISTING_CONNECTOR_TOKEN="$(read_env_key CLOUDFLARED_TOKEN)"
[ -n "$EXISTING_CONNECTOR_TOKEN" ] || EXISTING_CONNECTOR_TOKEN="$(cat "$CF_DIR/magician-token" 2>/dev/null || true)"
export ZONE CF_API_TOKEN ACCOUNT_ID EXISTING_CONNECTOR_TOKEN DRYRUN

# Cloudflare API helper. Prints nothing secret; a connector token is only ever
# written to the file named by TOKEN_OUT.
cf() {
  python3 - "$@" <<'PY'
import base64, json, os, sys, urllib.parse, urllib.request

API = "https://api.cloudflare.com/client/v4"
TOKEN = os.environ["CF_API_TOKEN"]
DRY = os.environ.get("DRYRUN") == "1"


def call(method, path, body=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(API + path, data=data, method=method, headers={
        "Authorization": f"Bearer {TOKEN}", "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            out = json.load(resp)
    except urllib.error.HTTPError as err:
        out = json.loads(err.read() or b"{}")
    if not out.get("success", False):
        errors = "; ".join(e.get("message", "") for e in out.get("errors", []))
        sys.exit(f"Cloudflare API {method} {path.split('?')[0]} failed: {errors}")
    return out["result"]


def account_id():
    if os.environ.get("ACCOUNT_ID"):
        return os.environ["ACCOUNT_ID"]
    tok = os.environ.get("EXISTING_CONNECTOR_TOKEN", "").strip()
    if not tok:
        sys.exit("set MAGICIAN_CF_ACCOUNT_ID (no connector token to read the account from)")
    return json.loads(base64.b64decode(tok + "=" * (-len(tok) % 4)))["a"]


def zone_id(zone):
    found = call("GET", f"/zones?name={urllib.parse.quote(zone)}")
    if not found:
        sys.exit(f"zone {zone} not found for this API token")
    return found[0]["id"]


def ingress_for(machine, zone):
    host, ui, hook = f"{machine}.{zone}", f"ui-{machine}.{zone}", f"webhook-{machine}.{zone}"
    env = os.environ.get
    api = f"http://localhost:{env('MAGICIAN_CONNECT_LOCAL_API_PORT') or 3002}"
    return [
        {"hostname": hook, "path": "^/agentmail-webhook/?$",
         "service": f"http://localhost:{env('AGENTMAIL_WEBHOOK_PORT') or 3011}"},
        {"hostname": hook, "service": f"http://localhost:{env('KAPSO_WEBHOOK_PORT') or 3010}"},
        {"hostname": ui, "service": f"http://localhost:{env('MAGICIAN_UI_PORT') or 5173}"},
        {"hostname": host, "path": "^/host(/.*)?$",
         "service": f"http://localhost:{env('MAGICIAN_CONNECT_GATEWAY_PORT') or 3017}"},
        {"hostname": host, "path": "^/health/?$", "service": api},
        {"hostname": host, "path": "^/\\.well-known/magican/mcp-profiles/[a-f0-9]{64}\\.json$", "service": api},
        {"hostname": host, "path": "^/api(/.*)?$", "service": api},
        {"service": "http_status:404"},
    ]


def ensure_machine(machine, zone, token_out):
    acct = account_id()
    name = f"magician-{machine}"
    found = call("GET", f"/accounts/{acct}/cfd_tunnel?name={name}&is_deleted=false")
    if found:
        tunnel = found[0]["id"]
        print(f"tunnel {name} exists ({tunnel[:8]}…)")
    elif DRY:
        tunnel = "DRYRUN"
        print(f"would create tunnel {name}")
    else:
        tunnel = call("POST", f"/accounts/{acct}/cfd_tunnel",
                      {"name": name, "config_src": "cloudflare"})["id"]
        print(f"created tunnel {name} ({tunnel[:8]}…)")
    ingress = ingress_for(machine, zone)
    if DRY:
        print(f"would set ingress for {machine}.{zone}, ui-{machine}.{zone}, webhook-{machine}.{zone}")
    else:
        call("PUT", f"/accounts/{acct}/cfd_tunnel/{tunnel}/configurations",
             {"config": {"ingress": ingress}})
        print("ingress set")
    zid = zone_id(zone)
    target = f"{tunnel}.cfargotunnel.com"
    for host in sorted({rule["hostname"] for rule in ingress if "hostname" in rule}):
        existing = call("GET", f"/zones/{zid}/dns_records?name={urllib.parse.quote(host)}")
        record = {"type": "CNAME", "name": host, "content": target, "proxied": True,
                  "comment": f"Magician tunnel {name}"}
        if existing and existing[0]["content"] == target and existing[0].get("proxied"):
            print(f"DNS {host} ok")
        elif DRY:
            print(f"would point DNS {host} -> {name}")
        elif existing:
            call("PUT", f"/zones/{zid}/dns_records/{existing[0]['id']}", record)
            print(f"DNS {host} -> {name} (updated)")
        else:
            call("POST", f"/zones/{zid}/dns_records", record)
            print(f"DNS {host} -> {name} (created)")
    if token_out and not DRY:
        token = call("GET", f"/accounts/{acct}/cfd_tunnel/{tunnel}/token")
        old = os.umask(0o077)
        with open(token_out, "w") as handle:
            handle.write(token.strip() + "\n")
        os.umask(old)
        print(f"connector token written to {token_out}")


def park(hostname):
    acct = account_id()
    for tunnel in call("GET", f"/accounts/{acct}/cfd_tunnel?is_deleted=false&per_page=100"):
        if tunnel.get("config_src") != "cloudflare":
            continue
        config = call("GET", f"/accounts/{acct}/cfd_tunnel/{tunnel['id']}/configurations")
        ingress = ((config or {}).get("config") or {}).get("ingress") or []
        changed = False
        for rule in ingress:
            if rule.get("hostname") == hostname and rule.get("service") != "http_status:404":
                rule["service"] = "http_status:404"
                rule.pop("originRequest", None)
                changed = True
        if not changed:
            continue
        if DRY:
            print(f"would park {hostname} on tunnel {tunnel['name']}")
            continue
        call("PUT", f"/accounts/{acct}/cfd_tunnel/{tunnel['id']}/configurations",
             {"config": {**config["config"], "ingress": ingress}})
        print(f"parked {hostname} on tunnel {tunnel['name']} (answers 404)")


command = sys.argv[1]
if command == "machine":
    ensure_machine(sys.argv[2], os.environ["ZONE"], sys.argv[3] if len(sys.argv) > 3 else "")
elif command == "park":
    park(sys.argv[2])
PY
}

# record_machine — upsert this machine in the machine registry
# ($DATA_DIR/remote-machines.yaml), the list the apps read to choose a Magician.
# A separate file: magician-config.yaml rejects unknown top-level keys.
record_machine() {
  local file="$1" machine="$2"
  if [ "$DRYRUN" = 1 ]; then log "would record ${machine} in ${file}"; return 0; fi
  MACHINE="$machine" python3 - "$file" <<'PY'
import os, re, sys

path, machine, zone = sys.argv[1], os.environ["MACHINE"], os.environ["ZONE"]
header = ("# Magician machines, each behind its own Cloudflare tunnel\n"
          "# (scripts/ensure-machine-tunnel.sh writes this file; one entry per machine).\n"
          "# Apps read it through the API to choose which Magician to connect to.\n"
          "machines:\n")
text = open(path).read() if os.path.exists(path) else header
entry = (f"  - name: {machine}\n"
         f"    api_origin: https://{machine}.{zone}\n"
         f"    ui_origin: https://ui-{machine}.{zone}\n"
         f"    webhook_origin: https://webhook-{machine}.{zone}\n")
block = re.compile(rf"  - name: {re.escape(machine)}\n(?:    .*\n)*")
if block.search(text):
    text = block.sub(entry, text, count=1)
else:
    text = text.rstrip("\n") + "\n" + entry
open(path, "w").write(text)
print(f"recorded {machine} in {path}")
PY
}

if [ "$1" = park ]; then
  [ $# -eq 2 ] || die "usage: $0 park <hostname>"
  cf park "$2"
  exit 0
fi

MACHINE="$1"; INSTALL=0; RECORD_ONLY=0
case "${2:-}" in
  --install) INSTALL=1 ;;
  --record-only) RECORD_ONLY=1 ;;
  "") ;;
  *) die "unknown option ${2}; use --install or --record-only" ;;
esac
[[ "$MACHINE" =~ ^[a-z0-9][a-z0-9-]{0,30}$ ]] || die "machine name must be lowercase letters, digits and dashes"
HOST="${MACHINE}.${ZONE}"; UI_HOST="ui-${MACHINE}.${ZONE}"; WEBHOOK_HOST="webhook-${MACHINE}.${ZONE}"
TOKEN_FILE="$CF_DIR/magician-${MACHINE}-token"

log "machine ${MACHINE}: ${HOST}, ${UI_HOST}, ${WEBHOOK_HOST}"
if [ "$RECORD_ONLY" = 1 ]; then
  record_machine "$DATA_DIR/remote-machines.yaml" "$MACHINE"
  exit 0
fi
mkdir -p "$CF_DIR"
if [ "$INSTALL" = 1 ]; then cf machine "$MACHINE" "$TOKEN_FILE"; else cf machine "$MACHINE"; fi

record_machine "$DATA_DIR/remote-machines.yaml" "$MACHINE"

if [ "$INSTALL" = 0 ]; then
  log "tunnel and DNS ready. On ${MACHINE} itself, run: scripts/ensure-machine-tunnel.sh ${MACHINE} --install"
  exit 0
fi

# --- on this machine: run its connector, point the runtime at its hostnames ---
if [ "$DRYRUN" = 1 ]; then
  log "would restart the ${LAUNCH_LABEL} connector on ${TOKEN_FILE} and update the runtime env"
else
  cloudflared_bin="$(command -v cloudflared)" || die "cloudflared is not installed (brew install cloudflared)"
  logf="$CF_DIR/magician-${MACHINE}-tunnel.log"
  launchctl remove "$LAUNCH_LABEL" >/dev/null 2>&1 || true
  pkill -f "cloudflared tunnel run --token" >/dev/null 2>&1 || true
  sleep 1
  launchctl submit -l "$LAUNCH_LABEL" -o "$logf" -e "$logf" -- \
    "$cloudflared_bin" tunnel run --token-file "$TOKEN_FILE"
  log "connector ${LAUNCH_LABEL} now runs tunnel magician-${MACHINE} (logs: ${logf})"
  upsert_env_key CLOUDFLARED_TOKEN "$(cat "$TOKEN_FILE")"
  if [ -f "$CF_DIR/magician-token" ] && [ ! -L "$CF_DIR/magician-token" ]; then
    cp -p "$CF_DIR/magician-token" "$CF_DIR/magician-token.shared-tunnel.bak"
  fi
  ln -sf "$TOKEN_FILE" "$CF_DIR/magician-token"
fi
upsert_env_key MAGICIAN_MACHINE_NAME "$MACHINE"
upsert_env_key MAGICIAN_CONNECT_HOST "$HOST"
upsert_env_key MAGICIAN_MOBILE_PUBLIC_ORIGIN "https://${HOST}"
upsert_env_key MAGICIAN_UI_HOST "$UI_HOST"
# The Kapso and AgentMail bots derive their webhook URLs from this and
# re-register on start, so nothing else names the webhook host.
upsert_env_key MAGICIAN_WEBHOOK_HOST "$WEBHOOK_HOST"
# Per-machine Access object names, so one machine's run never adopts another's.
upsert_env_key MAGICIAN_CONNECT_ACCESS_APP_NAME "Magican device connection (${MACHINE})"
upsert_env_key MAGICIAN_CONNECT_ACCESS_POLICY_NAME "magican device service token (${MACHINE})"
upsert_env_key MAGICIAN_CONNECT_ACCESS_EMAIL_POLICY_NAME "magican device browser emails (${MACHINE})"
upsert_env_key MAGICIAN_CONNECT_ACCESS_TOKEN_NAME "magican-device-${MACHINE}"
upsert_env_key MAGICIAN_MOBILE_ENROLLMENT_ACCESS_APP_NAME "Magican mobile one-time enrollment (${MACHINE})"
upsert_env_key MAGICIAN_UI_ACCESS_APP_NAME "Magician Dev UI (${MACHINE})"

# The Access scripts read these names from the process env, not the env files.
log "gating ${HOST} with Cloudflare Access"
MAGICIAN_CONNECT_HOST="$HOST" \
  MAGICIAN_CONNECT_ACCESS_APP_NAME="Magican device connection (${MACHINE})" \
  MAGICIAN_CONNECT_ACCESS_POLICY_NAME="magican device service token (${MACHINE})" \
  MAGICIAN_CONNECT_ACCESS_EMAIL_POLICY_NAME="magican device browser emails (${MACHINE})" \
  MAGICIAN_CONNECT_ACCESS_TOKEN_NAME="magican-device-${MACHINE}" \
  MAGICIAN_MOBILE_ENROLLMENT_ACCESS_APP_NAME="Magican mobile one-time enrollment (${MACHINE})" \
  bash "$ROOT_DIR/scripts/ensure-connect-access.sh"
# ensure-connect-access.sh writes the new service-token credential only to
# .env.development. Magician hands that credential to phones at pairing, and a
# stale copy left in .env (the previous host's token) was what it handed out:
# the new host's Access app rejected it and every phone pairing failed its
# verification. Keep every live env file on the same credential.
for key in CF_ACCESS_CLIENT_ID CF_ACCESS_CLIENT_SECRET; do
  value="$(grep -E "^[[:space:]]*${key}=" "$DATA_DIR/.env.development" 2>/dev/null | tail -1 | sed -E "s/^[[:space:]]*${key}=//")"
  [ -n "$value" ] && [ -f "$DATA_DIR/.env" ] && grep -qE "^[[:space:]]*${key}=" "$DATA_DIR/.env" \
    && upsert_env_key "$key" "$value"
done
log "gating ${UI_HOST} with Cloudflare Access"
MAGICIAN_UI_HOST="$UI_HOST" MAGICIAN_UI_ACCESS_APP_NAME="Magician Dev UI (${MACHINE})" \
  bash "$ROOT_DIR/scripts/ensure-ui-access.sh"
log "done. Restart Magician so it advertises https://${HOST} (make restart-magician) and re-pair phones. The Kapso and AgentMail bots register https://${WEBHOOK_HOST} on start (Kapso also retires other machines' hooks for its number)."
