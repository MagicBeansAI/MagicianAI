#!/usr/bin/env bash
# Fetch libvosk (iOS) + the Vosk model for the Magios ambient-mode wake spotter
# (`magios/Magios/VoskWakeSpotter.swift`). Both are large binaries and are
# gitignored, exactly as `setup-desktop-vosk.sh` handles the desktop pair.
# Idempotent (skips what's already present). Run once per clone/worktree, before
# `xcodegen generate`.
#
# Alpha Cephei never published an iOS artefact on the vosk-api releases page, so
# the only builds in circulation are the ones vendored inside app projects. The
# archive below is byte-identical across the ones that carry it — a single 2021
# Alpha Cephei build (SDK 15.2) passed hand to hand — and it is pinned to a TAG
# plus a SHA-256 rather than to a branch, because it is a binary blob that runs
# on the microphone path before any user gate. A substituted binary would be the
# single worst thing that could happen to this feature, so an unexpected digest
# is a hard failure and not a warning.
#
# Override via VOSK_IOS_REF / VOSK_MODEL_NAME / MAGIOS_VOSK_VENDOR_DIR /
# MAGIOS_VOSK_MODEL_DIR.
set -euo pipefail

MODE="setup"
case "${1:-}" in
  "") ;;
  --verify-only) MODE="verify" ;;
  -h|--help)
    echo "Usage: $0 [--verify-only]"
    exit 0
    ;;
  *)
    echo "Usage: $0 [--verify-only]" >&2
    exit 2
    ;;
esac

VOSK_IOS_REF="${VOSK_IOS_REF:-v2.1.7}"
VOSK_IOS_REPO="${VOSK_IOS_REPO:-riderodd/react-native-vosk}"
VOSK_MODEL_NAME="${VOSK_MODEL_NAME:-vosk-model-small-en-us-0.15}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VENDOR_DIR="${MAGIOS_VOSK_VENDOR_DIR:-$ROOT/magios/Vendor/vosk}"
XCFRAMEWORK="$VENDOR_DIR/libvosk.xcframework"
MODEL_DIR="${MAGIOS_VOSK_MODEL_DIR:-$ROOT/magios/Magios/Resources/vosk-model}"

# Slice name → expected SHA-256 of that slice's libvosk.a.
SIM_SLICE="ios-arm64_x86_64-simulator"
SIM_SHA256="7933cf4794fad8f066de78ed1b6f70797b41ce3f43b5944e151c6a1f6c08d76c"
DEVICE_SLICE="ios-arm64_armv7_armv7s"
DEVICE_SHA256="a6705b01390ec42a33f4f3b83ac4f2d760d74bf15e5a82cf7ea1710aca1115f9"

if [ "$(uname -s)" != "Darwin" ]; then
  echo "⏭  Magios Vosk setup is macOS-only (needs Xcode to be useful) — skipping."
  exit 0
fi

verify_slice() {
  local slice="$1" expected="$2" dest="$XCFRAMEWORK/$1/libvosk.a" actual
  if [ ! -s "$dest" ]; then
    echo "✗ Magios Vosk artifact is missing: $dest" >&2
    echo "  Run 'make setup-magios-vosk' and retry." >&2
    return 1
  fi
  actual="$(shasum -a 256 "$dest" | awk '{print $1}')"
  if [ "$actual" != "$expected" ]; then
    echo "✗ libvosk $slice checksum mismatch." >&2
    echo "  expected $expected" >&2
    echo "  actual   $actual" >&2
    echo "  Remove the mismatched xcframework and run 'make setup-magios-vosk'." >&2
    return 1
  fi
  echo "✓ libvosk $slice checksum verified"
}

if [ "$MODE" = "verify" ]; then
  verify_slice "$SIM_SLICE" "$SIM_SHA256"
  verify_slice "$DEVICE_SLICE" "$DEVICE_SHA256"
  if [ ! -s "$XCFRAMEWORK/Info.plist" ]; then
    echo "✗ Magios Vosk xcframework manifest is missing: $XCFRAMEWORK/Info.plist" >&2
    echo "  Run 'make setup-magios-vosk' and retry." >&2
    exit 1
  fi
  if [ ! -s "$MODEL_DIR/am/final.mdl" ]; then
    echo "✗ Magios Vosk model is missing or incomplete: $MODEL_DIR" >&2
    echo "  Run 'make setup-magios-vosk' and retry." >&2
    exit 1
  fi
  echo "✅ Magios Vosk framework and model verified."
  exit 0
