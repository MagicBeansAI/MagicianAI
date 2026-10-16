#!/usr/bin/env bash
# install-verify.sh — standalone health sweep for the magician stack.
#
# Curls each surface, prints OK/FAIL per line, and exits non-zero if any check
# that is EXPECTED for the current flow fails. Dependency-free (bash + curl).
#
# Required (always):       magician, magicutor
# Required (flow=local):   pi CLI at the pinned version (harness engine)
# Expected (fail on miss): Ollama generation + embedding,
#                          funnel (only when MAGICIAN_ENABLE_FUNNEL=1)
# Informational only:      host gateway (macOS-only; printed, never fails)
# --core checks the native backend, Magicutor, pinned Pi, resident embedder and
# macOS host gateway. Optional generation models and capabilities are separate.
# --core --wait SECONDS retries startup readiness for up to SECONDS (max 600).
#
# MAGICIAN_INSTALL_FLOW=local|container labels the run. It changes one check:
# the pi CLI is required only for flow=local, where the runtime runs on the host
# and finds engines on the host PATH. Every other surface runs on the host in
# both flows.
#
# Overrides:
#   MAGICIAN_VERIFY_MAGICUTOR_URL — probe magicutor at this URL instead of the
#     default 127.0.0.1:3003. install.sh's apple-container run exports the
#     container's vmnet IP here (apple-container does NOT bind the host loopback),
#     so a healthy apple stack passes the REQUIRED magicutor check.
#   MAGICIAN_ENABLE_FUNNEL=1 — add a REQUIRED Cloudflare-Tunnel health check; its
#     URL comes from MAGICIAN_FUNNEL_URL, else a state file ($MAGICIAN_ROOT_DIR/
#     funnel-url) written by scripts/ensure-magician-tunnel.sh.
set -uo pipefail

CORE_ONLY=0
WAIT_SECONDS=0
NATIVE_PREFIX=""
while [ $# -gt 0 ]; do
  case "$1" in
    --core) CORE_ONLY=1; shift;;
    --native-prefix)
      [ $# -ge 2 ] || { echo "--native-prefix needs a path" >&2; exit 2; }
      NATIVE_PREFIX="$2"; shift 2;;
    --wait)
      [ $# -ge 2 ] || { echo "--wait needs seconds" >&2; exit 2; }
      WAIT_SECONDS="$2"; shift 2;;
    *) echo "Unknown verification argument: $1" >&2; exit 2;;
  esac
done
case "$WAIT_SECONDS" in ''|*[!0-9]*) echo "--wait must be an integer" >&2; exit 2;; esac
[ "$WAIT_SECONDS" -le 600 ] || { echo "--wait must be at most 600 seconds" >&2; exit 2; }
if [ "$WAIT_SECONDS" -gt 0 ] && [ "$CORE_ONLY" != 1 ]; then
  echo "--wait is supported with --core" >&2; exit 2
fi

FLOW="${MAGICIAN_INSTALL_FLOW:-local}"

MAGICIAN_URL="http://127.0.0.1:3002/health"
MAGICUTOR_URL="${MAGICIAN_VERIFY_MAGICUTOR_URL:-http://127.0.0.1:3003/health}"
GATEWAY_URL="http://127.0.0.1:3017/host/automation/status"
OLLAMA_GENERATION_URL="http://127.0.0.1:11434/api/tags"
OLLAMA_EMBEDDING_URL="http://127.0.0.1:11435/api/ps"

# Funnel health check (opt-in). URL precedence: explicit env > runtime-root state
# file. Left empty when neither is present (we then warn instead of probing).
ENABLE_FUNNEL="${MAGICIAN_ENABLE_FUNNEL:-0}"

FUNNEL_URL="${MAGICIAN_FUNNEL_URL:-}"
if [ -z "$FUNNEL_URL" ]; then
  _funnel_root="${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}"
  if [ -f "$_funnel_root/funnel-url" ]; then
    FUNNEL_URL="$(head -1 "$_funnel_root/funnel-url" 2>/dev/null || true)"
  fi
fi

GREEN=$'\033[1;32m'; RED=$'\033[1;31m'; DIM=$'\033[2m'; RESET=$'\033[0m'

FAILURES=0

