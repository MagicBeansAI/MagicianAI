#!/usr/bin/env bash
# Install the decision engine's local ONNX runtime and models, each pinned
# and verified (structured-decision plan Part IV, E5).
#
#   scripts/setup-decision-models.sh [COMPONENT...]
#
# Components (default: onnxruntime laya-multilingual):
#   onnxruntime        ONNX Runtime 1.30.0 (Microsoft release, SHA-256 pinned)
#                      -> <root>/lib/onnxruntime/  (Kev needs >= 1.30)
#   laya-multilingual  int8 laya (soyelmismo/laya-multilingual-onnx, pinned)
#   laya-multilingual-mlx
#                      laya for the GPU (`laya-mlx`, a decision engine built
#                      with the `mlx` feature): the original weights
#                      (convaiinnovations/laya-multilingual, pinned), the
#                      encoder config, and the tokenizer, SHA-256 checked
#   laya-english-mlx   the English laya (convaiinnovations/laya, pinned):
#                      ModernBERT-large, 512-token context, calibrated
#   kev-0.8b, kev-4b   kev.js q8f32 bundles (ai-ecoverse/kev.js, pinned),
#                      each file checked against the SHA-256 Hugging Face
#                      records at the pinned commit
#   kev-0.8b-mlx,      Kev for the GPU (`kev-mlx`, a decision engine built
#   kev-4b-mlx         with the `mlx` feature): the Qwen3.5 base and the Kev
#                      LoRA at the revisions kev-rs pins, the pointer head
#                      from the pinned kev.js bundle, and the checkpoint's
#                      fitted temperature; large files SHA-256 checked
# Where things land (what the decision engine reads by default):
#   models          DECISION_MODELS_DIR, default <root>/models/decision/<name>/
#                   (an entry names the folder: `model_dir: <name>`; the
#                   engine's `models_dir` setting says where the folders are)
#   ONNX Runtime    DECISION_ORT_DIR, default <root>/lib/onnxruntime/
#                   (the engine's `onnxruntime_path` setting)
# <root> is DECISION_MODELS_ROOT, else the runtime root the decision engine
# resolves (MAGICIAN_ROOT_DIR, MAGICIAN_STORAGE_PATH, ~/MagicianNotes). When
# a folder other than the default is used, the lines to add to
# decision-engine.yaml are printed at the end.
set -euo pipefail

root="${DECISION_MODELS_ROOT:-${MAGICIAN_ROOT_DIR:-${MAGICIAN_STORAGE_PATH:-$HOME/MagicianNotes}}}"
models_dir="${DECISION_MODELS_DIR:-$root/models/decision}"
ort_dir="${DECISION_ORT_DIR:-$root/lib/onnxruntime}"
components=("$@")
[[ ${#components[@]} -gt 0 ]] || components=(onnxruntime laya-multilingual)

ORT_VERSION="1.30.0"
KEVJS_REV="74e6ccbc79201da4beb63385802fa3e6bcc92914"
LAYA_REPO="soyelmismo/laya-multilingual-onnx"
LAYA_REV="0966c4fa58da6878b39e7e14cb5e93313b82d828"
LAYA_SRC_REPO="convaiinnovations/laya-multilingual"
LAYA_SRC_REV="052592a15d198d9ad47da779604259b10b47b7aa"
LAYA_EN_REPO="convaiinnovations/laya"
LAYA_EN_REV="55cf4c4ebb4ebe31b2550e8bdf3bd21b99753851"

fetch() {  # url dest [sha256]
  local url="$1" dest="$2" sha="${3:-}"
  if [[ -n "$sha" && -f "$dest" ]] && echo "$sha  $dest" | shasum -a 256 -c --status; then
    return 0
  fi
  curl -sfL --http1.1 --retry 5 --retry-all-errors -o "$dest.part" "$url"
  if [[ -n "$sha" ]] && ! echo "$sha  $dest.part" | shasum -a 256 -c --status; then
    rm -f "$dest.part"
    echo "   ✗ $(basename "$dest"): SHA-256 mismatch — refusing it" >&2
    exit 1
  fi
  mv "$dest.part" "$dest"
}

install_onnxruntime() {
  local platform sha
  case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) platform=osx-arm64; sha=6ebb5062a934537c352937821f9fe9718e7de1a2db1122a93dd363ffd53a7012 ;;
    Linux-x86_64) platform=linux-x64; sha=a5ed5a3cac51fbb2e90da632ae43d19212faaa20e76484e62bcb7c23ddb3b3fd ;;
    Linux-aarch64 | Linux-arm64) platform=linux-aarch64; sha=e16a27a8ed330bbc698df7330b0cf56e722f354e3bcc92118682c74ef3c3e3da ;;
    *) echo "   ✗ no pinned ONNX Runtime $ORT_VERSION build for $(uname -s)-$(uname -m)" >&2; exit 1 ;;
  esac
  local dest="$ort_dir" work
  mkdir -p "$dest"
  if compgen -G "$dest/libonnxruntime.$ORT_VERSION.*" >/dev/null || [[ -e "$dest/libonnxruntime.so.$ORT_VERSION" ]]; then
    echo "📦 ONNX Runtime $ORT_VERSION already in $dest"
    return 0
  fi
  work="$(mktemp -d)"
  echo "📥 ONNX Runtime $ORT_VERSION ($platform) -> $dest"
  fetch "https://github.com/microsoft/onnxruntime/releases/download/v$ORT_VERSION/onnxruntime-$platform-$ORT_VERSION.tgz" "$work/ort.tgz" "$sha"
  tar -xzf "$work/ort.tgz" -C "$work"
  # The library and its version symlinks; not the debug-symbol bundle.
  for lib in "$work/onnxruntime-$platform-$ORT_VERSION"/lib/libonnxruntime*; do
    [[ -d "$lib" ]] || cp -P "$lib" "$dest/"
  done
  rm -rf "$work"
  echo "   ✓ $(ls "$dest" | tr '\n' ' ')"
}

