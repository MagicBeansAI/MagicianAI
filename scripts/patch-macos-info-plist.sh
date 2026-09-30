#!/usr/bin/env bash
# Legacy repair helper: inject NSMicrophoneUsageDescription /
# NSCameraUsageDescription into an already-built .app and re-sign it when asked.
#
# Current Tauri automatically merges desktop/src-tauri/Info.plist before it
# signs and packages the app. Production builds therefore MUST NOT call this
# script after `tauri build`: doing so would mutate only the unpacked .app after
# the DMG and updater archive were already created. Keep this helper only for
# repairing older/manual bundles.
#
# Dev builds embed the same Info.plist into the MachO `__TEXT,__info_plist`
# section via `desktop/src-tauri/build.rs`; this script handles the prod
# .app bundle path.
#
# Usage:
#   scripts/patch-macos-info-plist.sh [optional explicit .app path]
#   APPLE_SIGNING_IDENTITY=...   # optional; if set the .app is re-signed
#   TAURI_TARGET=aarch64-apple-darwin   # optional; constrains search

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Build a search list. CI passes an explicit path; release-desktop in the
# Makefile uses default target dirs.
candidates=()
if [[ $# -ge 1 && -n "${1:-}" ]]; then
  candidates+=("$1")
fi
if [[ -n "${TAURI_TARGET:-}" ]]; then
  candidates+=(
    "${CARGO_TARGET_DIR:-$REPO_ROOT/desktop/src-tauri/target}/${TAURI_TARGET}/release/bundle/macos"
    "$REPO_ROOT/desktop/src-tauri/target/${TAURI_TARGET}/release/bundle/macos"
  )
fi
candidates+=(
  "${CARGO_TARGET_DIR:-$REPO_ROOT/desktop/src-tauri/target}/release/bundle/macos"
  "$REPO_ROOT/desktop/src-tauri/target/release/bundle/macos"
  "/Volumes/build/magician/builds/release/bundle/macos"
)

APP_PATH=""
for dir in "${candidates[@]}"; do
  if [[ -d "$dir" ]]; then
    found="$(find "$dir" -maxdepth 2 -name "*.app" -type d 2>/dev/null | head -1)"
    if [[ -n "$found" ]]; then
      APP_PATH="$found"
      break
    fi
  elif [[ -d "${dir%/}" && "${dir%.app}" != "$dir" ]]; then
    # Direct .app path passed
    APP_PATH="$dir"
    break
  fi
done

if [[ -z "$APP_PATH" ]]; then
  echo "⚠️  No .app bundle found in any candidate dir; skipping plist patch." >&2
  exit 0
fi

INFO_PLIST="$APP_PATH/Contents/Info.plist"
if [[ ! -f "$INFO_PLIST" ]]; then
  echo "⚠️  $INFO_PLIST does not exist; skipping plist patch." >&2
  exit 0
fi

echo "  → Patching $INFO_PLIST"

# `plutil -insert` fails if the key already exists; use `-replace` to be
# idempotent (no error if the key was added by a previous run).
plutil -replace NSMicrophoneUsageDescription \
  -string "Magician records voice notes and supports live voice calls in the chat composer." \
  "$INFO_PLIST"

plutil -replace NSCameraUsageDescription \
  -string "Magician uses the camera for attachment capture in the chat composer." \
  "$INFO_PLIST"

# Verify the result parses.
plutil -lint "$INFO_PLIST" >/dev/null
echo "  ✓ Plist keys injected and lint passed."

# Re-sign the .app if signing was used during the original build.
# Modifying Info.plist invalidates the existing codesign signature; without
# re-signing, macOS Gatekeeper / notarization will reject the bundle.
if [[ -n "${APPLE_SIGNING_IDENTITY:-}" ]]; then
  echo "  → Re-signing $APP_PATH with identity $APPLE_SIGNING_IDENTITY..."
  ENTITLEMENTS="$REPO_ROOT/desktop/src-tauri/Magician.entitlements"
  if [[ -f "$ENTITLEMENTS" ]]; then
    codesign --force --deep --options runtime \
      --entitlements "$ENTITLEMENTS" \
      --sign "$APPLE_SIGNING_IDENTITY" \
      "$APP_PATH"
  else
    codesign --force --deep --options runtime \
      --sign "$APPLE_SIGNING_IDENTITY" \
      "$APP_PATH"
  fi
  # Quick verification — full notarization is run separately by the
  # release workflow.
  codesign --verify --verbose=2 "$APP_PATH" 2>&1 | head -5 || true
  echo "  ✓ Re-signed."
else
  echo "  (APPLE_SIGNING_IDENTITY not set; skipping re-sign — fine for unsigned local dev bundles.)"
fi
