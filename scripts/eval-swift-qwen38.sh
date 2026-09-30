#!/usr/bin/env bash
# Bake-off ukisai/Swift-Qwen3.8-27B-GGUF against production qwen3.8-ud2-mtp.
#
# Same harness as the 2026-09-13 kitty table:
#   channel  = scripts/golden-eval-channel.py  (think:false, classify agree, distill recall, tok/s)
#   browser  = scripts/probe-browser-tools-real.py  (think:true, real Chrome fixtures)
#
# Does NOT rewrite runtime.ollama.local_generation.selected.
#
# This 32 GB host cannot keep Magician + embedder + Swift Q4_K_M resident.
# Run on a 48 GB+ machine (or stop the stack and try Q2_K here).
#
# Usage:
#   scripts/eval-swift-qwen38.sh                     # Q4_K_M, download + eval
#   scripts/eval-swift-qwen38.sh --quant Q2_K        # same-RAM band as UD-Q2_K_XL
#   scripts/eval-swift-qwen38.sh --quant Q4_K_M --skip-download
#   scripts/eval-swift-qwen38.sh --dry-run
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DATA_DIR="${MAGICIAN_ROOT_DIR:-${MAGICIAN_STORAGE_PATH:-$HOME/MagicianNotes}}"
OLLAMA_URL="${MAGICIAN_OLLAMA_URL:-http://127.0.0.1:11434}"
TEMPLATE="$SCRIPT_DIR/ollama-swift-qwen38.Modelfile"
HF_REPO="ukisai/Swift-Qwen3.8-27B-GGUF"
BASELINE="qwen3.8-ud2-mtp"

QUANT="Q4_K_M"
SKIP_DOWNLOAD=0
SKIP_CHANNEL=0
SKIP_BROWSER=0
DRY=0

ok()   { printf '  OK %s\n' "$*"; }
err()  { printf '  ERROR %s\n' "$*" >&2; }
note() { printf '       %s\n' "$*"; }

models_dir() {
  if [[ -n "${MAGICIAN_MODELS_DIR:-}" ]]; then
    printf '%s' "$MAGICIAN_MODELS_DIR"
  elif [[ -d /Volumes/build/magician/models ]]; then
    printf '%s' /Volumes/build/magician/models
  else
    printf '%s' "$DATA_DIR/models"
  fi
}

coverage_dir() {
  if [[ -n "${COVERAGE_BASE_DIR:-}" ]]; then
    printf '%s' "$COVERAGE_BASE_DIR"
  elif [[ -d /Volumes/build/magician/coverage ]]; then
    printf '%s' /Volumes/build/magician/coverage
  else
    printf '%s' "$ROOT_DIR/coverage"
  fi
}

usage() {
  sed -n '2,18p' "$0" | sed 's/^# \?//'
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --quant) QUANT="${2:?}"; shift 2 ;;
    --skip-download) SKIP_DOWNLOAD=1; shift ;;
    --skip-channel) SKIP_CHANNEL=1; shift ;;
    --skip-browser) SKIP_BROWSER=1; shift ;;
    --dry-run) DRY=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) err "unknown arg $1"; usage; exit 1 ;;
  esac
done

TAG="swift-qwen3.8-$(echo "$QUANT" | tr 'A-Z' 'a-z')"
GGUF="$(models_dir)/Swift-Qwen3.8-27B-${QUANT}.gguf"
HF_URL="https://huggingface.co/${HF_REPO}/resolve/main/Swift-Qwen3.8-27B-${QUANT}.gguf"
STAMP="$(date +%Y-%m-%d)"
OUT="$(coverage_dir)/evals/swift-qwen38/${STAMP}"
DB="${MAGICIAN_MAIL_ASSIST_DB:-$DATA_DIR/scopes/anonymous/default/mail_assist/mail_assist.duckdb}"

case "$QUANT" in
  Q4_K_M) note "Q4_K_M ≈ 18 GB disk, ~19 GB RAM at 32k q8_0. Need ~48 GB host (32 GB will swap)." ;;
  Q2_K)   note "Q2_K ≈ 11 GB disk, ~12 GB RAM — same band as production UD-Q2_K_XL." ;;
  Q6_K)   note "Q6_K ≈ 23 GB disk, ~24 GB RAM. Swift's agentic/tool-call pick." ;;
  *)      note "quant $QUANT — see https://huggingface.co/${HF_REPO} for size." ;;
