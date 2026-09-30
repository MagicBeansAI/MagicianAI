#!/usr/bin/env bash
#
# Live-eval preflight: verify (and optionally start/rebuild) everything the
# `make test-live-evals` suite depends on, then print a readiness table.
#
# Dependency buckets (see docs/components/scripts/README.md):
#   - Cloud key:  OPENAI_API_KEY for the agentic-contract lanes (Responses +
#                 Realtime). Cannot be auto-provisioned — only reported.
#   - Local model: TWO Ollama daemons — generation on :11434 (qwen3.8-ud2-mtp) and a
#                 dedicated embedding daemon on :11435 (pplx embed). `make
#                 run-ollama` starts BOTH and prewarms them.
#   - Server:     the Magician HTTP server on :3002 (monitor-live + freshness of
#                 the llm-observability lanes). Per request, when --fix is set
#                 the Magician service is rebuilt from source and restarted (a
#                 fast `make check-magician` guards the rebuild so a tree that
#                 does not compile is reported instead of burning a full build).
#
# Modes:
#   --check   (default) report only, no side effects.
#   --fix     start any down dependency; rebuild + restart Magician.
#   --no-rebuild  with --fix, start/restart Magician only if down (skip rebuild).
#
# Env overrides:
#   MAGICIAN_BASE_URL      (default http://127.0.0.1:3002)
#   OLLAMA_GEN_URL         (default http://127.0.0.1:11434)
#   OLLAMA_EMBED_URL       (default http://127.0.0.1:11435)
#   PREFLIGHT_GEN_MODEL    (default qwen3.8-ud2-mtp)
#   PREFLIGHT_EMBED_MODEL  (default: embedding_model from magician-config.yaml)
#   PREFLIGHT_OPENAI_KEY_ENV (default OPENAI_API_KEY)
#   MAGICIAN_BEARER_TOKEN   scoped API credential (optional in local open mode)
#
# Not using -e: every check must run so the table is complete even when one
# dependency is down.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# --------------------------------------------------------------------------
# Configuration / resolution
# --------------------------------------------------------------------------
MAGICIAN_BASE_URL="${MAGICIAN_BASE_URL:-http://127.0.0.1:3002}"
OLLAMA_GEN_URL="${OLLAMA_GEN_URL:-http://127.0.0.1:11434}"
OLLAMA_EMBED_URL="${OLLAMA_EMBED_URL:-http://127.0.0.1:11435}"
GEN_MODEL="${PREFLIGHT_GEN_MODEL:-qwen3.8-ud2-mtp}"
OPENAI_KEY_ENV="${PREFLIGHT_OPENAI_KEY_ENV:-OPENAI_API_KEY}"
MAGICIAN_BEARER_TOKEN="${MAGICIAN_BEARER_TOKEN:-}"
AUTH_HEADERS=()
if [[ -n "$MAGICIAN_BEARER_TOKEN" ]]; then
  AUTH_HEADERS=(-H "Authorization: Bearer $MAGICIAN_BEARER_TOKEN")
fi

# Embedding model tag: read from config so it tracks the source of truth.
EMBED_MODEL="${PREFLIGHT_EMBED_MODEL:-}"
if [[ -z "$EMBED_MODEL" ]]; then
  EMBED_MODEL="$(grep -E '^[[:space:]]*embedding_model:' magician-config.yaml 2>/dev/null \
    | head -1 | sed -E 's/.*embedding_model:[[:space:]]*//; s/[[:space:]]*$//')"
fi
: "${EMBED_MODEL:=hf.co/mykor/pplx-embed-v1-4b-GGUF:Q6_K}"

mode="check"
rebuild=1
for arg in "$@"; do
  case "$arg" in
    --check) mode="check" ;;
    --fix|--ensure) mode="fix" ;;
    --no-rebuild) rebuild=0 ;;
    -h|--help)
      sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "Unknown argument: $arg" >&2; exit 2 ;;
  esac
done

