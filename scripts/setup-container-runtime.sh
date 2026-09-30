#!/bin/bash
# setup-container-runtime.sh — bootstrap the HOST container runtime for Magician.
#
# Customer-grade, idempotent host bootstrap. Mirrors the desktop app's first-run
# setup (desktop/src-tauri/src/container/{detect,apple,docker}.rs) so the SAME
# runtime decision happens whether the app auto-runs setup or a customer/operator
# runs this script directly:
#     macOS >= 26 on Apple Silicon   -> native Apple `container` runtime
#     everything else (pre-26/Intel) -> Docker via Colima
#
# Usage:
#   scripts/setup-container-runtime.sh            # detect + install + start + verify
#   scripts/setup-container-runtime.sh --check    # report state ONLY, install nothing
#
# NOTE: the Apple `container system start` step may prompt for admin the first
# time (it installs a kernel/network helper). Run this in a real terminal (or via
# `! scripts/setup-container-runtime.sh` in the agent) so the prompt can be answered.
#
# See: docs/components/desktop/distribution-decision.md
#      docs/runbooks/2026-06-22-container-tauri-browser-local-e2e.md (Tier 1)
set -uo pipefail

CHECK_ONLY=0
[ "${1:-}" = "--check" ] && CHECK_ONLY=1

log()  { printf '  %s\n' "$*"; }
step() { printf '\n==> %s\n' "$*"; }
fail() { printf '  \xe2\x9c\x97 %s\n' "$*" >&2; exit 1; }

# ---- detect (mirrors detect.rs::is_apple_container_eligible) ----
OS="$(uname -s)"
ARCH="$(uname -m)"
MAC_MAJOR=0
if [ "$OS" = "Darwin" ]; then
  MAC_MAJOR="$(sw_vers -productVersion 2>/dev/null | cut -d. -f1)"
fi
case "$MAC_MAJOR" in ''|*[!0-9]*) MAC_MAJOR=0;; esac

USE_APPLE=0
if [ "$OS" = "Darwin" ] && [ "$ARCH" = "arm64" ] && [ "$MAC_MAJOR" -ge 26 ]; then
  USE_APPLE=1
fi

# Explicit override: MAGICIAN_CONTAINER_RUNTIME forces the runtime regardless of the
# auto-detect above, so an operator who chose `docker` on an Apple-eligible Mac
# bootstraps Docker/Colima (not the Apple `container` runtime). install.sh passes
# the chosen RUNTIME through this env. Empty/unset -> keep the auto-detect.
RUNTIME_OVERRIDE="${MAGICIAN_CONTAINER_RUNTIME:-}"
case "$RUNTIME_OVERRIDE" in
  apple-container) USE_APPLE=1 ;;
  docker)          USE_APPLE=0 ;;
  "")              ;;  # no override — keep auto-detect
  *) fail "invalid MAGICIAN_CONTAINER_RUNTIME '$RUNTIME_OVERRIDE' (expected: docker | apple-container)" ;;
esac

step "Detected: OS=$OS arch=$ARCH macOS-major=$MAC_MAJOR"
if [ -n "$RUNTIME_OVERRIDE" ]; then
  log "(override) MAGICIAN_CONTAINER_RUNTIME=$RUNTIME_OVERRIDE"
fi
if [ "$USE_APPLE" = "1" ]; then
  log "-> runtime: Apple native 'container' (macOS ${MAC_MAJOR}+ Apple Silicon)"
else
  log "-> runtime: Docker via Colima (pre-26 / Intel / non-macOS)"
fi

ensure_brew() {
  if command -v brew >/dev/null 2>&1; then log "ok brew"; return 0; fi
  if [ "$CHECK_ONLY" = "1" ]; then log "missing: brew"; return 1; fi
  log "Installing Homebrew..."
  NONINTERACTIVE=1 /bin/bash -c \
    "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)" \
    || fail "Homebrew install failed"
  [ -x /opt/homebrew/bin/brew ] && eval "$(/opt/homebrew/bin/brew shellenv)"
}

apple_host_services_ready() {
  container system dns list --quiet 2>/dev/null \
    | grep -Fxq "host.container.internal" || return 1
  python3 "$(dirname "${BASH_SOURCE[0]}")/check-apple-container-host.py" --quiet
}

