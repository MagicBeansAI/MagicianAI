#!/usr/bin/env bash
#
# Local Update Server for Magician Desktop
#
# Serves a Tauri update manifest (latest.json) and signed app artifacts
# from a local directory, enabling end-to-end update testing without
# GitHub Releases or CI.
#
# Usage:
#   scripts/local-update-server.sh [port]
#
# Default port: 8432
#
# Prerequisites:
#   1. Generate signing keypair:  make generate-signing-key
#   2. Build the app:             make dev-desktop-build
#   3. Run this server:           scripts/local-update-server.sh
#   4. Build a new version:       (bump version, rebuild, re-run server)
#
# The server auto-generates latest.json from whatever .tar.gz.sig artifacts
# exist in CARGO_TARGET_DIR/release/bundle/macos. The old desktop-local target
# directory is checked as a fallback for artifacts built before the shared
# target dir default was introduced.

set -euo pipefail

PORT="${1:-8432}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/Volumes/build/magician/builds}"
SERVE_DIR="$PROJECT_ROOT/.local-update-server"

mkdir -p "$SERVE_DIR"

# ── Detect platform and find artifacts ────────────────────────────────────

ARCH=$(uname -m)
OS=$(uname -s)

case "$OS-$ARCH" in
  Darwin-arm64)  PLATFORM="darwin-aarch64" ;;
  Darwin-x86_64) PLATFORM="darwin-x86_64" ;;
  Linux-x86_64)  PLATFORM="linux-x86_64" ;;
  *)
    echo "Unsupported platform: $OS-$ARCH"
    exit 1
    ;;
esac

echo "Platform: $PLATFORM"

# Find the .tar.gz updater artifact + its .sig
BUNDLE_DIRS=(
  "$CARGO_TARGET_DIR/release/bundle"
  "$PROJECT_ROOT/desktop/src-tauri/target/release/bundle"
)

for candidate in "${BUNDLE_DIRS[@]}"; do
  if [[ "$OS" == "Darwin" ]]; then
    UPDATER_ARTIFACT=$(find "$candidate/macos" -name "*.app.tar.gz" 2>/dev/null | head -1)
  elif [[ "$OS" == "Linux" ]]; then
    UPDATER_ARTIFACT=$(find "$candidate/appimage" -name "*.AppImage.tar.gz" 2>/dev/null | head -1)
  fi
  if [[ -n "${UPDATER_ARTIFACT:-}" ]]; then
    BUNDLE_DIR="$candidate"
    SIG_FILE="${UPDATER_ARTIFACT}.sig"
    break
  fi
done

if [[ -z "${UPDATER_ARTIFACT:-}" || ! -f "$UPDATER_ARTIFACT" ]]; then
  echo ""
  echo "ERROR: No updater artifact found in:"
  printf '  %s\n' "${BUNDLE_DIRS[@]}"
  echo ""
  echo "Build the app first:"
  echo "  make dev-desktop-build"
  echo ""
  echo "Or with explicit signing key:"
  echo "  TAURI_SIGNING_PRIVATE_KEY=\$(cat ~/.tauri/magician.key) make release-desktop"
  exit 1
fi

if [[ ! -f "$SIG_FILE" ]]; then
  echo ""
  echo "ERROR: Signature file not found: $SIG_FILE"
  echo ""
  echo "Make sure TAURI_SIGNING_PRIVATE_KEY is set when building:"
  echo "  TAURI_SIGNING_PRIVATE_KEY=\$(cat ~/.tauri/magician.key) make release-desktop"
  exit 1
fi

# Read the signature
SIGNATURE=$(cat "$SIG_FILE")

# Get version from tauri.conf.json
VERSION=$(python3 -c "import json; print(json.load(open('$PROJECT_ROOT/desktop/src-tauri/tauri.conf.json'))['version'])")

# Copy artifact to serve directory
ARTIFACT_NAME=$(basename "$UPDATER_ARTIFACT")
cp "$UPDATER_ARTIFACT" "$SERVE_DIR/$ARTIFACT_NAME"

# Generate latest.json
ARTIFACT_URL="http://localhost:${PORT}/${ARTIFACT_NAME}"
PUB_DATE=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

cat > "$SERVE_DIR/latest.json" <<MANIFEST
{
  "version": "${VERSION}",
  "notes": "Local dev build ${VERSION}",
  "pub_date": "${PUB_DATE}",
  "platforms": {
    "${PLATFORM}": {
      "signature": "${SIGNATURE}",
      "url": "${ARTIFACT_URL}"
    }
  }
}
MANIFEST

echo ""
echo "=== Local Update Server ==="
echo "  Version:   ${VERSION}"
echo "  Platform:  ${PLATFORM}"
echo "  Artifact:  ${ARTIFACT_NAME}"
echo "  Manifest:  http://localhost:${PORT}/latest.json"
echo ""
echo "  The installed app (built with 'make dev-desktop-build') is"
echo "  already configured to check this endpoint for updates."
echo ""
echo "  To trigger an update:"
echo "    1. Bump version in desktop/src-tauri/tauri.conf.json"
echo "    2. make dev-desktop-build"
echo "    3. Re-run: make dev-update-server"
echo "    4. In the app tray menu: Check for Updates"
echo ""
echo "  Serving on port ${PORT}..."
echo "  Press Ctrl+C to stop."
echo ""

# Serve the directory
cd "$SERVE_DIR"
python3 -m http.server "$PORT"
