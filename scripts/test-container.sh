#!/usr/bin/env bash
# Container release qualification for Docker and Apple Container.

set -euo pipefail

IMAGE_NAME="${CONTAINER_IMAGE_REF:-magician:test}"
CONTAINER_NAME="${CONTAINER_TEST_NAME:-magician-test}"
HOST_PORT_MAGICIAN="${CONTAINER_TEST_MAGICIAN_PORT:-13002}"
HOST_PORT_MAGICUTOR="${CONTAINER_TEST_MAGICUTOR_PORT:-13003}"
HEALTH_TIMEOUT="${CONTAINER_TEST_HEALTH_TIMEOUT:-90}"
CPU_LIMIT="${CONTAINER_TEST_CPU_LIMIT:-1}"
MEMORY_LIMIT="${CONTAINER_TEST_MEMORY_LIMIT:-2g}"
MAX_COLD_START_MS="${CONTAINER_MAX_COLD_START_MS:-}"
SKIP_BUILD=false
KEEP_DATA=false
RUNTIME="${CONTAINER_RUNTIME:-}"
REPORT_PATH="${CONTAINER_QUALIFICATION_REPORT:-}"
DATA_DIR=""
OWN_DATA_DIR=true
TMPDIR_BASE=""
PASSED=0
FAILED=0
SKIPPED=0
TESTS_RUN=0
COLD_START_MS=0
STARTED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
RESULTS_JSON='[]'

usage() {
    cat <<'EOF'
Usage: scripts/test-container.sh [options]

Options:
  --skip-build                    Use an existing image.
  --runtime docker|container      Runtime CLI; apple-container is an alias for container.
  --image IMAGE                   Image reference to build or qualify.
  --container-name NAME           Isolated qualification container name.
  --host-port-magician PORT       Host port mapped to container port 3002.
  --host-port-magicutor PORT      Host port mapped to container port 3003.
  --health-timeout SECONDS        Per-start health deadline.
  --cpus COUNT                    Runtime CPU limit used by the test container.
  --memory SIZE                   Runtime memory limit used by the test container.
  --max-cold-start-ms MS          Fail when health takes longer than this budget.
  --data-dir PATH                 Existing or fresh runtime root to preserve after the run.
  --keep-data                     Keep an auto-created test runtime root.
  --report PATH                   Write machine-readable JSON qualification evidence.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-build) SKIP_BUILD=true; shift ;;
        --runtime) RUNTIME="${2:?missing runtime}"; shift 2 ;;
        --image) IMAGE_NAME="${2:?missing image}"; shift 2 ;;
        --container-name) CONTAINER_NAME="${2:?missing container name}"; shift 2 ;;
        --host-port-magician) HOST_PORT_MAGICIAN="${2:?missing port}"; shift 2 ;;
        --host-port-magicutor) HOST_PORT_MAGICUTOR="${2:?missing port}"; shift 2 ;;
        --health-timeout) HEALTH_TIMEOUT="${2:?missing timeout}"; shift 2 ;;
        --cpus) CPU_LIMIT="${2:?missing CPU limit}"; shift 2 ;;
        --memory) MEMORY_LIMIT="${2:?missing memory limit}"; shift 2 ;;
        --max-cold-start-ms) MAX_COLD_START_MS="${2:?missing cold-start budget}"; shift 2 ;;
        --data-dir) DATA_DIR="${2:?missing data directory}"; OWN_DATA_DIR=false; shift 2 ;;
        --keep-data) KEEP_DATA=true; shift ;;
        --report) REPORT_PATH="${2:?missing report path}"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "ERROR: unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

for value in "$HOST_PORT_MAGICIAN" "$HOST_PORT_MAGICUTOR" "$HEALTH_TIMEOUT"; do
    [[ "$value" =~ ^[0-9]+$ ]] || { echo "ERROR: ports and timeout must be integers" >&2; exit 2; }
done
[[ -z "$MAX_COLD_START_MS" || "$MAX_COLD_START_MS" =~ ^[0-9]+$ ]] || { echo "ERROR: cold-start budget must be integer milliseconds" >&2; exit 2; }
[[ "$HOST_PORT_MAGICIAN" -ge 1024 && "$HOST_PORT_MAGICIAN" -le 65535 ]] || { echo "ERROR: invalid Magician host port" >&2; exit 2; }
[[ "$HOST_PORT_MAGICUTOR" -ge 1024 && "$HOST_PORT_MAGICUTOR" -le 65535 ]] || { echo "ERROR: invalid Magicutor host port" >&2; exit 2; }
[[ "$HOST_PORT_MAGICIAN" != "$HOST_PORT_MAGICUTOR" ]] || { echo "ERROR: service host ports must differ" >&2; exit 2; }
command -v jq >/dev/null 2>&1 || { echo "ERROR: jq is required" >&2; exit 2; }

