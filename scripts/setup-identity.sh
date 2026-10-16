#!/usr/bin/env bash
# setup-identity.sh — the CLI writer of the operator identity layer (the
# Skills page's Environment tab writes the same .env keys through
# /api/magician/v2/runtime/env).
#
# Prompts (or takes env answers), validates, and writes identity values into
# the untracked layer: the runtime data root's .env (0600) and the
# operator-config instance (skillshub/operator-config.yaml, gitignored).
# Keys: MAGICIAN_OWNER_NAME, MAGICIAN_PRINCIPAL, MAGICIAN_AGENT_EMAIL,
# MAGICIAN_AGENT_WHATSAPP_JID, MAGICIAN_INGRESS_MODE, MAGICIAN_TUNNEL_ZONE,
# plus the gmail channel-provider `domains:` list in operator-config.
#
# Non-clobber: only the named keys are replaced; every other .env line is
# preserved. Re-running is safe — current values become the prompt defaults.
# Non-interactive: pre-set any of the vars in the environment and/or run with
# MAGICIAN_SETUP_IDENTITY_YES=1 (implied when stdin is not a TTY); prompts are
# skipped and defaults/env answers are used.
#
# Design: docs/plans/2026-09-01-github-go-live.md §4 (A2 defines the keys,
# A3 defines this writer). The setup wizard's identity step wraps this script.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA_DIR="${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}"
ENV_FILE="$DATA_DIR/.env"
OPCONF="${MAGICIAN_OPERATOR_CONFIG:-$REPO/skillshub/operator-config.yaml}"
ASSUME_YES="${MAGICIAN_SETUP_IDENTITY_YES:-0}"
[ -t 0 ] || ASSUME_YES=1

log()  { printf '\033[1;36m==> %s\033[0m\n' "$*"; }
warn() { printf '\033[1;33mWARN: %s\033[0m\n' "$*" >&2; }
die()  { printf '\033[1;31mERROR: %s\033[0m\n' "$*" >&2; exit 1; }

env_get() { # env_get KEY -> current value from $ENV_FILE (quotes stripped)
  local line
  line="$(grep -E "^$1=" "$ENV_FILE" 2>/dev/null | tail -1 || true)"
  line="${line#"$1"=}"
  line="${line#\"}"
  printf '%s' "${line%\"}"
}

env_put() { # env_put KEY VALUE — replace-or-append; preserves all other lines
  local tmp
  tmp="$(mktemp)"
  grep -v "^$1=" "$ENV_FILE" > "$tmp" 2>/dev/null || true
  printf '%s="%s"\n' "$1" "$2" >> "$tmp"
  mv "$tmp" "$ENV_FILE"
  chmod 600 "$ENV_FILE"
}

ask() { # ask VAR "question" "fallback"  (process env > existing .env > prompt)
  local __v="$1" q="$2" d="$3" cur ans
  cur="$(printenv "$__v" 2>/dev/null || true)"
  if [ -z "$cur" ]; then cur="$(env_get "$__v")"; fi
  if [ -n "$cur" ]; then d="$cur"; fi
  if [ "$ASSUME_YES" = 1 ]; then printf -v "$__v" '%s' "$d"; return 0; fi
  read -r -p "$q [$d]: " ans || true
  printf -v "$__v" '%s' "${ans:-$d}"
}

mkdir -p "$DATA_DIR"
touch "$ENV_FILE"
chmod 600 "$ENV_FILE"

log "Operator identity layer — writes $ENV_FILE (values never enter the repo)"

ask MAGICIAN_OWNER_NAME        "Your name (how the agent addresses you)" ""
ask MAGICIAN_PRINCIPAL         "Principal / scope name" "$(id -un 2>/dev/null || echo owner)"
ask MAGICIAN_AGENT_EMAIL       "The AGENT's own inbox (AgentMail; blank = none)" ""
ask MAGICIAN_AGENT_WHATSAPP_JID "The AGENT's own WhatsApp number, digits only (blank = none)" ""
ask MAGICIAN_INGRESS_MODE      "Public ingress mode (local | quick | named)" "local"
ask MAGICIAN_GWS_DOMAINS       "Google Workspace mail domains, comma-separated" "gmail.com"

# --- validation (fail loudly, write nothing on a bad answer) ----------------
case "$MAGICIAN_INGRESS_MODE" in
  local|quick|named) ;;
  *) die "invalid MAGICIAN_INGRESS_MODE '$MAGICIAN_INGRESS_MODE' (expected: local | quick | named)";;
