#!/bin/sh
# Print a Magician bearer token on stdout (nothing else), for curl and evals.
#
#   export MAGICIAN_BEARER_TOKEN="$(scripts/magician-token.sh)"          # session (mag_…)
#   export MAGICIAN_BEARER_TOKEN="$(scripts/magician-token.sh --pat my-label)"  # PAT (mag_pat_…)
#
# Credentials: MAGICIAN_EVAL_USERNAME / MAGICIAN_EVAL_PASSWORD from the
# environment, else from $MAGICIAN_ROOT_DIR/.env.development then .env
# (default root ~/MagicianNotes). API base: MAGICIAN_API (default
# http://127.0.0.1:3002). A PAT is shown once by the server; store it yourself.
set -eu

API="${MAGICIAN_API:-http://127.0.0.1:3002}"
ROOT="${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}"
PAT_LABEL=""
if [ "${1:-}" = "--pat" ]; then
  PAT_LABEL="${2:?usage: $0 [--pat <label>]}"
fi

read_env() {
  # First definition wins: environment, then .env.development, then .env.
  name="$1"
  eval "value=\${$name:-}"
  if [ -z "$value" ]; then
    for file in "$ROOT/.env.development" "$ROOT/.env"; do
      [ -f "$file" ] || continue
      value=$(sed -n "s/^$name=//p" "$file" | tail -n 1 | sed -e 's/^"//' -e 's/"$//' -e "s/^'//" -e "s/'$//")
      [ -n "$value" ] && break
    done
  fi
  printf '%s' "$value"
}

USERNAME=$(read_env MAGICIAN_EVAL_USERNAME)
PASSWORD=$(read_env MAGICIAN_EVAL_PASSWORD)
if [ -z "$USERNAME" ] || [ -z "$PASSWORD" ]; then
  echo "magician-token: set MAGICIAN_EVAL_USERNAME and MAGICIAN_EVAL_PASSWORD (env or $ROOT/.env.development)" >&2
  exit 2
fi

body=$(USERNAME="$USERNAME" PASSWORD="$PASSWORD" python3 -c \
  'import json,os; print(json.dumps({"username": os.environ["USERNAME"], "password": os.environ["PASSWORD"]}))')
login=$(curl -sS -f -X POST "$API/api/magician/v2/auth/login" \
  -H "Content-Type: application/json" --data "$body") || {
  echo "magician-token: login failed at $API (is Magician up? check the credentials)" >&2
  exit 1
}
session=$(printf '%s' "$login" | python3 -c 'import json,sys; print(json.load(sys.stdin)["token"])')

if [ -z "$PAT_LABEL" ]; then
  printf '%s\n' "$session"
  exit 0
fi

pat_body=$(LABEL="$PAT_LABEL" python3 -c 'import json,os; print(json.dumps({"label": os.environ["LABEL"]}))')
curl -sS -f -X POST "$API/api/magician/v2/auth/tokens" \
  -H "Authorization: Bearer $session" -H "Content-Type: application/json" \
  --data "$pat_body" | python3 -c 'import json,sys; print(json.load(sys.stdin)["token"])'
