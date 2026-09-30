#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

SUPERVISOR_PORT="${SUPERVISOR_PORT:-8081}"
SERVICE_PORTS=(3002 3003)
LOG_FILE="${SUPERVISOR_LOG_FILE:-magician.log}"

ensure_runtime_config() {
    local runtime_root="${MAGICIAN_ROOT_DIR:-${MAGICIAN_STORAGE_PATH:-$HOME/MagicianNotes}}"
    local dest="$runtime_root/magician-config.yaml"
    local src="$ROOT_DIR/magician-config.yaml"
    local label="magician-config.yaml"

    if [ -e "$dest" ]; then
        return 0
    fi
    if [ ! -f "$src" ]; then
        src="$ROOT_DIR/share/seed/magician-config.yaml"
        label="packaged magician-config.yaml"
    fi
    if [ ! -f "$src" ]; then
        echo "warning: no config seed found for $dest" >&2
        return 0
    fi

    mkdir -p "$runtime_root"
    cp "$src" "$dest"
    echo "Seeded runtime config from $label: $dest"
}

seed_runtime_file_if_missing() {
    local src="$1"
    local dest="$2"
    local label="$3"

    if [ -e "$dest" ]; then
        return 0
    fi
    if [ ! -f "$src" ]; then
        echo "warning: seed source missing for $label: $src" >&2
        return 0
    fi

    mkdir -p "$(dirname "$dest")"
    cp "$src" "$dest"
    echo "Seeded $label: $dest"
}

ensure_runtime_default_harness_seeds() {
    local runtime_root="${MAGICIAN_ROOT_DIR:-${MAGICIAN_STORAGE_PATH:-$HOME/MagicianNotes}}"
    local seed_root="${MAGICIAN_SEED_ROOT:-$ROOT_DIR/magician_data_v3}"
    if [ ! -d "$seed_root/scopes" ] && [ -d "$ROOT_DIR/share/seed/scopes" ]; then
        seed_root="$ROOT_DIR/share/seed"
    fi
    local default_seed="$seed_root/scopes/anonymous/default"
    local default_root="$runtime_root/scopes/anonymous/default"

    seed_runtime_file_if_missing \
        "$default_seed/programs/harness_reliability.md" \
        "$default_root/programs/harness_reliability.md" \
        "default harness reliability program"

    local agent
    for agent in harness-sre cto internal-system-analyst; do
        seed_runtime_file_if_missing \
            "$default_seed/agent_runtime/agents/$agent/definition.agent.yaml" \
            "$default_root/agent_runtime/agents/$agent/definition.agent.yaml" \
            "default agent definition $agent"
    done
}

# Do not mask oversized synchronous frames or async poll chains with a larger
# process-wide spawned-thread stack. Rust reads RUST_MIN_STACK lazily, so remove
# any inherited value before the supervisor creates Tokio/Actix workers.
#
# A deliberately awkward emergency escape hatch remains while default-stack
# qualification rolls through production. It is opt-in, never inherited by
# accident, and is logged loudly below. Do not use it as a permanent fix.
unset RUST_MIN_STACK
if [ -n "${MAGICIAN_EMERGENCY_RUST_MIN_STACK:-}" ]; then
    RUST_MIN_STACK="$MAGICIAN_EMERGENCY_RUST_MIN_STACK"
    export RUST_MIN_STACK
fi

# Raise per-process file-descriptor limit. macOS default soft limit (256) is
# starvation territory for a multi-service Rust binary: at steady state we
# hold ~96 fds (12 actix workers' kqueues + per-scope DuckDB handles +
# tokio internals + listening sockets); boot-time peak briefly exceeds 256
# while the memory-index maintainer reads source files concurrently with
# agent-definition load. When boot peak hits the wall, the maintainer fails,
# the LanceDB manifest is never written, and every memory query for the
# rest of the process lifetime falls back to direct ranking (visible as a
# storm of WARN lines in magician.log:
# "Falling back to direct memory candidate ranking because LanceDB hybrid
#  retrieval is unavailable ... fallback_reason=memory_index_stale:missing_manifest").
#
# 8192 is ~80× steady state, survives reasonable per-scope linear growth,
# and is well under macOS's hard cap (kern.maxfilesperproc, default ~120k).
# Skip silently when the soft limit is already higher (e.g. operator
# already raised it system-wide via launchctl).
CURRENT_NOFILE="$(ulimit -n 2>/dev/null || echo 0)"
if [ "$CURRENT_NOFILE" = "unlimited" ] || [ "$CURRENT_NOFILE" -ge 8192 ] 2>/dev/null; then
    :
else
    if ulimit -n 8192 2>/dev/null; then
        echo "Raised soft NOFILE limit to 8192 (was $CURRENT_NOFILE)"
    else
        echo "warning: could not raise NOFILE limit above $CURRENT_NOFILE — memory-index maintainer may fail at boot" >&2
    fi
fi

