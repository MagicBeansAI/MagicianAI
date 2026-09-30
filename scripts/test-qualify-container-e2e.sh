#!/usr/bin/env bash
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
FAKE_BIN="$TMP/bin"
mkdir -p "$FAKE_BIN"

cat > "$TMP/setup.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'fake runtime setup: %s\n' "${MAGICIAN_CONTAINER_RUNTIME:-unset}"
SH

cat > "$TMP/install.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
mkdir -p "$MAGICIAN_ROOT_DIR"
if [[ "${MAGICIAN_FAKE_OVERWRITE:-0}" == "1" ]]; then
  printf 'overwritten\n' > "$MAGICIAN_ROOT_DIR/magician-config.yaml"
elif [[ ! -f "$MAGICIAN_ROOT_DIR/magician-config.yaml" ]]; then
  printf 'seeded\n' > "$MAGICIAN_ROOT_DIR/magician-config.yaml"
fi
printf 'fake install: %s %s\n' "$MAGICIAN_CONTAINER_RUNTIME" "$MAGICIAN_IMAGE_REF"
SH

cat > "$TMP/verify.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'fake verify\n'
SH

cat > "$TMP/qualify.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
report=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --report) report="$2"; shift 2 ;;
    *) shift ;;
  esac
done
[[ -n "$report" ]]
mkdir -p "$(dirname "$report")"
if [[ "${MAGICIAN_FAKE_QUALIFY_FAIL:-0}" == "1" ]]; then
  printf '%s\n' '{"schema_version":1,"totals":{"passed":15,"failed":1,"skipped":0}}' > "$report"
  exit 1
fi
printf '%s\n' '{"schema_version":1,"totals":{"passed":16,"failed":0,"skipped":0}}' > "$report"
SH

cat > "$FAKE_BIN/fake-make" <<'SH'
#!/usr/bin/env bash
exit 0
SH

cat > "$FAKE_BIN/fake-curl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
url="${*: -1}"
if [[ "$url" == *'/host/runtime/endpoints' ]]; then
  printf '%s\n' '{"schemaVersion":1,"magicianApiBase":"http://127.0.0.1:3002/api/magician/v2","magicianHealthUrl":"http://127.0.0.1:3002/health","magicutorApiBase":"http://127.0.0.1:3003","magicutorBridgeUrl":"ws://127.0.0.1:3003/bridge/native"}'
fi
exit 0
SH

cat > "$FAKE_BIN/fake-docker" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  inspect) printf '%s\n' '[{"Name":"magician"}]' ;;
  logs) printf '%s\n' 'fake logs' ;;
esac
exit 0
SH

chmod +x "$TMP/setup.sh" "$TMP/install.sh" "$TMP/verify.sh" "$TMP/qualify.sh" "$FAKE_BIN/fake-make" "$FAKE_BIN/fake-curl" "$FAKE_BIN/fake-docker"

COMMON_ENV=(
  MAGICIAN_E2E_SETUP_SCRIPT="$TMP/setup.sh"
  MAGICIAN_E2E_INSTALL_SCRIPT="$TMP/install.sh"
  MAGICIAN_E2E_VERIFY_SCRIPT="$TMP/verify.sh"
  MAGICIAN_E2E_QUALIFICATION_SCRIPT="$TMP/qualify.sh"
  MAGICIAN_E2E_MAKE_BIN="$FAKE_BIN/fake-make"
  MAGICIAN_E2E_CURL_BIN="$FAKE_BIN/fake-curl"
  MAGICIAN_E2E_RUNTIME_CLI="$FAKE_BIN/fake-docker"
)

env "${COMMON_ENV[@]}" bash "$REPO/scripts/qualify-container-e2e.sh" \
  --runtime docker \
  --skip-runtime-setup \
  --skip-build \
  --report-dir "$TMP/isolated-report"
jq -e '.result == "pass" and .mode == "isolated" and (.stages | length) == 2' "$TMP/isolated-report/summary.json" >/dev/null

if env "${COMMON_ENV[@]}" MAGICIAN_FAKE_QUALIFY_FAIL=1 \
  bash "$REPO/scripts/qualify-container-e2e.sh" \
  --runtime docker --skip-runtime-setup --skip-build \
  --report-dir "$TMP/failed-isolated-report"; then
  echo "failed qualification unexpectedly passed" >&2
  exit 1
fi
jq -e '.result == "fail" and (.stages[-1].status == "fail")' "$TMP/failed-isolated-report/summary.json" >/dev/null

LIVE_ROOT="$TMP/live-root"
mkdir -p "$LIVE_ROOT"
printf 'operator-owned-config\n' > "$LIVE_ROOT/magician-config.yaml"
printf 'operator-owned-router\n' > "$LIVE_ROOT/llm-router.yaml"
before_hash="$(shasum -a 256 "$LIVE_ROOT/magician-config.yaml" | awk '{print $1}')"

env "${COMMON_ENV[@]}" bash "$REPO/scripts/qualify-container-e2e.sh" \
  --runtime docker \
  --skip-runtime-setup \
  --live \
  --yes \
  --root "$LIVE_ROOT" \
  --report-dir "$TMP/live-report"
after_hash="$(shasum -a 256 "$LIVE_ROOT/magician-config.yaml" | awk '{print $1}')"
[[ "$before_hash" == "$after_hash" ]]
jq -e '.result == "pass" and .mode == "live" and (.stages | length) == 10' "$TMP/live-report/summary.json" >/dev/null
jq -e '.totals.failed == 0' "$TMP/live-report/qualification.json" >/dev/null
jq -e '.files[] | select(.path == "llm-router.yaml" and .state == "present" and (.sha256 | length) == 64)' "$TMP/live-report/live-root-before.json" >/dev/null

CLOBBER_ROOT="$TMP/clobber-root"
mkdir -p "$CLOBBER_ROOT"
printf 'must-survive\n' > "$CLOBBER_ROOT/magician-config.yaml"
if env "${COMMON_ENV[@]}" MAGICIAN_FAKE_OVERWRITE=1 \
  bash "$REPO/scripts/qualify-container-e2e.sh" \
  --runtime docker --skip-runtime-setup --live --yes --root "$CLOBBER_ROOT" \
  --report-dir "$TMP/clobber-report"; then
  echo "live-root clobber unexpectedly passed" >&2
  exit 1
fi
jq -e '.result == "fail" and (.stages[-1].name == "Live root non-clobbering") and (.stages[-1].status == "fail")' "$TMP/clobber-report/summary.json" >/dev/null

if env "${COMMON_ENV[@]}" bash "$REPO/scripts/qualify-container-e2e.sh" \
  --runtime docker --skip-runtime-setup --live --root "$LIVE_ROOT" \
  --report-dir "$TMP/unconfirmed-report" </dev/null; then
  echo "live mode unexpectedly ran without --yes" >&2
  exit 1
fi

printf 'container E2E orchestrator tests passed\n'
