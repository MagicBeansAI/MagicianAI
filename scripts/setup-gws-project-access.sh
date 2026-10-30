#!/usr/bin/env bash
# setup-gws-project-access.sh — give every Google Workspace account the roles it
# needs on the Cloud project behind client_secret.json: Service Usage Consumer
# (every API call bills to that project) and Pub/Sub Editor (the Gmail bot's
# watch topic and subscription). Without them mail sync fails with "Caller does
# not have required permission to use project" and the Gmail bot restarts
# forever. The project owner's own account needs neither.
#
# Checks with `magician.bin google-project-access` (the same check Service
# accounts and Desktop onboarding run). When this computer's gcloud account can
# set the project policy, offers to grant the missing roles; otherwise prints
# the gcloud commands for a project owner to run.
#
# Accounts: every expected_email in $MAGICIAN_ROOT_DIR/operator-config.yaml, or
# the --email values passed through. Non-interactive (no TTY, or
# MAGICIAN_SETUP_GWS_PROJECT_ACCESS_YES=1): grants without asking.
#
# Called by `make setup-gws-project-access` (the setup wizard's
# workspace-project-access step) and by skillshub's setup-gws-accounts.
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${MAGICIAN_BIN:-$REPO/magician.bin}"
if [ ! -x "$BIN" ]; then
  for candidate in "$REPO/target/debug/magician" "$REPO/target/release/magician"; do
    if [ -x "$candidate" ]; then BIN="$candidate"; break; fi
  done
fi
if [ ! -x "$BIN" ]; then
  echo "  magician binary not found (looked for $REPO/magician.bin); build or install Magician first" >&2
  exit 2
fi

run() { MAGICIAN_SKIP_KEYCHAIN=1 "$BIN" --log-level error google-project-access "$@"; }

run "$@"
status=$?
# 3 = a role is missing, 4 = the policy could not be read, 0 = all granted.
if [ "$status" -ne 3 ]; then
  exit "$status"
fi

grantable="$(run --json "$@" 2>/dev/null | python3 -c '
import json, sys
try:
    reports = json.load(sys.stdin)
except Exception:
    sys.exit(0)
accounts = {r["grant_account"] for r in reports if r.get("status") == "missing" and r.get("grant_account")}
print(" ".join(sorted(accounts)))
')"
if [ -z "$grantable" ]; then
  echo
  echo "  This computer's gcloud account cannot change the project policy."
  echo "  Ask a project owner to run the commands above, then re-run this step."
  exit 3
fi

if [ -t 0 ] && [ "${MAGICIAN_SETUP_GWS_PROJECT_ACCESS_YES:-}" != "1" ]; then
  printf '\n  Grant the missing roles now as %s? [Y/n] ' "$grantable"
  read -r answer || answer=""
  case "$answer" in
    n|N|no|NO) echo "  Not granted. Run the commands above when ready."; exit 3 ;;
  esac
fi
run --grant "$@"
