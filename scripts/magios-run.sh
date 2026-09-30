#!/usr/bin/env bash
# magios-run.sh [build|deploy|run|live-chat|live-recovery|live-today|live-notes|live-appearance|live-upload|live-playback|live-session-routing|live-enrollment|live-question]   (default: run)
#
# Magios debug build / deploy, targeting a CONNECTED iPhone when one is present,
# otherwise an iOS simulator. Modes:
#   build  — COMPILE the checked-in project. No install.
#   deploy — INSTALL + launch the LAST build. No rebuild (run `build`/`run` first).
#   run    — build then deploy.
#   live-chat — opt-in real-network XCTest on an already enrolled test iPhone.
#   live-recovery — terminate an active accepted turn and verify one canonical reply.
#   live-today — verify cold-launch widget loading on that test iPhone.
#   live-notes — open the in-app notes library without editing notes.
#   live-appearance — capture Day/Night surfaces, restoring the original mode.
#   live-upload — share a synthetic PDF from Safari and verify its chat reply.
#   live-playback — verify backend reply audio starts and finishes on the iPhone.
#   live-session-routing — verify live chat isolation and shared messages with a second phone.
#   live-enrollment — confirm a supplied link for the selected host, then restart.
#   live-question — answer a pending choice and verify the continued chat reply.
#
# Customer hosts and credentials are never compiled into this generic build;
# connect it by scanning the one-time QR from Magician Settings. The Cloudflare
# Access GATE itself is a one-time setup (see `make ios-access`). First device
# install may need the developer trusted:
# Settings → General → VPN & Device Management. Requires Xcode 15+ (devicectl).
# The normal Debug lane is compatible with Xcode's free Personal Team and does
# not request APNs. Set MAGIOS_PUSH_NOTIFICATIONS=1 (or use the Make push target)
# only with a paid team whose App ID has Push Notifications enabled.
set -euo pipefail
cd "$(dirname "$0")/.."

MODE="${1:-run}"
PROJ=magios/Magios.xcodeproj
SCHEME=Magios
DD="${MAGIOS_DERIVED_DATA:-${CARGO_TARGET_DIR:-target}/magios-device}"
SPM="${MAGIOS_SPM_DIR:-${CARGO_TARGET_DIR:-target}/magios-spm}"
BUILD_JOBS="${MAGIOS_BUILD_JOBS:-2}"

log(){ printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }
die(){ printf '\033[1;31mERROR: %s\033[0m\n' "$*" >&2; exit 1; }
command -v xcodebuild >/dev/null 2>&1 || die "xcodebuild not found (need macOS + Xcode)."
case "$BUILD_JOBS" in
  1|2|3|4) ;;
  *) die "MAGIOS_BUILD_JOBS must be 1–4 (default 2)." ;;
esac
case "${MAGIOS_PUSH_NOTIFICATIONS:-0}" in
  0|1) ;;
  *) die "MAGIOS_PUSH_NOTIFICATIONS must be 0 or 1" ;;
esac