ensure_apple_host_services() {
  local container_cli
  if apple_host_services_ready; then
    log "ok Apple Container host services: host.container.internal"
    return 0
  fi
  if [ "$CHECK_ONLY" = "1" ]; then
    log "missing or misconfigured: Apple Container localhost forwarding (host.container.internal)"
    return 1
  fi
  container_cli="$(command -v container)"
  log "Configuring host.container.internal for host Ollama and desktop services..."
  log "note: Apple Container localhost forwarding disables iCloud Private Relay while active"
  if container system dns list --quiet 2>/dev/null | grep -Fxq "host.container.internal"; then
    log "Repairing the existing non-forwarding host.container.internal entry..."
    sudo "$container_cli" system dns delete host.container.internal \
      || fail "could not remove the misconfigured host-service domain"
  fi
  sudo "$container_cli" system dns create --localhost 203.0.113.113 host.container.internal \
    || fail "could not configure Apple Container localhost forwarding"
  apple_host_services_ready \
    || fail "Apple Container did not retain host.container.internal"
}

verify_apple() {
  command -v container >/dev/null 2>&1 || { log "missing: container CLI"; return 1; }
  log "ok container CLI: $(container --version 2>&1 | head -1)"
  if container system status >/dev/null 2>&1; then
    log "ok container system: running"
  else
    log "missing: container system not running (run: container system start --enable-kernel-install)"
    return 1
  fi
  if container build --help >/dev/null 2>&1; then
    log "ok container build: available (local image builds work, no registry/publish needed)"
  else
    log "warn container build NOT in this version -> use 'container image load' of a tarball, or a registry"
  fi
  apple_host_services_ready || {
    log "missing or misconfigured: Apple Container localhost forwarding (host.container.internal)"
    return 1
  }
  return 0
}

verify_docker() {
  command -v docker >/dev/null 2>&1 || { log "missing: docker CLI"; return 1; }
  if docker info >/dev/null 2>&1; then
    log "ok docker daemon reachable ($(docker --version 2>&1))"
  else
    log "missing: docker daemon not reachable (is colima started?)"
    return 1
  fi
  return 0
}

# ---- check-only: report and exit ----
if [ "$CHECK_ONLY" = "1" ]; then
  step "Check only (no install)"
  if [ "$USE_APPLE" = "1" ]; then verify_apple; else verify_docker; fi
  exit $?
fi

# ---- install + start ----
if [ "$USE_APPLE" = "1" ]; then
  step "Apple 'container' runtime"
  if ! command -v container >/dev/null 2>&1; then
    ensure_brew || fail "Homebrew is required to install the container CLI"
    log "Installing the Apple container CLI (brew install container)..."
    if ! brew install container; then
      if ! brew install --cask container; then
        fail "Could not install via Homebrew. Install the signed package from https://github.com/apple/container/releases then re-run."
      fi
    fi
  else
    log "ok container CLI already installed"
  fi
  if ! container system status >/dev/null 2>&1; then
    log "Starting the container system service (may prompt for admin -- answer the prompt)..."
    container system start --enable-kernel-install \
      || fail "'container system start' failed (admin approval may be required; re-run in a terminal)"
  fi
  ensure_apple_host_services
  step "Verify"
  verify_apple || fail "Apple container runtime not healthy after setup"
else
  step "Docker via Colima"
  if ! command -v docker >/dev/null 2>&1 || ! command -v colima >/dev/null 2>&1; then
    ensure_brew || fail "Homebrew is required to install Colima + Docker"
    log "Installing Colima + Docker CLI (brew install colima docker)..."
    brew install colima docker || fail "brew install colima docker failed"
  else
    log "ok colima + docker already installed"
  fi
  if ! colima status >/dev/null 2>&1; then
    log "Starting Colima VM (--cpu 2 --memory 4)..."
    colima start --cpu 2 --memory 4 || fail "colima start failed"
  fi
  step "Verify"
  verify_docker || fail "Docker runtime not healthy after setup"
fi

step "Container runtime ready."
if [ "$USE_APPLE" = "1" ]; then
  log "Next (no publish needed): build the image into the native local store, then launch the app:"
  log "  container build -t ghcr.io/magicbeanbs100x/magician:latest ."
  log "  container image inspect ghcr.io/magicbeanbs100x/magician:latest   # confirms it's local; the tray skips the pull"
else
  log "Next: build + run via Docker:"
  log "  make dev-container-rebuild"
fi
log "Runbook: docs/runbooks/2026-06-22-container-tauri-browser-local-e2e.md (Tier 1)."
