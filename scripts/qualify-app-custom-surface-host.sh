#!/usr/bin/env bash
#
# Phase 7 web/desktop custom-surface host qualification.
# Desktop embeds Unified UI, so one host suite covers both shells.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/Volumes/build/magician/builds}"

echo "===> kernel + worker host contract"
for test_filter in \
  phase7_web_desktop_host_qualification \
  scripted_host_spawns_a_killable_worker \
  wasm_package_is_refused_even_with_a_worker \
  red_team_tokens_cannot_open_network \
  watchdog_trips_on_message_flood
do
  cargo test -p magician --lib --offline "$test_filter"
done

echo "===> Unified UI host (web and desktop)"
npm --prefix ui/unified-ui test -- \
  src/lib/apps/appCustomSurface.test.ts \
  src/lib/apps/AppCustomSurfaceHost.component.test.ts

echo
echo "PASS: Phase 7 web/desktop custom-surface host qualification."
echo "Display stays no-script. Scripted JS stays in the Magician worker."
echo "Iframe.svelte is not the app-document host."