# Physical iPhones carry an 8-16 hex UDID (`00008130-0010359C2ED8001C`); only
# simulators use the 8-4-4-4-12 UUID form. Matching just the latter made
# every connected phone invisible and silently deployed to a simulator.
UDID_RE='[0-9A-Fa-f]{8}-[0-9A-Fa-f]{16}|[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}'
bundle_id_of(){ /usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$1/Info.plist" 2>/dev/null; }
bundle_executable_of(){ /usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$1/Info.plist" 2>/dev/null; }
installable_app_in(){
  # XCTest leaves *-Runner.app beside the product. `find -print -quit` can
  # return that stale runner first, causing deploy's freshness check to reject
  # a current Magican build or install the wrong bundle.
  find "$1" -maxdepth 1 -type d -name '*.app' ! -name '*-Runner.app' -print -quit 2>/dev/null || true
}
# Current CoreDevice reports a Wi-Fi-reachable, trusted phone as
# `available (paired)` rather than the older `connected` state. Both are valid
# device destinations; excluding the former silently sends a wireless install
# to the simulator fallback.
device_udid(){
  if [ -n "${MAGIOS_DEVICE_ID:-}" ]; then printf '%s' "$MAGIOS_DEVICE_ID"; return; fi
  xcrun devicectl list devices 2>/dev/null | grep -iE 'iPhone' | grep -iE 'connected|available[[:space:]]+\(paired\)' | grep -iE 'physical' | grep -oE "$UDID_RE" | head -1 || true
}
sim_udid(){
  local s; s="$(xcrun simctl list devices booted 2>/dev/null | grep -oE "$UDID_RE" | head -1 || true)"
  [ -n "$s" ] || s="$(xcrun simctl list devices available 2>/dev/null | grep -E 'iPhone' | grep -oE "$UDID_RE" | head -1 || true)"
  printf '%s' "$s"
}

assert_build_is_current(){
  local app="$1"
  local executable; executable="$(bundle_executable_of "$app")"
  local marker="$app/$executable.debug.dylib"
  [ -f "$marker" ] || marker="$app/$executable"
  [ -f "$marker" ] || die "built app has no executable marker: $app"

  # `deploy` intentionally does not compile, but silently reinstalling an older
  # product makes an on-device bug look unchanged after its source was fixed.
  # Refuse only when behaviour-bearing project input is newer than the main
  # debug image; generated credentials and build output live outside these roots.
  local newer
  newer="$(find \
    magios/Magios magios/Shared magios/MagiosIntents \
    magios/MagiosWidgets magios/MagiosShare magios/MagiosAction \
    magios/MagiosBroadcast magios/MagiosKeyboard magios/project.yml \
    -type f -newer "$marker" -print -quit 2>/dev/null || true)"
  [ -z "$newer" ] || die "device build is stale (newer input: $newer). Run: make ios-debug-build"
}

reset_stale_explicit_module_cache(){
  # Keep historical automatic cleanup limited to its original disposable root.
  # Never recursively erase a configured/shared build cache on failure.
  [ "$DD" = "/tmp/magios_dd" ] || die "refusing to clean unexpected DerivedData root: $DD"
  log "Xcode explicit-module cache is stale — rebuilding its derived intermediates once"
  rm -rf -- \
    /tmp/magios_dd/Build/Intermediates.noindex \
    /tmp/magios_dd/ModuleCache.noindex \
    /tmp/magios_dd/SDKStatCaches.noindex
}

run_xcode_build(){
  local build_log status
  # The Xs must be the LAST characters of the template: BSD mktemp only
  # substitutes trailing Xs, so a `.log` suffix made it create the literal
  # file /tmp/magios-xcodebuild.XXXXXX.log once and fail with "File exists"
  # on every build after (2026-09-06).
  build_log="$(mktemp "${TMPDIR:-/tmp}/magios-xcodebuild.log.XXXXXX")"

  # Preserve the complete first failure for an exact diagnosis while keeping
  # normal xcodebuild output visible. `pipefail` makes the condition reflect
  # xcodebuild rather than tee.
  if xcodebuild "$@" 2>&1 | tee "$build_log"; then
    rm -f -- "$build_log"
    return 0
  else
    status=${PIPESTATUS[0]}
  fi

  # Xcode can retain an XCBuild graph that names explicit precompiled modules
  # already evicted from a persistent DerivedData cache (commonly after an SDK
  # update). The IDE succeeds because it uses a different cache. Repair only
  # that exact missing-PCM class, only once; signing, source, package, and all
  # other compiler failures remain immediate failures.
  if grep -Eq "ExplicitPrecompiledModules/[^[:space:]]+\\.pcm.*not found" "$build_log"; then
    reset_stale_explicit_module_cache
    if xcodebuild "$@"; then
      status=0
    else
      status=$?
    fi
  fi

  rm -f -- "$build_log"
  return "$status"
}

do_build(){
  log "Building the checked-in Xcode project"
  # macOS still ships Bash 3.2, where expanding an empty local array under
  # `set -u` aborts. Keep one truthful, harmless setting in the argv array so
  # the ordinary push-free lane and the opt-in push lane share one command.
  local -a build_settings=("CODE_SIGN_STYLE=Automatic")
  if [ -n "${MAGIOS_DEVELOPMENT_TEAM:-}" ]; then
    build_settings+=("DEVELOPMENT_TEAM=${MAGIOS_DEVELOPMENT_TEAM}")
  fi
  if [ "${MAGIOS_PUSH_NOTIFICATIONS:-0}" = "1" ]; then
    log "APNs sandbox signing enabled — this requires a paid push-capable Apple team"
    # Xcode, rather than this shell, must expand $(inherited).
    # shellcheck disable=SC2016
    build_settings+=(
      "CODE_SIGN_ENTITLEMENTS=Magios/MagicanPush.entitlements"
      "APS_ENVIRONMENT=development"
      'SWIFT_ACTIVE_COMPILATION_CONDITIONS=$(inherited) DEBUG MAGIOS_REMOTE_PUSH'
    )
  fi
  local dev; dev="$(device_udid)"
  if [ -n "$dev" ]; then
    log "Connected iPhone $dev — building for device (Debug, signed)"
    run_xcode_build -jobs "$BUILD_JOBS" -project "$PROJ" -scheme "$SCHEME" -configuration Debug \
      -destination "generic/platform=iOS" -allowProvisioningUpdates \
      -derivedDataPath "$DD" -clonedSourcePackagesDirPath "$SPM" \
      "${build_settings[@]}" build
  else
    local sim; sim="$(sim_udid)"; [ -n "$sim" ] || die "no iPhone simulator available."
    log "No iPhone connected — building for simulator $sim"
    run_xcode_build -jobs "$BUILD_JOBS" -project "$PROJ" -scheme "$SCHEME" -configuration Debug \
      -destination "id=$sim" -derivedDataPath "$DD" -clonedSourcePackagesDirPath "$SPM" \
      "${build_settings[@]}" build
  fi
  log "Build done ✅"
}

do_live_acceptance(){
  local suite="$1"
  [ -n "${MAGIOS_DEVICE_ID:-}" ] || die "$MODE requires an explicit MAGIOS_DEVICE_ID (physical iPhone)."
  local expected_host="${MAGIOS_LIVE_TEST_HOST:-}"
  [[ "$expected_host" =~ ^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$ ]] || die "set MAGIOS_LIVE_TEST_HOST to the enrolled test server hostname (no URL or credentials)."
  if [ "$MODE" = live-upload ]; then
    [ -n "${MAGIOS_LIVE_UPLOAD_FILENAME:-}" ] && [ -n "${MAGIOS_LIVE_UPLOAD_REPLY:-}" ] && [ -n "${MAGIOS_LIVE_UPLOAD_URL:-}" ] || die "live-upload requires MAGIOS_LIVE_UPLOAD_URL / MAGIOS_LIVE_UPLOAD_FILENAME / MAGIOS_LIVE_UPLOAD_REPLY for a synthetic PDF fixture."
  fi
  if [ "$MODE" = live-playback ] && [ -n "${MAGIOS_LIVE_PLAYBACK_TEXT:-}" ]; then
    [ -n "${MAGIOS_LIVE_PLAYBACK_MESSAGE_ID:-}" ] || die "an existing playback fixture requires MAGIOS_LIVE_PLAYBACK_MESSAGE_ID."
  fi
  if [ "$MODE" = live-session-routing ]; then
    [ -n "${MAGIOS_LIVE_ROUTING_FIXTURE_JSON:-}" ] || die "live-session-routing requires MAGIOS_LIVE_ROUTING_FIXTURE_JSON and a second enrolled client to send its probes."
  fi
  if [ "$MODE" = live-question ]; then
    [ -n "${MAGIOS_LIVE_QUESTION_FIXTURE_JSON:-}" ] || die "live-question requires MAGIOS_LIVE_QUESTION_FIXTURE_JSON and a pending test question."
  fi
  local result="${MAGIOS_LIVE_TEST_RESULT:-$DD/$MODE-$(date +%Y%m%d-%H%M%S).xcresult}"
  [ ! -e "$result" ] || die "result bundle already exists: $result"
  # The ordinary simulator test targets disable signing. A device runner must
  # be signed by the same local development team as the app, not left carrying
  # the SDK's original XCTRunner signature.
  local -a live_settings=("CODE_SIGN_STYLE=Automatic" "CODE_SIGNING_ALLOWED=YES" "CODE_SIGNING_REQUIRED=YES")
  if [ -n "${MAGIOS_DEVELOPMENT_TEAM:-}" ]; then
    live_settings+=("DEVELOPMENT_TEAM=$MAGIOS_DEVELOPMENT_TEAM")
  fi
  log "Live iPhone $MODE acceptance against $expected_host (one job, no retries)"
  TEST_RUNNER_MAGIOS_LIVE_TEST_HOST="$expected_host" \
    TEST_RUNNER_MAGIOS_LIVE_NOTES_HOST="${MAGIOS_LIVE_NOTES_HOST:-}" \
    TEST_RUNNER_MAGIOS_LIVE_NOTES_BROWSER="${MAGIOS_LIVE_NOTES_BROWSER:-}" \
    TEST_RUNNER_MAGIOS_LIVE_ENROLLMENT="$([ "$MODE" = live-enrollment ] && echo 1 || echo 0)" \
    TEST_RUNNER_MAGIOS_LIVE_ROUTING_FIXTURE_JSON="${MAGIOS_LIVE_ROUTING_FIXTURE_JSON:-}" \
    TEST_RUNNER_MAGIOS_LIVE_QUESTION_FIXTURE_JSON="${MAGIOS_LIVE_QUESTION_FIXTURE_JSON:-}" \
    TEST_RUNNER_MAGIOS_LIVE_PLAYBACK_TEXT="${MAGIOS_LIVE_PLAYBACK_TEXT:-}" \
    TEST_RUNNER_MAGIOS_LIVE_PLAYBACK_MESSAGE_ID="${MAGIOS_LIVE_PLAYBACK_MESSAGE_ID:-}" \
    TEST_RUNNER_MAGIOS_LIVE_PLAYBACK_SESSION_TITLE="${MAGIOS_LIVE_PLAYBACK_SESSION_TITLE:-}" \
    TEST_RUNNER_MAGIOS_LIVE_UPLOAD_FILENAME="${MAGIOS_LIVE_UPLOAD_FILENAME:-}" \
    TEST_RUNNER_MAGIOS_LIVE_UPLOAD_URL="${MAGIOS_LIVE_UPLOAD_URL:-}" \
    TEST_RUNNER_MAGIOS_LIVE_UPLOAD_REPLY="${MAGIOS_LIVE_UPLOAD_REPLY:-}" run_xcode_build \
    -jobs 1 -project "$PROJ" -scheme "$SCHEME" -configuration Debug \
    -destination "platform=iOS,id=$MAGIOS_DEVICE_ID" -allowProvisioningUpdates \
    -derivedDataPath "$DD" -clonedSourcePackagesDirPath "$SPM" \
    -resultBundlePath "$result" -parallel-testing-enabled NO -enableCodeCoverage NO \
    "-only-testing:MagiosUITests/$suite" \
    "${live_settings[@]}" test
  log "Live iPhone result: $result"
}

do_deploy(){
  local dev; dev="$(device_udid)"
  if [ -n "$dev" ]; then
    local app; app="$(installable_app_in "$DD/Build/Products/Debug-iphoneos")"
    [ -n "$app" ] || die "no device build at $DD/Build/Products/Debug-iphoneos — run: make ios-debug-build"
    assert_build_is_current "$app"
    local bid; bid="$(bundle_id_of "$app")"
    log "Installing $(basename "$app") ($bid) onto the iPhone"
    xcrun devicectl device install app --device "$dev" "$app"
    log "Launching on the iPhone"
    if ! xcrun devicectl device process launch --device "$dev" "$bid"; then
      die "installed, but iOS refused launch. Check signing/profile validity and Settings → General → VPN & Device Management for developer trust, then retry."
    fi
    log "Running on your iPhone ✅"
  else
    local sim; sim="$(sim_udid)"; [ -n "$sim" ] || die "no iPhone simulator available."
    local app; app="$(installable_app_in "$DD/Build/Products/Debug-iphonesimulator")"
    [ -n "$app" ] || die "no simulator build at $DD/Build/Products/Debug-iphonesimulator — run: make ios-debug-build"
    assert_build_is_current "$app"
    local bid; bid="$(bundle_id_of "$app")"
    xcrun simctl boot "$sim" >/dev/null 2>&1 || true
    open -a Simulator >/dev/null 2>&1 || true
    log "Installing + launching $(basename "$app") ($bid) on simulator $sim"
    xcrun simctl install "$sim" "$app"
    xcrun simctl launch "$sim" "$bid"
    log "Running on the simulator ✅"
  fi
}

case "$MODE" in
  build)  do_build ;;
  deploy) do_deploy ;;
  run)    do_build; do_deploy ;;
  live-chat) do_live_acceptance MagiosLiveConnectivityUITests ;;
  live-recovery) do_live_acceptance MagiosLiveAcceptedTurnRecoveryUITests ;;
  live-today) do_live_acceptance MagiosLiveTodayUITests ;;
  live-notes) do_live_acceptance MagiosLiveNotesUITests ;;
  live-appearance) do_live_acceptance MagiosLiveAppearanceUITests ;;
  live-upload) do_live_acceptance MagiosLiveUploadUITests ;;
  live-playback) do_live_acceptance MagiosLivePlaybackUITests ;;
  live-session-routing) do_live_acceptance MagiosLiveSessionRoutingUITests ;;
  live-enrollment) do_live_acceptance MagiosLiveEnrollmentUITests ;;
  live-question) do_live_acceptance MagiosLiveQuestionUITests ;;
  *)      die "unknown mode '$MODE' (use: build | deploy | run | live-chat | live-recovery | live-today | live-notes | live-appearance | live-upload | live-playback | live-session-routing | live-enrollment | live-question)" ;;
esac