install_laya() {
  local dest="$models_dir/laya-multilingual"
  mkdir -p "$dest"
  echo "📥 laya-multilingual ($LAYA_REPO@${LAYA_REV:0:7}) -> $dest"
  local base="https://huggingface.co/$LAYA_REPO/resolve/$LAYA_REV"
  fetch "$base/model.onnx" "$dest/model.onnx" d389d2304822a59569387e257067360a84e016aed43b407f1cfde87dadb7e485
  fetch "$base/tokenizer.json" "$dest/tokenizer.json" 609d8f4c067cd3950f88594c5a802616cea245823836ef5848ee4fc40aab5b6f
  fetch "$base/tokenizer/tokenizer_config.json" "$dest/tokenizer_config.json"
  fetch "$base/rl_agent_config.json" "$dest/rl_agent_config.json"
  echo "   ✓ entry: { adapter: laya-onnx, model: laya-multilingual-int8, model_dir: laya-multilingual }"
}

# "path sha256" for each file of REPO at REV under PREFIX (sha256 empty for
# a small, non-LFS file).
tree_files() {  # repo rev [prefix]
  local listing
  listing="$(curl -sfL --http1.1 --retry 5 "https://huggingface.co/api/models/$1/tree/$2${3:+/$3}?recursive=true")"
  python3 -c '
import json, sys
for f in json.loads(sys.argv[1]):
    if f.get("type") == "file":
        print(f["path"], (f.get("lfs") or {}).get("oid", ""))
' "$listing"
}

install_kev_mlx() {  # kev-0.8b-mlx | kev-4b-mlx
  local name="$1" kev="${1%-mlx}" dest="$models_dir/$1"
  local base base_rev adapter_rev temperature
  case $kev in
    kev-0.8b) base=Qwen/Qwen3.5-0.8B-Base; base_rev=dc7cdfe2ee4154fa7e30f5b51ca41bfa40174e68
              adapter_rev=54f4f8777356cd5bbbb6c6919c657f26e6f2f6d8; temperature=2.406050072164233 ;;
    kev-4b)   base=Qwen/Qwen3.5-4B-Base; base_rev=1001bb4d826a52d1f399e183466143f4da7b741b
              adapter_rev=485ace8703592fcf405488b262449990824cfed1; temperature=2.1435469250725863 ;;
  esac
  mkdir -p "$dest/base" "$dest/adapter"
  echo "📥 $name ($base@${base_rev:0:7} + jaredpalmer/$kev@${adapter_rev:0:7}) -> $dest"
  tree_files "$base" "$base_rev" | while read -r path sha; do
    [[ "$path" == .gitattributes ]] && continue
    fetch "https://huggingface.co/$base/resolve/$base_rev/$path" "$dest/base/$path" "$sha"
  done
  tree_files "jaredpalmer/$kev" "$adapter_rev" | while read -r path sha; do
    case "$path" in adapter_config.json | adapter_model.safetensors) ;; *) continue ;; esac
    fetch "https://huggingface.co/jaredpalmer/$kev/resolve/$adapter_rev/$path" "$dest/adapter/$path" "$sha"
  done
  # The bundle's manifest names the head's path (files.head).
  local head
  head="$kev/$(curl -sfL --http1.1 --retry 5 "https://huggingface.co/ai-ecoverse/kev.js/resolve/$KEVJS_REV/$kev/manifest.json" \
    | python3 -c 'import json, sys; print(json.load(sys.stdin)["files"]["head"])')"
  tree_files ai-ecoverse/kev.js "$KEVJS_REV" "$kev" | while read -r path sha; do
    [[ "$path" == "$head" ]] || continue
    fetch "https://huggingface.co/ai-ecoverse/kev.js/resolve/$KEVJS_REV/$path" "$dest/head.safetensors" "$sha"
  done
  [[ -f "$dest/head.safetensors" ]] || { echo "   ✗ $kev: no pointer head in the kev.js bundle" >&2; exit 1; }
  printf '{"meta": {"temperature": %s}}\n' "$temperature" > "$dest/head.meta.json"
  echo "   ✓ $(du -sh "$dest" | cut -f1); entry: { adapter: kev-mlx, model: $name, model_dir: $name }"
}

