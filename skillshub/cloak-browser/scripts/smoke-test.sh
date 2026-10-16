#!/usr/bin/env bash
# smoke-test.sh — verify CloakBrowser + agent-browser end-to-end without
# any Python daemon or wrapper. Uses the env-var integration path
# (AGENT_BROWSER_EXECUTABLE_PATH, AGENT_BROWSER_INIT_SCRIPTS, etc.).
#
# Steps:
#   1. Resolve CLOAKBROWSER_LICENSE_KEY from the current environment or the
#      runtime dotenv files without printing it
#   2. Run resolve.py to get binary path + args + init scripts
#   3. Set agent-browser env vars from that output
#   4. Open httpbin headers + eval fingerprint signals → verify clean
#   5. Open Google search → verify no captcha redirect
#   6. Open FlightAware example flight → verify content loads
#
# Exit 0 on full success, non-zero with a diagnostic otherwise.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SKILL_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
SKILLSHUB_DIR="$(cd "$SKILL_DIR/.." && pwd)"
VENV_PYTHON="$SKILLSHUB_DIR/.venv/bin/python"
AGENT_BROWSER_BIN="${AGENT_BROWSER_BIN:-$SKILLSHUB_DIR/browser/bin/agent-browser}"

load_license_from_env_file() {
  local env_file="$1"
  local line value first last

  [[ -f "$env_file" ]] || return 1
  while IFS= read -r line || [[ -n "$line" ]]; do
    line="${line%$'\r'}"
    case "$line" in
      CLOAKBROWSER_LICENSE_KEY=*)
        value="${line#CLOAKBROWSER_LICENSE_KEY=}"
        ;;
      export\ CLOAKBROWSER_LICENSE_KEY=*)
        value="${line#export CLOAKBROWSER_LICENSE_KEY=}"
        ;;
      *)
        continue
        ;;
    esac

    # dotenv values may be surrounded by whitespace or one matching pair of
    # quotes. Do not eval/source the file: the smoke test needs one secret, not
    # arbitrary shell execution or the rest of the runtime environment.
    value="${value#"${value%%[![:space:]]*}"}"
    value="${value%"${value##*[![:space:]]}"}"
    if (( ${#value} >= 2 )); then
      first="${value:0:1}"
      last="${value: -1}"
      if [[ ( "$first" == '"' && "$last" == '"' ) ||
            ( "$first" == "'" && "$last" == "'" ) ]]; then
        value="${value:1:${#value}-2}"
      fi
    fi
    [[ -n "$value" ]] || return 1
    export CLOAKBROWSER_LICENSE_KEY="$value"
    return 0
  done < "$env_file"

  return 1
}

load_runtime_license_key() {
  [[ -n "${CLOAKBROWSER_LICENSE_KEY:-}" ]] && return 0

  local runtime_root
  runtime_root="$("$VENV_PYTHON" "$SKILLSHUB_DIR/scripts/runtime_root_shim.py" --owner cloak-browser-smoke)"
  local env_file
  for env_file in "$runtime_root/.env.development" "$runtime_root/.env"; do
    if load_license_from_env_file "$env_file"; then
      echo "  license:      loaded from $env_file (value hidden)"
      return 0
    fi
  done

  echo "✗ CLOAKBROWSER_LICENSE_KEY is missing" >&2
  echo "  export it in the shell or add it to:" >&2
  echo "    $runtime_root/.env.development" >&2
  echo "    $runtime_root/.env" >&2
  echo "  the key value was not printed" >&2
  return 1
}

if [[ ! -x "$VENV_PYTHON" ]]; then
  echo "✗ venv python missing at $VENV_PYTHON" >&2
  echo "  run: make -C skillshub setup-python" >&2
  exit 1
fi
if [[ ! -x "$AGENT_BROWSER_BIN" ]]; then
  echo "✗ agent-browser missing at $AGENT_BROWSER_BIN" >&2
  exit 1
fi

echo "→ resolving CloakBrowser license"
load_runtime_license_key || exit 1

echo "→ resolving cloak-browser config"
RESOLVE_OUT="$("$VENV_PYTHON" "$SCRIPT_DIR/resolve.py")" || {
  echo "✗ cloak-resolve failed" >&2
  exit 1
}

BIN_PATH="$(printf '%s' "$RESOLVE_OUT" | "$VENV_PYTHON" -c 'import json,sys; print(json.load(sys.stdin)["binary_path"])')"
INIT_SCRIPTS="$(printf '%s' "$RESOLVE_OUT" | "$VENV_PYTHON" -c 'import json,sys; print(",".join(json.load(sys.stdin)["init_scripts"]))')"
ARGS_CSV="$(printf '%s' "$RESOLVE_OUT" | "$VENV_PYTHON" -c 'import json,sys; print(",".join(json.load(sys.stdin)["args"]))')"
VERSION="$(printf '%s' "$RESOLVE_OUT" | "$VENV_PYTHON" -c 'import json,sys; print(json.load(sys.stdin).get("version") or "?")')"

echo "  binary:       $BIN_PATH"
echo "  version:      $VERSION"
echo "  init scripts: $INIT_SCRIPTS"
echo "  chromium args:"
printf '    %s\n' "${ARGS_CSV//,/$'\n'    }"

OUT_DIR="$(mktemp -d -t cloak-smoke.XXXXXX)"
SCREENSHOT="$OUT_DIR/google.png"

run_ab() {
  AGENT_BROWSER_EXECUTABLE_PATH="$BIN_PATH" \
  AGENT_BROWSER_INIT_SCRIPTS="$INIT_SCRIPTS" \
  AGENT_BROWSER_ARGS="$ARGS_CSV" \
  AGENT_BROWSER_HEADED=false \
  "$AGENT_BROWSER_BIN" "$@"
}

echo
echo "→ fingerprint probe via httpbin"
PROBE_OUT="$(run_ab batch \
  "open https://httpbin.org/headers" \
  "eval 'JSON.stringify({wd: navigator.webdriver, ua: navigator.userAgent, chromeRuntime: !!(window.chrome && window.chrome.runtime), plugins: navigator.plugins.length})'")"
echo "$PROBE_OUT" | tail -5
if echo "$PROBE_OUT" | grep -q '"wd":true'; then
  echo "✗ navigator.webdriver still leaks as true"
  exit 2
fi
if echo "$PROBE_OUT" | grep -q 'HeadlessChrome'; then
  echo "✗ User-Agent still contains HeadlessChrome"
  exit 2
fi
echo "✓ fingerprint probe clean (wd=false, no HeadlessChrome)"

echo
echo "→ Google search (most aggressive bot detector we typically hit)"
GOOGLE_OUT="$(run_ab batch \
  "open 'https://www.google.com/search?q=akasa+air+QP1314+flight+status'" \
  "wait 1500" \
  "get title" \
  "get url" \
  "screenshot $SCREENSHOT")"
echo "$GOOGLE_OUT" | tail -8
if echo "$GOOGLE_OUT" | grep -q "google.com/sorry"; then
  echo "✗ Google captcha redirect — bot detection still firing"
  exit 3
fi
if echo "$GOOGLE_OUT" | grep -q "Google Search"; then
  echo "✓ Google served results normally"
else
  echo "⚠ Google response did not clearly show search results; review screenshot $SCREENSHOT"
fi

echo
echo "→ FlightAware (sanity check; should be wide-open)"
FA_OUT="$(run_ab batch \
  "open https://www.flightaware.com/live/flight/AKJ1314" \
  "wait 1500" \
  "get title")"
echo "$FA_OUT" | tail -4
if echo "$FA_OUT" | grep -qi "AKJ1314\|QP1314"; then
  echo "✓ FlightAware loaded flight page"
else
  echo "⚠ FlightAware response unexpected"
fi

echo
echo "✓ smoke test passed"
echo "  screenshot: $SCREENSHOT"
echo "  out dir:    $OUT_DIR"
