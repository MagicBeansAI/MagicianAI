#!/usr/bin/env bash
# setup-ollama-host.sh - install Ollama on the HOST and pull the models Magician uses.
#
# Topology: SilverBullet + Ollama run on the HOST (not inside the magician
# container). Generation remains on localhost:11434. Embeddings use the
# dedicated, pinned daemon on localhost:11435.
#
# Idempotent. Model tags are resolved from runtime.ollama embedding settings
# and Ollama profiles referenced by llm.router.operation_mapping.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DATA_DIR="${MAGICIAN_ROOT_DIR:-${MAGICIAN_STORAGE_PATH:-$HOME/MagicianNotes}}"
OLLAMA_URL="${MAGICIAN_OLLAMA_URL:-http://127.0.0.1:11434}"

CONFIG_PATH="${MAGICIAN_CONFIG_PATH:-}"
if [[ -z "$CONFIG_PATH" ]]; then
  for candidate in \
    "$DATA_DIR/magician-config.yaml" \
    "$ROOT_DIR/magician-config.yaml"; do
    if [[ -f "$candidate" ]]; then
      CONFIG_PATH="$candidate"
      break
    fi
  done
fi
if [[ -z "$CONFIG_PATH" || ! -f "$CONFIG_PATH" ]]; then
  printf '  ERROR magician-config.yaml was not found\n' >&2
  exit 1
fi
if ! command -v ruby >/dev/null 2>&1; then
  printf '  ERROR ruby is required to resolve Ollama profiles from %s\n' "$CONFIG_PATH" >&2
  exit 1
fi
# `resolve-ollama-config.rb` reads router["profiles"] and
# router["operation_mapping"], which live in a sibling `llm-router.yaml` rather
# than in the config. Splice through the one Python implementation instead of
# writing a third one in Ruby or shell.
SPLICED_CONFIG="$(mktemp -t magician-config-spliced)"
trap 'rm -f "$SPLICED_CONFIG"' EXIT
if ! python3 "$SCRIPT_DIR/magician_config_text.py" "$CONFIG_PATH" > "$SPLICED_CONFIG"; then
  printf '  ERROR could not assemble %s with its router tables\n' "$CONFIG_PATH" >&2
  exit 1
fi
CONFIG_OUTPUT="$(ruby "$SCRIPT_DIR/resolve-ollama-config.rb" "$SPLICED_CONFIG")"
read_config_key() {
  local key="$1"
  printf '%s\n' "$CONFIG_OUTPUT" | awk -F= -v key="$key" '$1 == key { sub(/^[^=]*=/, "", $0); print; exit }'
}
EMBEDDING_MODEL="$(read_config_key embedding_model)"

ok()   { printf '  OK %s\n' "$*"; }
err()  { printf '  ERROR %s\n' "$*" >&2; }
warn() { printf '  WARN %s\n' "$*" >&2; }
note() { printf '       %s\n' "$*"; }

# --- 0. what this machine is --------------------------------------------------
# The embedding model is not optional: memory is indexed on this machine in both
# processing modes. The generation model is, and whether it is offered at all is
# decided here rather than by whoever is watching the install.
host_os() {
  case "$(uname -s)" in
    Darwin) printf 'macos' ;;
    Linux)  printf 'linux' ;;
    *)      printf '%s' "$(uname -s | tr '[:upper:]' '[:lower:]')" ;;
  esac
}
host_memory_gb() {
  local bytes=""
  case "$(uname -s)" in
    Darwin) bytes="$(sysctl -n hw.memsize 2>/dev/null || true)" ;;
    Linux)
      local kb
      kb="$(awk '/^MemTotal:/ {print $2; exit}' /proc/meminfo 2>/dev/null || true)"
      [[ -n "$kb" ]] && bytes=$((kb * 1024))
      ;;
  esac
  # Unknown memory is reported as 0, which fails the gate. Guessing high here
  # would install a model the machine cannot run and blame the user for it.
  if [[ -z "$bytes" || "$bytes" -le 0 ]]; then printf '0'; else printf '%s' $((bytes / 1024 / 1024 / 1024)); fi
}
HOST_OS="$(host_os)"
HOST_ARCH="$(uname -m)"
HOST_MEMORY_GB="$(host_memory_gb)"

GATE_MIN_GB="$(read_config_key local_generation_min_memory_gb)"
GATE_ARCH="$(read_config_key local_generation_requires_arch)"
GATE_OS="$(read_config_key local_generation_requires_os)"
TIER_COUNT="$(read_config_key local_generation_tier_count)"
: "${GATE_MIN_GB:=0}" "${TIER_COUNT:=0}"

# --- 0b. does the generation model get installed at all? ----------------------
GENERATION_MODEL=""
GENERATION_SKIPPED_BECAUSE=""
SETUP_GENERATION="${MAGICIAN_OLLAMA_SETUP_GENERATION:-1}"
if [[ "$SETUP_GENERATION" =~ ^(0|false|no|off)$ ]]; then
  GENERATION_SKIPPED_BECAUSE="embedding-only setup was requested"