install_laya_mlx() {
  local dest="$models_dir/laya-multilingual-mlx"
  mkdir -p "$dest/encoder"
  echo "📥 laya-multilingual-mlx ($LAYA_SRC_REPO@${LAYA_SRC_REV:0:7}) -> $dest"
  local base="https://huggingface.co/$LAYA_SRC_REPO/resolve/$LAYA_SRC_REV"
  fetch "$base/model.safetensors" "$dest/model.safetensors" 9d628fd971b700382ac6f65920a86f149777b2e748e0c955fb3b19695aa8f204
  fetch "$base/encoder/config.json" "$dest/encoder/config.json"
  fetch "$base/rl_agent_config.json" "$dest/rl_agent_config.json"
  fetch "$base/tokenizer/tokenizer.json" "$dest/tokenizer.json" 609d8f4c067cd3950f88594c5a802616cea245823836ef5848ee4fc40aab5b6f
  fetch "$base/tokenizer/tokenizer_config.json" "$dest/tokenizer_config.json"
  echo "   ✓ $(du -sh "$dest" | cut -f1); entry: { adapter: laya-mlx, model: laya-multilingual-mlx, model_dir: laya-multilingual-mlx }"
}

install_laya_english_mlx() {
  local dest="$models_dir/laya-english-mlx"
  mkdir -p "$dest/encoder"
  echo "📥 laya-english-mlx ($LAYA_EN_REPO@${LAYA_EN_REV:0:7}) -> $dest"
  local base="https://huggingface.co/$LAYA_EN_REPO/resolve/$LAYA_EN_REV"
  fetch "$base/model.safetensors" "$dest/model.safetensors" 891102d372688fc2a094dac56a384bc537b87c63f21f9f3dac0be2b7cbc8d86c
  fetch "$base/encoder/config.json" "$dest/encoder/config.json"
  fetch "$base/rl_agent_config.json" "$dest/rl_agent_config.json"
  fetch "$base/tokenizer/tokenizer.json" "$dest/tokenizer.json"
  fetch "$base/tokenizer/tokenizer_config.json" "$dest/tokenizer_config.json"
  echo "   ✓ $(du -sh "$dest" | cut -f1); entry: { adapter: laya-mlx, model: laya-english-mlx, model_dir: laya-english-mlx }"
}

install_kev() {  # kev-0.8b | kev-4b
  local name="$1" dest="$models_dir/$1"
  mkdir -p "$dest"
  echo "📥 $name (ai-ecoverse/kev.js@${KEVJS_REV:0:7}, q8f32) -> $dest"
  local base="https://huggingface.co/ai-ecoverse/kev.js/resolve/$KEVJS_REV/$name"
  fetch "$base/manifest.json" "$dest/manifest.json"
  local tree
  tree="$(curl -sfL --http1.1 --retry 5 "https://huggingface.co/api/models/ai-ecoverse/kev.js/tree/$KEVJS_REV/$name?recursive=true")"
  # Each file the q8f32 variant needs, with its SHA-256 at the pinned commit.
  python3 - "$dest/manifest.json" "$tree" <<'PY' | while read -r path sha; do
import json, sys
manifest = json.load(open(sys.argv[1]))
oids = {f["path"]: (f.get("lfs") or {}).get("oid", "") for f in json.loads(sys.argv[2]) if f.get("type") == "file"}
name = manifest["name"]
variant = manifest["variants"]["q8f32"]
for path in [variant["model"], *variant["data"], *manifest["files"].values()]:
    print(path, oids.get(f"{name}/{path}", ""))
PY
    fetch "$base/$path" "$dest/$(basename "$path")" "$sha"
  done
  echo "   ✓ $(du -sh "$dest" | cut -f1); entry: { adapter: kev-onnx, model: $name-q8f32, model_dir: $name }"
}

for component in "${components[@]}"; do
  case "$component" in
    onnxruntime) install_onnxruntime ;;
    laya-multilingual) install_laya ;;
    laya-multilingual-mlx) install_laya_mlx ;;
    laya-english-mlx) install_laya_english_mlx ;;
    kev-0.8b | kev-4b) install_kev "$component" ;;
    kev-0.8b-mlx | kev-4b-mlx) install_kev_mlx "$component" ;;
    *) echo "unknown component: $component (onnxruntime, laya-multilingual, laya-multilingual-mlx, laya-english-mlx, kev-0.8b, kev-4b, kev-0.8b-mlx, kev-4b-mlx)" >&2; exit 2 ;;
  esac
done
echo "✅ decision models ready: models in $models_dir, ONNX Runtime in $ort_dir"
if [[ "$models_dir" != "$root/models/decision" || "$ort_dir" != "$root/lib/onnxruntime" ]]; then
  echo "   Point the engine at them in decision-engine.yaml:"
  [[ "$models_dir" != "$root/models/decision" ]] && echo "     models_dir: $models_dir"
  [[ "$ort_dir" != "$root/lib/onnxruntime" ]] && echo "     onnxruntime_path: $ort_dir"
fi
true