esac
if [ -n "$MAGICIAN_AGENT_EMAIL" ] && ! printf '%s' "$MAGICIAN_AGENT_EMAIL" | grep -Eq '^[^@[:space:]]+@[^@[:space:]]+\.[^@[:space:]]+$'; then
  die "MAGICIAN_AGENT_EMAIL '$MAGICIAN_AGENT_EMAIL' is not a valid email address"
fi
if [ -n "$MAGICIAN_AGENT_WHATSAPP_JID" ] && ! printf '%s' "$MAGICIAN_AGENT_WHATSAPP_JID" | grep -Eq '^[0-9]{8,15}$'; then
  die "MAGICIAN_AGENT_WHATSAPP_JID '$MAGICIAN_AGENT_WHATSAPP_JID' must be 8-15 digits (no +, no @suffix)"
fi
MAGICIAN_TUNNEL_ZONE="${MAGICIAN_TUNNEL_ZONE:-$(env_get MAGICIAN_TUNNEL_ZONE)}"
if [ "$MAGICIAN_INGRESS_MODE" = named ]; then
  ask MAGICIAN_TUNNEL_ZONE "Your Cloudflare zone (e.g. example.com)" ""
  [ -n "$MAGICIAN_TUNNEL_ZONE" ] || die "MAGICIAN_INGRESS_MODE=named requires MAGICIAN_TUNNEL_ZONE (no script defaults a zone)"
fi
if [ -n "$MAGICIAN_TUNNEL_ZONE" ] && ! printf '%s' "$MAGICIAN_TUNNEL_ZONE" | grep -Eq '^[a-z0-9][a-z0-9.-]*\.[a-z]{2,}$'; then
  die "MAGICIAN_TUNNEL_ZONE '$MAGICIAN_TUNNEL_ZONE' is not a bare domain (expected e.g. example.com)"
fi

# --- write .env -------------------------------------------------------------
WRITTEN=""
put_nonempty() { # put_nonempty KEY VALUE
  if [ -n "$2" ]; then
    env_put "$1" "$2"
    WRITTEN="$WRITTEN $1"
  fi
}
put_nonempty MAGICIAN_OWNER_NAME        "$MAGICIAN_OWNER_NAME"
put_nonempty MAGICIAN_PRINCIPAL         "$MAGICIAN_PRINCIPAL"
put_nonempty MAGICIAN_AGENT_EMAIL       "$MAGICIAN_AGENT_EMAIL"
put_nonempty MAGICIAN_AGENT_WHATSAPP_JID "$MAGICIAN_AGENT_WHATSAPP_JID"
put_nonempty MAGICIAN_INGRESS_MODE      "$MAGICIAN_INGRESS_MODE"
put_nonempty MAGICIAN_TUNNEL_ZONE       "$MAGICIAN_TUNNEL_ZONE"

# --- operator-config: gmail provider domains --------------------------------
# The domains list is operator-config, not env. Only the shipped template
# default line ([gmail.com]) is replaced; any other list counts as
# operator-customized and is left alone.
TEMPLATE_DOMAINS_RE='^([[:space:]]*)domains:[[:space:]]*\[gmail\.com\][[:space:]]*$'
if [ -f "$OPCONF" ]; then
  yaml_list="[$(printf '%s' "$MAGICIAN_GWS_DOMAINS" | sed -e 's/[[:space:]]//g' -e 's/,/, /g')]"
  if grep -Eq "$TEMPLATE_DOMAINS_RE" "$OPCONF"; then
    tmp="$(mktemp)"
    awk -v repl="$yaml_list" '
      /^[[:space:]]*domains:[[:space:]]*\[gmail\.com\][[:space:]]*$/ {
        match($0, /^[[:space:]]*/)
        printf "%sdomains: %s\n", substr($0, 1, RLENGTH), repl
        next
      }
      { print }
    ' "$OPCONF" > "$tmp"
    mv "$tmp" "$OPCONF"
    log "operator-config gmail domains -> $yaml_list"
    WRITTEN="$WRITTEN gmail.domains"
  else
    warn "operator-config gmail domains already customized — left as-is ($OPCONF)"
  fi
else
  warn "no operator-config instance at $OPCONF — domains step skipped (copy the template first)"
fi

log "Identity layer written:${WRITTEN:- (nothing — all answers empty)}"
printf '   %s\n' "$ENV_FILE"