info() { printf '       %s\n' "$*"; }
now_ms() { perl -MTime::HiRes=time -e 'printf("%d\n", time()*1000)'; }

record() {
    local status="$1" name="$2" detail="${3:-}"
    RESULTS_JSON="$(jq -cn --argjson current "$RESULTS_JSON" --arg status "$status" --arg name "$name" --arg detail "$detail" '$current + [{status:$status,name:$name,detail:$detail}]')"
    case "$status" in
        pass) PASSED=$((PASSED + 1)); TESTS_RUN=$((TESTS_RUN + 1)); printf '[PASS] %s\n' "$name" ;;
        fail) FAILED=$((FAILED + 1)); TESTS_RUN=$((TESTS_RUN + 1)); printf '[FAIL] %s%s\n' "$name" "${detail:+: $detail}" ;;
        skip) SKIPPED=$((SKIPPED + 1)); printf '[SKIP] %s%s\n' "$name" "${detail:+: $detail}" ;;
    esac
}

detect_runtime() {
    [[ "$RUNTIME" == "apple-container" ]] && RUNTIME="container"
    if [[ -n "$RUNTIME" ]]; then
        command -v "$RUNTIME" >/dev/null 2>&1 || { echo "ERROR: runtime '$RUNTIME' not found" >&2; exit 1; }
        return
    fi
    if command -v container >/dev/null 2>&1 && container system status >/dev/null 2>&1; then
        RUNTIME="container"
    elif command -v docker >/dev/null 2>&1; then
        RUNTIME="docker"
    else
        echo "ERROR: no healthy Docker or Apple Container runtime found" >&2
        exit 1
    fi
}

runtime_image_exists() {
    if [[ "$RUNTIME" == "docker" ]]; then
        docker image inspect "$IMAGE_NAME" >/dev/null 2>&1
    else
        container image inspect "$IMAGE_NAME" >/dev/null 2>&1
    fi
}

start_container() {
    local args=(run -d --name "$CONTAINER_NAME"
        -v "${DATA_DIR}:/data"
        -p "127.0.0.1:${HOST_PORT_MAGICIAN}:3002"
        -p "127.0.0.1:${HOST_PORT_MAGICUTOR}:3003"
        --cpus "$CPU_LIMIT"
        --memory "$MEMORY_LIMIT"
        -e MAGICIAN_ROOT_DIR=/data)
    if [[ "$RUNTIME" == "docker" ]]; then
        args+=(--restart unless-stopped)
    fi
    "$RUNTIME" "${args[@]}" "$IMAGE_NAME" >/dev/null
}

stop_container() {
    "$RUNTIME" stop "$CONTAINER_NAME" >/dev/null 2>&1 || true
    "$RUNTIME" rm "$CONTAINER_NAME" >/dev/null 2>&1 || true
}

wait_for_health() {
    local deadline=$((SECONDS + HEALTH_TIMEOUT))
    while (( SECONDS < deadline )); do
        curl -fsS "http://127.0.0.1:${HOST_PORT_MAGICIAN}/health" >/dev/null 2>&1 && return 0
        sleep 1
    done
    return 1
}

write_report() {
    [[ -n "$REPORT_PATH" ]] || return 0
    mkdir -p "$(dirname "$REPORT_PATH")"
    jq -n \
        --arg schema_version "1" \
        --arg started_at "$STARTED_AT" \
        --arg completed_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        --arg os "$(uname -s)" \
        --arg arch "$(uname -m)" \
        --arg runtime "$RUNTIME" \
        --arg image "$IMAGE_NAME" \
        --arg container_name "$CONTAINER_NAME" \
        --arg data_dir "$DATA_DIR" \
        --argjson magician_port "$HOST_PORT_MAGICIAN" \
        --argjson magicutor_port "$HOST_PORT_MAGICUTOR" \
        --argjson cold_start_ms "$COLD_START_MS" \
        --argjson max_cold_start_ms "${MAX_COLD_START_MS:-null}" \
        --argjson passed "$PASSED" \
        --argjson failed "$FAILED" \
        --argjson skipped "$SKIPPED" \
        --argjson tests "$RESULTS_JSON" \
        '{schema_version:($schema_version|tonumber),started_at:$started_at,completed_at:$completed_at,platform:{os:$os,arch:$arch},runtime:$runtime,image:$image,container_name:$container_name,data_dir:$data_dir,ports:{magician:$magician_port,magicutor:$magicutor_port},cold_start_ms:$cold_start_ms,max_cold_start_ms:$max_cold_start_ms,totals:{passed:$passed,failed:$failed,skipped:$skipped},tests:$tests}' \
        > "$REPORT_PATH"
    info "Qualification report: $REPORT_PATH"
}