# check_strict NAME URL  — pass only on an HTTP 2xx/3xx (curl -sf).
check_strict() {
  local name="$1" url="$2"
  if curl -sf --max-time 3 -o /dev/null "$url"; then
    printf '  %sOK%s   %-13s %s\n' "$GREEN" "$RESET" "$name" "$url"
    return 0
  fi
  printf '  %sFAIL%s %-13s %s\n' "$RED" "$RESET" "$name" "$url"
  return 1
}

# check_any NAME URL  — pass on ANY HTTP response (even 4xx/5xx); FAIL only on
# a connection-level failure. curl writes %{http_code}=000 when it can't reach
# the host; on connect failure it also exits non-zero, so we must NOT append a
# fallback (that would concatenate onto the printed 000 and mask the failure).
check_any() {
  local name="$1" url="$2" code
  code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 3 "$url" 2>/dev/null)"
  if [ -n "$code" ] && [ "$code" != "000" ]; then
    printf '  %sOK%s   %-13s %s (HTTP %s)\n' "$GREEN" "$RESET" "$name" "$url" "$code"
    return 0
  fi
  printf '  %sFAIL%s %-13s %s (no response)\n' "$RED" "$RESET" "$name" "$url"
  return 1
}

# check_info NAME URL — informational; print OK/—, never affects exit code.
check_info() {
  local name="$1" url="$2"
  if curl -sf --max-time 3 -o /dev/null "$url"; then
    printf '  %sOK%s   %-13s %s\n' "$GREEN" "$RESET" "$name" "$url"
  else
    printf '  %s—    %-13s %s (not running — informational)%s\n' "$DIM" "$name" "$url" "$RESET"
  fi
}

# check_bin NAME BIN — verify a required host binary exists and runs.
# Binaries are not HTTP surfaces, so the curl-based helpers above cannot see
# them; media_edit shells out to ffmpeg and fails at runtime without it.
check_bin() {
  local name="$1" bin="$2"
  if command -v "$bin" >/dev/null 2>&1 && "$bin" -version >/dev/null 2>&1; then
    printf '  %sOK%s   %-13s %s\n' "$GREEN" "$RESET" "$name" "$(command -v "$bin")"
    return 0
  fi
  printf '  %sFAIL%s %-13s not found or not runnable (macOS: make setup-ffmpeg)\n' "$RED" "$RESET" "$name"
  return 1
}

# check_pi — the Pi CLI must be on PATH at the version setup-pi-coding-agent.sh
# pins (read from that script so the two cannot drift). The runtime lists a
# harness engine only when its binary is on PATH.
check_pi() {
  local want line
  want="$(sed -n 's/^PI_VERSION="\${MAGICIAN_PI_VERSION:-\([^}]*\)}"$/\1/p' "$(dirname "${BASH_SOURCE[0]}")/setup-pi-coding-agent.sh")"
  want="${MAGICIAN_PI_VERSION:-$want}"
  if ! command -v pi >/dev/null 2>&1; then
    printf '  %sFAIL%s %-13s not on PATH (run: make setup-pi-coding-agent)\n' "$RED" "$RESET" "pi"
    return 1
  fi
  line="$(pi --version 2>/dev/null | head -1)"
  if [ -n "$want" ] && ! printf '%s' "$line" | grep -Eq "(^|[^0-9])v?${want//./\\.}([^0-9]|$)"; then
    printf '  %sFAIL%s %-13s %s is %s, want %s (run: make setup-pi-coding-agent)\n' "$RED" "$RESET" "pi" "$(command -v pi)" "${line:-unknown}" "$want"
    return 1
  fi
  printf '  %sOK%s   %-13s %s (%s)\n' "$GREEN" "$RESET" "pi" "$(command -v pi)" "$line"
}

# The dedicated embedding daemon is only healthy for retrieval when its model
# is resident. `/api/ps` returning an empty model array is therefore a failure.
check_ollama_resident() {
  local name="$1" url="$2" body
  body="$(curl -sf --max-time 3 "$url" 2>/dev/null || true)"
  if printf '%s' "$body" | grep -Eq '"models"[[:space:]]*:[[:space:]]*\[[[:space:]]*\{'; then
    printf '  %sOK%s   %-13s %s (model resident)\n' "$GREEN" "$RESET" "$name" "$url"
    return 0
  fi
  printf '  %sFAIL%s %-13s %s (no resident model)\n' "$RED" "$RESET" "$name" "$url"
  return 1
}

