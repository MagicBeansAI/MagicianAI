#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 5 ]]; then
  echo "usage: $0 <desktop-bin> <debug-app> <icon.icns> <info.plist-seed> <version>" >&2
  exit 2
fi

desktop_bin=$1
debug_app=$2
icon_source=$3
plist_seed=$4
version=$5

contents="$debug_app/Contents"
macos_dir="$contents/MacOS"
resources_dir="$contents/Resources"
frameworks_dir="$contents/Frameworks"
executable_name=magician-desktop.bin
plist="$contents/Info.plist"

mkdir -p "$macos_dir" "$resources_dir" "$frameworks_dir"
install -m 755 "$desktop_bin" "$macos_dir/$executable_name"
install -m 644 "$icon_source" "$resources_dir/icon.icns"
install -m 644 "$plist_seed" "$plist"

# The Apple Speech helper the tray spawns (Settings › Speech, /host/speech/
# transcribe, macos_tts). The gateway resolves it env → config → a `.bin`
# beside its own executable → CARGO_TARGET_DIR → bare PATH name. Only the
# `make run/restart-desktop-tray-debug` targets set the env, and a bundle
# launched any other way (Finder, `open`, a bare exec) has none of it — so the
# helper rides inside the bundle as that exe sibling. It is staged at the
# repo root beside the tray binary by `make build-macos-speech-helper`.
speech_helper_name=magician-macos-speech-helper.bin
speech_helper_source="$(dirname "$desktop_bin")/$speech_helper_name"
if [[ -f "$speech_helper_source" ]]; then
  install -m 755 "$speech_helper_source" "$macos_dir/$speech_helper_name"
else
  echo "   ⚠️  $speech_helper_source is not staged; Speech will fail from this bundle." >&2
  echo "      Run 'make build-macos-speech-helper' and rebuild the tray." >&2
  rm -f "$macos_dir/$speech_helper_name"
fi

set_plist_value() {
  local key=$1
  local type=$2
  local value=$3
  /usr/libexec/PlistBuddy -c "Delete :$key" "$plist" >/dev/null 2>&1 || true
  /usr/libexec/PlistBuddy -c "Add :$key $type $value" "$plist"
}

set_plist_value CFBundleDevelopmentRegion string en
set_plist_value CFBundleDisplayName string "Magican Debug"
set_plist_value CFBundleExecutable string "$executable_name"
set_plist_value CFBundleIconFile string icon.icns
# Match the packaged product identity so macOS can associate the debug wrapper
# with the user's existing Magican permission choices where its code requirement
# permits that reuse. `open` receives the explicit bundle path, so this does not
# make LaunchServices choose an installed release app instead.
set_plist_value CFBundleIdentifier string ai.magicbeans.magican.desktop
set_plist_value CFBundleInfoDictionaryVersion string 6.0
set_plist_value CFBundleName string "Magican Debug"
set_plist_value CFBundlePackageType string APPL
set_plist_value CFBundleShortVersionString string "$version"
set_plist_value CFBundleVersion string "$version"
set_plist_value LSMinimumSystemVersion string 14.0
set_plist_value NSPrincipalClass string NSApplication

# The debug binary has an absolute development rpath, but carrying the dylib in
# the conventional bundle location keeps the generated app relocatable within
# this checkout and matches the binary's @executable_path/../Frameworks rpath.
vosk_dylib="$(dirname "$plist_seed")/vendor/vosk/libvosk.dylib"
if [[ -f "$vosk_dylib" ]]; then
  install -m 755 "$vosk_dylib" "$frameworks_dir/libvosk.dylib"
fi

touch "$debug_app"

# Sign the assembled bundle.
#
# TCC keys Automation grants (Apple Events, e.g. driving Reminders) to a stable
# code identity. An ad-hoc signature's identity changes on every rebuild, so a
# granted permission silently stops applying and the next Apple Event fails with
# -1743 having never re-prompted. Signing with a real identity keeps the grant
# attached across rebuilds. Falls back to ad-hoc so a machine without a
# certificate still produces a runnable (if permission-less) bundle.
entitlements="$(dirname "$plist_seed")/Magician.entitlements"
sign_identity="${MAGICIAN_DESKTOP_SIGN_IDENTITY:-}"
if [[ -z "$sign_identity" ]]; then
  # By SHA-1, skipping identities the keychain marks revoked or expired: a
  # re-minted certificate has the same name as the one it replaces, and a
  # binary signed with a revoked certificate is killed and trashed at launch.
  # Developer ID Application first, Apple Development as the fallback — the
  # same rule replace-debug-bin.sh applies, so the app and the staged
  # runtimes share one Team ID. Apple Development certificates are revoked
  # when re-minted on another machine; Developer ID does not churn that way.
  identities=$(security find-identity -v -p codesigning 2>/dev/null | grep -v CSSMERR_TP_CERT || true)
  sign_identity=$(printf '%s\n' "$identities" \
    | awk '!found && /Developer ID Application/ {print $2; found=1}')
  if [[ -z "$sign_identity" ]]; then
    sign_identity=$(printf '%s\n' "$identities" \
      | awk '!found && /Apple Development/ {print $2; found=1}')
  fi
fi
codesign_args=(--force --timestamp=none --options runtime)
if [[ -f "$entitlements" ]]; then
  codesign_args+=(--entitlements "$entitlements")