cleanup() {
    local incoming_status=$?
    trap - EXIT INT TERM
    stop_container
    if [[ -n "$DATA_DIR" && -f "$DATA_DIR/test-persistence.txt" ]]; then
        record pass "Cleanup preserves runtime data"
    else
        record fail "Cleanup preserves runtime data" "persistence marker is missing"
    fi
    if [[ "$OWN_DATA_DIR" == "true" && "$KEEP_DATA" == "false" && -n "$TMPDIR_BASE" && -d "$TMPDIR_BASE" ]]; then
        rm -rf "$TMPDIR_BASE"
    fi
    write_report
    printf '\n===========================================\n'
    printf '  Results: %s passed, %s failed, %s skipped\n' "$PASSED" "$FAILED" "$SKIPPED"
    printf '===========================================\n'
    if (( FAILED > 0 || incoming_status != 0 )); then exit 1; fi
    exit 0
}
trap cleanup EXIT INT TERM

detect_runtime
if [[ "$RUNTIME" == "docker" ]]; then
    docker info >/dev/null 2>&1 || { echo "ERROR: Docker daemon is unavailable" >&2; exit 1; }
else
    container system status >/dev/null 2>&1 || { echo "ERROR: Apple Container service is unavailable" >&2; exit 1; }
fi

if [[ "$SKIP_BUILD" == "false" ]]; then
    info "Building $IMAGE_NAME with $RUNTIME"
    "$RUNTIME" build -t "$IMAGE_NAME" .
    record pass "Image build"
elif runtime_image_exists; then
    record pass "Existing image is inspectable"
else
    echo "ERROR: image '$IMAGE_NAME' does not exist for $RUNTIME" >&2
    exit 1
fi

if [[ -z "$DATA_DIR" ]]; then
    TMPDIR_BASE="$(mktemp -d)"
    DATA_DIR="$TMPDIR_BASE/runtime"
fi
mkdir -p "$DATA_DIR"
stop_container

start_ms="$(now_ms)"
start_container
if wait_for_health; then
    COLD_START_MS=$(( $(now_ms) - start_ms ))
    if [[ -n "$MAX_COLD_START_MS" && "$COLD_START_MS" -gt "$MAX_COLD_START_MS" ]]; then
        record fail "Cold start reaches health budget" "${COLD_START_MS}ms > ${MAX_COLD_START_MS}ms"
    else
        record pass "Cold start reaches health" "${COLD_START_MS}ms"
    fi
else
    record fail "Cold start reaches health" "timed out after ${HEALTH_TIMEOUT}s"
fi

if find "$DATA_DIR" -mindepth 1 -print -quit | grep -q .; then
    record pass "Fresh runtime root is initialized"
else
    record fail "Fresh runtime root is initialized" "no files were created"
fi

if curl -fsS "http://127.0.0.1:${HOST_PORT_MAGICIAN}/health" >/dev/null 2>&1; then
    record pass "Configured Magician host port responds"
else
    record fail "Configured Magician host port responds"
fi
if curl -sS -o /dev/null "http://127.0.0.1:${HOST_PORT_MAGICUTOR}/" 2>/dev/null; then
    record pass "Configured Magicutor host port accepts HTTP"
elif nc -z 127.0.0.1 "$HOST_PORT_MAGICUTOR" 2>/dev/null; then
    record pass "Configured Magicutor host port accepts TCP"
else
    record fail "Configured Magicutor host port is reachable"
fi

whoami_value="$("$RUNTIME" exec "$CONTAINER_NAME" whoami 2>/dev/null || true)"
if [[ -n "$whoami_value" && "$whoami_value" != "root" ]]; then
    record pass "Container runs as non-root" "$whoami_value"
else
    record fail "Container runs as non-root" "reported '${whoami_value:-unknown}'"
fi

if "$RUNTIME" exec "$CONTAINER_NAME" env -u AGENT_BROWSER_SKILLS_DIR \
    /data/scopes/anonymous/default/skills/browser/bin/agent-browser \
    skills get core --full >/dev/null 2>&1; then
    record pass "Scoped browser discovers bundled core skill"
else
    record fail "Scoped browser discovers bundled core skill"
fi

printf 'host-write-test\n' > "$DATA_DIR/host-test.txt"
container_read="$("$RUNTIME" exec "$CONTAINER_NAME" cat /data/host-test.txt 2>/dev/null || true)"
if [[ "$container_read" == "host-write-test" ]]; then
    record pass "Host write is visible in container"
