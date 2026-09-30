#!/usr/bin/env bash
# Provider-free P7.7-P7.9 qualification for the bounded App Platform core.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/Volumes/build/magician/builds}"

PASS_K="${APP_PLATFORM_PASS_K:-3}"
if [[ "$PASS_K" != "3" ]]; then
  echo "FAIL: APP_PLATFORM_PASS_K must remain 3 for the published P7.9 gate." >&2
  exit 2
fi

run() {
  printf '\n===> %s\n' "$1"
  shift
  "$@"
}

run "P7 manifest and fail-closed repetition contract" \
  cargo test -p magician --lib magician_v2::apps::release_qualification::tests --offline

for trial in $(seq 1 "$PASS_K"); do
  run "P7.7-P7.9 deterministic App owner pass ${trial}/${PASS_K}" \
    cargo test -p magician --lib magician_v2::apps:: --offline -- --test-threads=1
done

run "P7 supported-public/API/SDK/client parity" make app-contract-check
run "P7 authoring/publication/review" make test-app-authoring
run "P7 web/desktop custom-surface containment" make qualify-app-custom-surface-host
run "P7 first-party Apps UI" npm --prefix ui/unified-ui test -- \
  src/lib/apps/appCustomSurface.test.ts \
  src/lib/apps/AppCustomSurfaceHost.component.test.ts \
  src/lib/apps/AppDeclarativeSurface.component.test.ts \
  src/lib/apps/actionRunHistory.test.ts \
  src/lib/apps/appDirectory.test.ts \
  src/lib/apps/appInteractiveState.test.ts \
  src/lib/apps/appLifecycle.test.ts \
  src/lib/apps/appSurfaceRuntime.test.ts \
  src/lib/apps/installationReview.test.ts
run "P7 external Research Planner static acceptance" \
  npm --prefix examples/reference-apps/research-planner run test:static
run "P7 documentation ownership" python3 scripts/docs_guard.py

printf '\nPASS: provider-free P7.7-P7.9 qualification completed at pass^%s.\n' "$PASS_K"
printf 'Live physical-owner and assembled-product canaries remain separate operator evidence.\n'