fi

fetch_slice() {
  local slice="$1" expected="$2" dest="$XCFRAMEWORK/$1/libvosk.a"
  if [ -s "$dest" ]; then
    verify_slice "$slice" "$expected"
    return
  fi
  echo "↓ libvosk $slice ($VOSK_IOS_REPO@$VOSK_IOS_REF)"
  mkdir -p "$(dirname "$dest")"
  curl -fL --retry 3 -o "$dest.tmp" \
    "https://raw.githubusercontent.com/$VOSK_IOS_REPO/$VOSK_IOS_REF/ios/libvosk.xcframework/$slice/libvosk.a"
  local actual
  actual="$(shasum -a 256 "$dest.tmp" | awk '{print $1}')"
  if [ "$actual" != "$expected" ]; then
    rm -f "$dest.tmp"
    echo "✗ libvosk $slice checksum mismatch." >&2
    echo "  expected $expected" >&2
    echo "  actual   $actual" >&2
    echo "  Refusing to vendor an unverified binary onto the microphone path." >&2
    exit 1
  fi
  mv "$dest.tmp" "$dest"
  echo "✓ libvosk $slice → $dest"
}

fetch_slice "$SIM_SLICE" "$SIM_SHA256"
fetch_slice "$DEVICE_SLICE" "$DEVICE_SHA256"

# The xcframework manifest. Written here rather than downloaded so the vendored
# tree is fully described by this script.
cat > "$XCFRAMEWORK/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>AvailableLibraries</key>
	<array>
		<dict>
			<key>LibraryIdentifier</key>
			<string>$DEVICE_SLICE</string>
			<key>LibraryPath</key>
			<string>libvosk.a</string>
			<key>SupportedArchitectures</key>
			<array>
				<string>arm64</string>
				<string>armv7</string>
				<string>armv7s</string>
			</array>
			<key>SupportedPlatform</key>
			<string>ios</string>
		</dict>
		<dict>
			<key>LibraryIdentifier</key>
			<string>$SIM_SLICE</string>
			<key>LibraryPath</key>
			<string>libvosk.a</string>
			<key>SupportedArchitectures</key>
			<array>
				<string>arm64</string>
				<string>x86_64</string>
			</array>
			<key>SupportedPlatform</key>
			<string>ios</string>
			<key>SupportedPlatformVariant</key>
			<string>simulator</string>
		</dict>
	</array>
	<key>CFBundlePackageType</key>
	<string>XFWK</string>
	<key>XCFrameworkFormatVersion</key>
	<string>1.0</string>
</dict>
</plist>
PLIST
echo "✓ xcframework manifest → $XCFRAMEWORK/Info.plist"

# The model is BUNDLED into the app (see docs/components/magios/ambient-mode.md
# for why it is not an On-Demand Resource), so it lands under Magios/Resources
# and project.yml references it as a folder.
if [ -d "$MODEL_DIR/am" ]; then
  echo "✓ vosk model already present"
else
  echo "↓ model $VOSK_MODEL_NAME"
  rm -rf /tmp/magios-vosk-model
  curl -fL --retry 3 -o /tmp/magios-vosk-model.zip \
    "https://alphacephei.com/vosk/models/${VOSK_MODEL_NAME}.zip"
  unzip -q -o /tmp/magios-vosk-model.zip -d /tmp/magios-vosk-model
  mkdir -p "$(dirname "$MODEL_DIR")"
  rm -rf "$MODEL_DIR"
  mv "/tmp/magios-vosk-model/${VOSK_MODEL_NAME}" "$MODEL_DIR"
  rm -rf /tmp/magios-vosk-model /tmp/magios-vosk-model.zip
  echo "✓ model → $MODEL_DIR"
fi

echo "✅ Done. The checked-in Xcode project is ready for \`make test-ios\`."