core_sweep() {
  local failures=0 binding_report
  if [ -n "$NATIVE_PREFIX" ]; then
    if ! binding_report="$(python3 "$(dirname "${BASH_SOURCE[0]}")/native-runtime-service.py" verify \
      --prefix "$NATIVE_PREFIX" --data-dir "${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}" 2>&1)"; then
      printf '  %sFAIL%s native-service %s\n' "$RED" "$RESET" "$binding_report"
      failures=$((failures + 1))
    fi
  fi
  check_strict "magician" "$MAGICIAN_URL" || failures=$((failures + 1))
  check_strict "magicutor" "$MAGICUTOR_URL" || failures=$((failures + 1))
  if [ "$FLOW" = local ]; then check_pi || failures=$((failures + 1)); fi
  check_ollama_resident "ollama-embed" "$OLLAMA_EMBEDDING_URL" || failures=$((failures + 1))
  if [ "$(uname -s)" = Darwin ]; then
    check_strict "host-gateway" "$GATEWAY_URL" || failures=$((failures + 1))
  fi
  [ "$failures" -eq 0 ]
}

if [ "$CORE_ONLY" = 1 ]; then
  deadline=$((SECONDS + WAIT_SECONDS))
  while true; do
    if report="$(core_sweep)"; then
      printf '%s\n%s==> Required native services and memory are ready.%s\n' "$report" "$GREEN" "$RESET"
      exit 0
    fi
    if [ "$SECONDS" -ge "$deadline" ]; then
      printf '%s\n%s==> Required native services are not ready.%s\n' "$report" "$RED" "$RESET"
      exit 1
    fi
    printf 'Waiting for native services and the local memory model…\n'
    sleep 2
  done
fi

printf '\n\033[1;36m==> Magician health sweep (flow=%s)\033[0m\n\n' "$FLOW"

printf '%sRequired:%s\n' "$DIM" "$RESET"
check_strict "magician"  "$MAGICIAN_URL"  || FAILURES=$((FAILURES + 1))
check_strict "magicutor" "$MAGICUTOR_URL" || FAILURES=$((FAILURES + 1))
if [ "$FLOW" = local ]; then
  check_pi || FAILURES=$((FAILURES + 1))
fi

printf '\n%sExpected:%s\n' "$DIM" "$RESET"
check_strict "ollama-gen"   "$OLLAMA_GENERATION_URL" || FAILURES=$((FAILURES + 1))
check_ollama_resident "ollama-embed" "$OLLAMA_EMBEDDING_URL" || FAILURES=$((FAILURES + 1))
check_bin "ffmpeg"  "ffmpeg"  || FAILURES=$((FAILURES + 1))
check_bin "ffprobe" "ffprobe" || FAILURES=$((FAILURES + 1))
if bash "$(dirname "${BASH_SOURCE[0]}")/setup-media-tools.sh" --check-binaries; then
  printf '  %sOK%s   %-13s required filters and encoders verified\n' "$GREEN" "$RESET" "media_edit"
else
  printf '  %sFAIL%s %-13s required filters or encoders unavailable\n' "$RED" "$RESET" "media_edit"
  FAILURES=$((FAILURES + 1))
fi

# Funnel — only when the public Cloudflare Tunnel was requested. Counts toward
# FAILURES so a requested-but-unreachable tunnel is a real failure. Uses check_any
# (not check_strict): the webhook endpoint is POST-only, so a GET legitimately
# returns 4xx/405 — ANY HTTP response proves cloudflared routed through to the
# live upstream; only a connection failure (000 / tunnel 530) is a real fault. If
# the URL could not be resolved (no env + no state file) we FAIL loudly.
if [ "$ENABLE_FUNNEL" = 1 ]; then
  if [ -n "$FUNNEL_URL" ]; then
    check_any "funnel" "$FUNNEL_URL" || FAILURES=$((FAILURES + 1))
  else
    printf '  %sFAIL%s %-13s (MAGICIAN_ENABLE_FUNNEL=1 but no MAGICIAN_FUNNEL_URL / funnel-url to probe)\n' "$RED" "$RESET" "funnel"
    FAILURES=$((FAILURES + 1))
  fi
fi

printf '\n%sInformational:%s\n' "$DIM" "$RESET"
check_info "host-gateway" "$GATEWAY_URL"

printf '\n'
if [ "$FAILURES" -eq 0 ]; then
  printf '%s==> All required + expected surfaces are up.%s\n' "$GREEN" "$RESET"
  exit 0
fi
printf '%s==> %d surface(s) down — stack is not fully healthy.%s\n' "$RED" "$FAILURES" "$RESET"
exit 1