elif [[ "$TIER_COUNT" -eq 0 ]]; then
  GENERATION_SKIPPED_BECAUSE="no local_generation tiers are configured"
elif [[ -n "$GATE_OS" && "$HOST_OS" != "$GATE_OS" ]]; then
  GENERATION_SKIPPED_BECAUSE="this is $HOST_OS and local generation needs $GATE_OS"
elif [[ -n "$GATE_ARCH" && "$HOST_ARCH" != "$GATE_ARCH" ]]; then
  GENERATION_SKIPPED_BECAUSE="this is $HOST_ARCH and local generation needs $GATE_ARCH"
elif [[ "$HOST_MEMORY_GB" -eq 0 ]]; then
  GENERATION_SKIPPED_BECAUSE="this machine's memory could not be read, so the model that fits cannot be chosen"
elif [[ "$HOST_MEMORY_GB" -lt "$GATE_MIN_GB" ]]; then
  GENERATION_SKIPPED_BECAUSE="this machine has ${HOST_MEMORY_GB} GB and local generation needs ${GATE_MIN_GB} GB"
else
  # First tier the machine satisfies wins; the config keeps them descending.
  for ((index = 0; index < TIER_COUNT; index++)); do
    tier_min="$(read_config_key "local_generation_tier_${index}_min_memory_gb")"
    tier_model="$(read_config_key "local_generation_tier_${index}_model")"
    if [[ -n "$tier_min" && "$HOST_MEMORY_GB" -ge "$tier_min" ]]; then
      GENERATION_MODEL="$tier_model"
      GENERATION_TIER_MIN="$tier_min"
      GENERATION_RESIDENT_GB="$(read_config_key "local_generation_tier_${index}_resident_gb")"
      break
    fi
  done
  [[ -n "$GENERATION_MODEL" ]] || GENERATION_SKIPPED_BECAUSE="no tier matched ${HOST_MEMORY_GB} GB"
fi

MODELS=("$EMBEDDING_MODEL")
[[ -n "$GENERATION_MODEL" ]] && MODELS+=("$GENERATION_MODEL")

# --- 0c. what it will actually cost to run ------------------------------------
# Total memory decides which model is offered; free memory right now decides
# whether the install is about to be unpleasant. They are different questions,
# so both get asked.
host_available_gb() {
  case "$(uname -s)" in
    Darwin)
      # Free plus the pages macOS can hand back without swapping. An
      # approximation, and labelled as one where it is printed.
      vm_stat 2>/dev/null | awk '
        /page size of/ { for (i = 1; i <= NF; i++) if ($i ~ /^[0-9]+$/) size = $i }
        /Pages free/ || /Pages inactive/ || /Pages speculative/ { gsub(/\./, "", $NF); pages += $NF }
        END { if (size > 0) printf "%d", pages * size / 1024 / 1024 / 1024; else printf "0" }'
      ;;
    Linux)
      awk '/^MemAvailable:/ { printf "%d", $2 / 1024 / 1024; exit }' /proc/meminfo 2>/dev/null || printf '0'
      ;;
    *) printf '0' ;;
  esac
}
EMBEDDING_RESIDENT_GB="$(read_config_key local_generation_embedding_resident_gb)"
SYSTEM_HEADROOM_GB="$(read_config_key local_generation_system_headroom_gb)"
: "${EMBEDDING_RESIDENT_GB:=0}" "${SYSTEM_HEADROOM_GB:=0}" "${GENERATION_RESIDENT_GB:=0}"
AVAILABLE_GB="$(host_available_gb)"
NEEDED_GB=$((EMBEDDING_RESIDENT_GB + GENERATION_RESIDENT_GB + SYSTEM_HEADROOM_GB))

echo "==> this machine: ${HOST_OS} ${HOST_ARCH}, ${HOST_MEMORY_GB} GB total, ~${AVAILABLE_GB} GB free right now"
if [[ -n "$GENERATION_MODEL" ]]; then
  ok "local generation: $GENERATION_MODEL (tier >= ${GENERATION_TIER_MIN} GB)"
else
  warn "no local generation model: $GENERATION_SKIPPED_BECAUSE"
  note "Everything else still works. Chat and the rest run through a remote"
  note "provider key; what is unavailable is keeping classification,"
  note "distillation and monitors on this machine."
fi

# Warn on what is free now, not on what the machine has. A 64 GB laptop with
# 3 GB free will swap through this install exactly like a small one.
if [[ "$NEEDED_GB" -gt 0 && "$AVAILABLE_GB" -gt 0 && "$AVAILABLE_GB" -lt "$NEEDED_GB" ]]; then
  warn "only ~${AVAILABLE_GB} GB is free and Magician wants about ${NEEDED_GB} GB to run comfortably"
  note "embedding model  ~${EMBEDDING_RESIDENT_GB} GB, resident whenever Magician runs"
  if [[ -n "$GENERATION_MODEL" ]]; then
    note "generation model ~${GENERATION_RESIDENT_GB} GB, resident while local work is in flight"
  fi
  note "system and stack ~${SYSTEM_HEADROOM_GB} GB"
  note "The pull will still work. What suffers is everything else on this"
  note "machine while a local model is loaded — close what you can, or run"
  note "with a remote provider and no local generation model."