esac

if [[ "$DRY" -eq 1 ]]; then
  echo "would download $HF_URL"
  echo "would write  $GGUF"
  echo "would create ollama model $TAG (MTP on, think left to the harness)"
  echo "would not pin local_generation.selected"
  echo "channel report $OUT/channel-verdicts.jsonl"
  echo "browser report $OUT/browser-think-true.json"
  echo "mail_assist db $DB $([[ -f "$DB" ]] && echo present || echo MISSING)"
  exit 0
fi

if ! command -v ollama >/dev/null 2>&1; then
  err "ollama is not on PATH"
  exit 1
fi
export OLLAMA_HOST="${OLLAMA_HOST:-$OLLAMA_URL}"
if ! curl -sf "${OLLAMA_URL}/api/tags" >/dev/null 2>&1; then
  err "ollama is not reachable at $OLLAMA_URL — start it (make run-ollama) and retry"
  exit 1
fi

mkdir -p "$(models_dir)" "$OUT"

if [[ ! -s "$GGUF" ]]; then
  if [[ "$SKIP_DOWNLOAD" -eq 1 ]]; then
    err "missing $GGUF and --skip-download was set"
    exit 1
  fi
  echo "==> downloading $HF_URL"
  echo "    -> $GGUF"
  curl -L --fail --continue-at - -o "$GGUF" "$HF_URL"
  [[ -s "$GGUF" ]] || { err "download finished without $GGUF"; exit 1; }
  ok "downloaded $GGUF"
else
  ok "weights already at $GGUF"
fi

if ollama list 2>/dev/null | awk 'NR>1 {print $1}' | grep -Eqx "${TAG}(:latest)?"; then
  ok "$TAG already in Ollama"
else
  tmpfile="$(mktemp -t swift-modelfile)"
  sed "s|__SWIFT_GGUF__|$GGUF|" "$TEMPLATE" > "$tmpfile"
  echo "==> ollama create $TAG"
  ollama create "$TAG" -f "$tmpfile"
  rm -f "$tmpfile"
  ok "$TAG created (draft_num_predict 4)"
fi

MODELS="$TAG"
if ollama list 2>/dev/null | awk 'NR>1 {print $1}' | grep -Eqx "${BASELINE}(:latest)?"; then
  MODELS="${BASELINE},${TAG}"
  ok "baseline $BASELINE present — A/B"
else
  note "baseline $BASELINE not in Ollama — Swift-only run. Compare to the 2026-09-13 table:"
  note "qwen3.8-ud2-mtp classify 68% · distill 82.7% · browser 4/4·3/4 · ~70 tok/s · 11 GB"
fi

if [[ "$SKIP_CHANNEL" -eq 0 ]]; then
  channel_args=(
    python3 "$SCRIPT_DIR/golden-eval-channel.py"
    --models "$MODELS"
    --stop-stack
    --report "$OUT/channel-verdicts.jsonl"
  )
  if [[ -f "$DB" ]]; then
    channel_args+=(--db "$DB")
  else
    note "mail_assist.duckdb missing at $DB — distill only. Copy the db from the live machine for classify."
    channel_args+=(--only distill)
  fi
  echo "==> channel eval (${channel_args[*]})"
  "${channel_args[@]}"
  ok "channel report $OUT/channel-verdicts.jsonl"
fi

if [[ "$SKIP_BROWSER" -eq 0 ]]; then
  echo "==> browser probe think=true (this is the Swift claim: shorter reasoning traces)"
  python3 "$SCRIPT_DIR/probe-browser-tools-real.py" \
    --models "$MODELS" \
    --think true \
    --report "$OUT/browser-think-true.json"
  ok "browser report $OUT/browser-think-true.json"
fi

echo
echo "Bake-off artifacts: $OUT"
echo "Do not pin $TAG. Production stays $BASELINE until these numbers beat the kitty table."
echo "Restart the stack if the channel eval stopped it: make run-supervisor"
