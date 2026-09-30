#!/usr/bin/env bash
# Real local container qualification for the composed Magician installation.

set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNTIME="auto"
IMAGE="magician:e2e"
LIVE=false
ASSUME_YES=false
SKIP_RUNTIME_SETUP=false
SKIP_BUILD=false
KEEP_DATA=false
INSTALL_MODE="dev"
LIVE_ROOT="${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}"
LIVE_CONTAINER_NAME="magician"
QUALIFICATION_CONTAINER_NAME="magician-e2e-qualification"
HOST_PORT_MAGICIAN="13002"
HOST_PORT_MAGICUTOR="13003"
PROBE_TIMEOUT="${MAGICIAN_CONTAINER_E2E_PROBE_TIMEOUT:-10}"
HEALTH_TIMEOUT="${MAGICIAN_CONTAINER_E2E_HEALTH_TIMEOUT:-120}"
REPORT_DIR=""
STARTED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)-$$"
RESULTS_JSON='[]'
FAILED_STAGES=0
FINALIZED=false
LIVE_MARKER_PATH=""

SETUP_SCRIPT="${MAGICIAN_E2E_SETUP_SCRIPT:-$REPO/scripts/setup-container-runtime.sh}"
INSTALL_SCRIPT="${MAGICIAN_E2E_INSTALL_SCRIPT:-$REPO/scripts/install.sh}"
VERIFY_SCRIPT="${MAGICIAN_E2E_VERIFY_SCRIPT:-$REPO/scripts/install-verify.sh}"
QUALIFICATION_SCRIPT="${MAGICIAN_E2E_QUALIFICATION_SCRIPT:-$REPO/scripts/test-container.sh}"
MAKE_BIN="${MAGICIAN_E2E_MAKE_BIN:-make}"
CURL_BIN="${MAGICIAN_E2E_CURL_BIN:-curl}"
RUNTIME_CLI_OVERRIDE="${MAGICIAN_E2E_RUNTIME_CLI:-}"

usage() {
  cat <<'EOF'
Usage: scripts/qualify-container-e2e.sh [options]

By default, this configures the selected runtime and performs a disposable,
real-container qualification on alternate host ports. Add --live to first run
the complete composed installer against the selected runtime root and leave the
installed stack running after qualification.

Options:
  --runtime auto|docker|apple-container  Runtime selection (default: auto).
  --image IMAGE                         Image built/pulled and qualified.
  --report-dir PATH                     Evidence directory.
  --skip-runtime-setup                  Do not install/start the runtime.
  --skip-build                          Qualify an image already in the runtime.
  --keep-data                           Preserve isolated qualification data.
  --host-port-magician PORT             Isolated Magician port (default: 13002).
  --host-port-magicutor PORT             Isolated Magicutor port (default: 13003).
  --live                                Exercise the composed installer and live root.
  --yes                                 Confirm live disruption non-interactively.
  --root PATH                           Live runtime root (default: ~/MagicianNotes).
  --install-mode dev|user               Build locally or pull a released image.
  --container-name NAME                 Installed live container name.
  -h, --help                            Show this help.

Live mode intentionally stops the native supervisor and desktop tray before
installation. Existing config/secrets are fingerprinted and must remain byte
identical. The installed stack is left running for manual application checks.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --runtime) RUNTIME="${2:?missing runtime}"; shift 2 ;;
    --image) IMAGE="${2:?missing image}"; shift 2 ;;
    --report-dir) REPORT_DIR="${2:?missing report directory}"; shift 2 ;;
    --skip-runtime-setup) SKIP_RUNTIME_SETUP=true; shift ;;
    --skip-build) SKIP_BUILD=true; shift ;;
    --keep-data) KEEP_DATA=true; shift ;;
    --host-port-magician) HOST_PORT_MAGICIAN="${2:?missing port}"; shift 2 ;;
    --host-port-magicutor) HOST_PORT_MAGICUTOR="${2:?missing port}"; shift 2 ;;
    --live) LIVE=true; shift ;;
    --yes) ASSUME_YES=true; shift ;;
    --root) LIVE_ROOT="${2:?missing runtime root}"; shift 2 ;;
    --install-mode) INSTALL_MODE="${2:?missing install mode}"; shift 2 ;;
    --container-name) LIVE_CONTAINER_NAME="${2:?missing container name}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) printf 'ERROR: unknown option: %s\n' "$1" >&2; usage >&2; exit 2 ;;
  esac
