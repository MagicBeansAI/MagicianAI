#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "$(uname -s)" == Darwin ]]; then
  export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
fi
MODE="${1:-setup}"
case "$MODE" in setup|--check-binaries|--install-binaries) ;; *) echo "Unknown media setup mode: $MODE" >&2; exit 2 ;; esac

verify_binaries() {
  local filters encoders required
  command -v ffmpeg >/dev/null && command -v ffprobe >/dev/null || return 1
  ffmpeg -version >/dev/null 2>&1 && ffprobe -version >/dev/null 2>&1 || return 1
  filters="$(ffmpeg -hide_banner -filters 2>/dev/null)" || return 1
  encoders="$(ffmpeg -hide_banner -encoders 2>/dev/null)" || return 1
  # These are dependencies of the shipped media_edit operations, not provider
  # generation options. A runnable minimal FFmpeg can still omit drawtext.
  for required in scale pad concat drawtext atempo setpts adelay atrim asetpts; do
    if ! awk -v required="$required" '$2 == required {found=1} END {exit !found}' <<< "$filters"; then
      echo "FFmpeg is missing the required media_edit filter: $required" >&2
      return 1
    fi
  done
  for required in libx264 aac libmp3lame; do
    if ! awk -v required="$required" '$2 == required {found=1} END {exit !found}' <<< "$encoders"; then
      echo "FFmpeg is missing the required media_edit encoder: $required" >&2
      return 1
    fi
  done
}

if [[ "$MODE" == --check-binaries ]]; then
  verify_binaries || { echo "Install a full FFmpeg build and ffprobe (macOS: make setup-ffmpeg)." >&2; exit 1; }
  echo "✓ FFmpeg/ffprobe and media_edit filters/encoders available"
  exit 0
fi
if ! verify_binaries; then
  if [[ "$(uname -s)" = Darwin ]] && command -v brew >/dev/null; then
    HOMEBREW_NO_AUTO_UPDATE=1 brew install ffmpeg-full
    # Retain the minimal package, but select the full binaries for native
    # editing and governed pack-local links. No formula is uninstalled.
    if brew list --versions ffmpeg >/dev/null 2>&1; then brew unlink ffmpeg; fi
    brew link --force ffmpeg-full
  else
    echo "Install full ffmpeg and ffprobe using the system package manager, then retry media setup." >&2
    exit 1
  fi
fi
verify_binaries
if [[ "$MODE" == --install-binaries ]]; then exit 0; fi
make -C "$ROOT_DIR/skillshub" setup-wizard-media \
  DATA_ROOT="${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}" \
  SCOPE="${MAGICIAN_SETUP_SCOPE:-anonymous/default}"