ensure_bin() {
    local bin="$1"
    if [ ! -x "$bin" ]; then
        echo "Missing executable: $bin" >&2
        echo "Run 'make build-all-release' first." >&2
        exit 1
    fi
}

port_is_listening() {
    local port="$1"
    nc -z 127.0.0.1 "$port" >/dev/null 2>&1
}

get_port_holder() {
    local port="$1"
    local out pid cmd
    out="$(lsof -nP -iTCP:"$port" -sTCP:LISTEN -Fpc 2>/dev/null || true)"
    pid="$(printf '%s\n' "$out" | awk '/^p/ { sub(/^p/, "", $0); print; exit }')"
    cmd="$(printf '%s\n' "$out" | awk '/^c/ { sub(/^c/, "", $0); print; exit }')"

    if [ -n "$pid" ] && [ -n "$cmd" ]; then
        printf '%s %s\n' "$pid" "$cmd"
    fi
}

is_managed_listener() {
    local cmd="$1"
    case "$cmd" in
        magician|magician.bin|magicutor|magicutor.bin|magic-supervisor|magic-supervisor.bin)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

show_port_holder() {
    local port="$1"
    if command -v lsof >/dev/null 2>&1; then
        lsof -nP -iTCP:"$port" -sTCP:LISTEN 2>/dev/null || true
    else
        echo "Port $port is occupied; install lsof to see the listener." >&2
    fi
}

wait_for_port_clear() {
    local port="$1"
    local attempts=40
    local i
    for ((i = 0; i < attempts; i++)); do
        if ! port_is_listening "$port"; then
            return 0
        fi
        sleep 0.25
    done
    return 1
}

stop_stale_listener() {
    local port="$1"
    local holder pid cmd
    holder="$(get_port_holder "$port")"
    if [ -z "$holder" ]; then
        return 1
    fi

    pid="${holder%% *}"
    cmd="${holder#* }"

    if ! is_managed_listener "$cmd"; then
        return 1
    fi

    echo "Port $port is occupied by stale $cmd (pid $pid); stopping it first..."
    kill "$pid" 2>/dev/null || true
    if wait_for_port_clear "$port"; then
        return 0
    fi

    echo "Process $pid did not release port $port after SIGTERM; sending SIGKILL..."
    kill -9 "$pid" 2>/dev/null || true
    wait_for_port_clear "$port"
}

ensure_port_clear() {
    local port="$1"
    if ! port_is_listening "$port"; then
        return 0
    fi

    if stop_stale_listener "$port"; then
        return 0
    fi

    return 1
}

supervisor_status() {
    if [ -x ./supervisor-ctl ]; then
        SUPERVISOR_CTL_QUIET=1 ./supervisor-ctl status 2>/dev/null || true
    else
        ./magic-supervisor.bin client status 2>/dev/null || true
    fi
}

ensure_bin "./magic-supervisor.bin"
ensure_bin "./magician.bin"
ensure_bin "./magicutor.bin"
ensure_runtime_config
ensure_runtime_default_harness_seeds

cleanup_ollama() {
    bash scripts/stop-ollama.sh >/dev/null 2>&1 || true
}
trap cleanup_ollama EXIT

bash scripts/run-ollama.sh

status_output="$(supervisor_status)"
if printf '%s' "$status_output" | grep -q '"magician"' && printf '%s' "$status_output" | grep -q '"magicutor"'; then
    echo "Existing magic-supervisor detected on port $SUPERVISOR_PORT; stopping it first..."
    if [ -x ./supervisor-ctl ]; then
        SUPERVISOR_CTL_ASSUME_YES=1 SUPERVISOR_CTL_QUIET=1 ./supervisor-ctl shutdown >/dev/null
    else
        ./magic-supervisor.bin client shutdown >/dev/null
    fi
elif ! ensure_port_clear "$SUPERVISOR_PORT"; then
    echo "Port $SUPERVISOR_PORT is occupied by a non-responsive or foreign process:" >&2
    show_port_holder "$SUPERVISOR_PORT" >&2
    exit 1
fi

for port in "$SUPERVISOR_PORT" "${SERVICE_PORTS[@]}"; do
    if ! ensure_port_clear "$port"; then
        echo "Port $port is still occupied; stop the listener and retry." >&2
        show_port_holder "$port" >&2
        exit 1
    fi
done

if [ "${MAGICIAN_MEMORY_INDEX_PREWARM:-0}" = "1" ]; then
    echo "Prewarming scoped memory index before Magician starts..."
    make --no-print-directory memory-index-prewarm
else
    echo "Skipping automatic memory index rebuild (opt in with MAGICIAN_MEMORY_INDEX_PREWARM=1)"
fi

if [ -n "${RUST_MIN_STACK:-}" ]; then
    echo "WARNING: using emergency RUST_MIN_STACK=$RUST_MIN_STACK" >&2
else
    echo "Using ordinary Rust/Tokio spawned-thread stack defaults"
fi
RUST_LOG="${RUST_LOG:-info}" ./magic-supervisor.bin 2>&1 | tee "$LOG_FILE"
