#!/usr/bin/env bash
# Install or pin one local generation model from the kitty.
#
# Usage:
#   scripts/setup-local-generation-model.sh              # print the catalog
#   scripts/setup-local-generation-model.sh woof-4b
#   scripts/setup-local-generation-model.sh woof-4b --select
#   scripts/setup-local-generation-model.sh woof-4b --configured
#   make setup-local-generation MODEL=woof-4b SELECT=1
#
# Does not change the auto-tier. `selected` is only rewritten with --select.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DATA_DIR="${MAGICIAN_ROOT_DIR:-${MAGICIAN_STORAGE_PATH:-$HOME/MagicianNotes}}"
OLLAMA_URL="${MAGICIAN_OLLAMA_URL:-http://127.0.0.1:11434}"
CATALOG="$ROOT_DIR/data/magician_v2/local_generation_catalog.yaml"
TEMPLATE="$SCRIPT_DIR/ollama-woof-4b.Modelfile"

ok()   { printf '  OK %s\n' "$*"; }
err()  { printf '  ERROR %s\n' "$*" >&2; }
note() { printf '       %s\n' "$*"; }

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

models_dir() {
  if [[ -n "${MAGICIAN_MODELS_DIR:-}" ]]; then
    printf '%s' "$MAGICIAN_MODELS_DIR"
  elif [[ -d /Volumes/build/magician/models ]]; then
    printf '%s' /Volumes/build/magician/models
  else
    printf '%s' "$DATA_DIR/models"
  fi
}

catalog_field() {
  local id="$1" key="$2"
  python3 - "$CATALOG" "$id" "$key" <<'PY'
import sys
from pathlib import Path
path, model_id, key = sys.argv[1], sys.argv[2], sys.argv[3]
text = Path(path).read_text(encoding="utf-8")
# Tiny YAML subset: models: list of maps with scalar values.
items = []
cur = None
for raw in text.splitlines():
    if raw.startswith("  - id:"):
        if cur:
            items.append(cur)
        cur = {"id": raw.split(":", 1)[1].strip()}
    elif cur is not None and raw.startswith("    ") and ":" in raw:
        k, _, v = raw.strip().partition(":")
        cur[k.strip()] = v.strip().strip('"')
if cur:
    items.append(cur)
row = next((m for m in items if m.get("id") == model_id), None)
if row is None:
    sys.exit(2)
print(row.get(key, ""))
PY
}

print_catalog() {
  python3 - "$CATALOG" "$CONFIG_PATH" <<'PY'
import sys
from pathlib import Path
catalog, config = sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else ""
text = Path(catalog).read_text(encoding="utf-8")
items, cur = [], None
for raw in text.splitlines():
    if raw.startswith("  - id:"):
        if cur:
            items.append(cur)
        cur = {"id": raw.split(":", 1)[1].strip()}
    elif cur is not None and raw.startswith("    ") and ":" in raw:
        k, _, v = raw.strip().partition(":")
        cur[k.strip()] = v.strip().strip('"')
if cur:
    items.append(cur)
selected = ""
if config and Path(config).exists():
    for line in Path(config).read_text(encoding="utf-8").splitlines():
        if "selected: &local_generation_model" in line:
            selected = line.split()[-1]
            break
print(f"{'id':<18} {'disk':>5} {'RAM':>4} {'classify':>9} {'distill':>8} {'browser':>8}  notes")
for m in items:
    classify = m.get("classify_agree_pct") or "—"
    distill = m.get("distill_recall_pct") or "—"
    if classify not in {"—", "null"}:
        classify = f"{float(classify):.0f}%"
    else:
        classify = "—"
    if distill not in {"—", "null"}:
        distill = f"{float(distill):.0f}%"
    else:
        distill = "—"
    mark = "  <-- selected" if m.get("ollama") == selected or m.get("id") == selected else ""
    print(
        f"{m['id']:<18} {m.get('disk_gb','?'):>4}G {m.get('resident_gb','?'):>3}G "
        f"{classify:>9} {distill:>8} {m.get('browser_effect','?'):>8}  "
        f"{m.get('label','')}{mark}"
    )
print()
print(f"selected: {selected or '(none)'}")
print("Install:  make setup-local-generation MODEL=<id>")
print("Pin it:   make setup-local-generation MODEL=<id> SELECT=1")
print("JSON workers stay think:false regardless of which model is selected.")
PY
}

model_present() {
  local want="$1"
  ollama list 2>/dev/null | awk 'NR>1 {print $1}' | grep -Eqx "${want}(:latest)?"
}

ensure_ollama() {
  if ! command -v ollama >/dev/null 2>&1; then
    err "ollama is not on PATH"
    exit 1
  fi
  export OLLAMA_HOST="${OLLAMA_HOST:-$OLLAMA_URL}"
  if ! curl -sf "${OLLAMA_URL}/api/tags" >/dev/null 2>&1; then
    err "ollama is not reachable at $OLLAMA_URL — start it (make run-ollama) and retry"
    exit 1
  fi
}