# --------------------------------------------------------------------------
# Presentation helpers
# --------------------------------------------------------------------------
if [[ -t 1 ]]; then
  C_OK="$(tput setaf 2 2>/dev/null || true)"; C_BAD="$(tput setaf 1 2>/dev/null || true)"
  C_WARN="$(tput setaf 3 2>/dev/null || true)"; C_DIM="$(tput setaf 8 2>/dev/null || true)"
  C_RST="$(tput sgr0 2>/dev/null || true)"
else
  C_OK=""; C_BAD=""; C_WARN=""; C_DIM=""; C_RST=""
fi
glyph() { case "$1" in ok) printf '%s' "${C_OK}✓${C_RST}";; bad) printf '%s' "${C_BAD}✗${C_RST}";; warn) printf '%s' "${C_WARN}⚠${C_RST}";; esac; }

# Result rows accumulate as "state\tlabel\tdetail\tremedy" (state: ok|warn|bad).
ROWS=()
add_row() { ROWS+=("$1"$'\t'"$2"$'\t'"$3"$'\t'"$4"); }

http_code() { curl -s -o /dev/null -m "${1:-6}" -w '%{http_code}' "$2" 2>/dev/null || echo "000"; }

ollama_has_model() {
  # $1 base url, $2 model tag. Returns 0 if the tag is served.
  curl -s -m 8 "$1/api/tags" 2>/dev/null \
    | PREFLIGHT_MODEL="$2" python3 -c 'import sys,json,os
try: d=json.load(sys.stdin)
except Exception: sys.exit(1)
want=os.environ["PREFLIGHT_MODEL"]
sys.exit(0 if any(m.get("name")==want for m in d.get("models",[])) else 1)' 2>/dev/null
}

# --------------------------------------------------------------------------
# Load the same dotenv files the live evals read, for the key check only.
# --------------------------------------------------------------------------
load_key_from_dotenv() {
  local f
  for f in "$HOME/MagicianNotes/.env.development" "$HOME/MagicianNotes/.env" \
           "$repo_root/.env.development" "$repo_root/.env"; do
    [[ -r "$f" ]] || continue
    local line
    line="$(grep -E "^[[:space:]]*(export[[:space:]]+)?${OPENAI_KEY_ENV}=" "$f" 2>/dev/null | tail -1)"
    if [[ -n "$line" ]]; then
      line="${line#*=}"; line="${line%\"}"; line="${line#\"}"; line="${line%\'}"; line="${line#\'}"
      [[ -n "$line" ]] && { printf '%s' "$f"; return 0; }
    fi
  done
  return 1
}

# --------------------------------------------------------------------------
# Checks
# --------------------------------------------------------------------------
KEY_STATE="bad"; GEN_STATE="bad"; EMBED_STATE="bad"; MAG_STATE="bad"

check_openai_key() {
  local val src
  val="$(printenv "$OPENAI_KEY_ENV" 2>/dev/null || true)"
  if [[ -n "$val" ]]; then
    KEY_STATE="ok"; add_row ok "OpenAI key ($OPENAI_KEY_ENV)" "set in environment" ""
  elif src="$(load_key_from_dotenv)"; then
    KEY_STATE="ok"; add_row ok "OpenAI key ($OPENAI_KEY_ENV)" "found in ${src/#$HOME/~}" ""
  else
    KEY_STATE="bad"
    add_row bad "OpenAI key ($OPENAI_KEY_ENV)" "not set" "export $OPENAI_KEY_ENV=... or add to ~/MagicianNotes/.env.development"
  fi
}

check_ollama() {
  # $1 label, $2 url, $3 model, sets the named state var via $4
  local label="$1" url="$2" model="$3" __out="$4" code
  code="$(http_code 5 "$url/api/tags")"
  if [[ "$code" != "200" ]]; then
    printf -v "$__out" '%s' "bad"
    add_row bad "Ollama $label ($url)" "daemon down (HTTP $code)" "make run-ollama"
    return
  fi
  if ollama_has_model "$url" "$model"; then
    printf -v "$__out" '%s' "ok"
    add_row ok "Ollama $label ($url)" "up; $model present" ""
  else
    printf -v "$__out" '%s' "warn"
    add_row warn "Ollama $label ($url)" "up but $model MISSING" "make run-ollama  (pulls + prewarms)"
  fi
}

