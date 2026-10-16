#!/usr/bin/env bash
#
# Device-side helpers for the Android companion: install, run, log, screenshot.
#
# `adb` is not on PATH in this workspace's shell, so every one of these was
# being typed with a full SDK path and a hand-built device check. That is how a
# screenshot ends up taken from the wrong device, or a stale APK gets installed
# because the build was skipped and nobody noticed.
set -euo pipefail

APP_ID="ai.magicbeans.magican"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APK="$ROOT/magdroid/android/app/build/outputs/apk/debug/app-debug.apk"

adb_bin() {
  if command -v adb >/dev/null 2>&1; then
    command -v adb
    return
  fi
  local sdk="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-$HOME/Library/Android/sdk}}"
  if [ -x "$sdk/platform-tools/adb" ]; then
    echo "$sdk/platform-tools/adb"
    return
  fi
  echo "adb not found. Install the Android SDK platform-tools, or set ANDROID_SDK_ROOT." >&2
  exit 1
}

ADB="$(adb_bin)"

# Refuse to guess when several devices are attached. Installing on the wrong
# handset is not something you notice until the change you are looking for is
# missing from a screen you are not holding.
require_one_device() {
  local devices
  devices="$("$ADB" devices | awk 'NR>1 && $2=="device" {print $1}')"
  local count
  count="$(printf '%s\n' "$devices" | grep -c . || true)"
  if [ "$count" -eq 0 ]; then
    echo "No device. Connect a phone with USB debugging on, or start an emulator." >&2
    exit 1
  fi
  if [ "$count" -gt 1 ] && [ -z "${ANDROID_SERIAL:-}" ]; then
    echo "Several devices are attached; set ANDROID_SERIAL to choose one:" >&2
    printf '  %s\n' $devices >&2
    exit 1
  fi
}

require_apk() {
  if [ ! -f "$APK" ]; then
    echo "No debug APK. Run 'make build-magdroid' first." >&2
    exit 1
  fi
}

# Android drops the selected input method when its package is replaced: the
# binding is invalidated, and the system falls back to the stock keyboard. That
# is correct for a phone and useless for a development loop, where every install
# silently un-picks the keyboard being worked on and the next test is run
# against Gboard without anyone noticing.
#
# Only restored, never imposed: if Magican was not the keyboard before the install,
# nothing here changes which one is.
KEYBOARD_IME="$APP_ID/ai.magicbeans.magdroid.keyboard.MagicanKeyboardService"

selected_ime() {
  "$ADB" shell settings get secure default_input_method 2>/dev/null | tr -d '\r'
}

restore_ime_if_ours() {
  local before="$1"
  case "$before" in
    "$APP_ID"/*) ;;
    *) return 0 ;;
  esac
  local now
  now="$(selected_ime)"
  [ "$now" = "$before" ] && return 0
  # Enabling first: a replaced package can come back absent from the enabled
  # list, and selecting one that is not enabled silently does nothing.
  "$ADB" shell ime enable "$before" >/dev/null 2>&1 || true
  "$ADB" shell ime set "$before" >/dev/null 2>&1 || true
  if [ "$(selected_ime)" = "$before" ]; then
    echo "Keyboard reselected: $before"
  else
    echo "Could not reselect $before — pick it from the keyboard switcher." >&2
  fi
}

case "${1:-}" in
  install)
    require_one_device
    require_apk
    ime_before="$(selected_ime)"
    "$ADB" install -r "$APK"
    restore_ime_if_ours "$ime_before"
    ;;

  run)
    require_one_device
    require_apk
    ime_before="$(selected_ime)"
    "$ADB" install -r "$APK"
    restore_ime_if_ours "$ime_before"
    # Stopped first so this is a cold start: a warm relaunch keeps the previous
    # process, which hides anything that only happens on startup.
    "$ADB" shell am force-stop "$APP_ID"
    # Launched through the LAUNCHER intent rather than by component. The
    # activity is not exported — correctly, nothing else should be able to
    # start it — and naming it directly is refused with a SecurityException.
    "$ADB" shell monkey -p "$APP_ID" -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1
    echo "Launched $APP_ID"
    ;;

  logs)
    require_one_device
    # Filtered to this app's process. Unfiltered logcat on a real handset is
    # mostly the vendor's own noise.
    local_pid="$("$ADB" shell pidof "$APP_ID" 2>/dev/null | tr -d '\r' || true)"
    if [ -z "$local_pid" ]; then
      echo "$APP_ID is not running; showing the whole buffer instead." >&2
      exec "$ADB" logcat
    fi
    exec "$ADB" logcat --pid="$local_pid"
    ;;

  screenshot)
    require_one_device
    out="${2:-magdroid-screen.png}"
    "$ADB" exec-out screencap -p > "$out"
    echo "$out"
    ;;

  *)
    echo "usage: magdroid-device.sh {install|run|logs|screenshot [path]}" >&2
    exit 2
    ;;
esac