select_model() {
  local model="$1"
  if [[ -z "$CONFIG_PATH" || ! -f "$CONFIG_PATH" ]]; then
    err "no magician-config.yaml to pin selected="
    exit 1
  fi
  ruby -e '
    path, model = ARGV
    text = File.read(path)
    pattern = /^(\s*selected: &local_generation_model ).*$/
    matches = text.scan(pattern).length
    abort "expected exactly one selected anchor, found #{matches}" unless matches == 1
    File.write(path, text.sub(pattern) { "#{Regexp.last_match(1)}#{model}" })
  ' "$CONFIG_PATH" "$model"
  ok "config now selects $model in $CONFIG_PATH"
  note "Restart Magician (make restart-supervisor) so profiles reload the anchor."
}

install_woof() {
  local dest weights tmpfile
  dest="$(models_dir)/mlx/Underdog-Woof-4B-1.1"
  weights="$dest/model.safetensors"
  mkdir -p "$dest"
  if [[ -s "$weights" ]]; then
    ok "weights already at $weights"
  else
    echo "==> downloading ConwayResearch/Underdog-Woof-4B-1.1 to $dest"
    if command -v huggingface-cli >/dev/null 2>&1; then
      huggingface-cli download ConwayResearch/Underdog-Woof-4B-1.1 --local-dir "$dest"
    else
      python3 - "$dest" <<'PY'
import sys
from huggingface_hub import snapshot_download
snapshot_download("ConwayResearch/Underdog-Woof-4B-1.1", local_dir=sys.argv[1])
PY
    fi
    [[ -s "$weights" ]] || { err "download finished without $weights"; exit 1; }
    ok "downloaded $weights"
  fi
  tmpfile="$(mktemp -t woof-modelfile)"
  sed "s|__WOOF_WEIGHTS_DIR__|$dest|" "$TEMPLATE" > "$tmpfile"
  echo "==> ollama create --experimental woof-4b"
  ollama create --experimental woof-4b -f "$tmpfile"
  rm -f "$tmpfile"
  ok "woof-4b is in Ollama"
}

install_gemma() {
  echo "==> pulling gemma4:12b"
  ollama pull gemma4:12b
  ok "gemma4:12b present"
}

install_qwen() {
  local gguf
  gguf="$(models_dir)/Qwen3.8-27B-UD-Q2_K_XL.gguf"
  if model_present qwen3.8-ud2-mtp; then
    ok "qwen3.8-ud2-mtp already present"
    return 0
  fi
  if [[ -s "$gguf" ]]; then
    echo "==> ollama create qwen3.8-ud2-mtp from $gguf"
    # The checked-in Modelfile names the SSD1 path; point FROM at the GGUF
    # models_dir actually resolved (a missing FROM path reads as a model name
    # and fails with "invalid model name").
    local modelfile
    modelfile="$(mktemp)"
    sed "s|^FROM .*|FROM $gguf|" "$SCRIPT_DIR/ollama-qwen38-ud2-mtp.Modelfile" > "$modelfile"
    ollama create qwen3.8-ud2-mtp -f "$modelfile"
    rm -f "$modelfile"
    ok "qwen3.8-ud2-mtp created"
    return 0
  fi
  err "qwen3.8-ud2-mtp is not installed and $gguf is missing"
  note "See docs/components/magician/local-channel-llm.md for the GGUF curl."
  exit 1
}

SELECT=0
CONFIGURED=0
MODEL=""
for arg in "$@"; do
  case "$arg" in
    --select) SELECT=1 ;;
    --configured) CONFIGURED=1 ;;
    --list|-h|--help) print_catalog; exit 0 ;;
    -*) err "unknown flag $arg"; exit 1 ;;
    *) MODEL="$arg" ;;
  esac
done

if [[ -z "$MODEL" ]]; then
  print_catalog
  exit 0
fi

if [[ ! -f "$CATALOG" ]]; then
  err "catalog missing: $CATALOG"
  exit 1
fi
if ! catalog_field "$MODEL" id >/dev/null; then
  err "unknown model '$MODEL' — run without args to list the kitty"
  exit 1
fi

ensure_ollama
kind="$(catalog_field "$MODEL" install)"
tag="$(catalog_field "$MODEL" ollama)"
: "${tag:=$MODEL}"

if model_present "$tag"; then
  ok "$tag already present"
else
  case "$kind" in
    mlx-safetensors) install_woof ;;
    ollama-pull) install_gemma ;;
    create-gguf) install_qwen ;;
    *) err "catalog install kind '$kind' is not implemented"; exit 1 ;;
  esac
fi

if [[ "$SELECT" -eq 1 ]]; then
  select_model "$tag"
elif [[ "$CONFIGURED" -eq 1 ]]; then
  ok "$tag is installed and remains selected in $CONFIG_PATH"
else
  note "Installed but not pinned. Pass --select (or SELECT=1) to point runtime.ollama.local_generation.selected at $tag."
fi