check_magician() {
  local code
  code="$(curl -s -o /dev/null -m 8 -w '%{http_code}' \
    "${AUTH_HEADERS[@]}" \
    "$MAGICIAN_BASE_URL/api/magician/v3/monitors" 2>/dev/null || echo 000)"
  if [[ "$code" =~ ^2[0-9][0-9]$ ]]; then
    MAG_STATE="ok"; add_row ok "Magician server ($MAGICIAN_BASE_URL)" "up; monitors serving (HTTP $code)" ""
  elif [[ "$code" == "000" ]]; then
    MAG_STATE="bad"; add_row bad "Magician server ($MAGICIAN_BASE_URL)" "not reachable" "make run-supervisor  (or --fix)"
  else
    MAG_STATE="warn"; add_row warn "Magician server ($MAGICIAN_BASE_URL)" "up but monitors not served (HTTP $code) — stale build" "rebuild + restart (--fix)"
  fi
}

# --------------------------------------------------------------------------
# Fix actions
# --------------------------------------------------------------------------
supervisor_running() { pgrep -f 'magic-supervisor' >/dev/null 2>&1; }

fix_ollama() {
  echo "→ Starting/prewarming Ollama daemons (make run-ollama)…"
  make --no-print-directory run-ollama || echo "  ${C_WARN}run-ollama returned non-zero${C_RST}"
}

fix_magician() {
  if [[ "$rebuild" == "1" ]]; then
    echo "→ Guarding rebuild with a fast compile check (make check-magician)…"
    local checklog; checklog="$(mktemp -t preflight-check-magician.XXXXXX)"
    if ! make --no-print-directory check-magician >"$checklog" 2>&1; then
      echo "  ${C_BAD}✗ Magician does not compile — skipping rebuild.${C_RST}"
      echo "    (often uncommitted work-in-progress). Last errors:"
      grep -E '^error' "$checklog" | head -8 | sed 's/^/      /'
      echo "    Full log: $checklog"
      MAG_REBUILD_BLOCKED=1
      return
    fi
    rm -f "$checklog"
    echo "→ Rebuilding Magician (make build-magician-debug)…"
    if ! make --no-print-directory build-magician-debug; then
      echo "  ${C_BAD}build-magician-debug failed${C_RST}"; return
    fi
  fi
  if supervisor_running; then
    echo "→ Restarting Magician via supervisor (make restart-magician)…"
    make --no-print-directory restart-magician || echo "  ${C_WARN}restart-magician returned non-zero${C_RST}"
  else
    echo "→ Supervisor not running; starting it detached (make run-supervisor)…"
    local suplog; suplog="$repo_root/scripts/.preflight-supervisor.log"
    nohup make --no-print-directory run-supervisor >"$suplog" 2>&1 &
    echo "  supervisor log: $suplog"
  fi
  # Wait for the server to come back up (up to ~45s).
  local i code
  for i in $(seq 1 45); do
    code="$(curl -s -o /dev/null -m 3 -w '%{http_code}' \
      "${AUTH_HEADERS[@]}" \
      "$MAGICIAN_BASE_URL/api/magician/v3/monitors" 2>/dev/null || echo 000)"
    [[ "$code" =~ ^2[0-9][0-9]$ ]] && { echo "  ${C_OK}Magician healthy (HTTP $code)${C_RST}"; return; }
    sleep 1
  done
  echo "  ${C_WARN}Magician did not report healthy within 45s${C_RST}"
}

# --------------------------------------------------------------------------
# Run
# --------------------------------------------------------------------------
echo "Live-eval preflight  (mode: $mode$( [[ "$mode" == fix && "$rebuild" == 0 ]] && echo ', no-rebuild' ))"
echo "  generation model: $GEN_MODEL"
echo "  embedding model : $EMBED_MODEL"
echo