fi
# Nested code must be signed before the bundle that contains it, or codesign
# rejects the outer signature ("code object is not signed at all").
# An ad-hoc signature carries no Team ID, and the hardened runtime's library
# validation refuses a dylib whose Team ID differs from the process's — so an
# ad-hoc tray with `--options runtime` dies in dyld on its own bundled
# libvosk ("mapping process and mapped file have different Team IDs"; the
# Finder dialog says the app "cannot be opened"). Ad-hoc signs without the
# hardened runtime, like replace-debug-bin.sh does for the runtime binary.
adhoc_args=(--force --timestamp=none)
if [[ -f "$entitlements" ]]; then
  adhoc_args+=(--entitlements "$entitlements")
fi
sign_one() {
  local target=$1
  if [[ -n "$sign_identity" ]]; then
    codesign "${codesign_args[@]}" --sign "$sign_identity" "$target"
  else
    codesign "${adhoc_args[@]}" --sign - "$target"
  fi
}
sign_bundle() {
  shopt -s nullglob
  local nested
  for nested in "$frameworks_dir"/*.dylib "$frameworks_dir"/*.framework; do
    sign_one "$nested"
  done
  shopt -u nullglob
  # A second executable in Contents/MacOS is nested code the bundle seal must
  # cover; sign it with the same identity so the tray's spawn is not refused
  # for a Team ID mismatch under the hardened runtime.
  if [[ -f "$macos_dir/$speech_helper_name" ]]; then
    sign_one "$macos_dir/$speech_helper_name"
  fi
  # The raw repo-root binary keeps an absolute rpath to the vendor copy of the
  # same dylib. Sign that copy with the same identity too, so a direct launch of
  # the binary is not refused for a Team ID mismatch the way an ad-hoc vendor
  # copy was under the hardened runtime.
  if [[ -f "$vosk_dylib" ]]; then
    sign_one "$vosk_dylib"
  fi
  if [[ -n "$sign_identity" ]]; then
    echo "   signing debug app as: $sign_identity"
    sign_one "$debug_app"
  else
    echo "   ⚠️  no codesigning identity found; signing ad-hoc." >&2
    echo "      Automation (Apple Reminders) permission will not persist across rebuilds." >&2
    sign_one "$debug_app"
  fi
}
sign_bundle

# The keychain's validity flag lags Apple's revocation: `find-identity -v` can
# still offer a certificate Apple has revoked, and a bundle signed with it is
# refused by LaunchServices ("Launchd job spawn failed") or killed a minute
# after launch. Ask Gatekeeper about the signed bundle — the same check
# replace-debug-bin.sh makes for the runtime — and on a REVOKED verdict sign
# everything again ad-hoc, which launches. The verdict is captured, not piped:
# spctl exits non-zero for every rejected bundle and this script runs under
# pipefail. (Automation and Speech grants are keyed to the code identity, so
# they re-prompt until a new Apple Development certificate is minted and the
# bundle is re-staged with it.)
if [[ -n "$sign_identity" ]]; then
  gatekeeper_verdict=$(spctl --assess --type execute "$debug_app" 2>&1 || true)
  if [[ "$gatekeeper_verdict" == *CSSMERR_TP_CERT_REVOKED* ]]; then
    echo "   ⚠️  Gatekeeper reports the signing certificate REVOKED; re-signing the debug app ad-hoc." >&2
    echo "      Mint a new Apple Development certificate and rebuild to restore a stable code identity." >&2
    sign_identity=""
    sign_bundle
  fi
fi

# All bundle copies and mutations are complete above. Verify the final staged
# bundle now, then require its nested runtime to carry the same Team ID when a
# real identity was selected. This catches a copied-after-signing artifact or a
# desktop/runtime certificate mismatch before either process is launched.
codesign --verify --deep --strict --verbose=2 "$debug_app"

signature_field() {
  local target=$1
  local field=$2
  codesign --display --verbose=4 "$target" 2>&1 \
    | awk -F= -v key="$field" '$1 == key {print $2}'
}

app_identifier=$(signature_field "$debug_app" Identifier)
if [[ "$app_identifier" != "ai.magicbeans.magican.desktop" ]]; then
  echo "error: staged debug app identifier '$app_identifier' is not ai.magicbeans.magican.desktop" >&2
  exit 1
fi

app_team=$(signature_field "$debug_app" TeamIdentifier)
runtime_team=$(signature_field "$macos_dir/$executable_name" TeamIdentifier)
if [[ -n "$sign_identity" ]]; then
  if [[ -z "$app_team" || "$app_team" == "not set" ]]; then
    echo "error: real-signed debug app has no Apple Team ID" >&2
    exit 1
  fi
  if [[ "$runtime_team" != "$app_team" ]]; then
    echo "error: desktop runtime Team ID '$runtime_team' does not match app Team ID '$app_team'" >&2
    exit 1
  fi
  echo "   verified final debug app after copy: $debug_app (team $app_team)"
else
  if [[ -n "$app_team" && "$app_team" != "not set" ]]; then
    echo "error: ad-hoc debug app unexpectedly carries Team ID '$app_team'" >&2
    exit 1
  fi
  echo "   verified final ad-hoc debug app after copy: $debug_app"
fi