done

log() { printf '%s\n' "$*"; }
die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

resolve_runtime() {
  if [[ "$RUNTIME" != "auto" ]]; then
    return
  fi
  local os arch major
  os="$(uname -s)"
  arch="$(uname -m)"
  major=0
  if [[ "$os" == "Darwin" ]]; then
    major="$(sw_vers -productVersion 2>/dev/null | cut -d. -f1)"
    [[ "$major" =~ ^[0-9]+$ ]] || major=0
  fi
  if [[ "$os" == "Darwin" && "$arch" == "arm64" && "$major" -ge 26 ]]; then
    RUNTIME="apple-container"
  else
    RUNTIME="docker"
  fi
}

validate_inputs() {
  resolve_runtime
  case "$RUNTIME" in docker|apple-container) ;; *) die "invalid runtime '$RUNTIME'" ;; esac
  case "$INSTALL_MODE" in dev|user) ;; *) die "invalid install mode '$INSTALL_MODE'" ;; esac
  for value in "$HOST_PORT_MAGICIAN" "$HOST_PORT_MAGICUTOR" "$PROBE_TIMEOUT" "$HEALTH_TIMEOUT"; do
    [[ "$value" =~ ^[0-9]+$ ]] || die "ports and timeouts must be positive integers"
  done
  [[ "$HOST_PORT_MAGICIAN" != "$HOST_PORT_MAGICUTOR" ]] || die "isolated host ports must differ"
  [[ "$LIVE_CONTAINER_NAME" =~ ^[A-Za-z0-9][A-Za-z0-9_.-]*$ ]] || die "invalid container name"
  [[ -n "$IMAGE" ]] || die "image reference cannot be empty"
  command -v jq >/dev/null 2>&1 || die "jq is required"
  command -v "$CURL_BIN" >/dev/null 2>&1 || die "curl is required"
  [[ -f "$SETUP_SCRIPT" ]] || die "runtime setup script not found: $SETUP_SCRIPT"
  [[ -f "$INSTALL_SCRIPT" ]] || die "installer not found: $INSTALL_SCRIPT"
  [[ -f "$VERIFY_SCRIPT" ]] || die "verification script not found: $VERIFY_SCRIPT"
  [[ -f "$QUALIFICATION_SCRIPT" ]] || die "qualification script not found: $QUALIFICATION_SCRIPT"
  if [[ "$LIVE" == true ]]; then
    [[ "$LIVE_ROOT" == /* ]] || die "--root must be an absolute path in live mode"
    [[ "$LIVE_ROOT" != "/" ]] || die "refusing to use / as the runtime root"
  fi
  [[ -n "$REPORT_DIR" ]] || REPORT_DIR="$REPO/coverage/container-qualification/local-e2e/$RUN_ID"
  mkdir -p "$REPORT_DIR/logs"
}

record_stage() {
  local status="$1" name="$2" detail="${3:-}" log_path="${4:-}"
  RESULTS_JSON="$(jq -cn \
    --argjson current "$RESULTS_JSON" \
    --arg status "$status" \
    --arg name "$name" \
    --arg detail "$detail" \
    --arg log "$log_path" \
    '$current + [{status:$status,name:$name,detail:$detail,log:($log | select(length > 0))}]')"
  if [[ "$status" == "fail" ]]; then
    FAILED_STAGES=$((FAILED_STAGES + 1))
  fi
}

run_stage() {
  local name="$1"; shift
  local slug log_path status tee_status
  local pipeline_status=()
  slug="$(printf '%s' "$name" | tr '[:upper:] ' '[:lower:]_')"
  log_path="$REPORT_DIR/logs/${slug}.log"
  printf '\n==> %s\n' "$name"
  set +o pipefail
  "$@" 2>&1 | tee "$log_path"
  pipeline_status=("${PIPESTATUS[@]}")
  status="${pipeline_status[0]}"
  tee_status="${pipeline_status[1]:-0}"
  set -o pipefail
  if [[ "$status" -eq 0 && "$tee_status" -ne 0 ]]; then
    status="$tee_status"
  fi
  if [[ "$status" -eq 0 ]]; then
    record_stage pass "$name" "" "$log_path"
    return 0
  fi
  if [[ "$status" -eq 20 ]]; then
    record_stage skip "$name" "operator requested --skip-runtime-setup" "$log_path"
    return 0
  fi
  record_stage fail "$name" "exit $status" "$log_path"
  return "$status"
}

finalize() {
  local incoming_status=$? result="pass"
  [[ "$FINALIZED" == false ]] || return
  FINALIZED=true
  trap - EXIT
  if [[ "$incoming_status" -ne 0 || "$FAILED_STAGES" -ne 0 ]]; then
    result="fail"
  fi
  if [[ -n "$LIVE_MARKER_PATH" && -f "$LIVE_MARKER_PATH" ]]; then
    rm -f "$LIVE_MARKER_PATH"
    rmdir "$(dirname "$LIVE_MARKER_PATH")" 2>/dev/null || true
  fi
  mkdir -p "$REPORT_DIR"
  if ! jq -n \
    --argjson schema_version 1 \
    --arg run_id "$RUN_ID" \
    --arg started_at "$STARTED_AT" \
    --arg completed_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg result "$result" \
    --arg runtime "$RUNTIME" \
    --arg image "$IMAGE" \
    --arg mode "$([[ "$LIVE" == true ]] && printf live || printf isolated)" \
    --arg live_root "$([[ "$LIVE" == true ]] && printf '%s' "$LIVE_ROOT" || true)" \
    --arg qualification_report "$REPORT_DIR/qualification.json" \
    --argjson stages "$RESULTS_JSON" \
    '{schema_version:$schema_version,run_id:$run_id,started_at:$started_at,completed_at:$completed_at,result:$result,runtime:$runtime,image:$image,mode:$mode,live_root:(if $live_root == "" then null else $live_root end),qualification_report:$qualification_report,stages:$stages}' \
    > "$REPORT_DIR/summary.json"; then
    printf 'ERROR: failed to write E2E summary report\n' >&2
    incoming_status=1
  fi
  printf '\nEvidence: %s\n' "$REPORT_DIR/summary.json"
  [[ "$result" == "pass" ]] || incoming_status=1
  exit "$incoming_status"
}
trap finalize EXIT

confirm_live_run() {
  [[ "$LIVE" == true ]] || return 0
  if [[ "$ASSUME_YES" == true ]]; then
    return 0
  fi
  [[ -t 0 ]] || die "--live requires --yes in a non-interactive shell"
  printf '\nLive qualification will stop the native dev stack, replace container %s,\n' "$LIVE_CONTAINER_NAME"
  printf 'and run the installer against %s. Existing tracked config must remain unchanged.\n' "$LIVE_ROOT"
  printf 'Type LIVE to continue: '
  local answer
  read -r answer
  [[ "$answer" == "LIVE" ]] || die "live qualification cancelled"
}

stage_runtime_setup() {
  if [[ "$SKIP_RUNTIME_SETUP" == true ]]; then
    log "Runtime setup skipped by operator."
    return 20
  fi
  MAGICIAN_CONTAINER_RUNTIME="$RUNTIME" bash "$SETUP_SCRIPT"
}

hash_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

write_root_fingerprint() {
  local output="$1" rel state hash entries='[]'
  for rel in magician-config.yaml llm-router.yaml .env .env.development operator-config.yaml client_secret.json; do
    state="missing"
    hash=""
    if [[ -f "$LIVE_ROOT/$rel" ]]; then
      state="present"
      hash="$(hash_file "$LIVE_ROOT/$rel")"
    fi
    entries="$(jq -cn --argjson current "$entries" --arg path "$rel" --arg state "$state" --arg sha256 "$hash" '$current + [{path:$path,state:$state,sha256:($sha256 | select(length > 0))}]')"
  done
  jq -n --arg root "$LIVE_ROOT" --argjson files "$entries" '{root:$root,files:$files}' > "$output"
}

stage_snapshot_live_root() {
  mkdir -p "$LIVE_ROOT"
  write_root_fingerprint "$REPORT_DIR/live-root-before.json"
}

stage_stop_existing_stack() {
  "$MAKE_BIN" -C "$REPO" stop-supervisor >/dev/null 2>&1 || true
  "$MAKE_BIN" -C "$REPO" stop-desktop-tray >/dev/null 2>&1 || true
  log "Stopped existing native supervisor and desktop tray when present."
}

stage_composed_install() {
  MAGICIAN_INSTALL_MODE="$INSTALL_MODE" \
  MAGICIAN_INSTALL_FLOW="container" \
  MAGICIAN_CONTAINER_RUNTIME="$RUNTIME" \
  MAGICIAN_ROOT_DIR="$LIVE_ROOT" \
  MAGICIAN_IMAGE_REF="$IMAGE" \
  MAGICIAN_CONTAINER_NAME="$LIVE_CONTAINER_NAME" \
  MAGICIAN_INSTALL_YES=1 \
    bash "$INSTALL_SCRIPT"
}

stage_install_verify() {
  MAGICIAN_INSTALL_FLOW="container" MAGICIAN_ROOT_DIR="$LIVE_ROOT" bash "$VERIFY_SCRIPT"
}

wait_for_url() {
  local url="$1" deadline=$((SECONDS + HEALTH_TIMEOUT))
  while (( SECONDS < deadline )); do
    "$CURL_BIN" -fsS --max-time "$PROBE_TIMEOUT" -o /dev/null "$url" 2>/dev/null && return 0
    sleep 1
  done
  return 1
}

stage_host_contract() {
  local contract health_url magicutor_url
  wait_for_url "http://127.0.0.1:3002/health" || return 1
  "$CURL_BIN" -sS --max-time "$PROBE_TIMEOUT" -o /dev/null "http://127.0.0.1:3003/"
  if [[ "$(uname -s)" != "Darwin" ]]; then
    log "Linux headless flow has no desktop host-gateway contract."
    return 0
  fi
  wait_for_url "http://127.0.0.1:3017/host/runtime/endpoints" || return 1
  contract="$("$CURL_BIN" -fsS --max-time "$PROBE_TIMEOUT" "http://127.0.0.1:3017/host/runtime/endpoints")"
  printf '%s\n' "$contract" > "$REPORT_DIR/runtime-endpoints.json"
  printf '%s' "$contract" | jq -e '
    .schemaVersion == 1 and
    (.magicianApiBase | startswith("http://127.0.0.1:")) and
    (.magicianHealthUrl | startswith("http://127.0.0.1:")) and
    (.magicutorApiBase | startswith("http://127.0.0.1:")) and
    (.magicutorBridgeUrl | startswith("ws://127.0.0.1:"))
  ' >/dev/null
  health_url="$(printf '%s' "$contract" | jq -r '.magicianHealthUrl')"
  magicutor_url="$(printf '%s' "$contract" | jq -r '.magicutorApiBase')"
  "$CURL_BIN" -fsS --max-time "$PROBE_TIMEOUT" -o /dev/null "$health_url"
  "$CURL_BIN" -sS --max-time "$PROBE_TIMEOUT" -o /dev/null "$magicutor_url"
}

runtime_cli() {
  if [[ -n "$RUNTIME_CLI_OVERRIDE" ]]; then
    printf '%s' "$RUNTIME_CLI_OVERRIDE"
  elif [[ "$RUNTIME" == "apple-container" ]]; then
    printf 'container'
  else
    printf 'docker'
  fi
}

probe_from_container() {
  local url="$1" cli command
  cli="$(runtime_cli)"
  command="if command -v curl >/dev/null 2>&1; then curl -fsS --max-time $PROBE_TIMEOUT -o /dev/null '$url'; elif command -v wget >/dev/null 2>&1; then wget -q -T $PROBE_TIMEOUT -O /dev/null '$url'; else exit 127; fi"
  "$cli" exec "$LIVE_CONTAINER_NAME" sh -lc "$command"
}

stage_container_host_bridge() {
  local host cli
  cli="$(runtime_cli)"
  command -v "$cli" >/dev/null 2>&1 || die "runtime CLI not found: $cli"
  if [[ "$(uname -s)" == "Linux" ]]; then
    host="127.0.0.1"
  elif [[ "$RUNTIME" == "apple-container" ]]; then
    host="host.container.internal"
  else
    host="host.docker.internal"
  fi
  probe_from_container "http://$host:11434/api/tags"
  probe_from_container "http://$host:11435/api/ps"
  probe_from_container "http://$host:3021/"
  if [[ "$(uname -s)" == "Darwin" ]]; then
    probe_from_container "http://$host:3017/host/runtime/endpoints"
  fi
  "$cli" inspect "$LIVE_CONTAINER_NAME" > "$REPORT_DIR/live-container-inspect.json"
  "$cli" logs "$LIVE_CONTAINER_NAME" > "$REPORT_DIR/live-container.log" 2>&1
}

stage_restart_persistence() {
  local cli marker_dir marker_path marker_value
  cli="$(runtime_cli)"
  marker_dir="$LIVE_ROOT/.container-e2e"
  marker_path="$marker_dir/persistence-marker"
  LIVE_MARKER_PATH="$marker_path"
  marker_value="container-e2e-$RUN_ID"
  mkdir -p "$marker_dir"
  printf '%s\n' "$marker_value" > "$marker_path"
  "$cli" stop "$LIVE_CONTAINER_NAME" >/dev/null
  "$cli" start "$LIVE_CONTAINER_NAME" >/dev/null
  wait_for_url "http://127.0.0.1:3002/health" || return 1
  [[ "$(cat "$marker_path")" == "$marker_value" ]] || return 1
  MAGICIAN_INSTALL_FLOW="container" MAGICIAN_ROOT_DIR="$LIVE_ROOT" bash "$VERIFY_SCRIPT"
  rm -f "$marker_path"
  rmdir "$marker_dir" 2>/dev/null || true
  LIVE_MARKER_PATH=""
}

stage_verify_live_root_unchanged() {
  local before="$REPORT_DIR/live-root-before.json" after="$REPORT_DIR/live-root-after.json"
  local rel state expected actual
  write_root_fingerprint "$after"
  while IFS=$'\t' read -r rel state expected; do
    [[ "$state" == "present" ]] || continue
    actual="$(jq -r --arg path "$rel" '.files[] | select(.path == $path) | .sha256 // ""' "$after")"
    if [[ -z "$actual" || "$actual" != "$expected" ]]; then
      printf 'Tracked live-root file changed: %s\n' "$rel" >&2
      return 1
    fi
  done < <(jq -r '.files[] | [.path,.state,(.sha256 // "")] | @tsv' "$before")
}

stage_isolated_qualification() {
  local args=(
    --runtime "$RUNTIME"
    --image "$IMAGE"
    --container-name "$QUALIFICATION_CONTAINER_NAME"
    --host-port-magician "$HOST_PORT_MAGICIAN"
    --host-port-magicutor "$HOST_PORT_MAGICUTOR"
    --health-timeout "$HEALTH_TIMEOUT"
    --report "$REPORT_DIR/qualification.json"
  )
  if [[ "$LIVE" == true || "$SKIP_BUILD" == true ]]; then
    args+=(--skip-build)
  fi
  [[ "$KEEP_DATA" == true ]] && args+=(--keep-data)
  bash "$QUALIFICATION_SCRIPT" "${args[@]}"
  jq -e '.totals.failed == 0' "$REPORT_DIR/qualification.json" >/dev/null
}

validate_inputs
confirm_live_run

log "Magician container E2E qualification"
log "  runtime: $RUNTIME"
log "  image:   $IMAGE"
log "  mode:    $([[ "$LIVE" == true ]] && printf live || printf isolated)"
log "  report:  $REPORT_DIR"

run_stage "Runtime setup" stage_runtime_setup || exit 1

if [[ "$LIVE" == true ]]; then
  run_stage "Snapshot live root" stage_snapshot_live_root || exit 1
  run_stage "Stop existing stack" stage_stop_existing_stack || exit 1
  run_stage "Composed live installation" stage_composed_install || exit 1
  run_stage "Installed stack health" stage_install_verify || exit 1
  run_stage "Host runtime endpoint contract" stage_host_contract || exit 1
  run_stage "Container to host service bridge" stage_container_host_bridge || exit 1
  run_stage "Installed container restart and persistence" stage_restart_persistence || exit 1
  run_stage "Live root non-clobbering" stage_verify_live_root_unchanged || exit 1
fi

run_stage "Isolated real-container qualification" stage_isolated_qualification || exit 1

log "Container E2E qualification passed."
if [[ "$LIVE" == true ]]; then
  log "The installed stack remains running. Manual gates: approve macOS TCC prompts, load/pair the Chrome extension, and exercise one Chat/memory/browser workflow."
fi