elif [[ "$NEEDED_GB" -gt 0 ]]; then
  ok "memory: about ${NEEDED_GB} GB wanted, ~${AVAILABLE_GB} GB free"
fi

# --- 1. install Ollama if absent ----------------------------------------------
if ! command -v ollama >/dev/null 2>&1; then
  case "$(uname -s)" in
    Linux)
      echo "==> installing Ollama (Linux) ..."
      curl -fsSL https://ollama.com/install.sh | sh
      ;;
    Darwin)
      err "Ollama not found. Install the macOS app from https://ollama.com/download"
      err "(or 'brew install ollama'), start it, then re-run this script."
      exit 1
      ;;
    *) err "unsupported OS for Ollama install: $(uname -s)"; exit 1 ;;
  esac
fi
ok "ollama present"

# --- 2. ensure the daemon is reachable (start in background if needed) ---------
if ! curl -sf "$OLLAMA_URL/api/tags" >/dev/null 2>&1; then
  echo "==> starting 'ollama serve' through Magician lifecycle helper ..."
  MAGICIAN_OLLAMA_URL="$OLLAMA_URL" MAGICIAN_OLLAMA_PREWARM=false \
    MAGICIAN_OLLAMA_EMBEDDING_AUTOSTART=false \
    bash "$SCRIPT_DIR/run-ollama.sh"
  for _ in $(seq 1 30); do
    curl -sf "$OLLAMA_URL/api/tags" >/dev/null 2>&1 && break
    sleep 1
  done
fi
if curl -sf "$OLLAMA_URL/api/tags" >/dev/null 2>&1; then
  ok "ollama reachable at $OLLAMA_URL"
else
  err "ollama not reachable at $OLLAMA_URL — start it manually and re-run"
  exit 1
fi

# --- 3. pull configured embedding + mapped generation models ------------------
# Idempotent: skip the pull entirely when the exact tag is already installed.
# (`ollama pull` also short-circuits an up-to-date model, but this avoids even
# the manifest round-trip and makes the no-op obvious.)
model_present() {
  ollama list 2>/dev/null | awk 'NR>1 {print $1}' | grep -qx "$1"
}
for model in "${MODELS[@]}"; do
  if model_present "$model"; then
    ok "$model already present (skipping pull)"
    continue
  fi
  # Woof is 4-bit MLX safetensors, not an ollama.com tag.
  if [[ "$model" == "woof-4b" ]]; then
    echo "==> installing $model from the local-generation kitty ..."
    bash "$SCRIPT_DIR/setup-local-generation-model.sh" woof-4b
    continue
  fi
  echo "==> pulling $model ..."
  if ollama pull "$model"; then ok "pulled $model"; else err "failed to pull $model"; fi
done

# --- 4. point the runtime at the model that was actually pulled ---------------
# Every Ollama generation profile reads `selected` through a YAML anchor, so
# this one line is what the runtime will load. Written with Ruby rather than
# sed: the file is the operator's live config and a loose pattern here would
# rewrite a profile instead.
if [[ -n "$GENERATION_MODEL" ]]; then
  SELECTED_NOW="$(read_config_key local_generation_selected)"
  if [[ "$SELECTED_NOW" != "$GENERATION_MODEL" ]]; then
    if ruby -e '
      path, model = ARGV
      text = File.read(path)
      pattern = /^(\s*selected: &local_generation_model ).*$/
      matches = text.scan(pattern).length
      abort "expected exactly one selected anchor, found #{matches}" unless matches == 1
      File.write(path, text.sub(pattern) { "#{Regexp.last_match(1)}#{model}" })
    ' "$CONFIG_PATH" "$GENERATION_MODEL"; then
      ok "config now selects $GENERATION_MODEL (was $SELECTED_NOW) in $CONFIG_PATH"
    else
      err "could not update local_generation.selected in $CONFIG_PATH; it still says $SELECTED_NOW"
      err "the runtime would load a model that was never pulled — fix this before starting"
      exit 1
    fi
  else
    ok "config already selects $GENERATION_MODEL"
  fi
fi

MAGICIAN_OLLAMA_URL="$OLLAMA_URL" bash "$SCRIPT_DIR/run-ollama.sh"

echo
echo "Done. Ollama on $OLLAMA_URL with:"
for model in "${MODELS[@]}"; do echo "  - $model"; done
echo "Generation is served on localhost:11434; pinned embeddings use localhost:11435."
echo "The magician container reaches both over --network host."