else
    record fail "Host write is visible in container"
fi
"$RUNTIME" exec "$CONTAINER_NAME" sh -c 'printf "container-write-test\n" > /data/container-test.txt' >/dev/null 2>&1 || true
if [[ "$(cat "$DATA_DIR/container-test.txt" 2>/dev/null || true)" == "container-write-test" ]]; then
    record pass "Container write is visible on host"
else
    record fail "Container write is visible on host"
fi

inspect_json="$("$RUNTIME" inspect "$CONTAINER_NAME" 2>/dev/null || true)"
if printf '%s' "$inspect_json" | jq -e 'type == "array" or type == "object"' >/dev/null 2>&1; then
    record pass "Runtime inspect returns JSON"
else
    record fail "Runtime inspect returns JSON"
fi
if "$RUNTIME" logs "$CONTAINER_NAME" >/dev/null 2>&1; then
    record pass "Runtime logs are readable"
else
    record fail "Runtime logs are readable"
fi

if [[ "$RUNTIME" == "docker" ]]; then
    resources="$(docker inspect --format '{{.HostConfig.NanoCpus}}|{{.HostConfig.Memory}}' "$CONTAINER_NAME" 2>/dev/null || true)"
    IFS='|' read -r nano_cpus memory_bytes <<< "$resources"
    if [[ "${nano_cpus:-0}" -gt 0 && "${memory_bytes:-0}" -gt 0 ]]; then
        record pass "Docker CPU and memory limits are applied" "$resources"
    else
        record fail "Docker CPU and memory limits are applied" "$resources"
    fi
else
    if printf '%s' "$inspect_json" | jq -e '.. | objects | select((.cpus? // 0) > 0 and (.memoryInBytes? // 0) > 0)' >/dev/null 2>&1; then
        record pass "Apple Container CPU and memory limits are applied"
    else
        record fail "Apple Container CPU and memory limits are applied"
    fi
fi

config_path="$DATA_DIR/magician-config.yaml"
if [[ -f "$config_path" ]]; then
    if [[ "$OWN_DATA_DIR" == "true" ]]; then
        printf '\n# qualification-preservation-marker\n' >> "$config_path"
    fi
    config_hash_before="$(shasum -a 256 "$config_path" | awk '{print $1}')"
else
    config_hash_before=""
    record fail "Runtime config was seeded"
fi
router_path="$DATA_DIR/llm-router.yaml"
router_hash_before=""
if [[ -s "$router_path" ]]; then
    if [[ "$OWN_DATA_DIR" == "true" ]]; then
        printf '\n# qualification-router-preservation-marker\n' >> "$router_path"
    fi
    router_hash_before="$(shasum -a 256 "$router_path" | awk '{print $1}')"
    record pass "LLM router tables were seeded"
else
    record fail "LLM router tables were seeded"
fi
printf 'persistence-marker-%s\n' "$(date +%s)" > "$DATA_DIR/test-persistence.txt"
persistence_before="$(cat "$DATA_DIR/test-persistence.txt")"

stop_container
start_container
if wait_for_health; then
    record pass "Recreated container reaches health"
else
    record fail "Recreated container reaches health"
fi
config_hash_after="$(shasum -a 256 "$config_path" 2>/dev/null | awk '{print $1}')"
if [[ -n "$config_hash_before" && "$config_hash_before" == "$config_hash_after" ]]; then
    record pass "Existing runtime config is not overwritten"
else
    record fail "Existing runtime config is not overwritten"
fi
router_hash_after=""
if [[ -s "$router_path" ]]; then
    router_hash_after="$(shasum -a 256 "$router_path" | awk '{print $1}')"
fi
if [[ -n "$router_hash_before" && "$router_hash_before" == "$router_hash_after" ]]; then
    record pass "Existing LLM router tables are not overwritten"
else
    record fail "Existing LLM router tables are not overwritten"
fi
if [[ "$(cat "$DATA_DIR/test-persistence.txt" 2>/dev/null || true)" == "$persistence_before" ]]; then
    record pass "Runtime data survives container recreation"
else
    record fail "Runtime data survives container recreation"
fi

if [[ "$RUNTIME" == "docker" ]]; then
    docker kill "$CONTAINER_NAME" >/dev/null 2>&1 || true
    if wait_for_health; then
        record pass "Docker restart policy recovers after process kill"
    else
        record fail "Docker restart policy recovers after process kill"
    fi
else
    record skip "Automatic crash recovery" "owned by desktop reconciliation for Apple Container"
fi
