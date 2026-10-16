#!/usr/bin/env bash
# setup-pi-coding-agent.sh - install the Pi CLI used by run_coding_task.
# Idempotent: a matching reviewed `pi` is retained; a missing or mismatched
# executable is reconciled to the exact version required by the Rust RPC gate.
set -euo pipefail

PI_PACKAGE="${MAGICIAN_PI_PACKAGE:-@earendil-works/pi-coding-agent}"
PI_VERSION="${MAGICIAN_PI_VERSION:-0.87.1}"
PI_SPEC="${PI_PACKAGE}@${PI_VERSION}"
FORCE_INSTALL="${MAGICIAN_PI_FORCE_INSTALL:-0}"
MIN_NODE_VERSION="22.19.0"

log() { printf '==> %s\n' "$*"; }
ok() { printf '  OK %s\n' "$*"; }
warn() { printf '  WARN %s\n' "$*" >&2; }
die() {
  printf '  ERROR %s\n' "$*" >&2
  exit 1
}

pi_version_line() {
  pi --version 2>/dev/null | head -1 || true
}

pi_version_matches() {
  local version_line="$1"
  node -e '
const match = process.argv[1].match(/(?:^|[^0-9])v?([0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?)(?=$|[^0-9])/);
process.exit(match && match[1] === process.argv[2] ? 0 : 1);
' "$version_line" "$PI_VERSION"
}

log "Checking Pi coding agent CLI"

if ! command -v npm >/dev/null 2>&1 || ! command -v node >/dev/null 2>&1; then
  die "Node.js and npm are required to install $PI_SPEC. Run 'make setup-prerequisites' first."
fi

if ! node -e '
const actual = process.versions.node.split(".").map(Number);
const required = process.argv[1].split(".").map(Number);
for (let i = 0; i < 3; i += 1) {
  if (actual[i] > required[i]) process.exit(0);
  if (actual[i] < required[i]) process.exit(1);
}
' "$MIN_NODE_VERSION"; then
  die "Node.js >= $MIN_NODE_VERSION is required by $PI_SPEC (found $(node --version)). Run 'cd skillshub && nvm install && nvm use'."
fi

existing_pi="$(command -v pi || true)"
if [[ -n "$existing_pi" && "$FORCE_INSTALL" != "1" ]]; then
  existing_version="$(pi_version_line)"
  if pi_version_matches "$existing_version"; then
    ok "pi already installed at $existing_pi ($existing_version)"
    exit 0
  fi
  warn "pi at $existing_pi does not match the reviewed $PI_VERSION RPC contract"
  if [[ -n "$existing_version" ]]; then
    warn "detected version: $existing_version"
  else
    warn "could not read version with 'pi --version'"
  fi
  warn "replacing it with $PI_SPEC so setup and runtime cannot drift"
fi

if [[ -n "$existing_pi" && "$FORCE_INSTALL" == "1" ]]; then
  warn "MAGICIAN_PI_FORCE_INSTALL=1 set; replacing existing pi at $existing_pi"
fi

log "Installing $PI_SPEC with npm"
if ! npm install -g --ignore-scripts "$PI_SPEC"; then
  die "npm install failed for $PI_SPEC"
fi

installed_pi="$(command -v pi || true)"
if [[ -z "$installed_pi" ]]; then
  npm_prefix="$(npm prefix -g 2>/dev/null || true)"
  if [[ -n "$npm_prefix" ]]; then
    die "installed $PI_SPEC, but 'pi' is not on PATH. Add '$npm_prefix/bin' to PATH."
  fi
  die "installed $PI_SPEC, but 'pi' is not on PATH."
fi

installed_version="$(pi_version_line)"
if ! pi_version_matches "$installed_version"; then
  warn "pi installed at $installed_pi, but version output did not confirm $PI_VERSION"
  if [[ -n "$installed_version" ]]; then
    warn "version output: $installed_version"
  fi
  die "installed Pi does not satisfy the reviewed $PI_VERSION RPC contract"
else
  ok "pi installed at $installed_pi ($installed_version)"
fi

cat <<EOF

Pi is ready for Magician.
No Pi login is needed: Magician launches Pi with the selected LLM profile and
passes that provider's API key from the runtime .env files. 'pi' + '/login' is
only for Pi's own-credentials mode on a subscription account.
EOF
