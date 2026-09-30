#!/usr/bin/env bash
# Fetch libvosk + a small Vosk model for the native desktop wake word
# (`desktop/src-tauri/voice_wake.rs`). Both are large and gitignored; this is the
# one-time setup so the desktop links/bundles cleanly. Idempotent (skips what's
# already present). The canonical model lives under the runtime root so raw dev
# binaries and installed apps share it; a packaging copy is staged separately.
# Override via VOSK_VERSION / VOSK_MODEL_NAME / VOSK_VENDOR_DIR /
# VOSK_MODEL_DIR / MAGICIAN_VOSK_MODEL_DIR.
set -euo pipefail

# 0.3.42 is the last release with a `vosk-osx-*.zip` prebuilt C library, and it's
# a universal binary (x86_64 + arm64) — works natively on Apple Silicon.
VOSK_VERSION="${VOSK_VERSION:-0.3.42}"
VOSK_MODEL_NAME="${VOSK_MODEL_NAME:-vosk-model-small-en-us-0.15}"
VENDOR_DIR="${VOSK_VENDOR_DIR:-}"
MODEL_DIR="${VOSK_MODEL_DIR:-${MAGICIAN_VOSK_MODEL_DIR:-}}"
BUNDLE_MODEL_DIR="${VOSK_BUNDLE_MODEL_DIR:-}"

if [ -z "$VENDOR_DIR" ]; then
  echo "❌ VOSK_VENDOR_DIR is required; run this through make setup-desktop-vosk." >&2
  exit 1
fi
if [ -z "$MODEL_DIR" ]; then
  echo "❌ Vosk runtime model path is not configured; run this through make setup-desktop-vosk" >&2
  echo "   or set MAGICIAN_VOSK_MODEL_DIR / VOSK_MODEL_DIR explicitly." >&2
  exit 1
fi
if [ -z "$BUNDLE_MODEL_DIR" ]; then
  echo "❌ VOSK_BUNDLE_MODEL_DIR is required; run this through make setup-desktop-vosk." >&2
  exit 1
fi

if [ "$(uname -s)" != "Darwin" ]; then
  echo "⚠️  Native desktop wake (libvosk) setup is macOS-only for now — skipping."
  exit 0
fi

mkdir -p "$VENDOR_DIR"
# `-s` (exists AND non-empty): build.rs drops a 0-byte placeholder libvosk.dylib
# so non-wake cargo builds don't fail the framework copy — treat that as "fetch".
if [ ! -s "$VENDOR_DIR/libvosk.dylib" ]; then
  echo "↓ libvosk $VOSK_VERSION"
  curl -fL -o /tmp/vosk-osx.zip \
    "https://github.com/alphacep/vosk-api/releases/download/v${VOSK_VERSION}/vosk-osx-${VOSK_VERSION}.zip"
  rm -rf /tmp/vosk-osx && unzip -q -o /tmp/vosk-osx.zip -d /tmp/vosk-osx
  find /tmp/vosk-osx -name 'libvosk.dylib' -exec cp {} "$VENDOR_DIR/" \;
  find /tmp/vosk-osx -name 'vosk_api.h' -exec cp {} "$VENDOR_DIR/" \;
  # Rewrite the dylib id to @rpath/libvosk.dylib so both the dev rpath (vendor
  # dir) and the bundled .app's Contents/Frameworks rpath resolve it at runtime.
  install_name_tool -id @rpath/libvosk.dylib "$VENDOR_DIR/libvosk.dylib"
  echo "✓ libvosk → $VENDOR_DIR/libvosk.dylib"
else
  echo "✓ libvosk already vendored"
fi

if [ ! -f "$MODEL_DIR/am/final.mdl" ] || [ ! -f "$MODEL_DIR/conf/model.conf" ]; then
  echo "↓ model $VOSK_MODEL_NAME"
  TEMP_DIR="$(mktemp -d /tmp/magican-desktop-vosk.XXXXXX)"
  curl -fL -o "$TEMP_DIR/model.zip" \
    "https://alphacephei.com/vosk/models/${VOSK_MODEL_NAME}.zip"
  unzip -q -o "$TEMP_DIR/model.zip" -d "$TEMP_DIR/unpacked"
  mkdir -p "$(dirname "$MODEL_DIR")"
  if [ -e "$MODEL_DIR" ]; then
    if ! rmdir "$MODEL_DIR" 2>/dev/null; then
      echo "❌ Incomplete Vosk model already exists at $MODEL_DIR" >&2
      echo "   Move it aside, then rerun make setup-desktop-vosk." >&2
      rm -rf "$TEMP_DIR"
      exit 1
    fi
  fi
  mv "$TEMP_DIR/unpacked/${VOSK_MODEL_NAME}" "$MODEL_DIR"
  rm -rf "$TEMP_DIR"
  echo "✓ model → $MODEL_DIR"
else
  echo "✓ vosk model already present at $MODEL_DIR"
fi

# Tauri's bundle resource path is repo-relative and cannot point at a dynamic
# runtime root. Keep a validated staging copy for `.app` packaging, while dev
# execution resolves the canonical runtime-root model directly.
if [ "$MODEL_DIR" != "$BUNDLE_MODEL_DIR" ] \
  && { [ ! -f "$BUNDLE_MODEL_DIR/am/final.mdl" ] \
    || [ ! -f "$BUNDLE_MODEL_DIR/conf/model.conf" ]; }; then
  if [ -e "$BUNDLE_MODEL_DIR" ]; then
    if ! rmdir "$BUNDLE_MODEL_DIR" 2>/dev/null; then
      echo "❌ Incomplete bundle-staging model exists at $BUNDLE_MODEL_DIR" >&2
      echo "   Move it aside, then rerun make setup-desktop-vosk." >&2
      exit 1
    fi
  fi
  mkdir -p "$(dirname "$BUNDLE_MODEL_DIR")"
  cp -R "$MODEL_DIR" "$BUNDLE_MODEL_DIR"
  echo "✓ bundle model staged → $BUNDLE_MODEL_DIR"
fi

echo "✅ Done. Build with: make build-desktop-tray"
echo "   Runtime model: $MODEL_DIR"