MAG_REBUILD_BLOCKED=0
check_openai_key
check_ollama "generation" "$OLLAMA_GEN_URL" "$GEN_MODEL" GEN_STATE
check_ollama "embedding " "$OLLAMA_EMBED_URL" "$EMBED_MODEL" EMBED_STATE
check_magician

if [[ "$mode" == "fix" ]]; then
  echo "── applying fixes ─────────────────────────────────────────"
  if [[ "$GEN_STATE" != "ok" || "$EMBED_STATE" != "ok" ]]; then fix_ollama; fi
  if [[ "$MAG_STATE" != "ok" || "$rebuild" == "1" ]]; then fix_magician; fi
  echo "── re-checking ────────────────────────────────────────────"
  ROWS=()
  check_openai_key
  check_ollama "generation" "$OLLAMA_GEN_URL" "$GEN_MODEL" GEN_STATE
  check_ollama "embedding " "$OLLAMA_EMBED_URL" "$EMBED_MODEL" EMBED_STATE
  check_magician
  echo
fi

# --------------------------------------------------------------------------
# Readiness table
# --------------------------------------------------------------------------
echo "Readiness"
echo "─────────────────────────────────────────────────────────────"
for row in "${ROWS[@]}"; do
  IFS=$'\t' read -r st label detail remedy <<<"$row"
  printf "  %s  %-34s %s\n" "$(glyph "$st")" "$label" "$detail"
  [[ -n "$remedy" && "$st" != "ok" ]] && printf "       %s↳ %s%s\n" "$C_DIM" "$remedy" "$C_RST"
done
if [[ "${MAG_REBUILD_BLOCKED:-0}" == "1" ]]; then
  printf "  %s  %-34s %s\n" "$(glyph warn)" "Magician rebuild" \
    "SKIPPED — tree does not compile; still running the previously-built binary"
fi
echo

# --------------------------------------------------------------------------
# Lane readiness — which live-eval buckets can run now
# --------------------------------------------------------------------------
cloud_ready="no";  [[ "$KEY_STATE" == "ok" ]] && cloud_ready="yes"
local_ready="no";  [[ "$GEN_STATE" == "ok" && "$EMBED_STATE" == "ok" ]] && local_ready="yes"
server_ready="no"; [[ "$MAG_STATE" == "ok" ]] && server_ready="yes"

lane_line() { # $1 yes/no  $2 label  $3 lanes
  local g; [[ "$1" == "yes" ]] && g="$(glyph ok)" || g="$(glyph bad)"
  printf "  %s  %-16s %s\n" "$g" "$2" "$3"
}
echo "Lane buckets"
echo "─────────────────────────────────────────────────────────────"
lane_line "$cloud_ready"  "Cloud (OpenAI)" "task-state, decision-metadata/rationale, terminal, native-tool, agent-tool-visibility"
lane_line "$local_ready"  "Local (ollama)" "compactor, memory-temperature, chat-context-retrieval, ollama-logical-chunking"
lane_line "$server_ready" "Server"         "monitor-live, llm-observability phase2f/3/4"
lane_line "yes"           "Network-only"   "content-retrieval, content-retrieval-runtime (always runnable)"
echo

if [[ "${MAG_REBUILD_BLOCKED:-0}" == "1" ]]; then
  echo "${C_WARN}Magician rebuild was requested but blocked (tree does not compile).${C_RST}"
  echo "The server is still up on its previous binary — evals will run against stale code."
  exit 1
fi
if [[ "$cloud_ready" == "yes" && "$local_ready" == "yes" && "$server_ready" == "yes" ]]; then
  echo "${C_OK}All buckets ready → make test-live-evals${C_RST}"
  exit 0
fi
echo "${C_WARN}Some buckets are not ready.${C_RST} Run with --fix to start/rebuild, or use LIVE_EVAL_ONLY=<slug> to run a ready subset."
exit 1
